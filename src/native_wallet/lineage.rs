use stem_ledger::lineage as ledger;

use super::*;

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Derive a Lineage ID from the first registration input and output slot.
    Id {
        #[arg(long)]
        genesis: PathBuf,
        /// JSON OutPoint for the first registration input.
        #[arg(long)]
        anchor: PathBuf,
        #[arg(long)]
        output_index: u32,
    },
    /// Encode a LineageRef. Omit --version to select Latest.
    Reference {
        #[arg(long, value_parser = crate::bridge_cli::parse_hash)]
        id: H256,
        #[arg(long)]
        version: Option<u64>,
    },
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
    /// Bind several unsigned Lineage intents to one ordered atomic plan.
    Plan {
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long, required = true)]
        member: Vec<PathBuf>,
    },
    /// Encode the fully signed members of an atomic plan.
    Batch {
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long, required = true)]
        member: Vec<PathBuf>,
    },
    /// Sign the business terms independently of a fee Cell.
    SignBusiness {
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        transaction: PathBuf,
        /// Reviewed output of `wallet lineage plan` for a batch member.
        #[arg(long)]
        plan: Option<PathBuf>,
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
        /// Reviewed output of `wallet lineage plan` for a batch member.
        #[arg(long)]
        plan: Option<PathBuf>,
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
    retry: Option<RetryRequest>,
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
struct RetryRequest {
    max_attempts: u8,
    delay_blocks: u16,
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

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    plan: H256,
    members: Vec<JsonBytes>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    reference: JsonBytes,
    latest_bounds: Option<(u64, u64)>,
    min_capacity: u64,
}

impl Command {
    pub(super) fn run(self) -> Result<()> {
        let transaction = match self {
            Self::Id {
                genesis,
                anchor,
                output_index,
            } => {
                let network = native_spec::load(&genesis)?.network;
                let anchor: OutPoint = read_json_limited(&anchor, 1024)?;
                let anchor: packed::OutPoint = anchor.into();
                ensure!(
                    !anchor.is_null(),
                    "Lineage registration requires a real anchor"
                );
                return print_json(&H256::from(ledger::id(&network, &anchor, output_index)));
            }
            Self::Reference { id, version } => {
                let reference = ledger::LineageRef::new(id.pack(), version)?;
                return print_json(&JsonBytes::from_bytes(reference.encode()));
            }
            Self::Build { request } => {
                let request: Request = read_json_limited(&request, 512 * 1024)?;
                let business = request.business.map(|intent| ledger::Intent {
                    actors: intent.actors.iter().map(Pack::pack).collect(),
                    max_fee: intent.max_fee,
                    batch: None,
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
                            .map(|target| {
                                Ok(ledger::Target {
                                    reference: ledger::LineageRef::decode(
                                        target.reference.as_bytes(),
                                    )?,
                                    latest_bounds: target.latest_bounds,
                                    min_capacity: target.min_capacity,
                                })
                            })
                            .collect::<Result<_>>()?,
                    },
                )?;
                let tx = if let Some(intent) = business {
                    tx.with_lineage_intent(intent)?
                } else {
                    tx
                };
                use stem_ledger::keeper::{Retry, Schedule, Trigger};
                let trigger = match (request.execute_at, request.execute_after_ms) {
                    (Some(height), None) => Some(Trigger::Height(height)),
                    (None, Some(time)) => Some(Trigger::Time(time)),
                    (None, None) => None,
                    _ => anyhow::bail!("choose a single Keeper trigger"),
                };
                if let Some(trigger) = trigger {
                    tx.with_keeper_schedule(Schedule {
                        trigger,
                        retry: request.retry.map(|policy| Retry {
                            max_attempts: policy.max_attempts,
                            delay_blocks: policy.delay_blocks,
                        }),
                    })?
                } else {
                    ensure!(request.retry.is_none(), "Keeper retry requires a trigger");
                    tx
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
            Self::Plan { genesis, member } => {
                let network = native_spec::load(&genesis)?.network;
                let drafts = read_members(&member)?;
                let plan = Transaction::lineage_batch_plan(&network, &drafts)?;
                let bound = drafts
                    .iter()
                    .map(|draft| {
                        Ok(encode_transaction(
                            &draft.with_lineage_batch_plan(plan.clone())?,
                        ))
                    })
                    .collect::<Result<_>>()?;
                return print_json(&Plan {
                    plan: H256::from(plan),
                    members: bound,
                });
            }
            Self::Batch { genesis, member } => {
                let network = native_spec::load(&genesis)?.network;
                let members = read_members(&member)?;
                Transaction::verify_lineage_batch_plan(&network, &members)?;
                for child in &members {
                    stem_vm::executor::verify_business_actors(child, &network)?;
                }
                Transaction::new_lineage_batch(members)?
            }
            Self::SignBusiness {
                genesis,
                transaction,
                plan,
                executor_hash,
                key_file,
            } => {
                let spec = native_spec::load(&genesis)?;
                let tx = load_transaction(&transaction)?;
                check_plan(&tx, &spec.network, plan.as_deref())?;
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
                plan,
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
                check_plan(&tx, &spec.network, plan.as_deref())?;
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
        print_json(&encode_transaction(&transaction))
    }
}

fn encode_transaction(tx: &Transaction) -> JsonBytes {
    JsonBytes::from_vec(tx.as_slice().to_vec())
}

fn read_members(paths: &[PathBuf]) -> Result<Vec<Transaction>> {
    ensure!(
        (2..=ledger::MAX_BATCH_MEMBERS).contains(&paths.len()),
        "Lineage plan needs two to eight members"
    );
    paths.iter().map(|path| load_transaction(path)).collect()
}

fn check_plan(tx: &Transaction, network: &packed::Byte32, path: Option<&Path>) -> Result<()> {
    let Some(id) = tx.lineage_intent().and_then(|intent| intent.batch.as_ref()) else {
        ensure!(
            path.is_none(),
            "plan supplied for a standalone Lineage command"
        );
        return Ok(());
    };
    let path = path.context("batch-bound Lineage command requires the reviewed --plan")?;
    let plan: Plan = read_json_limited(path, 2 * stem_ledger::MAX_TRANSACTION_BYTES + 4096)?;
    ensure!(
        plan.plan.as_bytes() == id.as_slice(),
        "Lineage plan identity mismatch"
    );
    ensure!(
        (2..=ledger::MAX_BATCH_MEMBERS).contains(&plan.members.len()),
        "invalid Lineage plan member count"
    );
    let members = plan
        .members
        .iter()
        .map(|bytes| Transaction::from_slice(bytes.as_bytes()))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        &Transaction::verify_lineage_batch_plan(network, &members)? == id,
        "Lineage plan does not match its members"
    );
    let digest = tx.lineage_intent_digest(network)?;
    for member in members {
        if member.lineage_intent_digest(network)? == digest {
            return Ok(());
        }
    }
    anyhow::bail!("Lineage command is absent from the reviewed plan")
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
