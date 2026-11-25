//! Build the same native Lock Script for host-side integration tests.

use std::{fs, path::Path, process::Command, sync::OnceLock};

use anyhow::{Context, Result, ensure};
use ckb_types::bytes::Bytes;

pub fn native_lock(workspace: &Path) -> Result<Bytes> {
    native_contract(workspace, "stem-lock")
}

pub fn native_contract(workspace: &Path, name: &str) -> Result<Bytes> {
    // Comparison fixtures can share the exact RISC-V bytes across checkouts.
    if let Some(directory) = std::env::var_os("STEM_TEST_CONTRACT_DIR") {
        return Ok(Bytes::from(fs::read(Path::new(&directory).join(name))?));
    }
    static BUILD: OnceLock<Result<(), String>> = OnceLock::new();
    let target = workspace
        .join(std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()))
        .join("stem-contracts");
    BUILD
        .get_or_init(|| build(workspace, &target).map_err(|error| format!("{error:#}")))
        .clone()
        .map_err(anyhow::Error::msg)?;
    Ok(Bytes::from(fs::read(
        target
            .join("riscv64imac-unknown-none-elf/release")
            .join(name),
    )?))
}

fn build(workspace: &Path, target: &Path) -> Result<()> {
    let manifest = workspace.join("contracts/Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .args([
            "build",
            "--locked",
            "--release",
            "--target",
            "riscv64imac-unknown-none-elf",
            "--bin",
            "stem-lock",
            "--bin",
            "bridge-udt",
            "--bin",
            "stem-intent-lock",
            "--bin",
            "stem-lineage",
            "--bin",
            "stem-lineage-lock",
            "--bin",
            "stem-lineage-counter",
            "--bin",
            "stem-lineage-swap",
            "--bin",
            "stem-lineage-match",
            "--bin",
            "stem-lineage-reserve-udt",
        ])
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--target-dir")
        .arg(target)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env("RUSTFLAGS", "-C passes=lower-atomic -C target-feature=-c")
        .output()
        .context("build the native RISC-V contracts")?;
    ensure!(
        output.status.success(),
        "native RISC-V build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
