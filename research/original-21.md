# 原始 21 项验收索引

范围取自 [ee808c9 的逐项计划](https://github.com/a19q3/stem/blob/ee808c9/STEM-EVOLUTION.md)。表中的链接指向实现、实测记录或已经作出的协议取舍。第 10 和 20 项同时列出后续扩大验收范围后仍需的证据。性能数字均保留各自的副本数、工作负载及计时口径。

| 项 | 可核验的交付 | 实物或重现入口 |
| --- | --- | --- |
| 01 Stem 改名 | 仓库、二进制、crate、配置及签名域均有 Stem 标识。历史 Fern 归档保留可追溯性。 | [CLI 和 crate](../Cargo.toml) · [交易域](../ledger/src/lib.rs) |
| 02 Strand | 原生交易与认证记录有统一的规范读取入口。历史、质押证明及快照封套已做旧字节兼容复核。 | [证明与快照](../evidence/2026-11-27-strand-proofs.json.xz) · [专用系统记录](../evidence/2026-11-27-strand-system-records.json.xz) |
| 03 Indexed Script Table | 五种交易样本各重复四种编码。普通双锁支付为内联 312、索引 318 字节，十个重复锁资产输出为 1,762、652 字节。协议采用内联 Strand。 | [本轮重跑](../evidence/2026-11-27-script-table-repeat.json.xz) · [成本对比](../BENCHMARKS.md#encoding-comparison) |
| 04 Native Group Message | RISC-V Type Script 读取按派生组归属的已签 Action，缺失或错误的 Action 被拒绝。 | [VM 集成测试](../vm/tests/native.rs) |
| 05 完整输入签名 | 实际锁验证已解析输入、展开的依赖组和完整授权。多个 Lock 组分别收集签章。部分授权由第 12 项验收。 | [签名与改动输入的回归](../vm/tests/native.rs) |
| 06 Native Block Context | 高度及已提交父区块时间可由脚本读取，读上下文的执行缓存绑定原父区块。 | [真实区块记录](../benchmarks/2026-11-26-stem-block-context.json.xz) |
| 07 Weighted Jolteon | 原生 TCP 节点执行加权投票、两链最终性、持久化重启及跨 epoch 交接。128 委员通过 CKB 交接。 | [共识运行](../BENCHMARKS.md#native-weighted-jolteon) · [128 委员实测](../BENCHMARKS.md#ckb-funded-128-member-jolteon-handoff) · [14 项研究决策](README.md) |
| 08 StemVM 密码学 | 创世可选的 syscall 2131 提供确定性的批量验签和密码学操作，有真实 guest 计费与拒绝记录。 | [计费和差分结果](../benchmarks/2026-11-27-crypto-v1.json.xz) |
| 09 StemVM 优化 | 候选复验和热点 Lineage 经过逐收据及整网比较。已采用不可变 ID 与签名承诺复用，三组配对 CPU 均下降。 | [候选复验](candidates/README.md) · [16 副本热点对照](lineage-hashes/README.md) |
| 10 状态与存储 | Patricia 已批量更新共享路径。一小时 4 KiB 支付实验核对了四副本 34.809 GB 设备写入、414 个 SST 和 79 次压缩；另以独立输入实测饱和与排空。同机四副本在 tmpfs 上两次 100 ms 提议间隔的三分钟支付均超过 2,070 TPS，双 NVMe 卷上轮询投递为 1,837.26 和 1,844.03 TPS，共享队列配宽区块及两个 VM 工作线程为 2,221.14 TPS，跨主机持续吞吐仍待验证。 | [批量算法](../BENCHMARKS.md#patricia-batch-update) · [设备与 SST](storage/device/README.md) · [独立输入持续实验](storage/sustained/README.md) · [6 核与 12 核持续对照](storage/cpu-allocation/README.md) · [独立驱动及离线复验](storage/external/README.md) · [提议间隔、容量与投递对照](storage/proposal-cadence/README.md) · [同机双 NVMe 对照](storage/proposal-cadence/nvme/README.md) · [发送线程、宽区块与积压](storage/dispatch/README.md) · [每秒计划 4,000 笔的发送与节点排队对照](storage/submission-sessions/README.md) |
| 11 Native Keeper | 高度与父时间触发、预付、取消、执行和有界重试有四副本回执、重启与负载记录。 | [定时](../BENCHMARKS.md#consensus-time-keeper) · [有界重试](../BENCHMARKS.md#prepaid-bounded-keeper-retries) |
| 12 OTX / Intent | 可独立签署的配对输入、唯一交付和固定 Action 清单支持 Solver 重组。真实合约验证原子交付。 | [四副本 OTX](../BENCHMARKS.md#native-otx-action-authorization) |
| 13 Fee Sponsorship | 独立支付组及配对 OTX 代付绑定业务、Action 与净扣款上限。 | [代付 Lineage](../benchmarks/2026-11-25-stem-sponsored-lineage.json.xz) · [OTX 代付](../BENCHMARKS.md#native-otx-action-authorization) |
| 14 多维资源计费 | 创世固定费率及整数计价在入池、执行和缓存路径一致。四副本验证了 140 shannons 支付及低一单位的拒绝。 | [资源费价实测](../benchmarks/2026-11-25-stem-resource-pricing.json.xz) |
| 15 开放委托质押 | CKB 委托份额、权重交接、削减、收益及责任分摊已验证。公开验证者的网络发现与注册运营属于后续工作。 | [委托交接](../BENCHMARKS.md#ckb-delegation-and-weighted-handoffs) · [赎回](../BENCHMARKS.md#individual-stake-reward-redemption) |
| 16 DevFund / Ecosystem Fund | 收入分账、储备和兑付可认证。基金收款人逐笔授权普通 L1 支出后，CKB 核验并报销实际矿工费。 | [基金兑付](../BENCHMARKS.md#ckb-fee-fund-settlement) · [0.01 CKB 实付报销](../evidence/2026-11-27-operating-grant.json.xz) |
| 17 CKB 直接验证最终性 | CKB 合约核验 Stem 委员会及其最终性证书，包含 128 人检查点和交接。 | [CKB-VM 验证](../BENCHMARKS.md#jolteon-verifier-in-ckb-vm) · [CKB 检查点](../BENCHMARKS.md#128-member-checkpoint-through-ckb-p2p) |
| 18 Intercell Messaging | 双向消息、独立网络与端点、消费防重放、无许可转发和 CKB 重组恢复均有实测。 | [双向消息](../BENCHMARKS.md#bidirectional-intercell-messaging) · [恢复](../BENCHMARKS.md#intercell-mailbox-recovery-after-ckb-reorganization) |
| 19 L1 Cell Reference | Rust 与 VM 区分历史输出包含和当前锚点存活。存活依赖已配置的 guardian 认证。 | [VM 引用](../BENCHMARKS.md#ckb-cell-references-in-stemvm) · [准入和 guardian 权限](../OPERATIONS.md#validator-admission-and-guardian-authority) |
| 20 UDT / Spore 结算 | 原生 UDT 有独立标识与数量往返。原生 Spore 使用标准 CKB Type，已覆盖发行、钱包转让、返程、再次导出、永生、核销退款和审批。钱包现可离线认证任意指定的中间 Lock／数据哈希序列。CKB 签发的逐笔凭证已覆盖两次原生转移与 496 KiB 返程。上游 beta.2 的单、双 Mutant Spore 均已双向托管，覆盖原生首发、CKB 钱包换手、二次导出及同步销毁退款。普通 Cluster 成员携带一项或两项 Lua 注册的两个首发方向均已实测，钱包在同一 CKB 锚点核对成员与集合历史。两个首发方向均已用真实 CKB 原件授权 A→B→A，覆盖单注册、双注册、496 KiB 内容和旧凭证恢复。CKB 首发的集合换手后，成员已赎回并二次存入。两侧首发 496 KiB 成员的托管换手 CKB cycles 均降低约 36%。已认证测试网实际引用的带 1,000 shannons 最低铸造付款的 Lua 程序，两个首发方向的四副本均完成钱包换手与返程。四组独立收款方实验覆盖一笔或两笔注册费，钱包复核净收款恰为 1,000 或 2,000 shannons。四组后续铸造复用已有付款输入，主体各减少 153 字节。其他 Lua 策略和集合自身挂载 Mutant 仍待验收。[公开发行和本地双向结算](spore-permit/upstream/beta2/registered-policy/README.md)[三种内容校验对照](spore-permit/upstream/beta2/cost/README.md)[直接换手证明](spore-permit/upstream/beta2/handoff.md) | [UDT](../BENCHMARKS.md#stem-native-udt-settlement) · [审批](../evidence/2026-11-27-approved-standard-spore.json.xz) · [大对象](../evidence/2026-11-27-committed-spore-content.json.xz) · [496 KiB 全生命周期与返程计费](../evidence/2026-11-27-spore-content-cycles.json.xz) · [双端销毁](../evidence/2026-11-27-synchronized-spore-melt.json.xz) · [通用历史见证](type-history/README.md) · [CKB 逐笔审批与返程对照](spore-permit/README.md) · [上游 Mutant 注册实证](spore-permit/upstream/README.md) · [真实扩展资产双向托管](spore-permit/upstream/beta2/README.md) |
| 21 CKB PoW Entropy | 独立 CKB 开发链重跑固定锚点、4 个确认、同种子重现、epoch 域隔离，以及错误锚点与不足确认的拒绝。共识继续使用既有领导者选择。 | [区块 Header 和派生结果](../evidence/2026-11-27-pow-entropy-repeat.json.xz) · [派生工具](../tools/pow_entropy.py) |

资产往返使用本地资金与真实 CKB 程序。工作站四副本的共同可见吞吐、16 副本 AMM 的 CPU 对照、128 委员交接属于不同实验，不能合并为一组部署能力。涉及公开测试网准入、经济参数和新安全域的项目见[演化文档](../STEM-EVOLUTION.md#公开测试网准备与研究顺序)。
