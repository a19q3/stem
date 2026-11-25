use clap::Args;
use fern_ledger::protocol::{BridgeSettlement, SettlementAction};

use super::*;

#[derive(Args)]
pub struct AttestVault {
    #[arg(long)]
    genesis: PathBuf,
    #[arg(long)]
    l1_rpc: String,
    #[arg(long)]
    request: PathBuf,
    #[arg(long)]
    key_file: PathBuf,
    #[arg(long)]
    journal: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VaultAt {
    lane: H256,
    vault: JsonPoint,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    tx: H256,
    previous_l1_height: u64,
    previous_l1_hash: H256,
    source: VaultAt,
    destination: Option<VaultAt>,
}

impl AttestVault {
    pub fn run(self) -> Result<()> {
        let spec = native_spec::load(&self.genesis)?;
        let bridge = spec.bridge.as_ref().context("native bridge disabled")?;
        let request: Request = read_json_limited(&self.request, 64 * 1024)?;
        let rpc = Rpc::new(&self.l1_rpc)?;
        check_l1(&rpc, bridge)?;
        let journal = Journal::open(&self.journal, spec.network.clone())?;
        journal.pin(&rpc, request.previous_l1_height, &request.previous_l1_hash)?;
        let (tx, height, block) = committed(&rpc, &request.tx)?;
        let source = transition(&rpc, bridge, &tx, height, request.source)?;
        let action = if let Some(destination) = request.destination {
            let destination = transition(&rpc, bridge, &tx, height, destination)?;
            let capacity = u64::from(source.before.capacity())
                .checked_sub(u64::from(source.after.capacity()))
                .context("L1 reserve source did not lose Capacity")?;
            ensure!(
                capacity != 0
                    && u64::from(destination.before.capacity()).checked_add(capacity)
                        == Some(u64::from(destination.after.capacity()))
                    && source.before.lock() == source.after.lock()
                    && destination.before.lock() == destination.after.lock()
                    && source.state.get(1) == Some(&3)
                    && destination.state.get(1) == Some(&4),
                "L1 transaction does not move an equal amount between two vaults"
            );
            SettlementAction::ReserveMoved {
                source: source.update,
                destination: destination.update,
                capacity,
            }
        } else {
            ensure!(
                source.state.get(1) == Some(&2)
                    && source.before.capacity() == source.after.capacity()
                    && source.before.lock() != source.after.lock(),
                "L1 transaction is not a custody rotation"
            );
            SettlementAction::VaultRotated {
                vault: source.update,
            }
        };
        let confirmed_height = height
            .checked_add(bridge.confirmations)
            .context("L1 confirmation height overflow")?
            .max(
                request
                    .previous_l1_height
                    .checked_add(1)
                    .context("L1 anchor height overflow")?,
            );
        let confirmed = rpc.block_hash(confirmed_height)?;
        ensure!(
            rpc.block_hash(height)? == block,
            "vault transaction was reorganized during observation"
        );
        journal.pin(&rpc, confirmed_height, &confirmed)?;
        let statement = BridgeSettlement {
            l1_genesis: bridge.l1_genesis.clone(),
            authority: bridge.guardian_generation.clone(),
            previous_anchor: L1Anchor {
                height: request.previous_l1_height,
                hash: request.previous_l1_hash.pack(),
            },
            anchor: L1Anchor {
                height: confirmed_height,
                hash: confirmed.pack(),
            },
            action,
        };
        super::payout::sign_settlement(&spec, bridge, statement, &self.key_file)
    }
}

struct Transition {
    update: VaultUpdate,
    before: CellOutput,
    after: CellOutput,
    state: ckb_types::bytes::Bytes,
}

fn transition(
    rpc: &Rpc,
    bridge: &BridgeGenesis,
    tx: &TransactionView,
    height: u64,
    at: VaultAt,
) -> Result<Transition> {
    let id = at.lane.pack();
    let lane = find_lane(bridge, &id)?;
    let is_vault = |output: &CellOutput| {
        output
            .type_()
            .to_opt()
            .is_some_and(|script| script.calc_script_hash() == lane.vault_type_hash)
    };
    let point: OutPoint = at.vault.into();
    let (before, old, origin) = l1_cell(rpc, &point)?;
    ensure!(
        is_vault(&before) && height > origin && tx.input_pts_iter().any(|input| input == point),
        "L1 transition does not consume the specified vault"
    );
    let mut outputs = tx
        .outputs_with_data_iter()
        .enumerate()
        .filter(|(_, (output, _))| is_vault(output));
    let (index, (after, state)) = outputs.next().context("vault has no L1 successor")?;
    ensure!(
        outputs.next().is_none() && old.len() == state.len() && old.get(2..10) == state.get(2..10),
        "L1 custody operation changed the vault payout nonce"
    );
    Ok(Transition {
        before,
        after,
        state,
        update: VaultUpdate {
            lane: id,
            input: point,
            output: OutPoint::new(tx.hash(), u32::try_from(index)?),
        },
    })
}
