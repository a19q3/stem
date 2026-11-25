//! Browser bindings for the native ledger and Jolteon proof verifier.

mod wallet;
pub use wallet::*;

use anyhow::{Context, Result, ensure};
use ckb_types::{
    packed::{Byte32, OutPoint},
    prelude::*,
};
use stem_bridge_wire::intercell::Message;
use stem_ledger::{
    Transaction,
    block::Receipt,
    light::{self, EXTINGUISHED_FEES_KEY, FinalityVerifier, VerifiedHeader},
    patricia::{Namespace, Patricia, Proof},
    proof::InclusionProof,
    protocol::{BridgeBacking, VaultRevenue},
    record,
};
use stem_stake::jolteon::CommitteeChange;
use wasm_bindgen::prelude::*;

fn js<T>(result: Result<T>) -> std::result::Result<T, JsError> {
    result.map_err(|error| JsError::new(&format!("{error:#}")))
}

#[wasm_bindgen]
pub struct StemVerifier(FinalityVerifier);

#[wasm_bindgen]
impl StemVerifier {
    /// Each argument is pinned application trust, not an RPC response.
    #[wasm_bindgen(constructor)]
    pub fn new(
        network: &[u8],
        genesis_id: &[u8],
        initial_committee: &[u8],
    ) -> Result<StemVerifier, JsError> {
        js((|| {
            let roster = CommitteeChange::decode(initial_committee)?;
            ensure!(
                roster.epoch == 0,
                "initial committee must be from epoch zero"
            );
            Ok(Self(FinalityVerifier::new(
                Byte32::from_slice(network)?,
                Byte32::from_slice(genesis_id)?,
                &roster.members,
            )?))
        })())
    }

    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.0.epoch()
    }

    #[wasm_bindgen(js_name = acceptHandoff)]
    pub fn accept_handoff(&mut self, proof: &[u8]) -> Result<AuthenticatedHeader, JsError> {
        js(self.0.accept_handoff(proof).map(AuthenticatedHeader))
    }

    #[wasm_bindgen(js_name = verifyFinality)]
    pub fn verify_finality(
        &self,
        proof: &[u8],
        terminal: bool,
    ) -> Result<AuthenticatedHeader, JsError> {
        js(self.0.verify(proof, terminal).map(AuthenticatedHeader))
    }
}

/// Proof operations are only available after authenticating a Header.
#[wasm_bindgen]
pub struct AuthenticatedHeader(VerifiedHeader);

#[wasm_bindgen]
impl AuthenticatedHeader {
    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u64 {
        self.0.header().context.height
    }

    #[wasm_bindgen(getter, js_name = timestampMs)]
    pub fn timestamp_ms(&self) -> u64 {
        self.0.header().context.timestamp_ms
    }

    pub fn id(&self) -> Vec<u8> {
        self.0.header().id(self.0.network()).as_slice().to_vec()
    }

    #[wasm_bindgen(js_name = appHash)]
    pub fn app_hash(&self) -> Vec<u8> {
        self.0
            .header()
            .app_hash(self.0.network())
            .as_slice()
            .to_vec()
    }

    #[wasm_bindgen(js_name = verifyLiveCell)]
    pub fn verify_live_cell(
        &self,
        point: &[u8],
        raw: &[u8],
        proof: &[u8],
    ) -> Result<bool, JsError> {
        js(verify_cell(&self.0, point, raw, proof))
    }

    #[wasm_bindgen(js_name = verifyExecution)]
    pub fn verify_execution(
        &self,
        transaction: &[u8],
        receipt: &[u8],
        proof: &[u8],
    ) -> Result<VerifiedExecution, JsError> {
        js((|| {
            let tx = Transaction::from_slice(transaction)?;
            let receipt = Receipt::decode(receipt)?;
            let (tx_proof, receipt_proof) = InclusionProof::decode_execution_pair(proof)?;
            ensure!(
                tx_proof.verify_execution(
                    self.0.network(),
                    &self.0.header().execution(),
                    &tx,
                    &receipt_proof,
                    &receipt
                ),
                "execution proof does not match the authenticated Header"
            );
            Ok(VerifiedExecution {
                id: tx.id(self.0.network()),
                position: tx_proof.position,
                receipt,
            })
        })())
    }

    #[wasm_bindgen(js_name = verifyIntercell)]
    pub fn verify_intercell(
        &self,
        inbound: bool,
        id: &[u8],
        record: &[u8],
        proof: &[u8],
    ) -> Result<bool, JsError> {
        js((|| {
            ensure!(
                light::verify_intercell(
                    self.0.network(),
                    self.0.header(),
                    inbound,
                    id.try_into()?,
                    nonempty(record),
                    &Proof::decode(proof)?
                ),
                "Intercell proof does not match the authenticated Header"
            );
            Ok(!record.is_empty())
        })())
    }

    #[wasm_bindgen(js_name = verifyVaultRevenue)]
    pub fn verify_vault_revenue(
        &self,
        record: &[u8],
        proof: &[u8],
        fees: Option<u64>,
        fee_proof: &[u8],
    ) -> Result<VerifiedVaultRevenue, JsError> {
        js((|| {
            let revenue = VaultRevenue::decode(record)?;
            self.authenticate_record(VaultRevenue::KEY, Some(record), proof)?;
            let fee_bytes = fees.map(u64::to_le_bytes);
            self.authenticate_record(
                EXTINGUISHED_FEES_KEY,
                fee_bytes.as_ref().map(|bytes| bytes.as_slice()),
                fee_proof,
            )?;
            let total = revenue.total()?;
            ensure!(
                total == fees.unwrap_or_default(),
                "vault revenue differs from the fee tally"
            );
            Ok(VerifiedVaultRevenue { revenue, total })
        })())
    }

    #[wasm_bindgen(js_name = verifySystemRecord)]
    pub fn verify_system_record(
        &self,
        key: &[u8],
        record: Option<Vec<u8>>,
        proof: &[u8],
    ) -> Result<(), JsError> {
        js(self.authenticate_record(key, record.as_deref(), proof))
    }
}

impl AuthenticatedHeader {
    fn authenticate_record(&self, key: &[u8], record: Option<&[u8]>, proof: &[u8]) -> Result<()> {
        ensure!(
            light::verify_system_record(
                self.0.network(),
                self.0.header(),
                key,
                record,
                &Proof::decode(proof)?
            ),
            "system proof does not match the authenticated Header"
        );
        Ok(())
    }
}

fn nonempty(bytes: &[u8]) -> Option<&[u8]> {
    (!bytes.is_empty()).then_some(bytes)
}

fn verify_cell(header: &VerifiedHeader, point: &[u8], raw: &[u8], proof: &[u8]) -> Result<bool> {
    let point = OutPoint::from_slice(point)?;
    let record = nonempty(raw).map(record::decode_cell).transpose()?;
    if let Some((actual, _)) = &record {
        ensure!(
            actual == &point,
            "Live Cell record uses a different OutPoint"
        );
    }
    let tree = Patricia::new(header.network().clone(), Namespace::Cells);
    ensure!(
        record::verify_cell(
            &tree,
            &header.header().state.cell_root.unpack(),
            &point,
            record.as_ref().map(|(_, cell)| cell),
            &Proof::decode(proof)?,
        ),
        "Live Cell proof does not match the authenticated Header"
    );
    Ok(record.is_some())
}

#[wasm_bindgen]
pub struct VerifiedExecution {
    id: Byte32,
    position: u32,
    receipt: Receipt,
}

#[wasm_bindgen]
impl VerifiedExecution {
    #[wasm_bindgen(getter)]
    pub fn fee(&self) -> u64 {
        self.receipt.fee
    }

    #[wasm_bindgen(getter)]
    pub fn cycles(&self) -> u64 {
        self.receipt.cycles
    }

    #[wasm_bindgen(getter)]
    pub fn position(&self) -> u32 {
        self.position
    }

    #[wasm_bindgen(js_name = transactionId)]
    pub fn transaction_id(&self) -> Vec<u8> {
        self.id.as_slice().to_vec()
    }

    #[wasm_bindgen(js_name = protocolResult)]
    pub fn protocol_result(&self) -> Vec<u8> {
        self.receipt.protocol_result.to_vec()
    }
}

#[wasm_bindgen]
pub struct VerifiedVaultRevenue {
    revenue: VaultRevenue,
    total: u64,
}

#[wasm_bindgen]
impl VerifiedVaultRevenue {
    #[wasm_bindgen(getter)]
    pub fn total(&self) -> u64 {
        self.total
    }

    #[wasm_bindgen(getter, js_name = laneCount)]
    pub fn lane_count(&self) -> usize {
        self.revenue.entries().len()
    }

    #[wasm_bindgen(js_name = laneId)]
    pub fn lane_id(&self, index: usize) -> Option<Vec<u8>> {
        self.revenue
            .entries()
            .get(index)
            .map(|entry| entry.lane.as_slice().to_vec())
    }

    #[wasm_bindgen(js_name = attributedFees)]
    pub fn attributed_fees(&self, index: usize) -> Option<u64> {
        self.revenue
            .entries()
            .get(index)
            .map(|entry| entry.attributed)
    }
}

#[wasm_bindgen(js_name = decodeBridgeBacking)]
pub fn decode_bridge_backing(raw: &[u8]) -> Result<DecodedBridgeBacking, JsError> {
    js(BridgeBacking::decode(raw).map(DecodedBridgeBacking))
}

#[wasm_bindgen]
pub struct DecodedBridgeBacking(BridgeBacking);

#[wasm_bindgen]
impl DecodedBridgeBacking {
    #[wasm_bindgen(getter)]
    pub fn liquid(&self) -> u64 {
        self.0.liquid()
    }

    #[wasm_bindgen(getter)]
    pub fn circulating(&self) -> u64 {
        self.0.circulating()
    }

    #[wasm_bindgen(getter)]
    pub fn pending(&self) -> u64 {
        self.0.pending()
    }

    #[wasm_bindgen(getter)]
    pub fn unallocated(&self) -> u64 {
        self.0.unallocated()
    }

    #[wasm_bindgen(getter, js_name = accruedStakeRewards)]
    pub fn accrued_stake_rewards(&self) -> u64 {
        self.0.accrued_stake_rewards()
    }
}

#[wasm_bindgen(js_name = intercellMessageId)]
pub fn intercell_message_id(raw: &[u8]) -> Result<Vec<u8>, JsError> {
    js(Message::decode(raw)
        .map(|message| message.id().to_vec())
        .context("invalid Intercell message"))
}
