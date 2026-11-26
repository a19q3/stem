use ckb_vm::{
    ISA_A, ISA_B, RISCV_PAGESIZE, Register, SparseMemory, WXorXMemory,
    decoder::{Decoder, build_decoder},
    instructions::{a, b, i, m, rvc},
    machine::{VERSION0, VERSION1, VERSION2},
    memory::{FLAG_EXECUTABLE, Memory, round_page_up},
};

type TestMemory = WXorXMemory<SparseMemory<u64>>;

fn reference_decoder<R: Register>(isa: u8, version: u32) -> Decoder {
    let mut decoder = Decoder::new(false, version);
    decoder.add_instruction_factory(rvc::factory::<R>);
    decoder.add_instruction_factory(i::factory::<R>);
    decoder.add_instruction_factory(m::factory::<R>);
    if isa & ISA_B != 0 {
        decoder.add_instruction_factory(b::factory::<R>);
    }
    if isa & ISA_A != 0 {
        decoder.add_instruction_factory(a::factory::<R>);
    }
    decoder
}

fn compare_width<R: Register>(memory: &mut TestMemory, words: &[u32]) {
    for version in [VERSION0, VERSION1, VERSION2] {
        for isa in [0, ISA_A, ISA_B, ISA_A | ISA_B] {
            let mut reference = reference_decoder::<R>(isa, version);
            let mut standard = build_decoder::<R>(isa, version);
            for (index, &word) in words.iter().enumerate() {
                let pc = (index * 4) as u64;
                assert_eq!(
                    standard.decode_raw(memory, pc),
                    reference.decode_raw(memory, pc),
                    "RV{} V{version} ISA {isa:#x} instruction {word:#010x}",
                    R::BITS,
                );
            }
        }
    }
}

#[test]
fn standard_dispatch_matches_reference_factories() {
    let mut words: Vec<u32> = (0..=u32::from(u16::MAX))
        .filter(|word| word & 3 != 3)
        .collect();
    // Cover every primary opcode and function field with different zero,
    // small and high register operands, then sample the whole word space.
    for opcode in (3..=127).step_by(4) {
        for funct3 in 0..8 {
            for funct7 in 0..128 {
                let bits = funct7 << 25 | funct3 << 12 | opcode;
                for (rd, rs1, rs2) in [(0, 0, 0), (12, 1, 31), (31, 31, 1)] {
                    words.push(bits | rs2 << 20 | rs1 << 15 | rd << 7);
                }
            }
        }
    }
    let mut random = 0x1234_5678_u32;
    for _ in 0..16_384 {
        random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        words.push(random | 3);
    }

    let bytes: Vec<_> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    let mut memory = TestMemory::new();
    memory
        .init_pages(
            0,
            round_page_up(bytes.len() as u64),
            FLAG_EXECUTABLE,
            Some(bytes.into()),
            0,
        )
        .unwrap();
    compare_width::<u32>(&mut memory, &words);
    compare_width::<u64>(&mut memory, &words);
}

#[test]
fn custom_factories_keep_their_order_and_standard_fallback() {
    let mut memory = TestMemory::new();
    // Reserved compressed encoding, then uncompressed nop.
    memory
        .init_pages(
            0,
            RISCV_PAGESIZE as u64,
            FLAG_EXECUTABLE,
            Some(vec![0, 0, 0x13, 0, 0, 0].into()),
            0,
        )
        .unwrap();
    let mut custom = Decoder::new(false, VERSION2);
    custom.add_instruction_factory(|_, _| Some(123));
    custom.add_instruction_factory(|_, _| Some(456));
    assert_eq!(custom.decode_raw(&mut memory, 0), Ok(123));
    assert_eq!(custom.decode_raw(&mut memory, 2), Ok(123));

    let mut standard = build_decoder::<u64>(0, VERSION2);
    standard.add_instruction_factory(|_, _| Some(123));
    standard.add_instruction_factory(|_, _| Some(456));
    assert_eq!(standard.decode_raw(&mut memory, 0), Ok(123));
    assert_eq!(
        standard.decode_raw(&mut memory, 2),
        Ok(i::factory::<u64>(0x13, VERSION2).unwrap())
    );
}

#[test]
fn page_crossing_uses_executable_memory_for_both_halves() {
    let page = RISCV_PAGESIZE as u64;
    for (low_half, next_executable) in [(1_u16, false), (0x13, false), (0x13, true)] {
        let mut memory = TestMemory::new();
        memory
            .init_pages(
                0,
                page,
                FLAG_EXECUTABLE,
                Some(low_half.to_le_bytes().to_vec().into()),
                page - 2,
            )
            .unwrap();
        if next_executable {
            memory
                .init_pages(page, page, FLAG_EXECUTABLE, None, 0)
                .unwrap();
        }
        let mut standard = build_decoder::<u64>(0, VERSION2);
        let mut reference = reference_decoder::<u64>(0, VERSION2);
        let result = standard.decode_raw(&mut memory, page - 2);
        assert_eq!(result, reference.decode_raw(&mut memory, page - 2));
        assert_eq!(result.is_ok(), low_half == 1 || next_executable);
    }
}
