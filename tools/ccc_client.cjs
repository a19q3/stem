async function openClient(ccc, config) {
  const options = { url: config.rpc, fallbacks: [] };
  const base = new ccc.ClientPublicTestnet(options);
  const acp = ccc.KnownScript.AnyoneCanPay;
  const dao = ccc.KnownScript.NervosDao;
  return new ccc.ClientPublicTestnet({
    ...options,
    scripts: {
      [acp]: await base.getKnownScript(acp),
      // CCC recognizes this identity when computing input capacity and fees.
      [dao]: await base.getKnownScript(dao),
      [ccc.KnownScript.Secp256k1Blake160]: {
        codeHash: config.sighashCode,
        hashType: "type",
        cellDeps: [{ cellDep: { outPoint: config.sighashDep, depType: "depGroup" } }],
      },
    },
  });
}

module.exports = { openClient };
