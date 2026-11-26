use std::{hint::black_box, path::PathBuf, time::Instant};

use ckb_types::bytes::Bytes;
use ckb_vm::{elf::parse_elf, memory::sparse::SparseMemory};

fn exit_program(code: u8) -> Bytes {
    let mut image = vec![0u8; 132];
    image[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    image[16..18].copy_from_slice(&2u16.to_le_bytes());
    image[18..20].copy_from_slice(&243u16.to_le_bytes());
    image[20..24].copy_from_slice(&1u32.to_le_bytes());
    image[24..32].copy_from_slice(&0x10078u64.to_le_bytes());
    image[32..40].copy_from_slice(&64u64.to_le_bytes());
    image[52..58].copy_from_slice(&[64, 0, 56, 0, 1, 0]);
    image[64..68].copy_from_slice(&1u32.to_le_bytes());
    image[68..72].copy_from_slice(&5u32.to_le_bytes());
    image[80..88].copy_from_slice(&0x10000u64.to_le_bytes());
    image[96..104].copy_from_slice(&132u64.to_le_bytes());
    image[104..112].copy_from_slice(&132u64.to_le_bytes());
    image[112..120].copy_from_slice(&4096u64.to_le_bytes());
    let instructions = [0x513 | (u32::from(code) << 20), 0x05d00893, 0x73];
    image[120..].copy_from_slice(&instructions.map(u32::to_le_bytes).concat());
    image.into()
}

#[test]
fn repeated_headers_load_current_bytes_and_keep_parse_results() {
    let original = exit_program(0);
    for code in [0, 7, 2, 8, 1, 0] {
        let program = exit_program(code);
        assert_eq!(&program[..120], &original[..120]);
        assert_eq!(
            ckb_vm::run::<u64, SparseMemory<u64>>(&program, &[], 1024 * 1024).unwrap(),
            code as i8
        );
    }
    let mut corpus = vec![original.clone()];
    for i in 0..120 {
        let mut changed = original.to_vec();
        changed[i] ^= 1;
        corpus.push(changed.into());
    }
    for length in [0, 4, 52, 64, 65, 119, 120, 131] {
        corpus.push(original.slice(..length));
    }
    let mut padded = vec![0; 7];
    padded.extend_from_slice(&original);
    corpus.push(Bytes::from(padded).slice(7..));
    let mut digest = ckb_hash::new_blake2b();
    for version in 0..=2 {
        for program in corpus.iter().chain(corpus.iter().rev()) {
            for s in [
                format!("{:?}", parse_elf::<u64>(program, version)),
                format!("{:?}", parse_elf::<u32>(program, version)),
            ] {
                digest.update(s.as_bytes());
                digest.update(b"\n");
            }
        }
    }
    let mut result = [0; 32];
    digest.finalize(&mut result);
    let hex: String = result.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, "d544836c2a4efef2bfa9cf304c25062ff3d5234d7cde97dc9210eab70e0d9f0f");
    println!("ELF differential digest {hex}");
}

#[test]
#[ignore = "pinned CPU parser timing for four real Stem contracts"]
fn repeated_contract_parser_timing() {
    let directory = PathBuf::from(std::env::var_os("STEM_TEST_CONTRACT_DIR").unwrap());
    let contracts: Vec<Bytes> = [
        "stem-lock",
        "stem-lineage",
        "stem-lineage-swap",
        "stem-lineage-reserve-udt",
    ]
    .iter()
    .map(|name| std::fs::read(directory.join(name)).unwrap().into())
    .collect();
    for _ in 0..17 {
        let start = Instant::now();
        for i in 0..200_000 {
            let metadata = parse_elf::<u64>(black_box(&contracts[i % contracts.len()]), 2).unwrap();
            black_box(metadata);
        }
        println!(
            "parser_ns_per_call {:.3}",
            start.elapsed().as_nanos() as f64 / 200_000.0
        );
    }
}
