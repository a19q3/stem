//! Matched standalone fungible predicates in StemVM and CKB's Data2 dispatcher.

use std::{collections::BTreeMap, sync::Arc};

use anyhow::ensure;
use ckb_chain_spec::consensus::{Consensus, ConsensusBuilder};
use ckb_script::{ScriptError, ScriptGroupType, TransactionScriptsVerifier, TxVerifyEnv};
use ckb_traits::{CellDataProvider, ExtensionProvider, HeaderProvider};
use ckb_types::{
    core::{
        EpochNumberWithFraction, HeaderBuilder, HeaderView, TransactionBuilder,
        cell::{CellMeta, CellMetaBuilder, ResolvedTransaction},
        hardfork::HardForks,
    },
    packed::{self, Byte32, CellOutput},
};
use serde_json::json;
use stem_bridge_wire::metatype::{ManifestCell, ManifestDefinition, ManifestReference, MetatypeManifest, project};
use stem_ledger::{ScriptGroupKind, derive_script_groups};

use super::{polytype_attachment::build_contract, *};

const CELL_CAPACITY: u64 = 1_000_000_000_000;
const CAP: u128 = 60;
const CKB_CYCLES: u64 = 100_000_000;

#[derive(Clone, Copy)]
enum Row {
    Plain,
    Asset { units: u128, capped: bool, width: usize },
}

fn asset(units: u128, capped: bool) -> Row {
    Row::Asset { units, capped, width: 16 }
}

struct Case {
    name: &'static str,
    inputs: Vec<Row>,
    outputs: Vec<Row>,
    issuer: bool,
    cap_required: bool,
}

fn corpus() -> Vec<Case> {
    use Row::Plain;
    let mut result = vec![
        Case { name: "issue", inputs: vec![Plain], outputs: vec![asset(35, false)], issuer: true, cap_required: false },
        Case {
            name: "attach",
            inputs: vec![Plain, asset(55, false)],
            outputs: vec![asset(55, true), Plain],
            issuer: false,
            cap_required: false,
        },
        Case {
            name: "split",
            inputs: vec![asset(25, true), Plain, asset(35, false)],
            outputs: vec![asset(33, true), Plain, asset(27, true)],
            issuer: false,
            cap_required: false,
        },
        Case {
            name: "merge-at-cap",
            inputs: vec![asset(25, true), Plain, asset(35, true)],
            outputs: vec![Plain, asset(60, true)],
            issuer: false,
            cap_required: false,
        },
        Case {
            name: "merge-over-cap",
            inputs: vec![asset(35, true), Plain, asset(26, true)],
            outputs: vec![Plain, asset(61, true)],
            issuer: false,
            cap_required: false,
        },
        Case {
            name: "remove-optional-cap",
            inputs: vec![asset(25, true)],
            outputs: vec![asset(25, false)],
            issuer: false,
            cap_required: false,
        },
        Case {
            name: "keep-bound-cap",
            inputs: vec![asset(25, true)],
            outputs: vec![asset(25, true)],
            issuer: false,
            cap_required: true,
        },
        Case {
            name: "remove-bound-cap",
            inputs: vec![asset(25, true)],
            outputs: vec![asset(25, false)],
            issuer: false,
            cap_required: true,
        },
        Case {
            name: "inflate-without-issuer",
            inputs: vec![asset(25, true)],
            outputs: vec![asset(26, true)],
            issuer: false,
            cap_required: false,
        },
        Case { name: "burn-input-only-group", inputs: vec![asset(25, true)], outputs: vec![Plain], issuer: true, cap_required: false },
        Case {
            name: "zero-amount",
            inputs: vec![asset(25, true)],
            outputs: vec![asset(25, true), asset(0, true)],
            issuer: false,
            cap_required: false,
        },
        Case {
            name: "wrong-state-width",
            inputs: vec![asset(25, true)],
            outputs: vec![Row::Asset { units: 25, capped: true, width: 15 }],
            issuer: false,
            cap_required: false,
        },
        Case {
            name: "overflow-output-total",
            inputs: vec![Plain],
            outputs: vec![asset(u128::MAX, false), asset(1, false)],
            issuer: true,
            cap_required: false,
        },
    ];
    // Keep the boundary corpus deterministic and report the exact witness row
    // when a decision differs. Interior splits exercise multiple allocations.
    for first in [1, 29, 59] {
        result.push(Case {
            name: match first {
                1 => "split-one",
                29 => "split-interior",
                _ => "split-cap-minus-one",
            },
            inputs: vec![asset(CAP, true), Plain],
            outputs: vec![asset(first, true), Plain, asset(CAP - first, true)],
            issuer: false,
            cap_required: false,
        });
    }
    result
}

fn reference(case: &Case, count: usize) -> bool {
    let total = |rows: &[Row]| {
        rows.iter().try_fold(0u128, |sum, row| match row {
            Row::Plain => Some(sum),
            Row::Asset { units, capped, width }
                if *width == 16 && *units > 0 && (!case.cap_required || *capped) && (!(count > 1 && *capped) || *units <= CAP) =>
            {
                sum.checked_add(*units)
            }
            Row::Asset { .. } => None,
        })
    };
    match (total(&case.inputs), total(&case.outputs)) {
        (Some(input), Some(output)) => input == output || (output > input && case.issuer),
        _ => false,
    }
}

fn data_script(hash: [u8; 32], hash_type: ScriptHashType, args: &[u8]) -> Script {
    Script::new_builder().hash_type(hash_type).code_hash(hash).args(Bytes::copy_from_slice(args)).build()
}

fn make_cells(rows: &[Row], lock: &Script, asset: &Script, caps: &[Script], total: u64) -> Result<Vec<Cell>> {
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let mut capacity = total / rows.len() as u64;
            if index == 0 {
                capacity += total % rows.len() as u64;
            }
            let (types, data) = match row {
                Row::Plain => (vec![], Bytes::new()),
                Row::Asset { units, capped, width } => {
                    let mut types = vec![asset.clone()];
                    if *capped {
                        types.extend_from_slice(caps);
                    }
                    (types, Bytes::copy_from_slice(&units.to_le_bytes()[..*width]))
                }
            };
            Cell::new(capacity, lock.clone(), types, data)
        })
        .collect()
}

type Membership = BTreeMap<Vec<u8>, (Vec<usize>, Vec<usize>)>;

fn expected_groups(case: &Case, asset: &Script, caps: &[Script]) -> Membership {
    let collect = |script: &Script, required_capped: bool| {
        let positions = |rows: &[Row]| {
            rows.iter()
                .enumerate()
                .filter_map(|(i, row)| match row {
                    Row::Asset { capped, .. } if !required_capped || *capped => Some(i),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let (inputs, outputs) = (positions(&case.inputs), positions(&case.outputs));
        (!inputs.is_empty() || !outputs.is_empty()).then(|| (script.as_slice().to_vec(), (inputs, outputs)))
    };
    collect(asset, false).into_iter().chain(caps.iter().filter_map(|script| collect(script, true))).collect()
}

#[derive(Clone)]
struct Headers(HeaderView);

impl CellDataProvider for Headers {
    fn get_cell_data(&self, _: &OutPoint) -> Option<Bytes> {
        None
    }
    fn get_cell_data_hash(&self, _: &OutPoint) -> Option<Byte32> {
        None
    }
}

impl HeaderProvider for Headers {
    fn get_header(&self, hash: &Byte32) -> Option<HeaderView> {
        (self.0.hash() == *hash).then(|| self.0.clone())
    }
}

impl ExtensionProvider for Headers {
    fn get_block_extension(&self, _: &Byte32) -> Option<packed::Bytes> {
        None
    }
}

struct EncodedCell {
    output: CellOutput,
    data: Bytes,
}

#[derive(Clone, Copy, Debug)]
enum Encoding {
    Inline,
    Shared,
}

fn views<'a>(
    cells: &'a [EncodedCell],
    definitions: &'a BTreeMap<[u8; 32], ManifestDefinition>,
    encoding: Encoding,
) -> Result<Vec<ManifestCell<'a>>> {
    cells
        .iter()
        .enumerate()
        .filter(|(_, cell)| cell.output.type_().to_opt().is_some())
        .map(|(index, cell)| {
            let view = match encoding {
                Encoding::Inline => MetatypeManifest::decode(&cell.data),
                Encoding::Shared => {
                    let reference = ManifestReference::decode(&cell.data).context("encoded reference")?;
                    reference.view(&definitions[&reference.definition_hash], usize::MAX)
                }
            };
            Ok((index as u16, view.context("encoded manifest")?))
        })
        .collect()
}

struct Ckb {
    genesis: HeaderView,
    consensus: Arc<Consensus>,
    env: Arc<TxVerifyEnv>,
    physical: Script,
    deps: Vec<Bytes>,
}

impl Ckb {
    fn new(udt: [u8; 32], cap: [u8; 32]) -> Result<Self> {
        let dispatcher = build_contract("--bin", "metatype")?;
        let udt_adapter = build_contract("--bin", "metatype-udt")?;
        let cap_adapter = build_contract("--bin", "metatype-amount-cap")?;
        let records = |native: &[u8], adapter: &[u8]| [native, &[0], &blake2b_256(adapter)].concat();
        let mut entries = [records(&udt, &udt_adapter), records(&cap, &cap_adapter)];
        entries.sort();
        let table: Bytes = [&[2][..], &entries[0], &entries[1], &[0]].concat().into();
        let genesis = HeaderBuilder::default().build();
        let tip = HeaderBuilder::default().number(1u64).epoch(EpochNumberWithFraction::new(1, 0, 1)).build();
        let consensus = ConsensusBuilder::default().hardfork_switch(HardForks::new_dev()).build();
        let args = [genesis.hash().as_slice(), &[7; 32], &blake2b_256(&table)].concat();
        let physical = data_script(blake2b_256(&dispatcher), ScriptHashType::Data2, &args);
        Ok(Self {
            genesis,
            consensus: Arc::new(consensus),
            env: Arc::new(TxVerifyEnv::new_commit(&tip)),
            physical,
            deps: vec![dispatcher, udt_adapter, cap_adapter, table],
        })
    }

    fn run(
        &self,
        inputs: &[Cell],
        outputs: &[Cell],
        asset: &Script,
        expected: &Membership,
        encoding: Encoding,
    ) -> Result<Result<u64, ScriptError>> {
        let mut definitions = BTreeMap::new();
        let mut encode = |cells: &[Cell]| -> Vec<EncodedCell> {
            cells
                .iter()
                .map(|cell| {
                    let mut types = cell.type_scripts().collect::<Vec<_>>();
                    types.sort_unstable_by(|a, b| a.as_slice().cmp(b.as_slice()));
                    let mut output = CellOutput::new_builder().capacity(cell.capacity()).lock(cell.lock());
                    let data = if types.is_empty() {
                        cell.data()
                    } else {
                        output = output.type_(Some(self.physical.clone()).pack());
                        let mut scripts = vec![types.len() as u8];
                        let mut states = Vec::new();
                        for script in types {
                            let state = if &script == asset { cell.data() } else { Bytes::new() };
                            let script_bytes = script.as_slice();
                            scripts.extend((script_bytes.len() as u16).to_le_bytes());
                            scripts.extend_from_slice(script_bytes);
                            let target = match encoding {
                                Encoding::Inline => &mut scripts,
                                Encoding::Shared => {
                                    scripts.extend([0, 0]);
                                    &mut states
                                }
                            };
                            target.extend((state.len() as u16).to_le_bytes());
                            target.extend_from_slice(&state);
                        }
                        match encoding {
                            Encoding::Inline => scripts.into(),
                            Encoding::Shared => {
                                let hash = blake2b_256(&scripts);
                                definitions.insert(hash, Bytes::from(scripts));
                                [b"\0", hash.as_slice(), states.as_slice()].concat().into()
                            }
                        }
                    };
                    EncodedCell { output: output.build(), data }
                })
                .collect()
        };
        let inputs = encode(inputs);
        let outputs = encode(outputs);
        let decoded = definitions
            .iter()
            .map(|(hash, raw)| Ok((*hash, ManifestDefinition::decode(raw.to_vec()).context("encoded definition")?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let (from, to) = (views(&inputs, &decoded, encoding)?, views(&outputs, &decoded, encoding)?);
        let groups = project(&from, &to)
            .context("valid fixture group projection")?
            .into_iter()
            .map(|group| {
                (
                    group.script.as_slice().to_vec(),
                    (
                        group.input_indices.into_iter().map(usize::from).collect(),
                        group.output_indices.into_iter().map(usize::from).collect(),
                    ),
                )
            })
            .collect::<Membership>();
        assert_eq!(&groups, expected, "CKB projected group identity or positions differ");

        let meta = |index, output, data| {
            CellMetaBuilder::from_cell_output(output, data).out_point(OutPoint::new([80; 32].pack(), index)).build()
        };
        let resolved_inputs: Vec<CellMeta> =
            inputs.into_iter().enumerate().map(|(index, cell)| meta(index as u32, cell.output, cell.data)).collect();
        let resolved_cell_deps: Vec<CellMeta> = self
            .deps
            .iter()
            .chain(definitions.values())
            .enumerate()
            .map(|(index, data)| meta(100 + index as u32, CellOutput::default(), data.clone()))
            .collect();
        let transaction = TransactionBuilder::default()
            .inputs(resolved_inputs.iter().map(|cell| packed::CellInput::new(cell.out_point.clone(), 0)))
            .outputs(outputs.iter().map(|cell| cell.output.clone()))
            .outputs_data(outputs.into_iter().map(|cell| cell.data.pack()))
            .cell_deps(resolved_cell_deps.iter().map(|cell| CellDep::new_builder().out_point(cell.out_point.clone()).build()))
            .header_dep(self.genesis.hash())
            .build();
        let resolved = ResolvedTransaction { transaction, resolved_inputs, resolved_cell_deps, resolved_dep_groups: vec![] };
        let verifier = TransactionScriptsVerifier::new(
            Arc::new(resolved),
            Headers(self.genesis.clone()),
            self.consensus.clone(),
            self.env.clone(),
        );
        Ok(verifier.verify_single(ScriptGroupType::Type, &self.physical.calc_script_hash(), CKB_CYCLES))
    }
}

#[test]
fn native_and_metatype_fungible_groups_match() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let asset_code = build_contract("--bin", "stem-native-udt")?;
    let cap_code = build_contract("--bin", "stem-amount-cap")?;
    let asset_hash = blake2b_256(&asset_code);
    let cap_hash = blake2b_256(&cap_code);
    let ckb = Ckb::new(asset_hash, cap_hash)?;
    for (index, code) in [asset_code, cap_code].into_iter().enumerate() {
        let outpoint = OutPoint::new([81; 32].pack(), index as u32);
        let capacity = Cell::minimum_capacity(&fixture.lock, &[], code.len())?;
        let cell = Cell::new(capacity, fixture.lock.clone(), vec![], code)?;
        fixture.deps.push(CellDep::new_builder().out_point(outpoint.clone()).build());
        fixture.spec.development_cells.push((outpoint, cell));
    }
    let mut prepared_cases = Vec::new();
    for count in [1, 2, 4, 8, 16] {
        for case in corpus() {
            if case.cap_required && count == 1 {
                continue;
            }
            let index = prepared_cases.len() as u32;
            let issuer = if case.issuer { fixture.lock.calc_script_hash().unpack() } else { [82; 32] };
            let mut args = [issuer.as_slice(), &[count as u8; 32]].concat();
            if case.cap_required {
                args.extend(cap_hash);
                args.extend(CAP.to_le_bytes());
            }
            let asset = data_script(asset_hash, ScriptHashType::Data, &args);
            let caps = (0..count - 1)
                .map(|index| {
                    data_script(
                        cap_hash,
                        ScriptHashType::Data,
                        &[asset.calc_script_hash().as_slice(), &(CAP + index as u128).to_le_bytes()].concat(),
                    )
                })
                .collect::<Vec<_>>();
            let inputs = make_cells(&case.inputs, &fixture.lock, &asset, &caps, CELL_CAPACITY * case.inputs.len() as u64)?;
            let outputs = make_cells(&case.outputs, &fixture.lock, &asset, &caps, CELL_CAPACITY * case.inputs.len() as u64 - FEE)?;
            let point = |position| OutPoint::new([83; 32].pack(), 8 * index + position);
            let input_points = inputs
                .iter()
                .enumerate()
                .map(|(position, cell)| {
                    let outpoint = point(position as u32);
                    fixture.spec.development_cells.push((outpoint.clone(), cell.clone()));
                    outpoint
                })
                .collect::<Vec<_>>();
            let payment = point(7);
            fixture.spec.development_cells.push((payment.clone(), Cell::new(FUND, fixture.lock.clone(), vec![], Bytes::new())?));
            prepared_cases.push((count, case, asset, caps, inputs, outputs, input_points, payment));
        }
    }
    let serial_dir = tempfile::tempdir()?;
    let parallel_dir = tempfile::tempdir()?;
    let mut serial_store = Store::open(serial_dir.path(), &fixture.spec, Retention::Archive)?;
    let mut parallel_store = Store::open(parallel_dir.path(), &fixture.spec, Retention::Archive)?;
    // Both stores start cold. Four workers can run Type Groups of the two
    // transactions in this block in parallel as well as the transactions.
    let workers = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let mut report = Vec::new();
    for (index, (count, case, asset, caps, inputs, outputs, points, funding)) in prepared_cases.into_iter().enumerate() {
        let label = format!("{} ({count} groups)", case.name);
        let expected = expected_groups(&case, &asset, &caps);
        let actual_groups = derive_script_groups(&inputs, &outputs)?
            .into_iter()
            .filter(|group| group.kind == ScriptGroupKind::Type)
            .map(|group| (group.script.as_slice().to_vec(), (group.inputs, group.outputs)))
            .collect::<Membership>();
        assert_eq!(actual_groups, expected, "native group membership in {label}");
        let check = |encoding| -> Result<Option<u64>> {
            match ckb.run(&inputs, &outputs, &asset, &expected, encoding).with_context(|| label.clone())? {
                Ok(cycles) => Ok(Some(cycles)),
                Err(ScriptError::ValidationFailure(_, 46)) => Ok(None),
                Err(error) => anyhow::bail!("unexpected CKB outcome for {label} {encoding:?}: {error}"),
            }
        };
        let (inline, shared) = (check(Encoding::Inline)?, check(Encoding::Shared)?);
        let accepted = reference(&case, count);
        assert_eq!(inline.is_some(), accepted, "inline decision for {label}");
        assert_eq!(shared.is_some(), accepted, "shared decision for {label}");

        let payment = fixture.signed(funding.clone(), None, FUND - FEE, None)?;
        let spend = fixture.seal(Transaction::new(
            fixture.deps.clone(),
            vec![],
            points.iter().cloned().map(|point| CellInput::new(point, None)).collect(),
            outputs.clone(),
            vec![],
        )?)?;
        let payment_out = OutPoint::new(payment.id(&fixture.spec.network), 0);
        let asset_outputs = outputs
            .iter()
            .enumerate()
            .map(|(index, cell)| (OutPoint::new(spend.id(&fixture.spec.network), index as u32), cell))
            .collect::<Vec<_>>();
        let time = TIME + index as u64 + 1;
        let tip = serial_store.tip().clone();
        let pair = [payment, spend];
        let serial = serial_store.prepare(&pair, time, false);
        let parallel = workers.install(|| parallel_store.prepare(&pair, time, true));
        let cycles = if accepted {
            let serial = serial.with_context(|| format!("native serial accepted case {label}"))?;
            let parallel = parallel.with_context(|| format!("native parallel accepted case {label}"))?;
            assert_eq!(serial.header(), parallel.header(), "summary Header for {label}");
            assert_eq!(serial.receipts(), parallel.receipts(), "exact receipts for {label}");
            let warm = workers.install(|| parallel_store.prepare(&pair, time, true))?;
            assert_eq!(parallel.header(), warm.header(), "warm Header for {label}");
            assert_eq!(parallel.receipts(), warm.receipts(), "warm receipt for {label}");
            let cycles = serial.receipts()[1].cycles;
            serial_store.commit(serial)?;
            parallel_store.commit(parallel)?;
            for store in [&serial_store, &parallel_store] {
                assert!(store.prove_cell(&payment_out)?.0.is_some());
                for (point, expected) in &asset_outputs {
                    let actual = store.prove_cell(point)?.0.context("expected state output")?;
                    assert_eq!(actual.cell.as_slice(), expected.as_slice());
                }
                for point in points.iter().chain([&funding]) {
                    assert!(store.prove_cell(point)?.0.is_none());
                }
            }
            Some(cycles)
        } else {
            for verdict in [serial, parallel] {
                let error = verdict.err().with_context(|| format!("native accepted rejected {label}"))?;
                let reason = format!("{error:#}");
                ensure!(
                    [55, 92, 93, 94].into_iter().any(|code| reason.contains(&format!("exited with {code}"))),
                    "unexpected native outcome for {label}: {reason}"
                );
            }
            for store in [&serial_store, &parallel_store] {
                assert_eq!(store.tip(), &tip, "rejected preparation advanced state for {label}");
                assert!(store.prove_cell(&payment_out)?.0.is_none());
                for point in points.iter().chain([&funding]) {
                    assert!(store.prove_cell(point)?.0.is_some());
                }
            }
            None
        };
        report.push(json!({"case":case.name,"requested_width":count,"groups":expected.len(),"accepted":accepted,
            "native_cycles":cycles,"ckb_inline_cycles":inline,"ckb_shared_cycles":shared}));
    }
    println!("Compared {} native transactions with both CKB Manifest encodings", report.len());
    if let Some(root) = std::env::var_os("STEM_TEST_EVIDENCE_ROOT") {
        let root = std::path::Path::new(&root);
        std::fs::create_dir_all(root)?;
        std::fs::write(
            root.join("fungible-conformance.json"),
            serde_json::to_vec_pretty(&json!({
                "native_cycle_limit": fixture.spec.limits.transaction_cycles, "ckb_type_cycle_limit":CKB_CYCLES,
                "guest_hashes": ckb.deps[..3].iter().map(|program| hex::encode(blake2b_256(program))).collect::<Vec<_>>(),
                "native_guest_hashes":[hex::encode(asset_hash),hex::encode(cap_hash)], "cases":report,
            }))?,
        )?;
    }
    Ok(())
}
