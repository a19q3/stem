use stem_ledger::lineage as ledger;

use super::*;

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Encode a Lineage command from JSON with strictly ordered target IDs.
    Build {
        #[arg(long)]
        request: PathBuf,
    },
    /// Print the business actor ID controlled by a key.
    Actor {
        #[arg(long)]
        key_file: PathBuf,
    },
    /// Sign the business terms independently of a fee Cell.
    SignBusiness {
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        transaction: PathBuf,
        #[arg(long, value_parser = crate::bridge_cli::parse_hash)]
        executor_hash: H256,
        #[arg(long)]
        key_file: PathBuf,
    },
    /// Attach the complete set of business seals and choose the sponsor's fee.
    Assemble {
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        transaction: PathBuf,
        /// JSON OutPoint for the sponsor's plain Capacity Cell.
        #[arg(long)]
        fee_input: PathBuf,
        #[arg(long)]
        fee: u64,
        #[arg(long, required = true)]
        seal: Vec<PathBuf>,
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
    fee_input: Option<OutPoint>,
    valid_before_height: u64,
    execute_at: Option<u64>,
    execute_after_ms: Option<u64>,
    executor: H256,
    executor_dep: OutPoint,
    executor_cycles: u64,
    validation_cycles: u64,
    fee: Option<u64>,
    business: Option<Business>,
    payload: JsonBytes,
    targets: Vec<Target>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Business {
    actors: Vec<H256>,
    max_fee: u64,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct BusinessSeal {
    actor: H256,
    seal: JsonBytes,
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
                let business = request.business.map(|intent| ledger::Intent {
                    actors: intent.actors.iter().map(Pack::pack).collect(),
                    max_fee: intent.max_fee,
                });
                let fee_input = match request.fee_input {
                    Some(point) => point.into(),
                    None if business.is_some() => packed::OutPoint::new([0xff; 32].pack(), 0),
                    None => anyhow::bail!("fee_input is required for an ordinary Lineage command"),
                };
                let fee = request
                    .fee
                    .or(business.as_ref().map(|intent| intent.max_fee))
                    .context("missing fee")?;
                let tx = Transaction::new_lineage_command(
                    request.cell_deps.into_iter().map(Into::into).collect(),
                    fee_input,
                    request.valid_before_height,
                    ledger::Command {
                        executor: request.executor.pack(),
                        executor_dep: request.executor_dep.into(),
                        executor_cycles: request.executor_cycles,
                        validation_cycles: request.validation_cycles,
                        fee,
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
                )?;
                let tx = if let Some(intent) = business {
                    tx.with_lineage_intent(intent)?
                } else {
                    tx
                };
                match (request.execute_at, request.execute_after_ms) {
                    (Some(height), None) => tx.with_execute_at(height)?,
                    (None, Some(time)) => {
                        tx.with_keeper_trigger(stem_ledger::keeper::Trigger::Time(time))?
                    }
                    (None, None) => tx,
                    _ => anyhow::bail!("choose a single Keeper trigger"),
                }
            }
            Self::Actor { key_file } => {
                return print_json(&H256::from(ledger::actor_id(
                    key(&key_file)?
                        .pubkey()?
                        .serialize()
                        .as_slice()
                        .try_into()?,
                )));
            }
            Self::SignBusiness {
                genesis,
                transaction,
                executor_hash,
                key_file,
            } => {
                let spec = native_spec::load(&genesis)?;
                let tx = load_transaction(&transaction)?;
                let intent = tx.lineage_intent().context("expected a Lineage intent")?;
                ensure!(
                    tx.lineage_command().expect("validated intent").executor
                        == executor_hash.pack(),
                    "unexpected Lineage Executor"
                );
                let key = key(&key_file)?;
                let actor = ledger::actor_id(key.pubkey()?.serialize().as_slice().try_into()?);
                ensure!(
                    intent.actors.binary_search(&actor).is_ok(),
                    "key is not a business actor"
                );
                let digest = tx.lineage_intent_digest(&spec.network)?;
                let seal = key.sign_recoverable(&digest.unpack())?.serialize();
                return print_json(&BusinessSeal {
                    actor: H256::from(actor),
                    seal: JsonBytes::from_vec(seal),
                });
            }
            Self::Assemble {
                genesis,
                transaction,
                fee_input,
                fee,
                seal,
            } => {
                let spec = native_spec::load(&genesis)?;
                let tx = load_transaction(&transaction)?;
                let point: OutPoint = read_json_limited(&fee_input, 1024)?;
                let tx = tx.with_lineage_fee(point.into(), fee)?;
                let intent = tx.lineage_intent().expect("validated intent");
                ensure!(
                    seal.len() == intent.actors.len(),
                    "expected one seal per business actor"
                );
                let mut seals = BTreeMap::new();
                for path in seal {
                    let item: BusinessSeal = read_json_limited(&path, 1024)?;
                    ensure!(
                        item.seal.as_bytes().len() == ledger::SEAL_LEN,
                        "invalid business seal length"
                    );
                    ensure!(
                        seals.insert(item.actor.pack(), item.seal).is_none(),
                        "duplicate business actor seal"
                    );
                }
                let mut ordered = Vec::with_capacity(intent.actors.len() * ledger::SEAL_LEN);
                for actor in &intent.actors {
                    ordered.extend_from_slice(
                        seals
                            .get(actor)
                            .context("missing business actor seal")?
                            .as_bytes(),
                    );
                }
                let tx = tx.with_lineage_seals(ordered.into())?;
                stem_vm::executor::verify_business_actors(&tx, &spec.network)?;
                tx
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
                let tx = load_transaction(&transaction)?;
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
                stem_vm::executor::verify_business_actors(&tx, &spec.network)?;
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

fn load_transaction(path: &Path) -> Result<Transaction> {
    let raw: JsonBytes = read_json_limited(path, 2 * stem_ledger::MAX_TRANSACTION_BYTES + 1024)?;
    Transaction::from_slice(raw.as_bytes())
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
