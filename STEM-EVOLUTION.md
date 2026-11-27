# Stem 原生协议研发记录

基线为 `29872505350e931a252ddee9f88d94edf410c83a`，历史数据见 [早期实验归档](HISTORICAL-BENCHMARKS.md)。[Stem 协议](STEM-PROTOCOL.md)描述当前运行路径，[基准测试](BENCHMARKS.md)记录可复现的测量。 各轮原始报告、重跑证据和未采用实验见[实验档案](benchmarks/README.md)。

本轮从 `ee808c925186cc81629a1904093b416de311d8ba` 继续。原始 21 项的逐项验收证据和第 10、20 项的扩大验收范围见[验收索引](research/original-21.md)。

| 编号 | 方向 | 运行状态与剩余工作 |
| --- | --- | --- |
| 01 | Stem 改名 | GitHub 仓库、crate、CLI、服务、开发网及签名域已切换。历史分支和证据保留原名。 |
| 02 | Strand | Cell、交易、Group Message、Witness、Header、Receipt、执行证明、Live Cell 路径、原生控制交易、桥接聚合记账和逐通道状态已接入。Rust 和 C 读取器做了针对性差分测试。账本已用 Strand 描述流式快照封套、已采纳 L1 锚点、已接受质押快照头、成员和奖励记录。归档历史证明及委托本金的成员或缺席证明均可导出 Strand 文件。4 副本归档与独立 CLI 复验、旧快照逐字节对照见[操作说明](OPERATIONS.md#portable-strand-proofs-and-snapshots)。本分支已接入创世网络参数、桥接预算与收款配置、质押配置与创世快照、委员会位置、Keeper 策略与预约，以及提款收据前缀等专用记录。旧数据库经双版本导出得到相同的状态快照和证书。见[专用记录验证](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/evidence/2026-11-27-strand-system-records.json.xz)。 |
| 03 | Indexed Script Table | 四种布局实测。共识采用内联 Strand。索引式布局进一步缩小批量输出，其规范验证和解析开销见基准测试。 |
| 04 | Native Group Message | 已实现。消息进交易主体，通过 syscall 2123 读取。真实 Type Script 验证 Action 并拒绝缺失消息。 |
| 05 | 完整输入签名 | 已实现标准完整交易授权。标准锁和 CLI 共用解析规则，绑定输入、显式依赖、展开成员、Header Deps、原始交易及非签名见证。部分授权、Solver 重组及独立代付见下列第 12、13 项。 |
| 06 | Native Block Context | 已实现候选高度、已提交父时间和父 Header。真实 RISC-V 测试覆盖时间边界与缓存。 |
| 07 | Weighted Jolteon | TCP 帧前缀与载荷合并发送。256 次顺序请求与回复的发送系统调用从 1,024 次降至 512 次，大帧边界及混合版本网络已验证。见[传输实验](BENCHMARKS.md#jolteon-frame-writes)。已接入候选分支执行、载荷同步、Pacemaker、加权 Leader、Ed25519 位图 QC、两链最终性和投票持久化。4 至 128 同机副本确认真实支付。16 副本验证连续两位 Leader 离线和重启追赶。四副本验证双向时钟异常和 220 块以上的归档重放。真实 CKB 质押驱动加入、退出、全员替换和权重修改。观察节点跨两次交接重放 229 块并从真实 CKB 固定的快照启动。原生 CKB 提款、退休与双签罚没已验证。`stem run` 已默认运行 Jolteon。普通节点原子裁剪旧载荷与证书，归档节点提供长距离追赶，CometBFT 冻结为对照。 |
| 08 | StemVM 密码学 | 已接入固定计费的低 S secp256k1 恢复。与原 k256 路径差分验证。已对完整签名和摘要的成功恢复结果加入有界本地缓存，固定计费通过验证。本分支完成创世可选的 syscall 2131，涵盖 CKB BLAKE2b 与两种最多 64 条的确定性签名批量验证。RISC-V 脚本、Lineage Executor、旧创世重开和四副本追赶均已验证。见[计费与验证记录](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/benchmarks/2026-11-27-crypto-v1.json.xz)。 |
| 09 | StemVM 优化 | Strand 借用输出 Script 字节完成规范检查，普通支付解码的中位 CPU 耗时降低 23.1%，每笔分配从 17 次降到 9 次。四副本复跑的吞吐接近或略低于基线。见[解码对照](BENCHMARKS.md#borrowed-transaction-output-validation)。Jolteon 串行准入在副本线程运行，区块并行执行仍使用 VM 池。512 笔真实 AMM 命令的固定链尖对照逐笔收据一致，检查耗时下降 5.34%。四副本整网对照的主机争用已记录。见[准入实验](BENCHMARKS.md#serial-admission-dispatch)。候选分支直接读取已解码的 Live Cell 变更，最新删除记录优先。只共享已提交记录的有界缓存，提交按 OutPoint 失效，快照安装后清空。验证同一 OutPoint 在不同分支的创建时间、两级消费、最终提交与快照恢复。四副本支付与分散 AMM 等量对照基本持平，两轮过载结果方向相反。准备时分段计算 Live Cell 承诺，创建记录在最终化时通过复用缓冲区按原顺序加入原子批次。750 TPS 的 2 KiB 输出对照观察到同步均值下降及 CPU 上升。整网吞吐接近。参见 [延迟 Live Cell 编码](BENCHMARKS.md#deferred-live-cell-record-encoding) 和 [Live Cell 读取对照](BENCHMARKS.md#live-cell-reuse-across-candidate-branches)。执行缓存绑定完整见证、解析依赖和已认证的 L1 引用。未读取父上下文的验证可跨父区块复用，读取过的验证仍绑定原父 Header。真实 RISC-V 测试分别覆盖父区块 syscall、Spawn 与 Exec。Lineage Executor 经受控比较后保留便携 V2，短命令实测比复用汇编后端快。Jolteon 对无 Header Dep 的普通交易和单个 Lineage 命令复用选取时的执行状态与回执，固定夹具构建中位耗时从 10.436 降到 6.592 ms，Header 和回执与串行执行一致。节点新增提案构建、候选复验及同步提交计时。普通支付整网对照吞吐接近，16 副本热点负载的候选复验仍需优化。四副本支付剖析定位到 CKB-VM 译码和汇编核心重置。汇编复用现仅清理已写入的 trace 槽。稀疏槽微基准显著加速，四副本未证实持续吞吐提升。标准译码器按指令宽度和配置的 ISA 扩展派发，RV32、RV64 和三个 VM 版本经差分验证。三份真实 RISC-V ELF 的冷译码缩短约 10%–12%。 单代码依赖直接解析，多依赖索引按需建立。八依赖夹具的缓存命中路径减少解析开销。见[代码依赖对照](BENCHMARKS.md#on-demand-vm-code-dependency-indexing)。 提案、区块和 Keeper 共用从实际父 Header 推导的执行上下文。交易授权、VM 和输出 OutPoint 复用已计算的 ID。固定签名夹具和整网对照见[区块上下文实验](BENCHMARKS.md#shared-block-context-and-resolved-transaction-ids)。Jolteon 帧、提案和已验证交易共用不可变接收缓冲区。帧字节与交易 ID 保持一致，签名和规范编码检查继续执行。普通及 2 KiB 载荷的解析、混合版本恢复和整网对照见[接收缓冲区实验](BENCHMARKS.md#shared-jolteon-receive-buffers)。16 副本热点 AMM 在 45 和 100 TPS 两档完成相同签名输入，候选解码复用使复验均值变化小于 1.4%，保留现有默认路径。见[候选复验对照](research/candidates/README.md)。 |
| 10 | 状态与存储 | 区块提交和快照恢复使用排序键范围一次性更新 Patricia 公共路径。差分测试检查旧根的批量更新、路径回收和逐键证明。Jolteon 普通节点按窗口裁剪执行与共识正文，实际验证从归档恢复及跨裁剪边界重启。本地存储计数器按共识正文、执行、Patricia、Header 和历史拆分同步批次。Patricia 分支只在 RocksDB 内保存有效前缀，读取时还原规范节点。固定缓冲区还原与候选节点借用已接入共识运行路径。固定夹具的还原耗时下降 32.8%，完整编辑下降 1.2%，四副本支付未观察到稳定吞吐收益。见[对照实验](BENCHMARKS.md#fixed-buffer-patricia-reads)。已提交视图与候选视图共享只读 RocksDB 默认选项，减少逐次构造。内存表及预热 SST 的三种读取方法、四副本负载和证明回归见[读取选项实验](BENCHMARKS.md#reusing-rocksdb-point-read-options)。四副本等量确认 20,000 笔支付，Cell 树批次字节减少 17.23%，WAL 减少 11.94%。混合存储布局验证相同 Header、证明及重启。以实际 SST 创建事件核对持续负载写入，并独立测量历史索引。执行记录复用计算回执根时的规范字节，交易及回执 Merkle 树使用定长内部哈希。根、归档记录与包含证明逐字节一致，整网吞吐及分配对照见[执行记录实验](BENCHMARKS.md#canonical-execution-records-and-merkle-allocations)。后续实测按 RocksDB 压缩、SST 命名空间及块设备总写入分别计数，见[持续运行记录](research/storage/sustained/README.md)。 |
| 11 | Native Keeper | 一次性预约、取消、费用授权、按高度自动执行及跳过已接入。四验证者已验证基于父 Header 共识时间的一次性执行、等待期间重启和回执证明。快照恢复与串并行重放验证过期、有界积压解除及取消。已接入原子 Lineage 批次的高度与时间预约，四副本验证唤醒、所有代付占用证明及重启。现支持签名绑定的 2–4 次预付尝试。四副本验证首次失败、重启、按固定区块间隔重试及一次结算，本地验证耗尽次数、付款失效和取消。4 和 16 副本已实测 30 秒预约、重试与普通支付混合负载，另测时间队列饱和。定点对照复现过期预约占用，现由最终性驱动回收。更长持续窗口与更大预约负载待测。 |
| 12 | OTX / Intent | 已实现配对部分授权、唯一交付和 CLI 重组。两种真实 RISC-V UDT 已在四副本网络原子交换。签名绑定精确 OutPoint、单 Cell 交付、共同依赖及按稳定组身份排序的完整 Action 清单。四副本验证 Type 组编号变化后仍执行实际 Action，并拒绝修改载荷。Lineage 动态命令支持预签目标、版本、Executor 和载荷，双边业务签名可在选择 Sponsor 之前并行采集。Solver 已可组合 2–8 条绑定同一计划的独立 Lineage Intent，四副本验证两个市场共四个 Head 的原子结算。批内目标须互异。 |
| 13 | Fee Sponsorship | 完整交易支持独立代付锁组。配对 OTX 支持预签一笔纯 Capacity 代付，绑定指定业务配对和完整 Action 清单，并限制扣款。四副本已验证。Lineage 支持独立的纯 Capacity 扣费授权及固定退款 Lock。Keeper 已复用固定退款 Lock 和单独签名的执行费 Cell。独立业务参与者与代付方已分别授权，覆盖费用上限、精确版本防重放、双资产双边成交和 Keeper 续接。四副本验证同一原子批次内两个独立代付方及按成员退款。批次失败撤销全部费用和 Head 更新。 |
| 14 | 多维资源计费 | 已实现 genesis 费率、计费和缓存一致性测试。固定精度费率保留各资源维度，四副本确认 312 字节支付的 140 shannons 实验档位，拒绝同夹具的 139 shannons。系统操作继续走独立资源预算。生产费率需结合实际资源和安全预算。 |
| 15 | 开放委托质押 | 单笔准入按需加载质押记录，区块准备继续处理按高度触发的质押更新。四委员与百委员实资归档均验证相同准入收据，最新 128 委员本地网络通过 CKB 交接和重启。见[准入实验](BENCHMARKS.md#stake-registry-loading-during-admission)。相邻候选和已提交视图共享未改动的质押输入解码，四个依赖键有写入时换用新缓存，安装快照时清空。128 委员归档中，空块与单笔支付块的准备中位耗时分别下降 38.4% 和 8.0%，Header 与收据摘要相同。见[区块准备实验](BENCHMARKS.md#reusing-decoded-stake-inputs-across-blocks)。实际 CKB 委托、独立增押头寸、权重交接、退役返还和双签罚没已验证。每个头寸用快照应用证明确定生效和退出高度。任内部分减押与全额退出只承担本头寸区间内的历史责任。紧凑 Cell 占用 146 CKB。Registry 承诺逐笔本金、佣金与罚没票据的两个高度边界。小额份额可以分多笔累积，余额达到 CKB 输出下限后一次兑付，已接入 CKB 付款及永久去重。 |
| 16 | DevFund / Ecosystem Fund | 已接入四类手续费分配、提款排队、防重放、L1 实付与认证对账。启用质押后，验证者与委托人的已累积份额计入认证负债，兑付时按原额转入待兑。其余基金使用创世固定的收款 Lock。PoW 基金的检查点实际费用已用专用 CKB Lock 支付并验证。手续费按认证金库准备金确定逐通道收入归属，并与总费额原子承诺。已在 CKB/xUDT 双金库及真实基金兑付复验。基金收款人也可逐笔授权普通 L1 运营交易，并凭已认证交易报销实际矿工费。[四副本与 CKB 报销凭证](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/evidence/2026-11-27-operating-grant.json.xz) |
| 17 | CKB 直接验证 Stem 最终性 | Jolteon 原生检查点使用 CKB Type ID，验证交接和稀疏历史连续性。创世强制每座金库与 Registry 共用检查点，已用真实 L1 替代金库验证拒绝。真实开发网已验证 CKB 与 xUDT 无许可兑付、退休和双签罚没。128 委员普通检查点通过 CKB 默认交易池的双节点 P2P 中继及打包，估算 359,104,558 cycles。真实开发网已验证稀疏发布 CLI 的例行调度、精确高度更新、有序交接、重启续传和 CKB 重组处理。已用两个独立手续费钱包验证 CKB 竞争花费、连续接纳已确认后继、真实见证索引及确认锚重组停机。支持按序提供交易哈希或通过 CKB Indexer 自动发现已确认后继。新日志可跨委员会接续，同区块内按交易位置恢复，保留独立手续费输入。128 委员已实测两个独立 CKB 交易池准入相互竞争的检查点，重连后采纳胜出交易并续发。17 笔高 cycles 检查点全部经 P2P 中继，35 亿 cycles 对照分成 9 和 8 笔打包。真实 CKB Stake Registry 已驱动 128→128 交接，含一人退出、一人加入、一笔增押及离线验证者跨 epoch 恢复。CKB 合约接受边界与前后检查点并拒绝跳过交接。满规模边界与后继检查点已用独立验证前缀的两个 CKB 节点复测，接收端仅通过 P2P 准入后打包，双方链上 cycles 与包含证明一致。长期压力待测。 |
| 18 | Intercell Messaging | 已接入通用双向消息、Lock 授权、永久 nonce 记录与 CLI。Stem → CKB 由原生 Jolteon 最终性和 Patricia 证明认证。CKB → Stem 通过匹配已接受锚点的 guardian 历史引用认证。检查点推进后可通过 MMR 历史路径继续使用较早的 Outbox 证明。中继恢复验证 CKB 历史投递的交易和见证包含证明，稀疏防重放路径可直接生成投递见证。真实 CKB devnet 与四副本验证 4,096 字节双向边界、乱序、多引用授权、伪造、跨网重放、两侧重启及 CKB 重组。Stem 在锚点刷新并重新签发历史引用后仍拒绝已消费消息。真实 CKB 质押驱动的 4、16、128 委员交接均验证旧任期 Outbox 跨期投递，并在两个 CKB 节点间重放已签交易。通用消息编码与各链认证规则独立。额外网络适配属于后续工作。 每笔交易最多四个独立 Lock Group 参与消息原子提交。四条成功、第五条全批拒绝，回执按组顺序承诺消息 ID。 |
| 19 | L1 Cell Reference | 同一 CKB 锚点的最多四条引用共用一份证书。入池、候选块和 VM 可复用已通过 guardian 法定人数核验的解析结果，每次仍检查当前锚点及存活新鲜度，并按相同规则计费。四副本实测三引用 Intercell 接收与两引用 Metatype 返程均低于 2,000 万 cycles。Rust 与 CLI 使用可信 CKB Header、交易及见证 Merkle 路径核验历史输出。StemVM 通过独立 syscall 读取已认证的历史或锚点存活记录。独立锚点更新有单独签名域和区块预算，允许在桥接空闲时更新引用。实时 CKB 存活仍由观察者核验。见[验证对照](BENCHMARKS.md#reusing-verified-ckb-reference-sets)。 |
| 20 | UDT / Spore 结算 | CKB 与 xUDT 双向路径已用实际 devnet 复验。Stem 原生单 Type UDT 已实测托管导出、CKB 映射铸造与转移、销毁和部分及全部返回。同一原生 Polytype Cell 携带两个 UDT 的组合、托管往返、部分返程及本地拆分已实测。CKB Spore V2 已实测普通对象和 Cluster 成员的托管、Stem 转移、原件返回及再次导入。原版合约验证 Cluster 铸造授权和归属变更，独立 CKB/WASM 证明核对原对象及成员关系。见[Cluster 成员实测](BENCHMARKS.md#ckb-spore-v2-with-cluster-membership)。Stem 原生双 Type 对象已实测 CKB 映射发行、独立转移批准、销毁及原件恢复。Cluster 本体和铸造权限留在 CKB。Stem 原生 Spore 已用原版 Spore V2 Type 实测独立身份、转让、返程、重复导出与销毁，并验证永生、审批和大内容。任意可执行 Mutant 和依赖中间历史的约束仍需逐类验收。[生命周期、退款与执行成本](research/original-21.md) 主网发布版的普通 Cluster 成员已覆盖 Stem 首发和 CKB 首发。钱包可复核四字段集合原始数据与铸造时的成员资格。[双向结算与逐笔依赖](research/spore-permit/upstream/collections/README.md) |
| 21 | CKB PoW Entropy | 独立工具从固定锚点与确认后继推导域隔离种子。CKB devnet 重复值一致，拒绝错误锚点和确认不足。未进入领导者或委员会选择。 |

## 关键依赖

- 固定交易编码和解析顺序为 Group Message 与完整签名提供确定性前提。部分授权、代付、预约交易需要各自定义重组和取消条件。
- 投票权由 L1 质押认证产生。原生两链共识和 L1 直接验证都需要完整委员会交接证明以及重启后的投票安全。
- 通用消息、L1 只读引用和资产导出使用独立的来源、目的网络和重放规则。L1 交易包含证明缺少证明 Cell 仍存活所需的状态信息。
- 费用收取先于分配研究。应付收益与现有储备、流通负债和提款预留共同核算。L1 质押本金保持独立。

## 公开测试网准备与研究顺序

[原始 21 项验收索引](research/original-21.md)列出每项能够复核的实现或实测、两项研究取舍，以及尚待验证的扩展范围。

以[原始 21 项记录](https://github.com/a19q3/stem/blob/ee808c9/STEM-EVOLUTION.md)对照 [main 的 fefb8cc](https://github.com/a19q3/stem/blob/fefb8cc01771d59e228c72755ddd1242291f7f22/STEM-EVOLUTION.md)，按核心功能口径，13 项已有核心实现，2 项完成实验取舍，6 项仍有明确交付。这里的状态用于安排公开测试网准备；独立审计、异地主机运行和公开准入各有单独的验收证据。

| 原计划状态 | 编号及边界 |
| --- | --- |
| 核心功能已有实现（13） | 01、04、05、06、07、11、12、13、14、15、17、18、19。性能上限、长周期运行及生产参数继续按各项记录验证。 |
| 完成实验取舍（2） | 03 保留内联 Strand；21 的 CKB PoW 种子已有独立实验，进入 Leader 选择仍需新证据。 |
| 明确剩余交付（6） | 02 专用记录、历史与质押证明、快照封套；08 批量签名验证及通用密码学 syscall；09 候选复验与热点 Lineage；10 长期压缩、物理写入归因与持续饱和；16 其他 L1 运营支出的认证、报销与对账；20 Stem 原生 Spore 标准兼容导出、扩展生命周期与约束映射。 |

本分支按上述范围完成第 02、08 项核心功能。历史与质押证明、快照封套、专用记录和新创世可选的密码学 ABI 已有运行证据。第 16 项走通逐笔授权报销。第 09 项完成候选复验、程序加载和热点 Executor 的专项剖析，并采纳了降低相同确认量 CPU 的优化。本分支为 18 项核心完成、2 项实验取舍，剩余第 10 项跨主机吞吐待验收。第 20 项标记为 **Core Complete / Extended Compatibility Deferred**。普通 V2 结算冻结功能并持续回归，已验证的 Mutant 按矩阵维护，集合级 Mutant 和 Agent 独立授权铸造延期。跨链托管、退款及恢复继续审计，Spore V3 独立研发。

第 20 项已补验 CKB 原生 Spore V2 的 `immortal=true` 与 Cluster 组合。四副本在高度 35 一致，原版合约拒绝销毁（65），原件经过返程、再存入及重新映射。此轮覆盖从 CKB 出生的对象。[原始报告](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/evidence/2026-11-27-immortal-spore.json.xz)

第 16 项增加了单付款 Lock 的普通 CKB 支出凭证。在独立固定的区块哈希下，离线核验交易、见证及每一笔输入的来源。实测一笔 Live Cell 请求付出 0.01 CKB 矿工费，另向请求 Cell 锁入 794 CKB。该凭证用于下述逐笔授权报销。[可离线复核的凭证](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/evidence/2026-11-27-l1-expense.json.xz)

第 09 项补测 RISC-V 程序加载。复用已验证的 ELF 头部解析结果后，四个合约的解析从约 105–106 ns 降至 22–23 ns。16 副本 AMM、两档相同签名集的整网对照未改善共同确认吞吐，200 TPS 档 p99 从 919 ms 增至 1,058 ms。保留默认加载器。[原始报告和候选补丁](research/elf-loading/README.md) 单独替换 Lineage Executor 的 14 轮 16 副本对照也已完成。每次重新分配的汇编内核使 200 TPS 档候选复验从 33.06 增至 43.94 ms；复用内核后为 33.21 ms，对照为 32.83 ms。保留默认解释器。[整网与逐周期对照](research/lineage-engine/README.md) 已解码的 Lineage 命令现按网络复用不可变交易 ID 和签名承诺。16 副本三组配对各确认 8,000 笔，累计 CPU 分别下降 1.77%、0.94%、1.07%；两版窗口内共同确认 TPS 中位数同为 196.125。[带签名交易、归档复验与冷/热开销](research/lineage-hashes/README.md)

第 20 项已验证 Stem 原生普通不可变对象直接导出为 CKB 标准 Spore V2。普通 CKB 钱包用原版合约转让，返程恢复同一 Stem 对象，二次导出释放同一 Spore。新路径接受单一 Type 和不可变内容。Stem 原生 JSON 永生对象也完成了同一路径，Stem 和原版 CKB 合约分别以 117 和 65 拒绝持有人销毁。[永生对象往返](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/evidence/2026-11-27-native-immortal-json-spore.json.xz)已有 Cluster 永生对象及审批复合对象路径分别复跑通过。可销毁的原生 multipart 对象也已通过四副本和原版 CKB 合约往返。第二次导出后，持有人在 CKB 销毁了原件，Stem 托管输出仍可证明。[可销毁对象实测](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/evidence/2026-11-27-native-multipart-spore.json.xz) 可执行扩展与额外约束的等价映射仍有剩余范围。[四副本与 CKB 报告](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/evidence/2026-11-27-native-standard-spore.json.xz)

第 20 项新增了带审批的 Stem 原生对象导出。CKB 使用原版 Spore V2 Type，持有人 Lock 延续原有审批规则。四副本与 CKB 实测原生转让、标准发行、持有人和审批方共同转让、返程和同 ID 再导出。16 个反例覆盖去除审批、替换审批方或托管端点、缺少持有人以及销毁。此路径使用支持该 Lock 的钱包，其他可执行扩展仍按各自语义验收。[原始往返和三组兼容性报告](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/evidence/2026-11-27-approved-standard-spore.json.xz)

第 20 项的标准 Spore 通道已实测大内容。96 KiB 原生对象、16 KiB 带审批对象和 8 KiB 的 CKB 永生 Cluster 成员均以 104 字节消息往返。双方核对完整 Cell 的长度、数据哈希和原有 Type／Lock 约束，小对象继续使用原编码。客户端可从 Molecule 文件直接生成消息载荷。[五组终局报告与载荷](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/evidence/2026-11-27-committed-spore-content.json.xz)

第 20 项也支持持有人在 CKB 销毁可销毁的原生 Spore，同时指定 Stem 抵押容量收款人。认证销毁消息在 Stem 核销托管原件并按原额退款，已实测 multipart 与 96 KiB 对象，以及错误退款和重放拒绝。普通 CKB 钱包仍可独立销毁其对象。[双端核销、退款与兼容性报告](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/evidence/2026-11-27-synchronized-spore-melt.json.xz)

带认证内容承诺的返程已做实测优化。448 KiB 对象保持完整字节比较，返程从 23,104,360 降到 16,105,071 cycles；496 KiB 对象在默认 2,000 万预算内完成全生命周期。500 KiB 样本的 CKB 交易超过本地交易池体积上限。[逐步替换与完整收据](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/evidence/2026-11-27-spore-content-cycles.json.xz)

第 16 项已走通基金收入至 L1 运营支出对账。四副本费用分配后，Ecosystem Fund 从 CKB 金库收到 800 CKB，收款人对一笔 Live Cell 请求的实际矿工费做逐笔授权。CKB 合约重算并报销 0.01 CKB，消费唯一找零实现去重，结算另付 0.01 CKB。重复授权可退回。此机制由既有基金收款方主动选用；PoW 检查点专用 Lock 的支出范围仍由其原脚本约束。服务报酬或通用运营预算属后续政策选择。[四副本与 CKB 凭证](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/evidence/2026-11-27-operating-grant.json.xz)

第 10 项新增四副本 Btrfs 块设备归因。10 分钟内 96,000 笔全部确认，四副本累计写入 10.158 GB；60 秒高压组共同可见 1,124.67 TPS，p99 达 52.5 秒。5 分钟周转负载用 32,768 条支付链确认了全部 277,868 笔已准入交易。另用 4,096 条支付链运行 30 分钟，90,000 笔全部最终确认，收到 89,994 个成功回执，四副本共写 11.589 GB。捕获全部 20 个压缩任务的输出，按 SST 数据块区分命名空间，同时记录共享块、元数据和 15 个遗漏的刷盘文件。[逐副本设备计数、压缩与最终性报告](research/storage/device/README.md)

四副本新增了 3,000 TPS 目标档实测。30 秒独立普通支付计划 90,000 笔，写入并最终确认 72,015 笔，窗口内共同可见 2,106.97 TPS，p99 为 29.49 秒。120 秒支付链工作负载窗口内为 1,741.01 TPS，p99 为 38.90 秒。两轮完整捕获 SST。下一步应在独立负载机上持续供应互不依赖的可花费输入，分别核对客户端队列、准入和最终性积压。[负载、设备与压缩对照](research/storage/device/README.md#offering-3000-plain-payments-per-second)

独立输入已改为计时前由四副本确认的真实分拆交易。三分钟 3,000 TPS 档计划 54 万笔，实际发送并全部确认 327,306 笔，窗口内共同可见 1,629.64 TPS。十分钟 1,600 TPS 档计划 96 万笔，最终确认 726,324 笔，其中 21 笔未收到成功回执，窗口内 1,208.17 TPS。两个档位均完整归档 SST，分别完成 28 与 137 项有输出的压缩任务。[独立输入、逐分钟吞吐与设备写放大](research/storage/sustained/README.md)

第 10 项的一小时老化实验以每秒 100 笔投递 4 KiB 链式支付，四副本最终确认 309,011 笔，p99 为 1.924 秒。客户端跳过前序付款后，1,771 条支付链产生缺口，所有 48,953 次输入失效都来自这些缺口的后继交易。全部成功回执均有最终确认。四副本合计块设备写入 34.809 GB，逐文件核对了全部 414 个 SST 与 79 次压缩。此数据用于解释长期写入和支付链断裂，跨主机饱和仍需独立实测。[六个十分钟窗口、缺口及逐次压缩](research/storage/sustained/README.md#one-hour-with-4-kib-payment-chains)

第 10 项另做了四副本物理核心数对照。相同 126,000 笔支付的四轮实验中，验证者从 6 核扩至 12 核，窗口吞吐中位数从 1,891.63 增至 1,966.91 TPS。三分钟 3,000 TPS 投递压力下，两次实测分别为 1,656.59 和 1,616.45 TPS。每档仅一次，两种分配均未达到目标。[签名集、设备计数和积压](research/storage/cpu-allocation/README.md)

十轮等量选块对照分别确认同一批 126,000 笔支付。默认按已签字节哈希排序有明显尾部偏差。按准入顺序选块使 p99 从 63.62 降至 32.90 秒，窗口吞吐从 1,884.71 降至 1,466.11 TPS。每四块轮换一次居中，p99 为 60.81 秒，吞吐为 1,851.20 TPS。保留默认顺序，三个候选留作独立负载机对照。[四副本逐笔记录与候选补丁](research/admission/order/README.md)

普通 Spore 双向结算以九组开发网归档作为回归基线。标准新托管先核对资产与实际集合的数据，并拒绝该路线未支持的可执行扩展。[Spore 回归、托管预检与扩展矩阵](research/spore-permit/compatibility.md)

| 优先级 | 下一项交付 | 建议验收 |
| --- | --- | --- |
| P0 | **动态开放验证者准入与 guardian 权限说明。** 单机 CKB 开发网发现未预配置的验证者，确认其投票、退役和 400 CKB 赎回。签名端点在可选模式下启用。 | 在负载下复验发现、交接、重启和追赶。已测试两份 guardian 签名不足和补足第三份后的恢复。分别验收跨主机可达性、版本过渡和 quorum 无法恢复的运维边界。[端点发现验收](OPERATIONS.md#discovery-mode) · [权限与停机影响](OPERATIONS.md#validator-admission-and-guardian-authority) |
| P0 | **V1 原生流水线。** 保持 Jolteon 投票绑定已执行的 Header 和状态承诺。单独启用 Follower 在同机长窗口内减少控制等待，平均共同确认从 2,082 降至 1,911 TPS。 | 核对实际写入、四副本共同确认、控制消息等待、超时、CPU、尾延迟与排空。继续比较各阶段组合。[V1 验收顺序](#stem-v1-执行路线) · [长窗口对照](research/storage/external/README.md#follower-only-sustainable-profile-pair) |
| P1 | **后台持久化与联合入池。** 研究认证提交与后继执行重叠，并对照 Admission 与 Candidate Pipeline 的资源争用。 | 核对原子提交、投票日志、崩溃恢复以及单开与双开下的 CPU、排队和确认量。[入池原型](research/admission/pipeline/README.md) |
| P0 | **持续支付吞吐与热点 AMM 分开攻关。** 先定位客户端投递、候选复验、同步落盘各自占用，再逐级挑战 3,000、5,000、10,000 TPS。 | 在独立负载机与多主机副本上固定签名输入、机器规格及测试时长。每档记录实际写入、准入、窗口内全副本可见、排空后确认、丢弃、p99 和积压曲线。热点命令另外核对串行归档重放；存储分别记录 WAL、SST 和设备层写入。[跨主机可持续与过载验收](research/storage/external/qualification.md) · [准入实测](research/admission/README.md) · [存储实测](research/storage/README.md) |
| P1 | **自适应经济策略。** 定义质押门槛、委托比例上限与动态费率的计量输入、调整边界和生效高度。 | 先用历史负载离线重放，在费率和权重边界核对相同状态输入产生相同结果，覆盖费率振荡、委员会更替和恢复。策略参数由单独协议提案决定。 |
| P1 | **安全域独立资本与 L1 运营支出闭环。** 从已有 Live Cell 认证服务和检查点矿工费出发，选择一种额外服务和一种可验证支出。 | 走通服务收入、资本占用、支出证明、去重兑付、双端对账与重启；独立验证运营金库和质押本金的账目。 |

跨物理主机先建立 1,800 TPS 基线，再验收 3,000 TPS 可持续负载。3,300–5,000 TPS 按独立过载档测定饱和点。可持续验收同时核对尾延迟、积压增长和排空。实测候选父分支已花费输入的扫描频率后，再选择执行流水线改造并挑战 10,000 TPS。

实测档案中，四副本短窗口的一轮支付达到 1,757.58 全副本可见 TPS。60 秒的等量对照确认全部 60,000 笔，候选窗口内全副本可见 984.60 TPS，含排空 990.22 TPS，p99 为 947 ms。上述 3,000 至 10,000 TPS 是目标。可先冻结测试网创世与协议版本，邀请独立运营者演练部署、重启、快照恢复和升级，同时公布 guardian 假设与基准口径。[历史支付对照](BENCHMARKS.md#vm-verification-reuse-across-parent-changes)

[14 项独立实验索引](research/README.md)给出了候选实现与原始记录。其中 #5 的独立验证调度、#7 的证书复用适合进入真实区块或异地混合版本对照。#9/#10 的稀疏拓扑应先测独立主机出口、重复字节和补传时间。#11 的局部纠删码与 #13 的同步广播保留各自的研究边界。Strand 和密码学 syscall 的兼容性证据已归入[原始计划验收索引](research/original-21.md)。钱包已实测任意指定的中间 Lock／数据哈希按序出现，完整见证可离线复验；四副本已验证由 CKB 为每次原生对象转移签发凭证，并将返程凭证签发合并到 CKB 入库交易。beta.2 测试网发布的 Spore 和 Lua 程序已完成单 Mutant 资产的双向托管。Stem 首发路径也已实测 CKB 钱包换手、返程、再导出与同步销毁退款。Stem 和 CKB 首发的注册 Spore 均已用真实 CKB 托管换锁交易授权原生钱包直接花费。六组四副本实验覆盖 12 次换手、两个方向的 496 KiB 内容和旧输入凭证后的赎回。496 KiB 成员换手采用 CKB 数据长度和哈希校验后，两种首发方向的整笔 CKB cycles 均下降约 36%。其他 Lua 策略仍待实证。[托管合约成本对照](research/spore-permit/upstream/beta2/cost/README.md)[直接换手与费用](research/spore-permit/upstream/beta2/handoff.md)[双向生命周期和离线复验](research/spore-permit/upstream/beta2/README.md)[通用历史见证](research/type-history/README.md) · [逐笔凭证和实测成本](research/spore-permit/README.md)

第 10 项现已支持在独立压测机上连接预先启动的验证者。单机演练在 180 秒内共同确认 309,720 笔，停机后离线重建了签名交易根和最终性证书。下一次验收需在不同物理主机上使用同一口径。[跨主机压测流程与实测](research/storage/external/README.md)

## Stem V1 执行路线

Stem V1 沿用执行绑定的加权 Jolteon。投票签名继续认证完整执行 Header，排序与状态采用统一的最终性。CKB 通过独立结算适配器消费执行最终性。现有 Cell、Keeper、Lineage、提款和 CKB Checkpoint 证明格式沿用 V1 规则。下一阶段研究 Aptos／Zaptos 的本地任务依赖、有界调度、跨阶段并发和分阶段持久化。[执行绑定实验](research/execution-pipeline/README.md) · [CKB 结算适配器](stake/src/settlement/ckb.rs)

性能比较使用固定提交。正式历史基线为 `fefb8cc`，原生流水线候选为 `12338f2`。每次报告都记录实际使用的完整提交 ID、构建特性和运行配置。已发表的配对实验沿用各自报告中的构建版本。

| 优先级 | 工作 | 验收 |
| --- | --- | --- |
| P0 | Follower Candidate Pipeline | 同机长窗口对照两轮均未通过持续档。Follower 保留可选。联合对照继续核对事务积压、认证提交和超时。[3,000/s 对照](research/storage/external/README.md#follower-only-sustainable-profile-pair) |
| P0 | Leader Proposal Pipeline | 可选原型已移出选块和构块。主循环复核 epoch、view、父链和投票日志。混合版本重启、丢弃超时结果和两次 CKB 交接已验证。四轮同机消融未建立后台构块收益，35.5%–36.7% 的选块访问遇到候选父分支已花费输入。继续测量这些访问的耗时和跨主机频率。[Leader 验收](research/execution-pipeline/README.md#leader-proposal-construction) · [Leader 消融](research/execution-pipeline/README.md#leader-only-payment-comparison) |
| P0 | State Commitment Pipeline | Cell、System 和收据可并行计算。独立可选的 Cell Patricia 子树并行已核对精确根、写删顺序、回执与旧快照。同机两种运行顺序的长测分别改善共同确认吞吐 16.1% 和 19.4%，并行组均排空已接收交易，持续档仍未通过。继续联合消融与尾延迟验收。[子树长测](research/patricia/README.md#reverse-order-subtree-comparison) · [命名空间承诺对照](research/execution-pipeline/README.md#commitment-only-mixed-script-comparison) |
| P0 | 区块资源扩容 | 普通支付对照了 40 亿、80 亿、160 亿 cycles，以及 2,048 和 4,096 笔上限。混合 Type Group 已用同负载复测三档预算。40 亿一轮积压未排空，反向对照接近投递速率。较大预算全数排空，三个试次触及笔数上限。继续定位提案扫描与超时，再做独立主机对照。[区块资源实测](research/execution-pipeline/README.md#block-resource-limits-under-a-payment-burst) · [混合脚本](research/scheduling/README.md#mixed-type-groups-with-the-native-pipeline) |
| P1 | Commit Pipeline | 可选写入线程与后继候选准备已重叠。验证写入前、WAL 同步前及同步后的进程故障、混合版本历史裁剪与 CKB 交接。保留整网消融。[持久化验收](research/execution-pipeline/README.md#certified-commit-pipeline) |
| P1 | Admission 与 Candidate 联合测试 | 八轮同机对照已覆盖均关闭、单开和双开。双开窗口吞吐提高，重验次数减少，控制等待和 p99 升高。长窗口与独立主机验收待做。[联合实验](research/execution-pipeline/README.md#admission-and-follower-interaction) |
| P0 | 原生流水线联合验收 | 单机四副本长窗口六项优化全开，控制消息平均等待由 41.17 ms 降至 5.09 ms，共同确认吞吐由 2,071 降至 1,877 TPS。两轮均低于每秒投递 3,000 笔的持续档门槛。继续单机消融和故障验收，跨主机验收暂缓。[长窗口联合对照](research/storage/external/README.md#integrated-pipeline-sustainable-profile-pair) |

3,000、5,000 和 10,000 TPS 是性能目标。跨主机可持续结果低于 3,000 TPS 时继续剖析原生流水线。达到相应目标还需同时通过尾延迟、积压和排空验收。跨地域实验另行测量执行在共识关键路径上的占用。[可持续与过载各自的验收门槛](research/storage/external/qualification.md)

[英文验收计划与 31 项 Issue 进度](STEM-V1-PLAN.md)

## 本轮新增范围

| 方向 | 当前结果及剩余条件 |
| --- | --- |
| 百委员共识 | 4、16、32、64、100、128 委员的签名载荷及 CKB-VM cycles 已测，Jolteon 实际网络覆盖全部六档。短负载对照见[实测记录](BENCHMARKS.md#native-weighted-jolteon)。已实现 Jolteon 双签证据采集和原生历史任期核验。实际 CKB 开发链确认了冻结、退休和罚没。 |
| 高负载与写入 | 实测 40/80/160 亿 cycles、1000/500/250/100 ms、入口压力、5,000 与 20,000 mempool、应用与共识进程写入。各组合的投递、准入、最终确认及积压见基准测试。四副本新增 45 秒普通支付对照和高投递压力组。已落盘正文按原字节复用，减少 WAL 和进程写入。已分别记录逻辑批次、共享 WAL、SST 输出及独立索引进程。已拆分客户端排队、完整写入及逐节点处理延迟。16 副本的 240 TPS 热点 AMM 档位仍受节点排队限制。四副本普通支付在 20 秒内实际发送 20,000 笔，全部最终确认，投递窗口内全副本可见 940.15 TPS。计划 2,000 TPS 时实际写入 1,392.50 TPS，27,850 笔全部确认，投递窗口内全副本可见 1,280.55 TPS。跨父区块复用验证后，两轮四副本 24 秒高负载实测全副本可见 1,719–1,758 TPS。60 秒等量支付两版均确认 60,000 笔，候选版累计 CPU 减少 15.9%，p99 从 1.286 秒降至 0.947 秒。更长时间的饱和上限与共享 SST 的逐命名空间物理归属仍待验证。见[压测分段记录](BENCHMARKS.md#sustained-submission-and-admission-latency)。 |
| 开放委托质押 | 实际 CKB 委托和增押驱动共识权重。支持任内减押、全额退出、退役退款和罚没。按头寸核验生效与排除高度，重新委托可消费已返还的 CKB。历史本金和委员会证明确定任期收益及注册佣金，按可核验的原 CKB Lock 申领。 |
| 可编程安全域 | 首次实现独立 CKB 资本的 Live Cell 认证域，包含真实 Lock、固定初始权重、付费交付、回执、客观双签罚没、退役和解除质押。CKB 与 Stem 四副本验证了回执使用。链上本金、罚没、返还和服务收入可经 CKB 交易及见证证明独立核验，开发网验证了遗漏记录、见证篡改和重组拒绝。其他服务验证规则、应用安全域配置及复杂资本管理待研究。 |
| 四类收入账户和 PoW Security Fund | 聚合账户的分配、申领、过 CKB 金库兑付和认证结算已验证。65/15/10/10、约 10% 用于 L1 认证与结算，以及每 4 小时常规锚定均为实验场景。专用 CKB Lock 把基金检查点支出限定为实际矿工费。已验证例行发布、委员会交接、合并和持续使用发布器日志。Live Cell 认证域已实测独立 CKB 请求付款和签署人收入。其他运营报销及更多安全服务收入待实现。 |
| 应用与 SDK | Rust 可解析并验证桥接聚合负债、通道金额与 nonce，也可核验 CKB 历史输出。原生 Rust Client 提供投递、状态、执行 Header、两链证明、交接和应用查询。可选的持久提交连接保留逐笔回执，在四副本等量负载中减少 92.25% 的建连，确认吞吐基本持平。钱包工具继续构造与签署交易。待付款 RPC 返回记录和 Patricia 路径，CLI 可验证并生成 L1 Witness。质押 CLI 可重建本金证明、核验收益及构造申领交易。Rust API 与 RPC 提供去重记录证明。安全域的资本与收入已有 Rust API、在线采集和离线验证。已提供可打包的浏览器 WASM 模块和 TypeScript 声明。Chrome 与 Node.js 已用真实四副本和 CKB devnet 证明验证原生交易构造、完整组签名摘要、Live Cell、回执、Intercell、金库收入及两次委员会交接。应用提供 RPC 传输和外部签名器。 |
| Polytype | 当前仓库的 Cell Model 与性能记录统一使用 Polytype。Git 的旧提交及原始测量保持可追溯。 |
| Lineage Cell | 已实现显式注册、稳定 ID、单一 Live Head、单调版本、权限变更、退休和双根证明。执行时解析、RISC-V Executor、预签命令、原子多目标后继及费用回滚已进入运行路径。四个验证者与恢复节点验证动态 Head 证明，固定资产供应的双储备 AMM 完成同机热点实验。已复用至一次性 Keeper 预约。Solver 批量组合已接入同一执行路径并通过四副本验证。批次可由 Keeper 原子唤醒，任一代付方可凭精确预约 ID 撤销。4 和 16 个 Jolteon 副本已实测分散 AMM、单热点、独立及混合 Head、八 Head 原子命令。归档逐块串行复验 Header、回执、Head 版本与 AMM 资产守恒。本机 16 副本在 80 TPS 档位确认全部 2,400 笔，240 TPS 档位出现明显积压。提案迭代按剩余读取预算预筛，固定 4,096 条命令的选取中位耗时从 144.59 降至 5.04 ms，入选交易和 Header 一致。选取预筛的配对整网实验未测得吞吐提升。共识消息与交易转发已使用独立有界队列和 TCP 流，确认转发队列满时控制流仍可送达。16 副本 80 TPS 复验全部确认，240 TPS 对照仍见积压及状态查询超时，没有稳定吞吐收益。批量入池在固定状态上缩短校验时间，16 副本整网高压下 CPU 用量上升，故只保留为实验。默认改为当前和下一位 Leader 的定向转发，本地未确认交易按委员会有界重试。16 副本的 80 TPS 对照中，两版均确认 2,400 笔，收到的转发从 36,000 次降至 4,459 次，Store 入池校验从 38,400 次降至 6,859 次。240 TPS 计划档位两轮的实际投递约 143–146 TPS，定向转发均在排空窗口内确认全部已投递交易。两名初始转发目标同时停机时，一次提交经重试确认。原始计数和仍存在的客户端投递限制见[对照实验](BENCHMARKS.md#jolteon-leader-directed-transaction-relay)。 |
| Metatype | CKB 独立部署已接入规范 Manifest、按完整 Script 派生虚拟组、固定 Adapter 表、逐组 RISC-V Spawn 和 UDT 转移策略。CKB devnet 验证 2、4、8、16 项约束、不同 Manifest 共享组及同一输出承载两种资产。组级见证按完整部署和 Script 寻址。独立对象身份与 Ed25519 审批 Adapter 已在 CKB 验证发行和转移，四副本原生 Polytype 对照了相同内容和决策夹具。不可变托管路径已接入四副本 Stem 与 CKB devnet，完整绑定原始约束、数据、接收者、MailBox 及检查点。复用同一对象夹具比较现有专用映射。已实现按数据哈希解析共享定义，CKB 实测 2、4、8、16 项约束的两种存储布局、定义换点重发、混合组和展开上限，四副本完成共享布局的跨链往返。CKB 虚拟组直接借用共享定义和 Cell 状态。两轮实测 16 项约束的共享拆分 cycles 降低 27.1%，保留展开限额。按 Cell 数量上限的同质资产配置已接入。四副本与真实 CKB 验证独立脚本、共享及内联布局、部分与全额赎回，并在同一部署先后赎回 150 和 100 单位。独立 WASM 验证器核验了执行和状态证明。可修订对象已在真实 CKB 和四副本 Stem 验证两种 Manifest 布局。独立 WASM 核对保留原始 Capacity、两项 Type Scripts 并恢复认证的版本 4。三约束可修订对象已在两种布局下验证独立的内容寻址规则。两链均拒绝同一超限修订、规则缺失、替换和删除。CKB 规则换点后继续结算，Stem 恢复三项约束及认证的状态。CKB Type ID 注册表已支持可更新的规则来源。四副本与 CKB 验证对象在途时将状态上限从 120 调到 200 字节，用新锚点的存活认证恢复全部约束。独立的只追加约束已与身份、审批及存活规则组合，两条链对同一组重写、截断、MIME 变更和只增版本的状态给出相同拒绝结果。内联及共享 Manifest 均完成四副本往返，原生组直接验证跳过多个 CKB 修订的前缀关系。见[四约束对照](BENCHMARKS.md#append-only-metatype-revisions)。请求、审核、完成三步授权链已在两种 Manifest 下完成往返。CKB 逐步验证，Stem 从托管态重放两份中间签名并核对认证销毁状态。逐笔 CKB 交易及 Witness 包含证明、Stem 最终性和返程状态均已独立复验。见[工作流实验](BENCHMARKS.md#metatype-workflow-history)。恢复 CLI 已验证从 CKB 链上见证重建返程 Proof Data。两种布局、仅变更归属的 CKB 交易、提前返程和锚点重组均已实测。见[恢复实验](BENCHMARKS.md#recovering-metatype-workflow-proofs-from-ckb)。两个独立同质资产已在内联与共享部署中逐阶段原子导出、导入、交换、销毁和返程。单份 guardian 证书同时认证双资产销毁输出。见[双资产实测](BENCHMARKS.md#atomic-settlement-of-two-metatype-assets)。同一 Cell 的多资产托管可绑定 2–8 个完整资产 Script 哈希。两种 Manifest 布局已在真实 CKB 与四副本验证两个 UDT 及其两个独立上限脚本的组合、往返和拆分。选择性返程实测可仅减少一项数量、先耗尽一种资产再以连续 Mailbox nonce 返程剩余资产。两条链保留全部原有约束并逐项比较认证差额。见[选择性返程实验](BENCHMARKS.md#selective-metatype-redemptions)。固定 Capacity 版税作为独立约束，已在真实双链验证内联、共享及可修订对象的往返。同一 Lock 修订免收，Stem 导出与释放及 CKB 转移分别核验实际付款。见[版税实测](BENCHMARKS.md#metatype-object-royalties)。已验证 CKB Type ID 注册表轮换独立 Ed25519 审批键。对象流通中换键仍保留原有 Script，实际 CKB 与四副本拒绝退役密钥、已消费依赖及仅历史认证。四约束组合用同锚点证书核对新密钥、新上限和销毁消息。见[审批轮换实测](BENCHMARKS.md#metatype-approval-key-rotation)。同一注册表已支持冻结与解冻。两种布局在真实 CKB 与四副本验证冻结期间拒绝转移、销毁以及待返程消息的释放。经 WASM 独立核验，冻结时消息未消费且托管存活，确认解冻后同一条消息完成释放。见[冻结实测](BENCHMARKS.md#metatype-freeze-and-thaw)。轮换注册表已统一为一至八把钥匙的 Ed25519 阈值审批。CKB Adapter 与 Stem 原生组验证同一规范位图和精确交易承诺。真实双链已完成单签、2-of-3、3-of-5 和 8-of-8 往返，并经独立验证器复验证明。见[多方审批实测](BENCHMARKS.md#metatype-quorum-approval)。已验证上限在导入前、销毁前和返程前下调。合法旧状态可按原字节转移，超限跨链时核验既定托管路径。真实 CKB 与四副本拒绝错误路由证明和普通超限改写，并在返程后完成符合新上限的修订。见[上限下调实测](BENCHMARKS.md#metatype-ceiling-decreases)。钱包 CLI 已从可信 CKB 锚点、销毁交易和实际依赖生成返程 Proof Data。两种布局以 CLI 输出完成实际签名和返程，CKB 重启后恢复相同字节，并在锚点被重组移除时拒绝。见[返程证明恢复](BENCHMARKS.md#recovering-metatype-ceiling-proofs-from-ckb)。依赖任意中间链状态的规则仍需可验证的历史见证。见[独立规范和威胁模型](METATYPE.md)与[实测报告](BENCHMARKS.md#three-constraint-metatype-with-an-external-rule)。 |
| Universal Stake Protocol 与外部链再质押 | **Deferred / Future Research**。本轮共识抵押来源限定为 CKB L1。 |

## 参考来源与取舍

Strand 生成器和 Rust 读取运行时取自 v10 的 `7db6d0c4fa4eec07937c98fbc69ecdc884569e50`。Stem 使用独立 schema，许可证与来源见[复用声明](ledger/strand/UPSTREAM.md)。v10 的认证树、历史累积器、剪枝及锚定快照用于比较。Stem 保持现有独立的 Cell 状态结构。

加权参考实现位于 `codex/jolteon-experiment` 的 `90e0b1c49`，核心文件在 `consensus/karmadag/core/src/weighted_order/`。该实现的持久化、载荷可用性和视图计时器耦合到它自己的排序节点。已有实验的载荷、持久化条件和执行模型与 Stem 不同，适合指导实现，无法作为 Stem 的直接性能对照。其 Auto-Wake 同样依赖自身状态模型，Stem 预约功能需要以消费 Cell 的原子执行重新定义。

`cargo test --locked --test weighted_finality` 运行固定 epoch 实验。它验证超过三分之二的权重、顺序和去重、TC 对每位签名人最高 QC 的绑定、不同网络与 epoch 的签名域，以及父子执行 Header 的相邻视图。参考实现的先落盘再签名、身份绑定及超时重试规则用于设计 Stem 实验日志。Stem 独立实现并测试重启、精确重试以及拒签冲突和低于锁定水位的提案。该证书测试之外，`tools/jolteon_localnet.py` 运行实际多副本 Pacemaker、提案、签票和恢复。[Jolteon 论文](https://arxiv.org/abs/2106.10362)

CKB PoW 种子工具使用已固定高度和区块哈希，检查所读确认区间的连贯性。调用方依赖 RPC 观察，工具没有验证累计工作量。出块者可以尝试筛选候选哈希或扣留区块，预先确定的取样高度也让观察者在确认后提前知悉种子。出块选择、确认延迟、重组概率和有权重领导者抽样还需要独立证明和实验。开发链验证仅覆盖工具的确定性与错误处理。

Lineage 参考了 v10 在 `7db6d0c4` 的共享状态、实际并行执行及预约实现。共识有序解析、受限 Effects、费用与业务回滚分离、持久化预约游标可作为 Stem 的实现依据。Stem 的活动索引直接引用普通四字段 Cell，业务状态保持在原有 UTXO 中。[共享状态](https://github.com/Tondi-Foundation/Tondi/blob/7db6d0c4fa4eec07937c98fbc69ecdc884569e50/docs/SHAREDNOTE.md) [冲突执行](https://github.com/Tondi-Foundation/Tondi/blob/7db6d0c4fa4eec07937c98fbc69ecdc884569e50/docs/SHARED_EXECUTION.md) [Auto-Wake](https://github.com/Tondi-Foundation/Tondi/blob/7db6d0c4fa4eec07937c98fbc69ecdc884569e50/docs/AUTOWAKE.md)

Xuejie 的软件定义账户文章说明了如何在 UTXO 上构建可编程状态。CKB 的讨论区分了撮合权限与共享状态争用。Fuel 用稳定合约 ID 引用可消费的输出，Sui 将共享对象版本分配交给共识。Stem 保留实际 UTXO，并以 `LineageRef` 将稳定 ID 和版本选择器统一到 Strand 命令、RPC 和 SDK。精确引用写入前须匹配当前 Head，历史双根证明只在对应 Header 下有效。[Xuejie 原文](https://xuejie.space/2020_03_20_what_do_we_mean_when_we_say_account_model/) [CKB 状态争用](https://talk.nervos.org/t/permissioning-access-to-state-through-proof-of-work-verified-on-ckb/9952) [Fuel 交易规范](https://docs.fuel.network/docs/specs/protocol/tx-validity/) [Sui 共享对象](https://docs.sui.io/develop/objects/object-ownership/shared)

Axon 的参考资料展示了由 L1 Metadata Cell 提供验证者数据，再验证检查点 BLS 证明的思路。Stem 直接检查点从固定的创世委员承诺开始，逐块验证独立 Ed25519 签名及后继委员承诺。真实上链成本与百委员 VM 探针见[基准测试](BENCHMARKS.md#direct-ckb-finality)。Image Cell 的状态导入和只读访问用于比较接口。对 Stem 而言，未消费状态仍需证明锚点上的 Live Cell 状态。[Axon 质押与检查点](https://github.com/axonweb3/axon/discussions/898) [Axon Image Cell](https://github.com/axonweb3/axon/discussions/896)

第 20 项已在 CKB 首发与 Stem 首发两条路径验证了同一原件引用两份独立 Lua 注册的情况。两轮各有五次 CKB 状态转换，离线复验逐次核对两份注册的 Type 和代码哈希。Stem 首发的原件已在 CKB 销毁并退还原生押金。任意脚本控制的直接原生换手与 Cluster 扩展仍待验收。[双注册实测与原始证明](research/spore-permit/upstream/beta2/README.md#two-registrations-on-one-spore)

第 10 项用同一批 54 万笔签名支付复测了提议间隔。单机四副本、tmpfs 数据库、64 个发送线程、原区块上限下，两次 250 ms 的窗口吞吐为 1,720.67 和 1,698.24 TPS，两次 100 ms 为 2,079.95 和 2,099.64 TPS。后两次从套接字写入到全副本共同可见的 p99 为 1.32 和 1.25 秒。独立验证者主机仍需复测 3,000、5,000 和 10,000 TPS。[签名归档、计时分解与投递实验](research/storage/proposal-cadence/README.md)

将数据库和驱动目录放在跨两块 NVMe 的 Btrfs 卷后，同样的 250 ms 和 100 ms 配置在 180 秒内分别共同确认 303,734 和 330,706 笔支付，相当于 1,687.41 和 1,837.26 TPS。p99 从 175.32 秒降到 2.24 秒。含 45 秒排空期的设备总写入从约 11.49 GB 增至 12.64–12.67 GB。每档各一轮，四个副本仍在同一台机器。[设备计数、签名交易及收据](research/storage/proposal-cadence/nvme/README.md)

第 20 项用上游 beta.2 的 Cluster 程序独立验证了普通集合的三次所有权状态和成员 Spore 的发行、换手、销毁。普通集合成员携带一项或两项 Lua 注册时，CKB 首发和 Stem 首发各有两组四副本结算记录，覆盖集合转手、成员返程、再次导出和销毁退款。至测试网高度 23,194,162，索引器返回的 beta.2 Data1 身份全部 4,611 个历史输出均未填写集合自身的 Mutant 字段。集合级扩展仍需正例。[Cluster 对照、测试网抽样与离线证明](research/spore-permit/upstream/beta2/README.md#cluster-membership-and-collection-level-registration)

第 10 项在同一双 NVMe 卷上补做了投递与区块容量对照。固定 64 个发送线程，轮询投递在三分钟内四副本共同可见 1,844.03 TPS。改为每个节点 16 个线程从共享队列取支付后，原区块上限下达到 1,954.52 TPS，交易池同时拒绝了 24,054 笔。2,048 笔／40 亿 cycles 的宽区块用两个 VM 工作线程时达到 2,221.14 TPS，415,001 笔写入全部确认，p99 为 21.98 秒。相同创世改为四个 VM 工作线程后为 2,208.92 TPS，p99 为 14.89 秒。跨主机持续目标仍待验证。[四轮签名归档、实际节点分配与逐秒积压](research/storage/dispatch/README.md)

第 10 项将同一签名负载提高到每秒计划 4,000 笔。复用连接时，从 64 增到 128 个发送线程令实际写入从每秒 3,057 增至 3,543 笔，三分钟四副本共同确认从 1,987.32 降至 1,882.19 TPS。新连接模式下，单线程在满池回复后暂停 100 ms，拒绝减少 92.1%，窗口吞吐仅增加 0.21%。四组累计回包等待中，98.60%–99.28% 位于节点开始处理请求之前。后续已在相同签名负载下逐笔测量排队与各阶段的重叠。[逐笔归档、设备计数和进程 CPU](research/storage/submission-sessions/README.md)

第 20 项已验证一份由公开测试网 Spore 实际引用的 Lua 程序。17 个注册 ID 有 33 个历史输出，限定的发行窗口内有 11 个 Spore 引用它。钱包在同一 CKB 锚点验证其中一次发行及注册所有权转移，两条本地四副本路线使用相同程序原文和 1,000 shannons 最低付款，分别完成 7 次与 6 次 CKB 状态转换。把该 ID 放入集合自身扩展字段的独立候选仍被发布版 Cluster 程序拒绝。[公开链证明、付款对账及双向结算](research/spore-permit/upstream/beta2/registered-policy/README.md)

第 20 项用独立收款方复验了同一程序的铸造付款。单注册向受益人净付 1,000 shannons，双注册净付 2,000，资产由另一把锁持有。随后四组用发行或中继已有的输入支付，铸造主体均减少 153 字节，净收款与实付矿工费逐笔对平。两轮累计重放 52 个 CKB 资产状态、16 次托管换手授权和 44 份原生执行记录。[独立收款与逐笔对账](research/spore-permit/upstream/beta2/registered-policy/README.md#paying-a-separate-beneficiary)

第 10 项新测七轮同机四副本。每秒实际发送 1,800 笔的四轮各确认全部 324,000 笔。双线程下，53.30% 的累计提交排队时间与候选验证重叠。高压细分实验中，候选验证与排队重叠的时间有 50.98% 花在交易准备和执行，48.99% 花在生成状态根、收据和写入批次。收据并行候选在固定 2,048 笔区块的两组交叉对照中，把整个候选准备时间各缩短 5.14% 和 4.51%。三分钟整网确认分别为对照 1,851.14、候选 1,906.83 TPS，实际写入量也有差异。保留候选补丁，默认实现继续用串行编码。[逐笔等待、固定区块与整网复验](research/storage/queue-attribution/README.md)

第 20 项已在四条开发链上验证 SDK 永生与换锁程序组合。两项注册分别属于两个钱包时，两个首发方向均精确支付每个钱包 1,000 shannons。钱包验证了 26 个 CKB 成员状态及其集合历史，WASM 复验了 22 份原生执行。另两组使用输出位置策略的成员成功发行。托管后继位于输出 1 和输出 0 的两笔交易在 CKB 预执行时均收到代码 89。[程序来源、独立付款和托管边界](research/spore-permit/upstream/beta2/sdk-policies/README.md)

第 10 项补做了两种负载、每种四轮的同机交叉复测。高压四轮各在 120 秒内实际写入 360,000 笔。并行收据编码相对串行的两组窗口吞吐差分别为 +61.61 和 +4.15 TPS，p99 一组下降 0.77 秒、另一组上升 2.61 秒。较低负载四轮各确认全部 108,000 笔。默认继续使用串行编码。[八轮签名支付、四副本证书、进程 CPU 和同笔延迟](research/storage/receipt-crossover/README.md)

第 20 项在两个首发方向的 13 次成员转换中逐笔认证了实际引用的集合 OutPoint 和持有人 Lock。公开测试网高度 12,607,097 的铸造也认证了四个区块前换手的注册 Cell。[两条结算链及公开证明](research/spore-permit/upstream/beta2/dependency-uses/README.md)

第 10 项已在候选区块的 Cell Patricia 树上采用批量暂存写入。相同 2,048 笔签名支付的区块准备中位耗时缩短 1.84%–3.34%。同机四副本两组交叉对照的窗口吞吐分别增加 146.98 和 20.06 TPS，支付写入至共同确认的 p99 分别缩短 2.95 和 3.86 秒。新旧节点互开原数据库后，继续确认了花费旧输出的新交易。[逐笔支付、内存代价和混合版本证明](research/storage/patricia-finishing/README.md)

第 20 项已用主网发布版 Spore V2 验证普通 Cluster 成员从 Stem 首发，经集合持有人授权在 CKB 铸造、钱包换手、返回 Stem 和再次导出。六组四副本开发网还覆盖永生成员、两次 CKB 授权的原生换手、同步销毁退款及 CKB 首发方向的集合换主。三组实验复用了公开测试网四字段集合的原始字节。集合自身挂载 Mutant 仍缺少可确认的发行样本。[公开发行证明与六组结算](research/spore-permit/upstream/collections/README.md)

第 20 项新增 CKB 历史输入证明。四笔公开测试网成员铸造分别验证了实际花费的 Cluster Agent、引用的集合版本和另一个与集合持有人同锁的输入。两个方向的普通集合成员首发也用已有结算证明复验了实际花费的持有人输入。[16 份公开历史证明与两个本地铸造](research/spore-permit/upstream/collections/agent/README.md)

第 10 项用相同签名支付比较了三种提案顺序。在每秒投递 3,000 笔的两组同机对照中，按接收顺序选取分别少确认 319.43 和 248.18 TPS，按父 Header 哈希轮换起点分别少确认 230.29 和 152.69 TPS。两种候选在排空截止时都有更多已接收支付仍待共同确认。继续使用原有哈希顺序，独立节点的持续吞吐仍待实测。[逐笔时钟、四副本证明与固定区块对照](research/storage/admission-order/README.md)
