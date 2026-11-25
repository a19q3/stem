//! Verify message authorizations against resolved Lock groups and a certified CKB observation.

use anyhow::{Context, Result, ensure};
use ckb_types::{bytes::Bytes, core::ScriptHashType, packed::Byte32, prelude::*};
use stem_bridge_wire::intercell::{
    Identity, MailboxPolicy, MailboxState, Message, RECEIVE_ACTION, SEND_ACTION, record_key,
};
use stem_ledger::{
    ResolvedTransaction, ScriptGroupKind,
    block::Header,
    patricia::Proof,
    protocol::l1_reference::{L1Reference, L1ReferenceKind},
};

use super::{Bridge, BridgeChanges, StagedBridge, Store, View};

pub(in crate::native_store) fn apply(
    store: &Store,
    staged: Option<(&Bridge, &StagedBridge)>,
    tx: &ResolvedTransaction,
    changes: &mut BridgeChanges,
) -> Result<(u64, u64, u64)> {
    let mut actions = tx.transaction().messages().flat_map(|message| {
        message.actions.into_iter().filter_map(move |action| {
            matches!(action.kind, SEND_ACTION | RECEIVE_ACTION)
                .then_some((message.group_index, action))
        })
    });
    let Some((group_index, action)) = actions.next() else {
        return Ok((0, 0, 0));
    };
    ensure!(
        actions.next().is_none(),
        "only one Intercell action is allowed"
    );
    ensure!(
        changes.protocol_result.is_empty(),
        "Intercell actions cannot share a Bridge Burn receipt"
    );
    let (bridge, stage) = staged.context("Intercell requires the configured CKB bridge")?;
    let (code, checkpoint) = bridge
        .intercell_mailbox
        .as_ref()
        .context("Intercell mailbox code is not configured")?;
    let message = Message::decode(&action.data).context("invalid Intercell message")?;
    let group = tx
        .groups()
        .get(usize::from(group_index))
        .context("missing Intercell Lock group")?;
    let lock = group.script.calc_script_hash();
    ensure!(
        group.kind == ScriptGroupKind::Lock && !group.inputs.is_empty(),
        "Intercell actions require a consumed Lock group"
    );
    let network = store.network();
    let source = bridge.l1_genesis.as_slice();
    let inbound = action.kind == RECEIVE_ACTION;
    if inbound {
        ensure!(
            message.source == source
                && message.destination == network.as_slice()
                && message.receiver == lock.as_slice(),
            "Intercell receiver or network mismatch"
        );
        // The application can read other certified outputs in the same transaction.
        // Only the exact historical send from this mailbox authorizes delivery.
        let mut matches = 0;
        for raw in tx.l1_references() {
            let reference = L1Reference::decode(raw)?;
            let Some(script) = reference.output.type_().to_opt() else {
                continue;
            };
            if reference.kind != L1ReferenceKind::Historical
                || script.code_hash() != *code
                || ScriptHashType::try_from(script.hash_type())? != ScriptHashType::Data2
                || script.calc_script_hash().as_slice() != message.sender
            {
                continue;
            }
            let policy = MailboxPolicy::decode(&script.args().raw_data())
                .context("invalid Intercell mailbox policy")?;
            let state =
                MailboxState::decode(&reference.data).context("invalid Intercell mailbox state")?;
            if policy.ckb_genesis == source
                && policy.stem_network == network.as_slice()
                && policy.checkpoint_code == checkpoint.code_hash
                && policy.checkpoint_id == checkpoint.type_id
                && policy.owner == reference.output.lock().calc_script_hash().as_slice()
                && state.outgoing == action.data.as_ref()
            {
                matches += 1;
            }
        }
        ensure!(
            matches == 1,
            "Intercell receive requires one uniquely matching certified CKB mailbox output"
        );
    } else {
        ensure!(
            message.source == network.as_slice()
                && message.destination == source
                && message.sender == lock.as_slice(),
            "Intercell sender or network mismatch"
        );
    }
    let id = message.id();
    let key = record_key(inbound, &id);
    let view = View {
        store,
        staged: &stage.records,
    };
    ensure!(
        view.read(&key)?.is_none(),
        "Intercell message nonce already used"
    );
    changes.protocol_result = Bytes::copy_from_slice(&id);
    changes
        .records
        .insert(key.to_vec(), Some(action.data.to_vec()));
    Ok((1, key.len() as u64, (key.len() + action.data.len()) as u64))
}

impl Store {
    pub fn prove_intercell(
        &self,
        inbound: bool,
        id: &Identity,
    ) -> Result<(Option<Vec<u8>>, Proof)> {
        self.prove_system(&record_key(inbound, id))
    }

    pub fn verify_intercell(
        network: &Byte32,
        header: &Header,
        inbound: bool,
        id: &Identity,
        record: Option<&[u8]>,
        proof: &Proof,
    ) -> bool {
        stem_ledger::light::verify_intercell(network, header, inbound, id, record, proof)
    }
}
