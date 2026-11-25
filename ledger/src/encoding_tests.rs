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
    println!("payment ckb={l1_len} fern={native_len} bytes");
    println!("output vectors ckb={separated_len} fern={combined} bytes");
    assert_eq!(l1_len, 460);
    assert_eq!(native_len, 467);
    assert_eq!(separated_len, 590);
    assert_eq!(combined, 384);
    Ok(())
}
