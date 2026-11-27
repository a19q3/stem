//! Fixed-route Case A experiment. Each chain uses the native execution proof.

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use ckb_hash::{blake2b_256, new_blake2b};
use ckb_types::{packed::Byte32, prelude::*};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use prost::Message as _;
use stem_bridge_wire::intercell::{Identity, Message, record_key};
use stem_ledger::{
    block::{Header, HeaderContext, StateCommitment},
    history::History,
    light::{FinalityVerifier, verify_intercell, verify_system_record},
    patricia::{Namespace, Patricia, Proof},
};
use stem_stake::{
    Member,
    jolteon::{COMMITTEE_RECORD, Committee, CommitteeChange, FinalityProof, HandoffProof, Point},
};

struct Head {
    network: Byte32,
    verifier: FinalityVerifier,
    current: Header,
}

impl Head {
    fn advance(&mut self, certificate: &[u8]) -> Result<()> {
        let next = self.verifier.verify(certificate, false)?;
        self.check_successor(next.header())?;
        self.current = next.header().clone();
        Ok(())
    }

    fn advance_handoff(&mut self, certificate: &[u8]) -> Result<()> {
        let boundary = self.verifier.verify(certificate, true)?;
        self.check_successor(boundary.header())?;
        self.verifier.accept_handoff(certificate)?;
        self.current = boundary.header().clone();
        Ok(())
    }

    fn check_successor(&self, header: &Header) -> Result<()> {
        ensure!(
            self.current.context.height.checked_add(1) == Some(header.context.height)
                && header.context.parent == self.current.id(&self.network)
                && header.context.timestamp_ms > self.current.context.timestamp_ms,
            "checkpoint does not extend the accepted head"
        );
        Ok(())
    }

    fn require_record(&self, key: &[u8], value: &[u8], proof: &Proof) -> Result<()> {
        ensure!(
            verify_system_record(&self.network, &self.current, key, Some(value), proof),
            "record is outside the accepted checkpoint"
        );
        Ok(())
    }
}

struct Registration<'a> {
    network: Identity,
    deployment: Identity,
    genesis: &'a Header,
    manager: VerifyingKey,
    members: &'a [Member],
    initial_checkpoint: Byte32,
}

impl Registration<'_> {
    fn digest(&self, primary_network: &Identity, primary_genesis: &Byte32) -> Identity {
        let mut hash = new_blake2b();
        hash.update(b"Stem Primary native registration v1\0");
        hash.update(primary_network);
        hash.update(primary_genesis.as_slice());
        hash.update(&self.network);
        hash.update(&self.deployment);
        hash.update(self.genesis.id(&self.network.pack()).as_slice());
        hash.update(self.manager.as_bytes());
        hash.update(self.initial_checkpoint.as_slice());
        // Committee entries have fixed widths and occupy the end of the message.
        for member in self.members {
            hash.update(&member.key);
            hash.update(&member.power.to_le_bytes());
        }
        let mut digest = [0; 32];
        hash.finalize(&mut digest);
        digest
    }
}

struct Registry {
    network: Identity,
    genesis: Byte32,
    registrar: VerifyingKey,
    chains: BTreeMap<Identity, RegisteredChain>,
}

struct RegisteredChain {
    statement: Identity,
    head: Head,
}

impl Registry {
    fn register(
        &mut self,
        request: &Registration<'_>,
        registrar_signature: &Signature,
        manager_signature: &Signature,
        certificate: &[u8],
    ) -> Result<()> {
        ensure!(request.network != self.network && !self.chains.contains_key(&request.network), "network identity is unavailable");
        ensure!(request.deployment != [0; 32], "missing deployment identity");
        ensure!(
            request.genesis.context.height == 0 && request.genesis.context.parent == Byte32::default(),
            "registration requires a genesis Header"
        );
        let digest = request.digest(&self.network, &self.genesis);
        self.registrar.verify_strict(&digest, registrar_signature)?;
        request.manager.verify_strict(&digest, manager_signature)?;
        let network: Byte32 = request.network.pack();
        let genesis = request.genesis.id(&network);
        let mut head = Head {
            verifier: FinalityVerifier::new(network.clone(), genesis, request.members)?,
            current: request.genesis.clone(),
            network,
        };
        head.advance(certificate)?;
        ensure!(head.current.id(&head.network) == request.initial_checkpoint, "finality proof authenticates a different checkpoint");
        self.chains.insert(request.network, RegisteredChain { statement: digest, head });
        Ok(())
    }
}

struct Route {
    source: Identity,
    primary: Identity,
    destination: Identity,
    sender: Identity,
    receiver: Identity,
}

struct Request {
    id: Identity,
    bytes_hash: Identity,
    amount: u64,
    cancel_after: u64,
    destination_expiry: u64,
}

impl Request {
    fn authenticate(route: &Route, head: &Head, raw: &[u8], path: &Proof) -> Result<Self> {
        let message = Message::decode(raw).context("invalid Intercell envelope")?;
        ensure!(
            head.network.as_slice() == route.source
                && message.source == route.source
                && message.destination == route.destination
                && message.sender == route.sender
                && message.receiver == route.receiver,
            "message outside the pinned route"
        );
        let id = message.id();
        ensure!(
            verify_intercell(&head.network, &head.current, false, &id, Some(raw), path),
            "message outside the authenticated Outbox"
        );
        let (fields, rest) = message.payload.as_chunks::<8>();
        let [amount, cancel_after, destination_expiry] = fields else {
            anyhow::bail!("invalid workflow payload length");
        };
        ensure!(rest.is_empty(), "invalid workflow payload length");
        let amount = u64::from_le_bytes(*amount);
        ensure!(amount > 0, "empty reservation");
        Ok(Self {
            id,
            bytes_hash: blake2b_256(raw),
            amount,
            cancel_after: u64::from_le_bytes(*cancel_after),
            destination_expiry: u64::from_le_bytes(*destination_expiry),
        })
    }

    fn decision_key(&self) -> Vec<u8> {
        [b"primary/decision/".as_slice(), self.id.as_slice()].concat()
    }

    fn outcome_key(&self) -> Vec<u8> {
        [b"primary/outcome/".as_slice(), self.id.as_slice()].concat()
    }

    fn decision(&self, decision: Decision) -> [u8; 33] {
        let mut raw = [0; 33];
        raw[0] = decision as u8;
        raw[1..].copy_from_slice(&self.bytes_hash);
        raw
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Decision {
    Deliver = 0,
    Cancel = 1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Outcome {
    bytes_hash: Identity,
    executed: bool,
    height: u64,
}

impl Outcome {
    fn encode(self) -> [u8; 41] {
        let mut raw = [0; 41];
        raw[0] = u8::from(self.executed);
        raw[1..9].copy_from_slice(&self.height.to_le_bytes());
        raw[9..].copy_from_slice(&self.bytes_hash);
        raw
    }
}

#[derive(Default)]
struct Destination {
    outcomes: BTreeMap<Identity, Outcome>,
    delivered: u64,
}

impl Destination {
    fn apply(
        &mut self,
        route: &Route,
        request: &Request,
        primary: &Head,
        decision: Decision,
        proof: &Proof,
        height: u64,
    ) -> Result<Outcome> {
        ensure!(primary.network.as_slice() == route.primary, "wrong Primary");
        primary.require_record(&request.decision_key(), &request.decision(decision), proof)?;
        if let Some(outcome) = self.outcomes.get(&request.id) {
            ensure!(outcome.bytes_hash == request.bytes_hash, "changed request");
            return Ok(*outcome);
        }
        ensure!(
            (decision == Decision::Cancel) == (height >= request.destination_expiry),
            "decision is ineligible at the destination height"
        );
        let outcome = Outcome { bytes_hash: request.bytes_hash, executed: decision == Decision::Deliver, height };
        if outcome.executed {
            self.delivered = self.delivered.checked_add(request.amount).context("amount overflow")?;
        }
        self.outcomes.insert(request.id, outcome);
        Ok(outcome)
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Phase {
    Reserved,
    CancelRequested,
    Settled(Outcome),
    Refunded(Outcome),
}

struct Workflow {
    phase: Phase,
    request: Request,
}

impl Workflow {
    fn open(request: Request) -> Self {
        Self { request, phase: Phase::Reserved }
    }

    fn cancel(&mut self, height: u64) -> Result<()> {
        ensure!(self.phase == Phase::Reserved && height >= self.request.cancel_after, "cancellation is ineligible on Primary");
        self.phase = Phase::CancelRequested;
        Ok(())
    }

    fn decision(&self) -> Result<[u8; 33]> {
        match self.phase {
            Phase::Reserved => Ok(self.request.decision(Decision::Deliver)),
            Phase::CancelRequested => Ok(self.request.decision(Decision::Cancel)),
            _ => anyhow::bail!("workflow already finished"),
        }
    }

    fn finish(&mut self, route: &Route, head: &Head, outcome: Outcome, path: &Proof) -> Result<()> {
        ensure!(head.network.as_slice() == route.destination, "wrong destination");
        head.require_record(&self.request.outcome_key(), &outcome.encode(), path)?;
        ensure!(
            self.request.bytes_hash == outcome.bytes_hash
                && outcome.height <= head.current.context.height
                && outcome.executed == (outcome.height < self.request.destination_expiry),
            "outcome does not match the reserved workflow"
        );
        match self.phase {
            Phase::Settled(prior) | Phase::Refunded(prior) => {
                ensure!(outcome == prior, "conflicting terminal outcome");
            }
            Phase::Reserved | Phase::CancelRequested if outcome.executed => {
                self.phase = Phase::Settled(outcome);
            }
            Phase::CancelRequested => self.phase = Phase::Refunded(outcome),
            Phase::Reserved => anyhow::bail!("refund requires a prior cancellation request"),
        }
        Ok(())
    }
}

/// Produces synthetic state, signed by a real independent weighted committee.
struct Fixture {
    head: Head,
    committee: Committee,
    keys: Vec<SigningKey>,
    view_origin: u64,
    history: History,
    headers: Vec<Header>,
}

fn fixture_roster(seed: u8) -> (Vec<SigningKey>, Vec<Member>) {
    let mut keys: Vec<_> = (seed..seed + 4).map(|key| SigningKey::from_bytes(&[key; 32])).collect();
    keys.sort_by_key(|key| key.verifying_key().to_bytes());
    let members = keys.iter().zip([1, 2, 1, 3]).map(|(key, power)| Member { key: key.verifying_key().to_bytes(), power }).collect();
    (keys, members)
}

impl Fixture {
    fn new(network: Identity, seed: u8) -> Result<Self> {
        let (keys, members) = fixture_roster(seed);
        let network: Byte32 = network.pack();
        let history = History::empty(network.clone());
        let genesis = Header {
            context: HeaderContext { height: 0, timestamp_ms: 0, parent: Byte32::default(), transaction_root: Byte32::default() },
            state: StateCommitment {
                cell_root: Byte32::default(),
                system_root: Patricia::new(network.clone(), Namespace::System).empty_root().pack(),
                history_root: history.root(),
            },
            receipt_root: Byte32::default(),
        };
        let genesis_id = genesis.id(&network);
        let committee = Committee::new(network.unpack(), genesis_id.unpack(), 0, &members)?;
        let verifier = FinalityVerifier::new(network.clone(), genesis_id, &members)?;
        let head = Head { network, verifier, current: genesis.clone() };
        Ok(Self { head, committee, keys, view_origin: 0, history, headers: vec![genesis] })
    }

    fn append(&mut self, root: Byte32) -> Result<()> {
        let parent = self.headers.last().context("missing genesis")?;
        let context = HeaderContext::after(&self.head.network, parent, parent.context.timestamp_ms + 1, Byte32::default())?;
        self.headers.push(Header::next(
            &self.head.network,
            parent,
            &mut self.history,
            context,
            Byte32::default(),
            root,
            Byte32::default(),
        )?);
        Ok(())
    }

    fn quorum(&self, header: &Header) -> Result<Vec<u8>> {
        let view = header
            .context
            .height
            .checked_sub(self.view_origin)
            .filter(|&view| view != 0)
            .context("fixture Header predates this committee's first vote")?;
        self.sign_quorum(Point { view, parent_view: view - 1, header: header.id(&self.head.network).unpack() })
    }

    fn sign_quorum(&self, point: Point) -> Result<Vec<u8>> {
        let votes = self
            .keys
            .iter()
            .enumerate()
            .map(|(index, key)| (index as u16, key.sign(&self.committee.vote_digest(point)).to_bytes()))
            .collect();
        Ok(self.committee.assemble_quorum(point, &votes)?.encode())
    }

    fn certificate(&self, height: u64) -> Result<Vec<u8>> {
        let parent = &self.headers[height as usize];
        let child = &self.headers[(height + 1) as usize];
        Ok(FinalityProof {
            parent_header: parent.encode().to_vec(),
            parent_qc: self.quorum(parent)?,
            child_header: child.encode().to_vec(),
            child_qc: self.quorum(child)?,
        }
        .encode_to_vec())
    }

    fn commit_record(&mut self, height: u64, key: &[u8], value: &[u8]) -> Result<Proof> {
        // An earlier QC has fixed the current fixture tip. Fill the remaining
        // heights on that same parent chain before building the requested record.
        ensure!(height >= self.headers.len() as u64, "record height was already proposed");
        let (root, proof) = self.record_root(key, value)?;
        while self.headers.len() < height as usize {
            self.append(self.headers.last().unwrap().state.system_root.clone())?;
        }
        self.append(root.clone())?;
        self.append(root)?;
        self.advance_head(height)?;
        Ok(proof)
    }

    fn record_root(&self, key: &[u8], value: &[u8]) -> Result<(Byte32, Proof)> {
        let tree = Patricia::new(self.head.network.clone(), Namespace::System);
        let mut edit = tree.edit(tree.empty_root(), |_| Ok(None::<Vec<u8>>));
        let key = tree.record_key(key);
        edit.set(key, Some(tree.value_hash(value)))?;
        let proof = edit.prove(&key)?.1;
        Ok((edit.root().pack(), proof))
    }

    fn handoff(&self, announcement: &[u8], path: &Proof) -> Result<Vec<u8>> {
        let boundary = self.headers.last().context("missing handoff boundary")?;
        let view = boundary.context.height - self.view_origin;
        let seal = self.committee.seal_header(boundary.id(&self.head.network).unpack());
        Ok(HandoffProof {
            boundary_header: boundary.encode().to_vec(),
            parent_qc: self.quorum(boundary)?,
            seal_qc: self.sign_quorum(Point { view: view + 1, parent_view: view, header: seal })?,
            announcement: announcement.to_vec(),
            path: path.encode()?.to_vec(),
        }
        .encode_to_vec())
    }

    fn advance_head(&mut self, height: u64) -> Result<()> {
        while self.head.current.context.height < height {
            let certificate = self.certificate(self.head.current.context.height + 1)?;
            self.head.advance(&certificate)?;
        }
        Ok(())
    }

    fn extend_unchanged(&mut self, height: u64) -> Result<()> {
        while self.headers.len() <= height as usize + 1 {
            self.append(self.headers.last().unwrap().state.system_root.clone())?;
        }
        self.advance_head(height)
    }
}

fn make_route() -> Route {
    Route { source: [10; 32], primary: [20; 32], destination: [30; 32], sender: [40; 32], receiver: [50; 32] }
}

fn reserved(route: &Route) -> Result<(Fixture, Workflow, Vec<u8>, Proof)> {
    let mut source = Fixture::new(route.source, 1)?;
    let payload = [600u64, 5, 4].into_iter().flat_map(u64::to_le_bytes).collect::<Vec<_>>();
    let message = Message {
        source: route.source,
        destination: route.destination,
        sender: route.sender,
        receiver: route.receiver,
        nonce: 0,
        payload: &payload,
    };
    let raw = [message.header().as_slice(), &payload].concat();
    let proof = source.commit_record(1, &record_key(false, &message.id()), &raw)?;
    let request = Request::authenticate(route, &source.head, &raw, &proof)?;
    Ok((source, Workflow::open(request), raw, proof))
}

#[test]
fn signed_registration_pins_the_initial_checkpoint_and_ancestor_of_the_handoff() -> Result<()> {
    let route = make_route();
    let primary = Fixture::new(route.primary, 11)?;
    let primary_genesis = primary.head.current.id(&primary.head.network);
    let registrar = SigningKey::from_bytes(&[90; 32]);
    let manager = SigningKey::from_bytes(&[91; 32]);
    let mut registry = Registry {
        network: route.primary,
        genesis: primary_genesis.clone(),
        registrar: registrar.verifying_key(),
        chains: BTreeMap::new(),
    };

    let mut source = Fixture::new(route.source, 1)?;
    let (_, initial_members) = fixture_roster(1);
    let (successor_keys, successor_members) = fixture_roster(31);
    let announcement = CommitteeChange { epoch: 1, activation_height: 3, members: successor_members.clone() }.encode();
    let (root, path) = source.record_root(COMMITTEE_RECORD, &announcement)?;
    source.append(source.head.current.state.system_root.clone())?;
    source.append(root.clone())?;
    let initial = source.certificate(1)?;
    let mut request = Registration {
        network: route.source,
        deployment: [12; 32],
        genesis: &source.headers[0],
        manager: manager.verifying_key(),
        members: &initial_members,
        initial_checkpoint: source.headers[1].id(&source.head.network),
    };
    let statement = request.digest(&route.primary, &primary_genesis);
    let registrar_signature = registrar.sign(&statement);
    let manager_signature = manager.sign(&statement);
    let another_primary = request.digest(&route.primary, &[99; 32].pack());
    assert!(registry
        .register(&request, &registrar.sign(&another_primary), &manager.sign(&another_primary), &initial)
        .is_err());
    assert!(registry.register(&request, &registrar_signature, &registrar_signature, &initial).is_err());
    assert!(registry.register(&request, &manager_signature, &manager_signature, &initial).is_err());

    let checkpoint = request.initial_checkpoint.clone();
    request.initial_checkpoint = source.headers[2].id(&source.head.network);
    let wrong_checkpoint = request.digest(&route.primary, &primary_genesis);
    assert!(registry
        .register(&request, &registrar.sign(&wrong_checkpoint), &manager.sign(&wrong_checkpoint), &initial)
        .is_err());
    request.initial_checkpoint = checkpoint;
    request.members = &successor_members;
    let wrong_roster = request.digest(&route.primary, &primary_genesis);
    assert!(registry
        .register(&request, &registrar.sign(&wrong_roster), &manager.sign(&wrong_roster), &initial)
        .is_err());
    request.members = &initial_members;
    request.deployment = [13; 32];
    assert!(registry.register(&request, &registrar_signature, &manager_signature, &initial).is_err());
    request.deployment = [12; 32];
    assert!(registry.chains.is_empty());
    registry.register(&request, &registrar_signature, &manager_signature, &initial)?;
    assert!(registry.register(&request, &registrar_signature, &manager_signature, &initial).is_err());

    let mut destination = Fixture::new(route.destination, 21)?;
    destination.extend_unchanged(1)?;
    let destination_manager = SigningKey::from_bytes(&[92; 32]);
    let (_, destination_members) = fixture_roster(21);
    let destination_request = Registration {
        network: route.destination,
        deployment: [15; 32],
        genesis: &destination.headers[0],
        manager: destination_manager.verifying_key(),
        members: &destination_members,
        initial_checkpoint: destination.head.current.id(&destination.head.network),
    };
    let destination_statement = destination_request.digest(&route.primary, &primary_genesis);
    let destination_proof = destination.certificate(1)?;
    registry.register(
        &destination_request,
        &registrar.sign(&destination_statement),
        &destination_manager.sign(&destination_statement),
        &destination_proof,
    )?;

    let handoff = source.handoff(&announcement, &path)?;
    let mut fork = Fixture::new(route.source, 1)?;
    fork.append([88; 32].pack())?;
    fork.append(root)?;
    let fork_handoff = fork.handoff(&announcement, &path)?;
    let admitted = registry.chains.get_mut(&route.source).context("source registration")?;
    assert_eq!(admitted.statement, statement);
    assert_eq!(admitted.head.current, source.headers[1]);
    // Even a signed terminal proof must extend the exact accepted checkpoint.
    assert!(admitted.head.verifier.verify(&fork_handoff, true).is_ok());
    assert!(admitted.head.advance_handoff(&fork_handoff).is_err());
    assert_eq!(admitted.head.verifier.epoch(), 0);
    assert_eq!(admitted.head.current, source.headers[1]);

    source.advance_head(1)?;
    source.head.advance_handoff(&handoff)?;
    let boundary = source.head.current.id(&source.head.network);
    source.committee = Committee::at_anchor(
        route.source,
        source.headers[0].id(&source.head.network).unpack(),
        1,
        boundary.unpack(),
        &successor_members,
    )?;
    source.keys = successor_keys;
    source.view_origin = source.head.current.context.height;

    let payload = [600u64, 5, 4].into_iter().flat_map(u64::to_le_bytes).collect::<Vec<_>>();
    let message = Message {
        source: route.source,
        destination: route.destination,
        sender: route.sender,
        receiver: route.receiver,
        nonce: 0,
        payload: &payload,
    };
    let raw = [message.header().as_slice(), &payload].concat();
    let outbox_path = source.commit_record(3, &record_key(false, &message.id()), &raw)?;
    let successor = source.certificate(3)?;
    assert!(admitted.head.advance(&successor).is_err());
    admitted.head.advance_handoff(&handoff)?;
    assert_eq!(admitted.head.verifier.epoch(), 1);
    assert_eq!(admitted.head.current.id(&source.head.network), boundary);
    assert!(admitted.head.advance_handoff(&handoff).is_err());
    assert!(admitted.head.advance(&initial).is_err());
    admitted.head.advance(&successor)?;
    let request = Request::authenticate(&route, &admitted.head, &raw, &outbox_path)?;
    assert_eq!(request.id, message.id());
    assert_eq!(request.amount, 600);
    let independent = &registry.chains[&route.destination];
    assert_eq!(independent.statement, destination_statement);
    assert_eq!(independent.head.verifier.epoch(), 0);
    assert_eq!(independent.head.current, destination.head.current);
    Ok(())
}

#[test]
fn pre_expiry_execution_settles_even_if_its_receipt_arrives_after_cancel() -> Result<()> {
    let route = make_route();
    let (_, mut workflow, _, _) = reserved(&route)?;
    let mut primary = Fixture::new(route.primary, 11)?;
    let mut remote = Fixture::new(route.destination, 21)?;
    let mut destination = Destination::default();
    let key = workflow.request.decision_key();
    let relay = primary.commit_record(1, &key, &workflow.decision()?)?;
    let outcome = destination.apply(&route, &workflow.request, &primary.head, Decision::Deliver, &relay, 3)?;
    assert!(outcome.executed);
    let receipt = remote.commit_record(3, &workflow.request.outcome_key(), &outcome.encode())?;
    assert!(workflow.cancel(4).is_err());
    workflow.cancel(5)?;
    assert_eq!(workflow.phase, Phase::CancelRequested);
    let cancel = primary.commit_record(5, &key, &workflow.decision()?)?;
    assert_eq!(destination.apply(&route, &workflow.request, &primary.head, Decision::Cancel, &cancel, 8)?, outcome);
    assert!(destination.apply(&route, &workflow.request, &primary.head, Decision::Deliver, &relay, 8).is_err());
    remote.extend_unchanged(7)?;
    let cancelled = Outcome { executed: false, height: 4, ..outcome };
    assert!(workflow.finish(&route, &remote.head, cancelled, &receipt).is_err());
    assert_eq!(workflow.phase, Phase::CancelRequested);
    let mut recovered = Fixture::new(route.destination, 21)?;
    for height in 1..=7 {
        recovered.head.advance(&remote.certificate(height)?)?;
    }
    assert_eq!(recovered.head.current, remote.head.current);
    workflow.finish(&route, &recovered.head, outcome, &receipt)?;
    workflow.finish(&route, &remote.head, outcome, &receipt)?;
    assert_eq!(workflow.phase, Phase::Settled(outcome));
    assert_eq!(destination.delivered, 600);
    Ok(())
}

#[test]
fn cancellation_needs_destination_evidence_and_rules_out_late_execution() -> Result<()> {
    let route = make_route();
    let (_, mut workflow, _, _) = reserved(&route)?;
    let mut primary = Fixture::new(route.primary, 11)?;
    let mut remote = Fixture::new(route.destination, 21)?;
    let mut destination = Destination::default();
    let key = workflow.request.decision_key();
    let relay = primary.commit_record(1, &key, &workflow.decision()?)?;
    let mut delayed_relay = Fixture::new(route.primary, 11)?;
    delayed_relay.head.advance(&primary.certificate(1)?)?;
    assert!(destination.apply(&route, &workflow.request, &primary.head, Decision::Deliver, &relay, 4).is_err());
    assert_eq!(destination.delivered, 0);
    workflow.cancel(5)?;
    let cancel = primary.commit_record(5, &key, &workflow.decision()?)?;
    assert!(destination.apply(&route, &workflow.request, &primary.head, Decision::Cancel, &cancel, 3).is_err());
    let outcome = destination.apply(&route, &workflow.request, &primary.head, Decision::Cancel, &cancel, 4)?;
    assert!(!outcome.executed);
    let absent = Proof { nodes: vec![] };
    assert!(verify_system_record(&remote.head.network, &remote.head.current, &workflow.request.outcome_key(), None, &absent,));
    assert!(workflow.finish(&route, &remote.head, outcome, &absent).is_err());
    assert_eq!(workflow.phase, Phase::CancelRequested);
    let receipt = remote.commit_record(4, &workflow.request.outcome_key(), &outcome.encode())?;
    assert!(workflow.finish(&route, &primary.head, outcome, &receipt).is_err());
    workflow.finish(&route, &remote.head, outcome, &receipt)?;
    workflow.finish(&route, &remote.head, outcome, &receipt)?;
    assert_eq!(workflow.phase, Phase::Refunded(outcome));
    assert_eq!(destination.apply(&route, &workflow.request, &delayed_relay.head, Decision::Deliver, &relay, 5)?, outcome);
    assert_eq!(destination.apply(&route, &workflow.request, &primary.head, Decision::Cancel, &cancel, 5)?, outcome);
    assert_eq!(destination.delivered, 0);
    Ok(())
}

#[test]
fn source_requires_the_pinned_committee_ancestry_endpoint_and_exact_bytes() -> Result<()> {
    let route = make_route();
    let (source, workflow, raw, path) = reserved(&route)?;
    let mut impostor = Fixture::new(route.source, 51)?;
    let other_path = impostor.commit_record(1, &record_key(false, &workflow.request.id), &raw)?;
    assert_eq!(path, other_path);
    let mut verifier = Fixture::new(route.source, 1)?;
    assert!(verifier.head.advance(&impostor.certificate(1)?).is_err());
    assert!(verifier.head.advance(&source.certificate(1)?).is_ok());
    assert!(verifier.head.advance(&source.certificate(1)?).is_err());
    let mut skipped = Fixture::new(route.source, 1)?;
    skipped.commit_record(2, &record_key(false, &workflow.request.id), &raw)?;
    let mut start = Fixture::new(route.source, 1)?;
    assert!(start.head.advance(&skipped.certificate(2)?).is_err());
    let mut different_payload = raw.clone();
    *different_payload.last_mut().unwrap() ^= 1;
    assert_eq!(Message::decode(&different_payload).unwrap().id(), workflow.request.id);
    assert!(Request::authenticate(&route, &source.head, &different_payload, &path).is_err());
    let wrong_route = Route { receiver: [91; 32], ..route };
    assert!(Request::authenticate(&wrong_route, &source.head, &raw, &path).is_err());
    assert_eq!(verifier.head.current, source.head.current);
    Ok(())
}
