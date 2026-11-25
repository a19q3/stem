use anyhow::{Context, Result, ensure};
use ckb_vm::{
    DefaultMachineBuilder, DefaultMachineRunner, ISA_B, ISA_IMC, ISA_MOP, SupportMachine,
    cost_model::estimate_cycles,
    machine::{
        VERSION2,
        asm::{AsmCoreMachine, AsmMachine},
    },
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    committee: usize,
    signatures: usize,
    profile: String,
    genesis_app_hash: String,
    bundle: Bundle,
}

#[derive(Deserialize)]
struct Bundle {
    initial_committee: String,
    proof: String,
    tip: String,
}

#[test]
#[ignore = "run with Go-exported proof fixtures and the compiled RISC-V probe"]
fn direct_comet_verifier_cycles_at_weighted_committee_sizes() -> Result<()> {
    let source = std::env::var_os("STEM_DIRECT_COST_DIR").context("missing fixture directory")?;
    let elf = std::env::var_os("STEM_DIRECT_COST_ELF").context("missing RISC-V probe")?;
    let program = ckb_vm::Bytes::from(std::fs::read(elf)?);
    let mut paths: Vec<_> = std::fs::read_dir(source)?
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    ensure!(
        paths.len() == 12,
        "expected two profiles for each committee size"
    );
    for path in paths {
        let fixture: Fixture = serde_json::from_slice(&std::fs::read(path)?)?;
        let proof = fixture
            .bundle
            .proof
            .strip_prefix("0x")
            .context("missing proof prefix")?;
        let tip = fixture
            .bundle
            .tip
            .strip_prefix("0x")
            .context("missing tip prefix")?;
        let args = [
            &fixture.bundle.initial_committee[..],
            &fixture.genesis_app_hash[..],
            proof,
            tip,
        ]
        .map(|s| Ok(ckb_vm::Bytes::copy_from_slice(s.as_bytes())));
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
        vm.load_program(&program, args.into_iter())?;
        ensure!(vm.run()? == 0, "direct verifier rejected the Comet fixture");
        println!(
            "{}",
            serde_json::json!({
                "committee": fixture.committee, "signatures": fixture.signatures,
                "profile": fixture.profile, "proof_bytes": proof.len() / 2, "cycles": vm.machine.cycles(),
            })
        );
    }
    Ok(())
}
