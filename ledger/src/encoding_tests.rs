use crate::{Cell, CellInput, GroupWitness, Transaction, wire};
use anyhow::Result;
use ckb_types::{
    bytes::Bytes,
    core::{ScriptHashType, TransactionBuilder},
    packed::{self, CellDep, OutPoint, Script, WitnessArgs},
    prelude::*,
};

fn script(hash: u8, args: usize) -> Script {
    Script::new_builder()
        .code_hash([hash; 32])
        .hash_type(ScriptHashType::Data)
        .args(Bytes::from(vec![hash; args]))
        .build()
}

#[test]
fn measure_canonical_payment_and_combined_output_bytes() -> Result<()> {
    let prevout = OutPoint::new([7; 32].pack(), 0);
    let dep = CellDep::new_builder()
        .out_point(OutPoint::new([8; 32].pack(), 0))
        .build();
    let lock = script(3, 20);
    let capacity = 10_000_000_000u64;
    let sig = Bytes::from(vec![42; 65]);
    let l1_output = packed::CellOutput::new_builder()
        .capacity(capacity)
        .lock(lock.clone())
        .build();
    let l1 = TransactionBuilder::default()
        .input(packed::CellInput::new(prevout.clone(), 0))
        .cell_dep(dep.clone())
        .outputs(vec![l1_output.clone(), l1_output])
        .outputs_data(vec![Bytes::new().pack(), Bytes::new().pack()])
        .witness(
            WitnessArgs::new_builder()
                .lock(Some(sig.clone()).pack())
                .build()
                .as_bytes()
                .pack(),
        )
        .build();
    let payment_cell = Cell::new(capacity, lock.clone(), vec![], Bytes::new())?;
    let native = Transaction::new(
        vec![dep],
        vec![],
        vec![CellInput::new(prevout, None)],
        vec![payment_cell.clone(), payment_cell],
        vec![GroupWitness {
            group_index: 0,
            proof_data: Bytes::new(),
            seal: sig,
        }],
    )?;

    let types = vec![script(1, 32), script(2, 32), script(4, 32)];
    let separated = types
        .iter()
        .map(|type_script| {
            packed::CellOutput::new_builder()
                .capacity(capacity)
                .lock(lock.clone())
                .type_(Some(type_script.clone()).pack())
                .build()
        })
        .collect::<Vec<_>>();
    let separated: packed::CellOutputVec = separated.pack();
    let data: packed::BytesVec = vec![Bytes::new().pack(); 3].pack();
    let cell = Cell::new(capacity, lock, types, Bytes::new())?;
    let with_combined_output = Transaction::new(
        vec![],
        vec![],
        vec![CellInput::new(OutPoint::new([9; 32].pack(), 0), None)],
        vec![cell],
        vec![],
    )?;
    let serialized = wire::Transaction::from_slice(with_combined_output.as_slice())?;
    let combined = serialized.raw().outputs().as_slice().len();
    let l1_len = l1.data().as_slice().len();
    let native_len = native.as_slice().len();
    let separated_len = separated.as_slice().len() + data.as_slice().len();
    println!("payment ckb={l1_len} stem={native_len} bytes");
    println!("output vectors ckb={separated_len} stem={combined} bytes");
    assert_eq!(l1_len, 460);
    assert_eq!(native_len, 467);
    assert_eq!(separated_len, 590);
    assert_eq!(combined, 384);
    Ok(())
}

mod dictionary {
    #![allow(dead_code, clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/dictionary.rs"));
}
use crate::wire as ledger;

fn dictionary_transaction(
    original: &wire::Transaction,
) -> Result<dictionary::DictionaryTransaction> {
    let raw = original.raw();
    let mut scripts = raw
        .outputs()
        .into_iter()
        .flat_map(|cell| std::iter::once(cell.lock()).chain(cell.type_scripts()))
        .collect::<Vec<_>>();
    scripts.sort_unstable_by(|left, right| left.as_slice().cmp(right.as_slice()));
    scripts.dedup_by(|left, right| left.as_slice() == right.as_slice());
    let index = |script: wire::Script| -> Result<wire::Uint16> {
        let position = scripts
            .binary_search_by(|item| item.as_slice().cmp(script.as_slice()))
            .map_err(|_| anyhow::anyhow!("output script is missing from the dictionary"))?;
        Ok(wire::Uint16::new_unchecked(Bytes::copy_from_slice(
            &u16::try_from(position)?.to_le_bytes(),
        )))
    };
    let outputs = raw
        .outputs()
        .into_iter()
        .map(|cell| {
            Ok(dictionary::IndexedOutput::new_builder()
                .capacity(cell.capacity())
                .lock(index(cell.lock())?)
                .type_scripts(
                    dictionary::ScriptIndexVec::new_builder()
                        .set(
                            cell.type_scripts()
                                .into_iter()
                                .map(index)
                                .collect::<Result<_>>()?,
                        )
                        .build(),
                )
                .data(cell.data())
                .build())
        })
        .collect::<Result<_>>()?;
    Ok(dictionary::DictionaryTransaction::new_builder()
        .raw(
            dictionary::DictionaryRawTransaction::new_builder()
                .cell_deps(raw.cell_deps())
                .header_deps(raw.header_deps())
                .inputs(raw.inputs())
                .scripts(wire::ScriptVec::new_builder().set(scripts).build())
                .outputs(
                    dictionary::IndexedOutputVec::new_builder()
                        .set(outputs)
                        .build(),
                )
                .protocol(raw.protocol())
                .valid_before_height(raw.valid_before_height())
                .build(),
        )
        .witnesses(original.witnesses())
        .certificate(original.certificate())
        .build())
}

#[test]
fn measure_transaction_script_dictionary() -> Result<()> {
    let make = |locks: &[u8], asset: bool| -> Result<Transaction> {
        let outputs = locks
            .iter()
            .map(|lock| {
                Cell::new(
                    10_000_000_000,
                    script(*lock, 20),
                    asset.then(|| script(42, 32)).into_iter().collect(),
                    if asset {
                        Bytes::from(50u128.to_le_bytes().to_vec())
                    } else {
                        Bytes::new()
                    },
                )
            })
            .collect::<Result<_>>()?;
        Transaction::new(
            vec![
                CellDep::new_builder()
                    .out_point(OutPoint::new([8; 32].pack(), 0))
                    .build(),
            ],
            vec![],
            vec![CellInput::new(OutPoint::new([7; 32].pack(), 0), None)],
            outputs,
            vec![GroupWitness {
                group_index: 0,
                proof_data: Bytes::new(),
                seal: Bytes::from(vec![42; 65]),
            }],
        )
    };
    for (case, locks, asset, expected_direction) in [
        (
            "payment-distinct-locks",
            vec![1, 2],
            false,
            std::cmp::Ordering::Greater,
        ),
        (
            "payment-same-lock",
            vec![1, 1],
            false,
            std::cmp::Ordering::Less,
        ),
        (
            "one-asset-output",
            vec![1],
            true,
            std::cmp::Ordering::Greater,
        ),
        (
            "ten-asset-outputs-distinct-locks",
            (0..10).collect(),
            true,
            std::cmp::Ordering::Less,
        ),
        (
            "ten-asset-outputs-same-lock",
            vec![1; 10],
            true,
            std::cmp::Ordering::Less,
        ),
    ] {
        let original = make(&locks, asset)?;
        let original = wire::Transaction::from_slice(original.as_slice())?;
        let dictionary = dictionary_transaction(&original)?;
        let dictionary = dictionary::DictionaryTransaction::from_slice(dictionary.as_slice())?;
        assert_eq!(
            dictionary.as_slice().len().cmp(&original.as_slice().len()),
            expected_direction
        );

        let raw = dictionary.raw();
        assert_eq!(original.raw().outputs().len(), raw.outputs().len());
        assert_eq!(
            original.witnesses().as_slice(),
            dictionary.witnesses().as_slice()
        );
        let mut referenced = std::collections::BTreeSet::new();
        let resolve = |index: wire::Uint16| {
            usize::from(u16::from_le_bytes(index.as_slice().try_into().unwrap()))
        };
        for (before, after) in original.raw().outputs().into_iter().zip(raw.outputs()) {
            assert_eq!(before.capacity().as_slice(), after.capacity().as_slice());
            assert_eq!(before.data().as_slice(), after.data().as_slice());
            let lock = resolve(after.lock());
            assert_eq!(
                before.lock().as_slice(),
                raw.scripts().get(lock).unwrap().as_slice()
            );
            referenced.insert(lock);
            assert_eq!(before.type_scripts().len(), after.type_scripts().len());
            for (script, index) in before.type_scripts().into_iter().zip(after.type_scripts()) {
                let index = resolve(index);
                assert_eq!(
                    script.as_slice(),
                    raw.scripts().get(index).unwrap().as_slice()
                );
                referenced.insert(index);
            }
        }
        assert_eq!(referenced.len(), raw.scripts().len());
        println!(
            "{case}: current={} candidate={} bytes; {} unique scripts",
            original.as_slice().len(),
            dictionary.as_slice().len(),
            referenced.len()
        );
    }
    Ok(())
}
