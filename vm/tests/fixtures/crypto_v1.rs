// Runs in the native integration test so the guest is verified in both environments.
#[test]
fn crypto_v1_guest_uses_genesis_rules_and_fixed_cycle_limits() -> Result<()> {
    use ed25519_dalek::Signer;
    use stem_vm::executor;

    let dir = tempfile::tempdir()?;
    let mut secp = Vec::new();
    let mut ed = Vec::new();
    for index in 1..=64u8 {
        let digest = blake2b_256([index]);
        let key = k256::ecdsa::SigningKey::from_bytes((&[index; 32]).into())?;
        let (signature, _) = key.sign_prehash_recoverable(&digest)?;
        secp.extend_from_slice(key.verifying_key().to_encoded_point(true).as_bytes());
        secp.extend_from_slice(&digest);
        secp.extend_from_slice(&signature.to_bytes());
        let key = ed25519_dalek::SigningKey::from_bytes(&[index; 32]);
        ed.extend_from_slice(&key.verifying_key().to_bytes());
        ed.extend_from_slice(&digest);
        ed.extend_from_slice(&key.sign(&digest).to_bytes());
    }
    let message = [11u8; 32];
    let mut secp_bad = secp[..258].to_vec();
    secp_bad[129 + 33] ^= 1;
    let mut ed_bad = ed[..256].to_vec();
    ed_bad[128 + 32] ^= 1;
    let mut high_s = secp[..129].to_vec();
    let signature = k256::ecdsa::Signature::from_slice(&high_s[65..])?;
    let (r, s) = signature.split_scalars();
    let noncanonical = k256::ecdsa::Signature::from_scalars(r.to_bytes(), (-s).to_bytes())?;
    high_s[65..].copy_from_slice(&noncanonical.to_bytes());
    for (name, bytes) in [
        ("message", message.as_slice()),
        ("expected", &blake2b_256(message)),
        ("secp", &secp),
        ("ed", &ed),
        ("secp_bad", &secp_bad),
        ("ed_bad", &ed_bad),
        ("high_s", &high_s),
        ("empty_hash", &blake2b_256([])),
        ("large_hash", &blake2b_256(vec![0; 1 << 20])),
    ] {
        std::fs::write(dir.path().join(name), bytes)?;
    }
    let compiler = std::env::var_os("STEM_RISCV_CC").unwrap_or_else(|| "clang".into());
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crypto_v1.S");
    let output = Command::new(compiler)
        .current_dir(dir.path())
        .args([
            "--target=riscv64-unknown-elf",
            "-march=rv64im",
            "-mabi=lp64",
            "-nostdlib",
            "-fuse-ld=lld",
            "-Wl,--no-relax",
            "-Wl,-e,_start",
        ])
        .arg(source)
        .args(["-o", "guest"])
        .output()?;
    ensure!(
        output.status.success(),
        "guest build: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let guest = Bytes::from(std::fs::read(dir.path().join("guest"))?);
    let mut network = Network::new()?;
    let ty = script(&guest, &[]);
    network.add_dep(guest.clone())?;
    let input = network.add_input(network.output(vec![], Bytes::new())?);
    let tx = network.signed(vec![input], vec![network.output(vec![ty], Bytes::new())?])?;
    let tx = network.resolve(&network.id, tx)?;
    const LIMIT: u64 = 2_000_000_000;
    assert!(verify(tx.clone(), LIMIT, false).is_err());
    for parallel in [false, true] {
        let cache = VerificationCache::for_genesis(8, true);
        let cycles = cache.verify(tx.clone(), LIMIT, parallel)?;
        assert_eq!(cache.verify(tx.clone(), cycles, parallel)?, cycles);
        assert!(cache.verify(tx.clone(), cycles - 1, parallel).is_err());
        let cold = VerificationCache::for_genesis(8, true);
        assert!(cold.verify(tx.clone(), cycles - 1, parallel).is_err());
        println!("crypto v1 script parallel={parallel} cycles={cycles}");
    }
    let context = || executor::ContextData {
        height: 1,
        parent_time_ms: 1,
        payer_lock: Default::default(),
        actors: vec![],
        payload: Bytes::new(),
        inputs: vec![],
    };
    assert!(executor::run(&guest, context(), LIMIT).is_err());
    let (outputs, cycles) = executor::run_with_crypto(&guest, context(), LIMIT, true)?;
    assert!(outputs.is_empty());
    assert!(executor::run_with_crypto(&guest, context(), cycles - 1, true).is_err());
    assert_eq!(
        executor::run_with_crypto(&guest, context(), cycles, true)?.1,
        cycles
    );
    println!("crypto v1 executor cycles={cycles}");
    Ok(())
}
