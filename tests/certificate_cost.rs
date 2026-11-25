//! Single-threaded host measurements for the signer payload of a weighted QC.

use std::{collections::BTreeSet, hint::black_box, time::Instant};

use anyhow::{Context, Result, ensure};
use blst::{BLST_ERROR, min_pk as bls};
use ckb_hash::blake2b_256;
use ed25519_dalek::{Signer, SigningKey};
use stem_stake::{Attestation, Authority, Member, compact::SignerBitmap};

const VOTE_DST: &[u8] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";
const POP_DST: &[u8] = b"BLS_POP_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";
const MESSAGE: [u8; 32] = [21; 32];

struct Registration {
    key: [u8; 48],
    proof: [u8; 96],
}

impl Registration {
    fn new(secret: &bls::SecretKey) -> Self {
        let key = secret.sk_to_pk().to_bytes();
        let proof = secret.sign(&key, POP_DST, &[]).to_bytes();
        Self { key, proof }
    }
}

struct BlsCommittee {
    members: Vec<(bls::PublicKey, u64)>,
    total: u64,
}

impl BlsCommittee {
    fn register(registrations: &[Registration], members: &[Member]) -> Result<Self> {
        ensure!(
            registrations.len() == members.len(),
            "missing BLS registrations"
        );
        let mut authorities = BTreeSet::new();
        let mut registered = Vec::with_capacity(members.len());
        for (registration, member) in registrations.iter().zip(members) {
            ensure!(authorities.insert(registration.key), "duplicate BLS key");
            let key = bls::PublicKey::key_validate(&registration.key)
                .map_err(|err| anyhow::anyhow!("invalid BLS key: {err:?}"))?;
            let proof = bls::Signature::sig_validate(&registration.proof, true)
                .map_err(|err| anyhow::anyhow!("invalid possession proof: {err:?}"))?;
            ensure!(
                proof.verify(true, &registration.key, POP_DST, &[], &key, false)
                    == BLST_ERROR::BLST_SUCCESS,
                "BLS proof of possession is invalid"
            );
            registered.push((key, member.power));
        }
        Ok(Self {
            members: registered,
            total: members.iter().map(|member| member.power).sum(),
        })
    }

    fn verify(&self, message: &[u8], bitmap: &[u8], signature: &[u8]) -> Result<()> {
        let bitmap = SignerBitmap::new(self.members.len(), bitmap)?;
        let mut signed = 0u64;
        let mut keys = Vec::with_capacity(bitmap.signature_count());
        for index in bitmap.indices() {
            let (key, power) = &self.members[index];
            signed += power;
            keys.push(key);
        }
        ensure!(
            u128::from(signed) * 3 > u128::from(self.total) * 2,
            "underweight QC"
        );
        let signature = bls::Signature::sig_validate(signature, true)
            .map_err(|err| anyhow::anyhow!("invalid BLS signature: {err:?}"))?;
        ensure!(
            signature.fast_aggregate_verify(false, message, VOTE_DST, &keys)
                == BLST_ERROR::BLST_SUCCESS,
            "bad BLS aggregate"
        );
        Ok(())
    }
}

fn measure(samples: usize, mut run: impl FnMut() -> Result<()>) -> Result<u128> {
    run()?;
    let samples = if cfg!(debug_assertions) { 1 } else { samples };
    let mut times = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        run()?;
        times.push(start.elapsed().as_nanos());
    }
    times.sort_unstable();
    Ok(times[samples / 2])
}

fn fixture(size: usize, vm_program: Option<&ckb_vm::Bytes>) -> Result<serde_json::Value> {
    let mut ed_keys: Vec<_> = (0..size)
        .map(|index| SigningKey::from_bytes(&blake2b_256((index as u32).to_le_bytes())))
        .collect();
    ed_keys.sort_by_key(|key| key.verifying_key().to_bytes());
    let members: Vec<_> = ed_keys
        .iter()
        .enumerate()
        .map(|(index, key)| Member {
            key: key.verifying_key().to_bytes(),
            power: 1 + (index % 4) as u64,
        })
        .collect();
    let ed = Authority::new([1; 32], &members)?;
    let bls_keys: Vec<_> = (0..size)
        .map(|index| {
            bls::SecretKey::key_gen(
                &blake2b_256((index as u32).to_be_bytes()),
                b"Stem certificate lab",
            )
            .map_err(|err| anyhow::anyhow!("BLS keygen failed: {err:?}"))
        })
        .collect::<Result<_>>()?;
    let mut registrations: Vec<_> = bls_keys.iter().map(Registration::new).collect();
    let bls = BlsCommittee::register(&registrations, &members)?;
    registrations[0].proof[0] ^= 1;
    ensure!(
        BlsCommittee::register(&registrations, &members).is_err(),
        "corrupt possession proof accepted"
    );
    registrations[0] = Registration::new(&bls_keys[0]);
    registrations[1].key = registrations[0].key;
    ensure!(
        BlsCommittee::register(&registrations, &members).is_err(),
        "duplicate BLS registration accepted"
    );
    registrations[1] = Registration::new(&bls_keys[1]);

    let mut bitmap = vec![0; size.div_ceil(8)];
    let mut weight = 0;
    let mut signers = Vec::new();
    for (index, member) in members.iter().enumerate() {
        bitmap[index / 8] |= 1 << (index % 8);
        signers.push(index);
        weight += member.power;
        if weight * 3 > bls.total * 2 {
            break;
        }
    }
    let indexed_votes: Vec<_> = signers
        .iter()
        .map(|&i| Attestation {
            guardian: i as u16,
            signature: ed_keys[i].sign(&MESSAGE).to_bytes(),
        })
        .collect();
    let signatures: Vec<_> = indexed_votes.iter().map(|vote| vote.signature).collect();
    let individual: Vec<_> = signers
        .iter()
        .map(|&index| bls_keys[index].sign(&MESSAGE, VOTE_DST, &[]))
        .collect();
    let shares: Vec<_> = individual.iter().collect();
    let aggregate = || -> Result<[u8; 96]> {
        Ok(bls::AggregateSignature::aggregate(&shares, true)
            .map_err(|err| anyhow::anyhow!("aggregation failed: {err:?}"))?
            .to_signature()
            .to_bytes())
    };
    let aggregated = aggregate()?;
    ed.verify(&MESSAGE, &indexed_votes)?;
    ed.verify_compact(&MESSAGE, &bitmap, &signatures)?;
    bls.verify(&MESSAGE, &bitmap, &aggregated)?;
    let mut different: [u8; 32] = MESSAGE;
    different[0] ^= 1;
    ensure!(
        ed.verify_compact(&different, &bitmap, &signatures).is_err(),
        "wrong Ed25519 digest accepted"
    );
    ensure!(
        bls.verify(&different, &bitmap, &aggregated).is_err(),
        "wrong BLS digest accepted"
    );
    let mut underweight = bitmap.clone();
    let last = *signers.last().context("empty signing quorum")?;
    underweight[last / 8] &= !(1 << (last % 8));
    ensure!(
        ed.verify_compact(&MESSAGE, &underweight, &signatures)
            .is_err(),
        "signature count mismatch accepted"
    );
    ensure!(
        ed.verify_compact(&MESSAGE, &underweight, &signatures[..last])
            .err()
            == Some(stem_stake::Error::Quorum),
        "underweight Ed25519 certificate accepted"
    );
    ensure!(
        bls.verify(&MESSAGE, &underweight, &aggregated).is_err(),
        "underweight BLS certificate accepted"
    );
    if size % 8 != 0 {
        let mut padding = bitmap.clone();
        *padding.last_mut().context("missing bitmap")? |= 0x80;
        ensure!(
            SignerBitmap::new(size, &padding).is_err(),
            "nonzero padding accepted"
        );
    }

    let ed_compact_verify = measure(101, || {
        Ok(ed.verify_compact(black_box(&MESSAGE), &bitmap, &signatures)?)
    })?;
    let ed_indexed_verify = measure(101, || Ok(ed.verify(black_box(&MESSAGE), &indexed_votes)?))?;
    let bls_verify = measure(101, || {
        bls.verify(black_box(&MESSAGE), &bitmap, &aggregated)
    })?;
    let bls_aggregate = measure(101, || {
        black_box(aggregate()?);
        Ok(())
    })?;
    let ed_sign = measure(101, || {
        black_box(ed_keys[0].sign(black_box(&MESSAGE)));
        Ok(())
    })?;
    let bls_sign = measure(101, || {
        black_box(bls_keys[0].sign(black_box(&MESSAGE), VOTE_DST, &[]));
        Ok(())
    })?;
    let bls_register = measure(5, || {
        black_box(BlsCommittee::register(&registrations, &members)?);
        Ok(())
    })?;
    let mut report = serde_json::json!({
        "members": size,
        "signers": signers.len(),
        "signed_weight": weight,
        "total_weight": bls.total,
        "ed_indexed_payload_bytes": 2 + indexed_votes.len() * 66,
        "ed_compact_payload_bytes": bitmap.len() + signatures.len() * 64,
        "bls_compact_payload_bytes": bitmap.len() + aggregated.len(),
        "bls_registration_bytes": 144 * size,
        "ed_vote_signature_bytes": 64,
        "bls_vote_signature_bytes": 96,
        "ed_sign_ns_p50": ed_sign,
        "bls_sign_ns_p50": bls_sign,
        "ed_compact_verify_ns_p50": ed_compact_verify,
        "ed_indexed_verify_ns_p50": ed_indexed_verify,
        "bls_verify_ns_p50": bls_verify,
        "bls_aggregate_ns_p50": bls_aggregate,
        "bls_register_committee_ns_p50": bls_register,
    });
    if let Some(program) = vm_program {
        let ed_members = members
            .iter()
            .flat_map(|member| member.key.into_iter().chain(member.power.to_le_bytes()))
            .collect::<Vec<_>>();
        let bls_members = registrations
            .iter()
            .zip(&members)
            .flat_map(|(registration, member)| {
                registration
                    .key
                    .into_iter()
                    .chain(member.power.to_le_bytes())
            })
            .collect::<Vec<_>>();
        let (ed_exit, ed_cycles) = run_vm(
            program,
            "ed",
            &MESSAGE,
            &ed_members,
            &bitmap,
            signatures.as_flattened(),
        )?;
        let (bls_exit, bls_cycles) =
            run_vm(program, "bls", &MESSAGE, &bls_members, &bitmap, &aggregated)?;
        ensure!(
            (ed_exit, bls_exit) == (0, 0),
            "RISC-V failed to verify quorum"
        );
        ensure!(
            run_vm(
                program,
                "bls",
                &different,
                &bls_members,
                &bitmap,
                &aggregated
            )?
            .0 == 1,
            "RISC-V verified the signature over another message"
        );
        report["ed_ckb_vm_cycles"] = ed_cycles.into();
        report["bls_ckb_vm_cycles"] = bls_cycles.into();
    }
    Ok(report)
}

fn run_vm(
    program: &ckb_vm::Bytes,
    scheme: &'static str,
    message: &[u8],
    members: &[u8],
    bitmap: &[u8],
    signatures: &[u8],
) -> Result<(i8, u64)> {
    use ckb_vm::{
        DefaultMachineBuilder, DefaultMachineRunner, ISA_B, ISA_IMC, ISA_MOP, SupportMachine,
        cost_model::estimate_cycles,
        machine::{
            VERSION2,
            asm::{AsmCoreMachine, AsmMachine},
        },
    };
    let [message, members, bitmap, signatures] =
        [message, members, bitmap, signatures].map(|bytes| hex::encode(bytes).into());
    let args = [scheme.into(), message, members, bitmap, signatures];
    let core = <Box<AsmCoreMachine> as SupportMachine>::new(
        ISA_IMC | ISA_B | ISA_MOP,
        VERSION2,
        15_000_000_000,
    );
    let mut vm = AsmMachine::new(
        DefaultMachineBuilder::new(core)
            .instruction_cycle_func(Box::new(estimate_cycles))
            .build(),
    );
    vm.load_program(program, args.into_iter().map(Ok))?;
    let exit = vm.run()?;
    Ok((exit, vm.machine.cycles()))
}

#[test]
fn compares_ed25519_and_bls_certificate_payloads_and_host_cost() -> Result<()> {
    let vm_program = std::env::var_os("STEM_TEST_CERTIFICATE_ELF")
        .map(std::fs::read)
        .transpose()?
        .map(ckb_vm::Bytes::from);
    let measurements: Vec<_> = [4, 16, 32, 64, 100, 128]
        .into_iter()
        .map(|size| fixture(size, vm_program.as_ref()))
        .collect::<Result<_>>()?;
    println!("{}", serde_json::to_string(&measurements)?);
    Ok(())
}
