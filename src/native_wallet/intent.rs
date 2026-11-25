use std::{collections::BTreeSet, fs::File, io::Read};

use ckb_crypto::secp::Signature;
use ckb_types::{core::ScriptHashType, packed::Byte32};
use serde::Serialize;
use stem_ledger::intent::{Authorization, LOCK_ARGS_BYTES, SPONSOR_BYTES, Sponsorship, Terms};

use super::*;

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Sign the input and matching output in a draft for later assembly.
    Sign {
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        transaction: PathBuf,
        #[arg(long)]
        context: PathBuf,
        #[arg(long)]
        code: PathBuf,
        #[arg(long)]
        key_file: PathBuf,
        #[arg(long)]
        input_index: usize,
        #[arg(long)]
        valid_before_height: u64,
        #[arg(long)]
        max_fee: u64,
        /// Authorize fee-only funding at position 0 for the pair at position 1.
        #[arg(long)]
        sponsor_max_debit: Option<u64>,
    },
    /// Attach signed terms to an assembled draft before collecting complete seals.
    Attach {
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        transaction: PathBuf,
        #[arg(long)]
        context: PathBuf,
        #[arg(long)]
        code: PathBuf,
        #[arg(long, required = true)]
        intent: Vec<PathBuf>,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SignedIntent {
    out_point: OutPoint,
    proof_data: JsonBytes,
    seal: JsonBytes,
}

impl Command {
    pub(super) fn run(self) -> Result<()> {
        match self {
            Self::Sign {
                genesis,
                transaction,
                context,
                code,
                key_file,
                input_index,
                valid_before_height,
                max_fee,
                sponsor_max_debit,
            } => {
                let network = native_spec::load(&genesis)?.network;
                let tx = load_draft(&transaction)?;
                let context = ResolvedSigningContext::load(&context, &network)?;
                let input = tx.input(input_index).context("missing intent input")?;
                let offered = context.cell(&input.previous_output)?;
                let delivery = output(&tx, input_index)?;
                let key = key(&key_file)?;
                verify_owner(&offered.lock(), &key, &code)?;
                let sponsorship = if let Some(max_debit) = sponsor_max_debit {
                    let subject = tx
                        .input(1)
                        .context("missing sponsored input at position 1")?;
                    ensure!(input_index == 0, "sponsor belongs at position 0");
                    let sponsored = output(&tx, 1)?;
                    Some(Sponsorship {
                        max_debit,
                        subject: subject.previous_output.as_slice().try_into()?,
                        delivery: sponsored.content_hash().as_slice().try_into()?,
                    })
                } else {
                    None
                };
                let terms = Terms {
                    valid_before: valid_before_height,
                    max_fee,
                    delivery: delivery.content_hash().as_slice().try_into()?,
                    sponsorship,
                };
                check_pair(&tx, input_index, &input, &offered, &terms, None)?;
                let deps = tx.intent_dependency_commitment(
                    &network,
                    |point| context.cell(point),
                    |id| context.header(id),
                )?;
                let digest = authorization(&network, &deps, &input, &offered, &terms)?;
                let seal = key.sign_recoverable(&digest.into())?.serialize();
                let mut proof = [0; SPONSOR_BYTES];
                let len = terms.encode(&mut proof);
                print_json(&SignedIntent {
                    out_point: input.previous_output.into(),
                    proof_data: JsonBytes::from_vec(proof[..len].to_vec()),
                    seal: JsonBytes::from_vec(seal),
                })
            }
            Self::Attach {
                genesis,
                transaction,
                context,
                code,
                intent,
            } => {
                let network = native_spec::load(&genesis)?.network;
                let tx = load_draft(&transaction)?;
                let context = ResolvedSigningContext::load(&context, &network)?;
                let code_hash = read_code_hash(&code)?;
                let intents = intent
                    .iter()
                    .map(|path| read_json_limited(path, 4096))
                    .collect::<Result<Vec<SignedIntent>>>()?;
                let sealed = attach(&tx, &network, &context, &code_hash, intents)?;
                print_json(&JsonBytes::from_bytes(Bytes::copy_from_slice(
                    sealed.as_slice(),
                )))
            }
        }
    }
}

fn attach(
    tx: &Transaction,
    network: &Byte32,
    context: &ResolvedSigningContext,
    code_hash: &Byte32,
    intents: Vec<SignedIntent>,
) -> Result<Transaction> {
    ensure!(
        tx.witnesses().all(|witness| witness.seal.is_empty()),
        "attach intents before collecting other seals"
    );
    let inputs = tx
        .inputs()
        .map(|input| context.cell(&input.previous_output))
        .collect::<Result<Vec<_>>>()?;
    let fee = capacity(inputs.iter().map(Cell::capacity))?
        .checked_sub(capacity(tx.outputs().map(|cell| cell.capacity()))?)
        .context("outputs exceed input Capacity")?;
    let locks: BTreeSet<_> = inputs.iter().map(|cell| cell.lock().as_bytes()).collect();
    let deps = tx.intent_dependency_commitment(
        network,
        |point| context.cell(point),
        |id| context.header(id),
    )?;
    let mut witnesses: BTreeMap<_, _> = tx
        .witnesses()
        .map(|witness| (witness.group_index, witness))
        .collect();
    for signed in intents {
        let point: packed::OutPoint = signed.out_point.into();
        let index = tx
            .inputs()
            .position(|input| input.previous_output == point)
            .context("intent input is absent from the draft")?;
        let offered = &inputs[index];
        let lock = offered.lock();
        let owner = intent_owner(&lock, code_hash)?;
        let encoded_lock = lock.as_bytes();
        ensure!(
            inputs
                .iter()
                .filter(|cell| cell.lock().as_bytes() == encoded_lock)
                .count()
                == 1,
            "Intent Lock group must consume exactly one Cell",
        );
        let group_index = u16::try_from(
            locks
                .iter()
                .position(|candidate| *candidate == encoded_lock)
                .context("missing Intent Lock group")?,
        )?;
        let terms = Terms::decode(signed.proof_data.as_bytes()).context("invalid intent terms")?;
        let input = tx.input(index).context("missing intent input")?;
        check_pair(tx, index, &input, offered, &terms, Some(fee))?;
        let digest = authorization(network, &deps, &input, offered, &terms)?;
        let signature = Signature::from_slice(signed.seal.as_bytes())?;
        let standard = signature.to_recoverable()?.to_standard();
        let mut normalized = standard;
        normalized.normalize_s();
        ensure!(
            standard == normalized,
            "intent signature has a high S value"
        );
        ensure!(
            blake2b_256(signature.recover(&digest.into())?.serialize())[..20] == owner,
            "intent seal does not authorize this draft"
        );
        ensure!(
            witnesses
                .insert(
                    group_index,
                    GroupWitness {
                        group_index,
                        proof_data: signed.proof_data.into_bytes(),
                        seal: signed.seal.into_bytes(),
                    }
                )
                .is_none(),
            "intent replaces an existing witness or a repeated intent"
        );
    }
    tx.with_witnesses(witnesses.into_values().collect())
}

fn check_pair(
    tx: &Transaction,
    index: usize,
    input: &CellInput,
    offered: &Cell,
    terms: &Terms,
    fee: Option<u64>,
) -> Result<()> {
    ensure!(terms.is_valid(), "invalid intent bounds");
    ensure!(input.since.is_none(), "paired intent requires zero Since");
    let delivery = output(tx, index)?;
    ensure!(
        delivery.content_hash().as_slice() == terms.delivery,
        "paired output differs from signed delivery"
    );
    ensure!(
        fee.is_none_or(|fee| fee <= terms.max_fee),
        "transaction fee exceeds intent cap"
    );
    if let Some(sponsor) = &terms.sponsorship {
        let target = tx.input(1).context("missing sponsored input")?;
        let expected = output(tx, 1)?;
        ensure!(
            index == 0
                && target.previous_output.as_slice() == sponsor.subject
                && expected.content_hash().as_slice() == sponsor.delivery
                && delivery.lock() == offered.lock()
                && [&delivery, offered]
                    .iter()
                    .all(|cell| cell.data().is_empty() && cell.type_scripts().len() == 0),
            "sponsorship must preserve plain Capacity change and the signed sponsored pair"
        );
        let debit = offered
            .capacity()
            .checked_sub(delivery.capacity())
            .context("sponsor change exceeds its input")?;
        ensure!(
            debit > 0 && debit <= sponsor.max_debit && fee.is_none_or(|fee| debit <= fee),
            "sponsor debit exceeds its contribution"
        );
    }
    Ok(())
}

fn authorization(
    network: &Byte32,
    deps: &Byte32,
    input: &CellInput,
    offered: &Cell,
    terms: &Terms,
) -> Result<[u8; 32]> {
    Ok(terms.signing_digest(&Authorization {
        network: network.as_slice().try_into()?,
        lock: offered.lock().calc_script_hash().as_slice().try_into()?,
        out_point: input.previous_output.as_slice().try_into()?,
        input_cell: offered.content_hash().as_slice().try_into()?,
        dependencies: deps.as_slice().try_into()?,
    }))
}

fn load_draft(path: &Path) -> Result<Transaction> {
    let encoded: JsonBytes =
        read_json_limited(path, 2 * stem_ledger::MAX_TRANSACTION_BYTES + 1024)?;
    let tx = Transaction::from_slice(encoded.as_bytes())?;
    ensure!(
        tx.operation().is_none() && tx.messages().len() == 0,
        "intent draft must be an ordinary transaction without Group Messages"
    );
    Ok(tx)
}

fn output(tx: &Transaction, index: usize) -> Result<Cell> {
    tx.outputs().nth(index).context("missing paired output")
}

fn capacity(mut cells: impl Iterator<Item = u64>) -> Result<u64> {
    cells.try_fold(0u64, |total, value| {
        total.checked_add(value).context("Capacity overflow")
    })
}

pub(super) fn verify_owner(lock: &PackedScript, key: &Privkey, code: &Path) -> Result<()> {
    let owner = intent_owner(lock, &read_code_hash(code)?)?;
    ensure!(
        blake2b_256(key.pubkey()?.serialize())[..20] == owner,
        "key does not own this Intent Lock"
    );
    Ok(())
}

fn read_code_hash(path: &Path) -> Result<Byte32> {
    let mut code = Vec::new();
    File::open(path)?
        .take(stem_ledger::MAX_CELL_BYTES as u64 + 1)
        .read_to_end(&mut code)?;
    ensure!(
        !code.is_empty() && code.len() <= stem_ledger::MAX_CELL_BYTES,
        "invalid Intent Lock ELF size"
    );
    Ok(blake2b_256(code).pack())
}

fn intent_owner(lock: &PackedScript, expected: &Byte32) -> Result<[u8; 20]> {
    let args = lock.args().raw_data();
    ensure!(
        args.len() == LOCK_ARGS_BYTES
            && &lock.code_hash() == expected
            && ScriptHashType::try_from(lock.hash_type())? == ScriptHashType::Data,
        "Lock does not name the selected Intent Lock binary"
    );
    Ok(args[..20].try_into()?)
}
