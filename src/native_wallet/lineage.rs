use stem_ledger::lineage as ledger;

use super::*;

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Encode a Lineage command from JSON with strictly ordered target IDs.
    Build {
        #[arg(long)]
        request: PathBuf,
    },
    /// Seal an unsigned command with a standard Lineage fee Lock.
    Sign {
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        transaction: PathBuf,
        /// Authenticated fee and Executor Cells in native signing-context JSON.
        #[arg(long)]
        context: PathBuf,
        /// The trusted stem-lineage-lock ELF that protects the fee Cell.
        #[arg(long)]
        code: PathBuf,
        /// The expected Executor hash for the intended application.
        #[arg(long, value_parser = crate::bridge_cli::parse_hash)]
        executor_hash: H256,
        /// Local upper bound for the exact fee encoded in the command.
        #[arg(long)]
        max_fee: u64,
        #[arg(long)]
        key_file: PathBuf,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    cell_deps: Vec<CellDep>,
    fee_input: OutPoint,
    valid_before_height: u64,
    executor: H256,
    executor_dep: OutPoint,
    executor_cycles: u64,
    validation_cycles: u64,
    fee: u64,
    payload: JsonBytes,
    targets: Vec<Target>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    id: H256,
    min_version: u64,
    max_version: u64,
    min_capacity: u64,
}

impl Command {
    pub(super) fn run(self) -> Result<()> {
        let transaction = match self {
            Self::Build { request } => {
                let request: Request = read_json_limited(&request, 512 * 1024)?;
                Transaction::new_lineage_command(
                    request.cell_deps.into_iter().map(Into::into).collect(),
                    request.fee_input.into(),
                    request.valid_before_height,
                    ledger::Command {
                        executor: request.executor.pack(),
                        executor_dep: request.executor_dep.into(),
                        executor_cycles: request.executor_cycles,
                        validation_cycles: request.validation_cycles,
                        fee: request.fee,
                        payload: request.payload.into_bytes(),
                        targets: request
                            .targets
                            .into_iter()
                            .map(|target| ledger::Target {
                                id: target.id.pack(),
                                min_version: target.min_version,
                                max_version: target.max_version,
                                min_capacity: target.min_capacity,
                            })
                            .collect(),
                    },
                )?
            }
            Self::Sign {
                genesis,
                transaction,
                context,
                code,
                executor_hash,
                max_fee,
                key_file,
            } => {
                let spec = native_spec::load(&genesis)?;
                ensure!(
                    spec.lineage_type.is_some(),
                    "Lineages are disabled in this genesis"
                );
                let raw: JsonBytes =
                    read_json_limited(&transaction, 2 * stem_ledger::MAX_TRANSACTION_BYTES + 1024)?;
                let tx = Transaction::from_slice(raw.as_bytes())?;
                let command = tx.lineage_command().context("expected a Lineage command")?;
                ensure!(
                    tx.witnesses().len() == 0,
                    "expected an unsigned Lineage command"
                );
                ensure!(command.fee <= max_fee, "command exceeds the fee cap");
                ensure!(
                    &command.executor == &executor_hash.pack(),
                    "unexpected Lineage Executor"
                );
                let cells = ResolvedSigningContext::load(&context, &spec.network)?;
                let fee_point = tx.input(0).expect("validated fee input").previous_output;
                let fee_cell = cells.cell(&fee_point)?;
                let executor = cells.cell(&command.executor_dep)?;
                ensure!(
                    executor.data_hash() == &command.executor,
                    "Executor Cell has the wrong Data hash"
                );

                let key = key(&key_file)?;
                let owner = blake2b_256(key.pubkey()?.serialize());
                let lock = ledger::lock(&read_code_hash(&code)?, owner[..20].try_into()?, false);
                ensure!(
                    fee_cell.lock() == lock
                        && fee_cell.data().is_empty()
                        && fee_cell.type_scripts().len() == 0,
                    "fee input must be a plain CKB Cell protected by this Lineage fee Lock"
                );
                let refund = Cell::new(
                    fee_cell
                        .capacity()
                        .checked_sub(command.fee)
                        .context("insufficient Lineage fee Capacity")?,
                    lock,
                    vec![],
                    Bytes::new(),
                )?;
                spec.capacity.verify(&refund)?;
                let digest = tx.lineage_signing_digest(&spec.network, &fee_cell)?;
                tx.with_witnesses(vec![GroupWitness {
                    group_index: 0,
                    proof_data: Bytes::new(),
                    seal: Bytes::from(key.sign_recoverable(&digest.unpack())?.serialize()),
                }])?
            }
        };
        print_json(&JsonBytes::from_vec(transaction.as_slice().to_vec()))
    }
}

pub(super) fn verify_owner(lock: &PackedScript, key: &Privkey, code: &Path) -> Result<()> {
    let owner = blake2b_256(key.pubkey()?.serialize());
    let args = lock.args().raw_data();
    let is_head = match args.first() {
        Some(0) => false,
        Some(1) => true,
        _ => anyhow::bail!("invalid Lineage Lock role"),
    };
    ensure!(
        *lock == ledger::lock(&read_code_hash(code)?, owner[..20].try_into()?, is_head),
        "key or code does not match the Lineage Lock"
    );
    Ok(())
}
