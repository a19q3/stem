//! Build the same native Lock Script for host-side integration tests.

use std::{fs, path::Path, process::Command, sync::OnceLock};

use anyhow::{Context, Result, ensure};
use ckb_types::bytes::Bytes;

pub fn native_lock(workspace: &Path) -> Result<Bytes> {
    static BUILD: OnceLock<Result<Bytes, String>> = OnceLock::new();
    BUILD
        .get_or_init(|| build(workspace).map_err(|error| format!("{error:#}")))
        .clone()
        .map_err(anyhow::Error::msg)
}

fn build(workspace: &Path) -> Result<Bytes> {
    let manifest = workspace.join("contracts/Cargo.toml");
    let target = workspace.join("target/fern-contracts");
    let output = Command::new(env!("CARGO"))
        .args([
            "build",
            "--locked",
            "--release",
            "--target",
            "riscv64imac-unknown-none-elf",
            "--bin",
            "fern-lock",
        ])
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--target-dir")
        .arg(&target)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env("RUSTFLAGS", "-C passes=lower-atomic -C target-feature=-c")
        .output()
        .context("build the native secp256k1 Lock Script")?;
    ensure!(
        output.status.success(),
        "native secp256k1 build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(Bytes::from(fs::read(
        target.join("riscv64imac-unknown-none-elf/release/fern-lock"),
    )?))
}
