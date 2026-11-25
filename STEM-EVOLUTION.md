# Stem 原生协议研发记录

基线为 `29872505350e931a252ddee9f88d94edf410c83a`，历史数据见 [Fern 实验归档](FERN-BENCHMARKS.md)。[Stem 协议](STEM-PROTOCOL.md)描述当前运行路径，[基准测试](BENCHMARKS.md)记录可复现的测量。 各轮原始报告、重跑证据和未采用实验见[实验档案](benchmarks/README.md)。

本轮从 `ee808c925186cc81629a1904093b416de311d8ba` 继续。以下以实际运行路径和已执行实验为准。

| 编号 | 方向 | 运行状态与剩余工作 |
| --- | --- | --- |
| 01 | Stem 改名 | GitHub 仓库、crate、CLI、服务、开发网及签名域已切换。历史分支和证据保留原名。 |
| 02 | Strand | Cell、交易、Group Message、Witness、Header、Receipt、执行证明、Live Cell 路径、原生控制交易、桥接聚合记账和逐通道状态已接入。Rust 和 C 读取器做了针对性差分测试。其余专用系统记录、历史与质押证明、快照封套仍待评估和统一。 |
| 03 | Indexed Script Table | 四种布局实测。共识采用内联 Strand。索引式布局进一步缩小批量输出，其规范验证和解析开销见基准测试。 |
| 04 | Native Group Message | 已实现。消息进交易主体，通过 syscall 2123 读取。真实 Type Script 验证 Action 并拒绝缺失消息。 |
| 05 | 完整输入签名 | 已实现标准完整交易授权。标准锁和 CLI 共用解析规则，绑定输入、显式依赖、展开成员、Header Deps、原始交易及非签名见证。部分授权语义仍待独立实现。 |
| 06 | Native Block Context | 已实现候选高度、已提交父时间和父 Header。真实 RISC-V 测试覆盖时间边界与缓存。 |
| 07 | Weighted Jolteon | 已接入候选分支执行、载荷同步、Pacemaker、加权 Leader、Ed25519 位图 QC、两链最终性和投票持久化。4 至 128 同机副本确认真实支付。16 副本验证连续两位 Leader 离线和重启追赶。四副本验证双向时钟异常和 220 块以上的归档重放。真实 CKB 质押驱动加入、退出、全员替换和权重修改。观察节点跨两次交接重放 229 块并从真实 CKB 固定的快照启动。原生 CKB 提款、退休与双签罚没已验证。`stem run` 已默认运行 Jolteon。普通节点原子裁剪旧载荷与证书，归档节点提供长距离追赶，CometBFT 冻结为对照。 |
| 08 | StemVM 密码学 | 已接入固定计费的低 S secp256k1 恢复。与原 k256 路径差分验证。已对完整签名和摘要的成功恢复结果加入有界本地缓存，固定计费通过验证。批量验证和通用加密 syscall 尚未启用。 |
| 09 | StemVM 优化 | 加入有界 Live Cell 缓存，提交和状态恢复时清空。执行缓存绑定父 Header 和解析依赖。Lineage Executor 经受控比较后保留便携 V2，短命令实测比复用汇编后端快。代码与译码专项缓存待剖析。 |
| 10 | 状态与存储 | 区块提交和快照恢复使用排序键范围一次性更新 Patricia 公共路径。差分测试检查旧根的批量更新、路径回收和逐键证明。Jolteon 普通节点按窗口裁剪执行与共识正文，实际验证从归档恢复及跨裁剪边界重启。长期运行的压缩和索引写入归因仍待测量。 |
| 11 | Native Keeper | 一次性预约、取消、费用授权、按高度自动执行及跳过已接入。四验证者已验证基于父 Header 共识时间的一次性执行、等待期间重启和回执证明。快照恢复与串并行重放验证过期、有界积压解除及取消。已接入原子 Lineage 批次的高度与时间预约，四副本验证唤醒、所有代付占用证明及重启。现支持签名绑定的 2–4 次预付尝试。四副本验证首次失败、重启、按固定区块间隔重试及一次结算，本地验证耗尽次数、付款失效和取消。4 和 16 副本已实测 30 秒预约、重试与普通支付混合负载，另测时间队列饱和。定点对照复现过期预约占用，现由最终性驱动回收。更长持续窗口与更大预约负载待测。 |
| 12 | OTX / Intent | 已实现配对部分授权、唯一交付和 CLI 重组。两种真实 RISC-V UDT 已在四副本网络原子交换。签名绑定精确 OutPoint、单 Cell 交付、共同依赖及按稳定组身份排序的完整 Action 清单。四副本验证 Type 组编号变化后仍执行实际 Action，并拒绝修改载荷。Lineage 动态命令支持预签目标、版本、Executor 和载荷，双边业务签名可在选择 Sponsor 之前并行采集。Solver 已可组合 2–8 条绑定同一计划的独立 Lineage Intent，四副本验证两个市场共四个 Head 的原子结算。批内目标须互异。 |
| 13 | Fee Sponsorship | 完整交易支持独立代付锁组。配对 OTX 支持预签一笔纯 Capacity 代付，绑定指定业务配对和完整 Action 清单，并限制扣款。四副本已验证。Lineage 支持独立的纯 Capacity 扣费授权及固定退款 Lock。Keeper 已复用固定退款 Lock 和单独签名的执行费 Cell。独立业务参与者与代付方已分别授权，覆盖费用上限、精确版本防重放、双资产双边成交和 Keeper 续接。四副本验证同一原子批次内两个独立代付方及按成员退款。批次失败撤销全部费用和 Head 更新。 |
| 14 | 多维资源计费 | 已实现 genesis 费率、计费和缓存一致性测试。固定精度费率保留各资源维度，四副本确认 312 字节支付的 140 shannons 实验档位，拒绝同夹具的 139 shannons。系统操作继续走独立资源预算。生产费率需结合实际资源和安全预算。 |
| 15 | 开放委托质押 | 实际 CKB 委托、独立增押头寸、权重交接、退役返还和双签罚没已验证。每个头寸用快照应用证明确定生效和退出高度。任内部分减押与全额退出只承担本头寸区间内的历史责任。紧凑 Cell 占用 146 CKB。Registry 承诺逐笔本金、佣金与罚没票据的两个高度边界。小额份额可以分多笔累积，余额达到 CKB 输出下限后一次兑付，已接入 CKB 付款及永久去重。 |
| 16 | DevFund / Ecosystem Fund | 已接入四类手续费分配、提款排队、防重放、L1 实付与认证对账。启用质押后，验证者与委托人的已累积份额计入认证负债，兑付时按原额转入待兑。其余基金使用创世固定的收款 Lock。PoW 基金的检查点实际费用已用专用 CKB Lock 支付并验证。逐金库收入归属与其他运营支出待实现。 |
| 17 | CKB 直接验证 Stem 最终性 | Jolteon 原生检查点使用 CKB Type ID，验证交接和稀疏历史连续性。创世强制每座金库与 Registry 共用检查点，已用真实 L1 替代金库验证拒绝。真实开发网已验证 CKB 与 xUDT 无许可兑付、退休和双签罚没。128 委员普通检查点通过 CKB 默认交易池的双节点 P2P 中继及打包，估算 359,104,558 cycles。真实开发网已验证稀疏发布 CLI 的例行调度、精确高度更新、有序交接、重启续传和 CKB 重组处理。已用两个独立手续费钱包验证 CKB 竞争花费、连续接纳已确认后继、真实见证索引及确认锚重组停机。支持按序提供交易哈希或通过 CKB Indexer 自动发现已确认后继。新日志可跨委员会接续，同区块内按交易位置恢复，保留独立手续费输入。128 委员已实测两个独立 CKB 交易池准入相互竞争的检查点，重连后采纳胜出交易并续发。17 笔高 cycles 检查点全部经 P2P 中继，35 亿 cycles 对照分成 9 和 8 笔打包。真实 CKB Stake Registry 已驱动 128→128 交接，含一人退出、一人加入、一笔增押及离线验证者跨 epoch 恢复。CKB 合约接受边界与前后检查点并拒绝跳过交接。满规模边界与后继检查点已用独立验证前缀的两个 CKB 节点复测，接收端仅通过 P2P 准入后打包，双方链上 cycles 与包含证明一致。长期压力待测。 |
| 18 | Intercell Messaging | 已接入通用双向消息、Lock 授权、永久 nonce 记录与 CLI。Stem → CKB 由原生 Jolteon 最终性和 Patricia 证明认证。CKB → Stem 通过匹配已接受锚点的 guardian 历史引用认证。真实 CKB devnet 与四副本验证重放、伪造、重组和重启，并用稀疏发布器认证真实 Outbox 证明。CKB 检查点推进后可通过其 MMR 历史路径继续使用较早的 Outbox 证明。直接发布和稀疏发布两轮均验证了投递及篡改拒绝。CKB 中继可从 Live Head 重建消费索引，实测已投递消息因 CKB 重组被撤销后，按恢复的根重新投递。额外网络适配不在本轮范围。 |
| 19 | L1 Cell Reference | Rust 与 CLI 使用可信 CKB Header、交易及见证 Merkle 路径核验历史输出。原生交易可附 guardian 法定人数认证的历史或锚点存活记录，StemVM 通过独立 syscall 读取。引用有确认深度、共识锚点、新鲜度和资源计费约束。独立锚点更新有单独签名域和区块预算，允许在桥接空闲时更新引用。实时 CKB 存活仍由观察者核验。 |
| 20 | UDT / Spore 结算 | CKB 与 xUDT 双向路径已用实际 devnet 复验。Stem 原生单 Type UDT 已实测托管导出、CKB 映射铸造与转移、销毁和部分及全部返回。CKB Spore V2 基础对象已实测托管、Stem 转移、原件返回和再次导入。Stem 原生双 Type 对象已实测 CKB 映射发行、独立转移批准、销毁及原件恢复。任意额外 Type 规则、Cluster 和扩展生命周期仍待提供映射和验收。 |
| 21 | CKB PoW Entropy | 独立工具从固定锚点与确认后继推导域隔离种子。CKB devnet 重复值一致，拒绝错误锚点和确认不足。未进入领导者或委员会选择。 |

## 关键依赖

- 固定交易编码和解析顺序为 Group Message 与完整签名提供确定性前提。部分授权、代付、预约交易需要各自定义重组和取消条件。
- 投票权由 L1 质押认证产生。原生两链共识和 L1 直接验证都需要完整委员会交接证明以及重启后的投票安全。
- 通用消息、L1 只读引用和资产导出使用独立的来源、目的网络和重放规则。L1 交易包含证明缺少证明 Cell 仍存活所需的状态信息。
- 费用收取先于分配研究。应付收益与现有储备、流通负债和提款预留共同核算。L1 质押本金保持独立。

## 本轮新增范围

| 方向 | 当前结果及剩余条件 |
| --- | --- |
| 百委员共识 | 4、16、32、64、100、128 委员的签名载荷及 CKB-VM cycles 已测，Jolteon 实际网络覆盖全部六档。短负载对照见[实测记录](BENCHMARKS.md#native-weighted-jolteon)。已实现 Jolteon 双签证据采集和原生历史任期核验。实际 CKB 开发链确认了冻结、退休和罚没。 |
| 高负载与写入 | 实测 40/80/160 亿 cycles、1000/500/250/100 ms、入口压力、5,000 与 20,000 mempool、应用与共识进程写入。各组合的投递、准入、最终确认及积压见基准测试。单机可持续上限和单个存储组件物理写入仍待隔离测量。 |
| 开放委托质押 | 实际 CKB 委托和增押驱动共识权重。支持任内减押、全额退出、退役退款和罚没。按头寸核验生效与排除高度，重新委托可消费已返还的 CKB。历史本金和委员会证明确定任期收益及注册佣金，按可核验的原 CKB Lock 申领。 |
| 可编程安全域 | 首次实现独立 CKB 资本的 Live Cell 认证域，包含真实 Lock、固定初始权重、付费交付、回执、客观双签罚没、退役和解除质押。CKB 与 Stem 四副本验证了回执使用。其他服务验证规则、应用安全域配置及复杂资本管理待研究。 |
| 四类收入账户和 PoW Security Fund | 聚合账户的分配、申领、过 CKB 金库兑付和认证结算已验证。65/15/10/10、约 10% 用于 L1 认证与结算，以及每 4 小时常规锚定均为实验场景。专用 CKB Lock 把基金检查点支出限定为实际矿工费。已验证例行发布、委员会交接、合并和持续使用发布器日志。Live Cell 认证域已实测独立 CKB 请求付款和签署人收入。其他运营报销及更多安全服务收入待实现。 |
| 应用与 SDK | Rust 可解析并验证桥接聚合负债、通道金额与 nonce，也可核验 CKB 历史输出。原生 Rust Client 提供投递、状态、执行 Header、两链证明、交接和应用查询。钱包工具继续构造与签署交易。待付款 RPC 返回记录和 Patricia 路径，CLI 可验证并生成 L1 Witness。质押 CLI 可重建本金证明、核验收益及构造申领交易。Rust API 与 RPC 提供去重记录证明。WASM 或 TypeScript SDK 及安全服务查询待实现。 |
| Polytype | 当前仓库的 Cell Model 与性能记录统一使用 Polytype。Git 的旧提交及原始测量保持可追溯。 |
| Lineage Cell | 已实现显式注册、稳定 ID、单一 Live Head、单调版本、权限变更、退休和双根证明。执行时解析、RISC-V Executor、预签命令、原子多目标后继及费用回滚已进入运行路径。四个验证者与恢复节点验证动态 Head 证明，固定资产供应的双储备 AMM 完成同机热点实验。已复用至一次性 Keeper 预约。Solver 批量组合已接入同一执行路径并通过四副本验证。批次可由 Keeper 原子唤醒，任一代付方可凭精确预约 ID 撤销。多副本持续压力测试待完成。 |
| Universal Stake Protocol 与外部链再质押 | **Deferred / Future Research**。本轮共识抵押来源限定为 CKB L1。 |

## 参考来源与取舍

Strand 生成器和 Rust 读取运行时取自 v10 的 `7db6d0c4fa4eec07937c98fbc69ecdc884569e50`。Stem 使用独立 schema，许可证与来源见[复用声明](ledger/strand/UPSTREAM.md)。v10 的认证树、历史累积器、剪枝及锚定快照用于比较。Stem 保持现有独立的 Cell 状态结构。

加权参考实现位于 `codex/jolteon-experiment` 的 `90e0b1c49`，核心文件在 `consensus/karmadag/core/src/weighted_order/`。该实现的持久化、载荷可用性和视图计时器耦合到它自己的排序节点。已有实验的载荷、持久化条件和执行模型与 Stem 不同，适合指导实现，无法作为 Stem 的直接性能对照。其 Auto-Wake 同样依赖自身状态模型，Stem 预约功能需要以消费 Cell 的原子执行重新定义。

`cargo test --locked --test weighted_finality` 运行固定 epoch 实验。它验证超过三分之二的权重、顺序和去重、TC 对每位签名人最高 QC 的绑定、不同网络与 epoch 的签名域，以及父子执行 Header 的相邻视图。参考实现的先落盘再签名、身份绑定及超时重试规则用于设计 Stem 实验日志。Stem 独立实现并测试重启、精确重试以及拒签冲突和低于锁定水位的提案。该证书测试之外，`tools/jolteon_localnet.py` 运行实际多副本 Pacemaker、提案、签票和恢复。[Jolteon 论文](https://arxiv.org/abs/2106.10362)

CKB PoW 种子工具使用已固定高度和区块哈希，检查所读确认区间的连贯性。调用方依赖 RPC 观察，工具没有验证累计工作量。出块者可以尝试筛选候选哈希或扣留区块，预先确定的取样高度也让观察者在确认后提前知悉种子。出块选择、确认延迟、重组概率和有权重领导者抽样还需要独立证明和实验。开发链验证仅覆盖工具的确定性与错误处理。

Lineage 参考了 v10 在 `7db6d0c4` 的共享状态、实际并行执行及预约实现。共识有序解析、受限 Effects、费用与业务回滚分离、持久化预约游标可作为 Stem 的实现依据。Stem 的活动索引直接引用普通四字段 Cell，业务状态保持在原有 UTXO 中。[共享状态](https://github.com/Tondi-Foundation/Tondi/blob/7db6d0c4fa4eec07937c98fbc69ecdc884569e50/docs/SHAREDNOTE.md) [冲突执行](https://github.com/Tondi-Foundation/Tondi/blob/7db6d0c4fa4eec07937c98fbc69ecdc884569e50/docs/SHARED_EXECUTION.md) [Auto-Wake](https://github.com/Tondi-Foundation/Tondi/blob/7db6d0c4fa4eec07937c98fbc69ecdc884569e50/docs/AUTOWAKE.md)

Xuejie 的软件定义账户文章说明了如何在 UTXO 上构建可编程状态。CKB 的讨论区分了撮合权限与共享状态争用。Fuel 用稳定合约 ID 引用可消费的输出，Sui 将共享对象版本分配交给共识。这些设计支持在 Stem 保留实际 UTXO 的同时，将 LineageId 作为待执行命令的稳定引用。[Xuejie 原文](https://xuejie.space/2020_03_20_what_do_we_mean_when_we_say_account_model/) [CKB 状态争用](https://talk.nervos.org/t/permissioning-access-to-state-through-proof-of-work-verified-on-ckb/9952) [Fuel 交易规范](https://docs.fuel.network/docs/specs/protocol/tx-validity/) [Sui 共享对象](https://docs.sui.io/develop/objects/object-ownership/shared)

Axon 的参考资料展示了由 L1 Metadata Cell 提供验证者数据，再验证检查点 BLS 证明的思路。Stem 直接检查点从固定的创世委员承诺开始，逐块验证独立 Ed25519 签名及后继委员承诺。真实上链成本与百委员 VM 探针见[基准测试](BENCHMARKS.md#direct-ckb-finality)。Image Cell 的状态导入和只读访问用于比较接口。对 Stem 而言，未消费状态仍需证明锚点上的 Live Cell 状态。[Axon 质押与检查点](https://github.com/axonweb3/axon/discussions/898) [Axon Image Cell](https://github.com/axonweb3/axon/discussions/896)
