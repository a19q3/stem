use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    hint::black_box,
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use ckb_types::prelude::*;

use crate::{
    baseline as wire,
    strand::{self, prelude as tg},
    wire as codec,
};
use tg::{Builder as TgBuilder, Entity as TgEntity, Reader as TgReader};

thread_local! {
    static ALLOCATIONS: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
}

struct CountedAllocator;

#[global_allocator]
static ALLOCATOR: CountedAllocator = CountedAllocator;

unsafe impl GlobalAlloc for CountedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_allocation(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count_allocation(size);
        unsafe { System.realloc(ptr, layout, size) }
    }
}

fn count_allocation(size: usize) {
    let _ = ALLOCATIONS.try_with(|state| {
        if let Some((calls, bytes)) = state.get() {
            state.set(Some((calls.saturating_add(1), bytes.saturating_add(size))));
        }
    });
}

fn allocated(run: impl FnOnce()) -> (usize, usize) {
    ALLOCATIONS.set(Some((0, 0)));
    run();
    ALLOCATIONS.take().expect("allocation measurement")
}

fn fixed<T: TgEntity>(bytes: &[u8]) -> T {
    T::new_unchecked(strand::Bytes::copy_from_slice(bytes))
}

fn script(value: wire::Script) -> codec::Script {
    codec::Script::new_builder()
        .code_hash(fixed(value.code_hash().as_slice()))
        .hash_type(value.hash_type().as_slice()[0].into())
        .args(codec::Bytes::new_unchecked(value.args().as_bytes()))
        .build()
}

fn scripts(values: wire::ScriptVec) -> Vec<codec::Script> {
    let mut entries: Vec<_> = values.into_iter().map(script).collect();
    entries.sort_unstable_by(|left, right| left.as_slice().cmp(right.as_slice()));
    entries
}

fn witnesses(tx: &wire::Transaction) -> codec::GroupWitnessVec {
    codec::GroupWitnessVec::new_builder()
        .set(
            tx.witnesses()
                .into_iter()
                .map(|witness| {
                    codec::GroupWitness::new_builder()
                        .group_index(fixed(witness.group_index().as_slice()))
                        .proof_data(codec::Bytes::new_unchecked(witness.proof_data().as_bytes()))
                        .seal(codec::Bytes::new_unchecked(witness.seal().as_bytes()))
                        .build()
                })
                .collect(),
        )
        .build()
}

fn inline(tx: &wire::Transaction) -> codec::Transaction {
    let raw = tx.raw();
    let outputs = raw
        .outputs()
        .into_iter()
        .map(|cell| {
            codec::Cell::new_builder()
                .capacity(fixed(cell.capacity().as_slice()))
                .lock(script(cell.lock()))
                .type_scripts(
                    codec::ScriptVec::new_builder()
                        .set(scripts(cell.type_scripts()))
                        .build(),
                )
                .data(codec::Bytes::new_unchecked(cell.data().as_bytes()))
                .build()
        })
        .collect();
    codec::Transaction::new_builder()
        .raw(
            codec::RawTransaction::new_builder()
                .inputs(fixed(raw.inputs().as_slice()))
                .cell_deps(fixed(raw.cell_deps().as_slice()))
                .header_deps(fixed(raw.header_deps().as_slice()))
                .valid_before_height(fixed(raw.valid_before_height().as_slice()))
                .outputs(codec::CellVec::new_builder().set(outputs).build())
                .build(),
        )
        .witnesses(witnesses(tx))
        .build()
}

fn indexed(tx: &wire::Transaction) -> Result<codec::IndexedTransaction> {
    let raw = tx.raw();
    let mut table = raw
        .outputs()
        .into_iter()
        .flat_map(|cell| std::iter::once(script(cell.lock())).chain(scripts(cell.type_scripts())))
        .collect::<Vec<_>>();
    table.sort_unstable_by(|left, right| left.as_slice().cmp(right.as_slice()));
    table.dedup();
    let index = |script: &codec::Script| -> Result<codec::ScriptIndex> {
        let position = table
            .binary_search_by(|item| item.as_slice().cmp(script.as_slice()))
            .ok()
            .context("output Script missing from table")?;
        Ok(codec::ScriptIndex::new_builder()
            .set(u16::try_from(position)?.into())
            .build())
    };
    let outputs = raw
        .outputs()
        .into_iter()
        .map(|cell| {
            let types = scripts(cell.type_scripts())
                .iter()
                .map(index)
                .collect::<Result<Vec<_>>>()?;
            Ok(codec::IndexedOutput::new_builder()
                .capacity(fixed(cell.capacity().as_slice()))
                .lock(index(&script(cell.lock()))?)
                .type_scripts(codec::ScriptIndexVec::new_builder().set(types).build())
                .data(codec::Bytes::new_unchecked(cell.data().as_bytes()))
                .build())
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(codec::IndexedTransaction::new_builder()
        .raw(
            codec::IndexedRawTransaction::new_builder()
                .inputs(fixed(raw.inputs().as_slice()))
                .cell_deps(fixed(raw.cell_deps().as_slice()))
                .header_deps(fixed(raw.header_deps().as_slice()))
                .valid_before_height(fixed(raw.valid_before_height().as_slice()))
                .scripts(codec::ScriptVec::new_builder().set(table).build())
                .outputs(codec::IndexedOutputVec::new_builder().set(outputs).build())
                .build(),
        )
        .witnesses(witnesses(tx))
        .build())
}

fn parse_indexed(bytes: &[u8]) -> Result<codec::IndexedTransactionReader<'_>> {
    let reader = codec::IndexedTransactionReader::from_slice(bytes)?;
    let raw = reader.raw();
    let table = raw.scripts();
    ensure!(
        table.len() <= 1 + usize::from(u16::MAX),
        "Script Table exceeds index range"
    );
    let mut previous = None;
    for script in table.iter() {
        ensure!(
            previous.is_none_or(|value| value < script.as_slice()),
            "Script Table order"
        );
        previous = Some(script.as_slice());
    }
    let mut referenced = vec![false; table.len()];
    let mut mark = |index: codec::ScriptIndexReader<'_>| {
        *referenced
            .get_mut(index.value() as usize)
            .context("Script index out of bounds")? = true;
        Ok::<_, anyhow::Error>(())
    };
    for output in raw.outputs().iter() {
        mark(output.lock())?;
        let mut previous = None;
        for index in output.type_scripts().iter() {
            ensure!(
                previous.is_none_or(|value| value < index.value()),
                "Type Script index order"
            );
            previous = Some(index.value());
            mark(index)?;
        }
    }
    ensure!(
        referenced.into_iter().all(|used| used),
        "unreferenced Script Table entry"
    );
    Ok(reader)
}

fn measure(mut run: impl FnMut()) -> f64 {
    const RUNS: u32 = 5;
    const REPETITIONS: u32 = 2_000;
    let mut samples = [0.; RUNS as usize];
    for sample in &mut samples {
        let start = Instant::now();
        for _ in 0..REPETITIONS {
            run();
        }
        *sample = start.elapsed().as_nanos() as f64 / f64::from(REPETITIONS);
    }
    samples.sort_unstable_by(f64::total_cmp);
    samples[samples.len() / 2]
}

#[test]
fn c_and_rust_readers_accept_the_same_canonical_frames() -> Result<()> {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };

    let dir = tempfile::tempdir()?;
    let source = dir.path().join("verify.c");
    std::fs::write(
        &source,
        br#"
#include <stdio.h>
#include "strand_wire.h"
int main(int argc, char **argv) {
    unsigned char bytes[1024];
    size_t len = fread(bytes, 1, sizeof(bytes), stdin);
    tg_seg_t frame = {bytes, len};
    if (argc != 2) return 2;
    if (argv[1][0] == 'i') return TgReader_IntercellEnvelope_verify(&frame);
    if (argv[1][0] == 'b') return TgReader_BridgeGenesisConfig_verify(&frame);
    if (argv[1][0] == 'k') return TgReader_KeeperReservation_verify(&frame);
    if (argv[1][0] == 'r') return TgReader_Receipt_verify(&frame);
    if (argv[1][0] == 's') return TgReader_StateSnapshot_verify(&frame);
    if (argv[1][0] == 'h') return TgReader_HistoricalHeaderProof_verify(&frame);
    if (argv[1][0] == 'p') return TgReader_StakePrincipalProof_verify(&frame);
    return TgReader_Transaction_verify(&frame);
}
"#,
    )?;
    let binary = dir.path().join("verify");
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let result = Command::new(compiler)
        .args(["-std=c11", "-O2", "-I", env!("OUT_DIR")])
        .arg(source)
        .arg("-o")
        .arg(&binary)
        .output()?;
    ensure!(
        result.status.success(),
        "C schema compiler output: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let read_c = |kind: &str, bytes: &[u8]| -> Result<bool> {
        let mut child = Command::new(&binary)
            .arg(kind)
            .stdin(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .context("C decoder stdin")?
            .write_all(bytes)?;
        Ok(child.wait()?.success())
    };
    let transaction = codec::Transaction::default();
    let receipt = crate::block::Receipt {
        fee: u64::MAX,
        cycles: 1,
        protocol_result: strand::Bytes::new(),
    }
    .encode();
    let snapshot = codec::StateSnapshot::default();
    let history = codec::HistoricalHeaderProof::default();
    let principal = codec::StakePrincipalProof::default();
    let bridge = codec::BridgeGenesisConfig::new_builder()
        .lanes(codec::Byte32Vec::new_builder().set(vec![fixed(&[42; 32])]).build())
        .build();
    let message = stem_bridge_wire::intercell::Message {
        source: [1;32], destination: [2;32], sender: [3;32], receiver: [4;32],
        nonce: 42, payload: &[],
    };
    let envelope = message.header();
    let view = codec::IntercellEnvelopeReader::from_slice(&envelope)?;
    assert_eq!(view.source().as_slice(), message.source);
    assert_eq!(view.destination().as_slice(), message.destination);
    assert_eq!(view.sender().as_slice(), message.sender);
    assert_eq!(view.receiver().as_slice(), message.receiver);
    assert_eq!(view.nonce().as_slice(), message.nonce.to_le_bytes());
    let reservation = codec::KeeperReservation::default();
    for (kind, canonical) in [
        ("tx", transaction.as_slice()),
        ("receipt", &receipt),
        ("snapshot", snapshot.as_slice()),
        ("history", history.as_slice()),
        ("principal", principal.as_slice()),
        ("bridge", bridge.as_slice()),
        ("intercell", envelope.as_slice()),
        ("keeper", reservation.as_slice()),
    ] {
        let validate = |value: &[u8]| match kind {
            "tx" => codec::TransactionReader::verify(value).is_ok(),
            "receipt" => codec::ReceiptReader::verify(value).is_ok(),
            "intercell" => codec::IntercellEnvelopeReader::verify(value).is_ok(),
            "bridge" => codec::BridgeGenesisConfigReader::verify(value).is_ok(),
            "keeper" => codec::KeeperReservationReader::verify(value).is_ok(),
            "snapshot" => codec::StateSnapshotReader::verify(value).is_ok(),
            "history" => codec::HistoricalHeaderProofReader::verify(value).is_ok(),
            _ => codec::StakePrincipalProofReader::verify(value).is_ok(),
        };
        assert!(read_c(kind, canonical)?);
        for cut in 0..canonical.len() {
            let truncated = &canonical[..cut];
            assert!(!validate(truncated));
            assert!(!read_c(kind, truncated)?);
        }
        for index in 0..canonical.len() {
            let mut altered = canonical.to_vec();
            altered[index] ^= 0x80;
            assert_eq!(read_c(kind, &altered)?, validate(&altered));
        }
    }
    Ok(())
}

pub(super) fn compare(name: &str, original: &wire::Transaction, timing: bool) -> Result<()> {
    ensure!(
        original.raw().protocol().to_opt().is_none() && original.certificate().to_opt().is_none()
    );
    let molecule_dictionary = super::dictionary_transaction(original)?;
    let compact = inline(original);
    let compact_indexed = indexed(original)?;
    let decoded = codec::TransactionReader::from_slice(compact.as_slice())?;
    let indexed = parse_indexed(compact_indexed.as_slice())?;
    let table: Vec<_> = indexed.raw().scripts().iter().collect();
    for ((before, decoded), indexed) in original
        .raw()
        .outputs()
        .into_iter()
        .zip(decoded.raw().outputs().iter())
        .zip(indexed.raw().outputs().iter())
    {
        assert_eq!(before.capacity().as_slice(), decoded.capacity().as_slice());
        assert_eq!(before.data().raw_data(), decoded.data().raw_data());
        assert_eq!(decoded.capacity().as_slice(), indexed.capacity().as_slice());
        assert_eq!(decoded.data().raw_data(), indexed.data().raw_data());
        assert_eq!(script(before.lock()).as_slice(), decoded.lock().as_slice());
        assert_eq!(
            decoded.lock().as_slice(),
            table[indexed.lock().value() as usize].as_slice()
        );
        assert_eq!(decoded.type_scripts().len(), indexed.type_scripts().len());
        for (script, index) in decoded
            .type_scripts()
            .iter()
            .zip(indexed.type_scripts().iter())
        {
            assert_eq!(script.as_slice(), table[index.value() as usize].as_slice());
        }
    }
    assert_eq!(
        decoded.witnesses().as_slice(),
        indexed.witnesses().as_slice()
    );
    println!(
        "{name}: {} / {} / {} / {} bytes",
        original.as_slice().len(),
        molecule_dictionary.as_slice().len(),
        compact.as_slice().len(),
        compact_indexed.as_slice().len()
    );
    if timing {
        let parsers: [&dyn Fn(); 4] = [
            &|| {
                wire::TransactionReader::verify(black_box(original.as_slice()), false).unwrap();
            },
            &|| {
                super::dictionary::DictionaryTransactionReader::verify(
                    black_box(molecule_dictionary.as_slice()),
                    false,
                )
                .unwrap();
            },
            &|| {
                codec::TransactionReader::verify(black_box(compact.as_slice())).unwrap();
            },
            &|| {
                codec::IndexedTransactionReader::verify(black_box(compact_indexed.as_slice()))
                    .unwrap();
            },
        ];
        let parse = parsers.map(measure);
        let allocations = parsers.map(allocated);
        let hash = [
            original.as_slice(),
            molecule_dictionary.as_slice(),
            compact.as_slice(),
            compact_indexed.as_slice(),
        ]
        .map(|bytes| {
            measure(|| {
                black_box(ckb_hash::blake2b_256(black_box(bytes)));
            })
        });
        let check_index = || {
            black_box(parse_indexed(black_box(compact_indexed.as_slice())).unwrap());
        };
        let index_check = measure(check_index);
        let index_allocations = allocated(check_index);
        println!(
            "{name}: median structural parse ns={parse:.1?}, hash ns={hash:.1?}, full Strand table parse ns={index_check:.1}"
        );
        println!(
            "{name}: parse allocations (calls, bytes)={allocations:?}, full Strand table={index_allocations:?}"
        );
    }
    for tx in [compact.as_slice(), compact_indexed.as_slice()] {
        let mut invalid = tx.to_vec();
        invalid[0] |= 0x80;
        assert!(codec::TransactionReader::verify(&invalid).is_err());
        assert!(codec::IndexedTransactionReader::verify(&invalid).is_err());
    }
    assert!(codec::ScriptIndexReader::verify(&[0x80, 0]).is_err());
    assert!(codec::ScriptIndexReader::verify(&[0xff, 0xff, 0x04]).is_err());
    Ok(())
}
