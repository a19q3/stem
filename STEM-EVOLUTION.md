# Stem 原生协议研发记录

基线为 `29872505350e931a252ddee9f88d94edf410c83a`，历史数据见 [Fern 实验归档](FERN-BENCHMARKS.md)。[Stem 协议](STEM-PROTOCOL.md)描述当前运行路径，[基准测试](BENCHMARKS.md)记录可复现的测量。

| 编号 | 方向 | 运行状态与剩余工作 |
| --- | --- | --- |
| 01 | Stem 改名 | GitHub 仓库、crate、CLI、服务、开发网及签名域已切换。历史分支和证据保留原名。 |
| 02 | Telegraph | Cell、交易、Group Message、Witness、Header、Receipt、执行证明、Live Cell 路径证明及原生控制交易已接入。Rust 和 C 读取器做了针对性差分测试。专用系统记录、历史与质押证明、快照封套仍使用当前的各自规范。 |
| 03 | Indexed Script Table | 四种布局实测。共识采用内联 Telegraph。索引式布局进一步缩小批量输出，其规范验证和解析开销见基准测试。 |
| 04 | Native Group Message | 已实现。消息进交易主体，通过 syscall 2123 读取。真实 Type Script 验证 Action 并拒绝缺失消息。 |
| 05 | 完整输入签名 | 已实现标准完整交易授权。标准锁和 CLI 共用解析规则，绑定输入、显式依赖、展开成员、Header Deps、原始交易及非签名见证。部分授权语义仍待独立实现。 |
| 06 | Native Block Context | 已实现候选高度、已提交父时间和父 Header。真实 RISC-V 测试覆盖时间边界与缓存。 |
| 07 | Weighted Jolteon | 单 epoch 实验用真实 Ed25519 签名验证加权 QC、TC、领导者和直接绑定执行 Header 的两链证明。Stem 运行 CometBFT。持久化锁定、传输、跨 epoch 交接和同负载网络验证待实现。 |
| 08 | StemVM 密码学 | 已接入固定计费的低 S secp256k1 恢复。与原 k256 路径差分验证。批量验证和通用加密 syscall 尚未启用。 |
| 09 | StemVM 优化 | 加入有界 Live Cell 缓存，提交和状态恢复时清空。执行缓存绑定父 Header 和解析依赖。代码与译码专项缓存待剖析。 |
| 10 | 状态与存储 | 现有 Patricia、MMR、回执、裁剪、原子提交和快照在新编码下通过测试。固定写入量下比较了 514 与 8,194 Cells，以及增加 512 块历史后的提交开销。批量路径算法与长期压缩仍有工作。 |
| 11 | Native Keeper | 未实现预约队列、预付和取消。 |
| 12 | OTX / Intent | 未实现部分授权和唯一交付验证。 |
| 13 | Fee Sponsorship | 完整交易支持独立代付锁组，钱包检查净扣款限额及实际总手续费。可重组的 Solver 授权待实现。 |
| 14 | 多维资源计费 | 已实现 genesis 费率、计费和缓存一致性测试。当前收费按交易容量差额冲销负债。系统操作继续走独立资源预算。 |
| 15 | 开放委托质押 | CKB devnet 重新验证自质押和委员会变更。尚无委托份额、收益或责任分摊。 |
| 16 | DevFund / Ecosystem Fund | 已开放可认证的手续费冲销累计值，客户端可用 SystemRoot 与 H+1 核验。储备归属、分配份额、可兑付应付款及授权支出待实现。 |
| 17 | CKB 直接验证 Stem 最终性 | 实测当前 CKB Checkpoint 合约和 Stem 委员会交接。合约验证独立检查点权威，完整 Stem 最终性及交接验证仍由链下工具负责。 |
| 18 | Intercell Messaging | 现有 CKB 和 xUDT 通道重新验证。通用认证消息和无许可中继待实现。 |
| 19 | L1 Cell Reference | 鉴别了交易包含与锚点存活的区别。未实现合约只读引用或独立的 L1 Live Cell 认证。 |
| 20 | UDT / Spore 结算 | CKB 与 xUDT 双向路径已用实际 devnet 复验。Stem 原生 UDT 和 Spore 导出需独立身份、生命周期和供应规则。 |
| 21 | CKB PoW Entropy | 独立工具从固定锚点与确认后继推导域隔离种子。CKB devnet 重复值一致，拒绝错误锚点和确认不足。未进入领导者或委员会选择。 |

## 关键依赖

- 固定交易编码和解析顺序为 Group Message 与完整签名提供确定性前提。部分授权、代付、预约交易需要各自定义重组和取消条件。
- 投票权由 L1 质押认证产生。原生两链共识和 L1 直接验证都需要完整委员会交接证明以及重启后的投票安全。
- 通用消息、L1 只读引用和资产导出使用独立的来源、目的网络和重放规则。L1 交易包含证明缺少证明 Cell 仍存活所需的状态信息。
- 费用收取先于分配研究。应付收益与现有储备、流通负债和提款预留共同核算。L1 质押本金保持独立。

## 参考来源与取舍

Telegraph 生成器和 Rust 读取运行时取自 v10 的 `7db6d0c4fa4eec07937c98fbc69ecdc884569e50`。Stem 使用独立 schema，许可证与来源见[复用声明](ledger/telegraph/UPSTREAM.md)。v10 的认证树、历史累积器、剪枝及锚定快照用于比较。Stem 保持现有独立的 Cell 状态结构。

加权参考实现位于 `codex/jolteon-experiment` 的 `90e0b1c49`，核心文件在 `consensus/karmadag/core/src/weighted_order/`。该实现的持久化、载荷可用性和视图计时器耦合到它自己的排序节点。已有实验的载荷、持久化条件和执行模型与 Stem 不同，适合指导实现，无法作为 Stem 的直接性能对照。其 Auto-Wake 同样依赖自身状态模型，Stem 预约功能需要以消费 Cell 的原子执行重新定义。

`cargo test --locked --test weighted_finality` 运行固定 epoch 实验。它验证超过三分之二的权重、顺序和去重、TC 对每位签名人最高 QC 的绑定、不同网络与 epoch 的签名域，以及父子执行 Header 的相邻视图。两链证明的安全需要验证者执行并持久化签名锁定规则。该实验验证证书与提案条件，未运行节点视图计时与投票协议。[Jolteon 论文](https://arxiv.org/abs/2106.10362)

CKB PoW 种子工具使用已固定高度和区块哈希，检查所读确认区间的连贯性。调用方依赖 RPC 观察，工具没有验证累计工作量。出块者可以尝试筛选候选哈希或扣留区块，预先确定的取样高度也让观察者在确认后提前知悉种子。出块选择、确认延迟、重组概率和有权重领导者抽样还需要独立证明和实验。开发链验证仅覆盖工具的确定性与错误处理。

Axon 的参考资料展示了由 L1 Metadata Cell 提供验证者数据，再验证检查点 BLS 证明的思路。Stem 当前签名格式为独立 Ed25519，直接验证需要认证历届委员会并测量见证大小与 CKB-VM cycles。Image Cell 的状态导入和只读访问用于比较接口。对 Stem 而言，未消费状态仍需证明锚点上的 Live Cell 状态。[Axon 质押与检查点](https://github.com/axonweb3/axon/discussions/898) [Axon Image Cell](https://github.com/axonweb3/axon/discussions/896)
