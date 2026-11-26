# Stem 协议方案

## 1. 网络定位

Stem 是一条以 CKB 计价、由加权 Jolteon 最终确认的 Cell 网络。Stem 使用唯一的原生 Polytype Cell 模型与交易格式。一个 Cell 可同时绑定多个 Type Scripts，内核按完整 Script 身份派生并执行所有验证组。CKB L1 的真实质押决定验证者权重，独立桥金库负责 CKB 和 xUDT 的兑付。Stem 不发行独立原生代币。可兑付 CKB 的唯一增量来自已经确认并可兑付的 L1 存款。

[Metatype](METATYPE.md) 在 CKB 上用单一 Type Script 表示一个部署中的多项独立约束。CKB 合约从实际输入输出及 Manifest 推导完整虚拟组，保持 CKB Lock 的消费授权。当前运行路径覆盖 CKB 独立发行、转移及合并结算。定义 Cell 可按 CKB 数据哈希复用完整 Script 集合，状态仍在各自的 Cell 内。两种存储形式共享虚拟组，字节预算按展开后的逻辑视图计算。虚拟组可读取独立消息、证明和 Seal。不可变对象的审批 Seal 承诺实际 CKB 创世、完整部署和原始交易。对象夹具已与四副本原生 Polytype 对照。不可变托管路径可将同一对象及原始约束集合带到 CKB，再销毁映射、取回 Stem 原件。已接入每 Cell 数量上限与资产守恒两项独立约束的同质资产托管。CKB 按认证的增减量发行及销毁，Stem 支持部分和全额赎回并保留完整 Script 集合。可修订对象配置在两侧使用相同的单调版本规则并分别执行独立审批。导出时保留当前状态，返程使用已认证的 CKB 销毁状态，保留原始 Capacity 本金及完整 Type Script 集合。返程可通过普通授权输入补足额外状态抵押，CKB 导入时须先为信箱预留最大返程载荷容量。

对象可附加独立的状态长度约束。第三个 Type Script 承诺原对象 Script 及一个规则来源。固定规则按内容哈希从两链实际 Cell Dep 读取，可在新 OutPoint 重新发布相同字节。可更新规则按完整 CKB Type ID 注册表 Script 哈希寻址。CKB 合约强制保留同一 Type 与 Lock，只允许提高长度上限。CKB 从实际 Cell Dep 读取，Stem 从已提交锚点的 guardian 存活认证读取。返程恢复原始约束集合、资产本金及获认证的对象状态。降低上限、改变授权或要求验证每个中间状态的应用需要独立的结算语义。

可修订对象还可组合独立的只追加约束。它绑定完整对象身份，要求后继保持原状态，或在递增版本时保留 MIME 类型并严格扩展内容前缀。CKB Adapter 和 Stem 原生组使用同一验证谓词。前缀关系可传递，返程可直接验证托管状态到认证销毁状态的变化，并继续执行身份、审批及长度约束。

授权链工作流用于需要核验中间步骤的对象。请求、审核与完成分别由独立密钥签署，签名承诺网络、完整约束 Script、前一状态承诺、主题和步骤。CKB 对每次修改执行一步验证，Stem 返程从托管态重放有序证明，并核对认证销毁状态。工作流沿用组级 Proof Data 和既有跨链消息认证。

重建工具按实际 OutPoint 从 CKB 销毁回溯到导入，使用调用方认证的锚点核验区块祖先及交易和见证包含证明。恢复钱包可从 CKB 链上见证重建 Proof Data。仅变更归属的交易保留在可核验路径中，返程证明只包含实际完成的授权步骤。

同一 Polytype Cell 可用完整资产 Script 哈希为键分别记录多项数量。CKB Metatype 将数量放入各自的 Manifest 项，托管同时绑定完整约束和资产键集合。导出、转移、销毁和返程逐资产核对守恒与数量差额。返程可选择任意非空资产子集。零数量保留原有资产键及全部验证义务，剩余资产可用后续 Mailbox 消息返程。Cell 可在 Stem 本地组合和拆分，沿用原有资产 Script 身份。

对象可附加固定 Capacity 版税 Type Script。支付 Cell 绑定完整规则和目标部署，收款 Lock 由规则分别指定。Stem 的所有权变更及托管导出和释放、CKB 的映射对象所有权变更分别检查各自链上的实际支付输出。

对象可附加可轮换审批 Type Script，绑定 CKB Type ID 注册表及完整对象 Script。注册表记录代次、阈值、最多八把按字节排序的独立 Ed25519 公钥及冻结状态。两链按位图选定恰好达到阈值的签名者，并按公钥索引顺序验证签名。审批承诺完整注册记录、网络、部署域、规则 Script 和实际交易。CKB 读取实际依赖，Stem 读取已提交 CKB 锚点的存活认证。增改公钥、阈值与冻结都产生新的代次，保留原对象及审批 Script 身份。冻结期间待返程消息保持未消费且原托管继续锁定，解冻后使用当前代次的审批释放。

CKB 状态上限注册表可提高或下调当前限制。新状态及修改后的状态须满足当前上限，已有状态可按原字节转移。超限导入或返程必须核对绑定的两链托管代码及真实消息，才能保留此前合法接受的内容。

每个网络用全新的 genesis、数据库、签名域和桥实例启动。网络身份由 canonical network spec 和一次性网络盐导出。该预部署 spec 包含 CKB genesis、初始权限与经济配置，排除依赖网络身份才能部署的 Registry Type ID、金库 Type ID、Checkpoint Type ID 及初始化签名。部署完成后，Stem genesis 再固定这些 L1 合约身份和第一份已认证的完整质押快照。CKB Checkpoint Cell 的身份与初始认证方也由 genesis 固定。链身份与创世 Header 承诺进入 Jolteon 签名域。所有交易、证明和链间认证都明确绑定该网络。

| 组件 | 权责 |
| --- | --- |
| Stem Kernel | Cell 编码、交易验证、状态转换、资源计量、状态与执行承诺 |
| CKB-VM | 固定 RISC-V 执行配置和只读交易上下文 |
| 加权 Jolteon | 交易排序、执行 Header 最终性、历史委员会与 epoch 交接 |
| CKB Stake Registry | 质押保管、唯一记录、退出延时、证据与处罚 |
| L1 状态认证 guardians | 认证 CKB 上的 Registry、桥储备和历史。各用途分别固定权限与签名域 |
| L1 金库 | 独立的托管权限与兑付顺序，执行实际跨链交割 |
| L1 Checkpoint Cell | 在 CKB PoW 历史中固定已认证的 Stem 状态与委员会承诺 |

## 2. 原生 Cell 和交易

```rust
struct Cell {
    capacity: u64,
    lock: Script,
    type_scripts: Vec<Script>,
    data: Bytes,
}

struct Script {
    code_hash: Hash32,
    hash_type: ScriptHashType,
    args: Bytes,
}
```

Stem 采用 Strand 序列、密集向量和可选字段位。Type Script 集合按完整 Strand Script 字节序排序。Script 身份仍使用完整 CKB Script 的哈希。CKB L1 合约边界保留 Molecule。解码拒绝乱序、重复及超限编码，排序位置仅用于读取。代码标识、寻址方式、参数、所有 Type Script 和 Cell Data 都在输出原始字节中，资产身份因此独立于同一个 Cell 的抵押、监管或结算脚本。Cell 是消费和创建的原子单位。

交易使用一张 schema。原始部分含 Cell Inputs（OutPoint 与可选 Stem Since）、完整输出 Cells、Cell Deps、Header Deps、可选的截止高度、按 Script Group 寻址的应用消息，以及可选的协议操作。Witness 区域承载按组寻址的验证材料与签名，并为协议操作保留证书字段。普通交易至少消费一个可用 Cell。Bridge Mint、Stake Snapshot 和 Bridge Settlement 使用同一套编码、哈希和提案路径。原始交易中只允许一个操作，证书位于独立的 Witness 字段。Bridge Mint 的所有实际输入与输出仍由常规脚本规则验证。Stake Snapshot 和 Bridge Settlement 是控制交易，Cell 输入输出、Cell Deps、Header Deps 与普通 Witness 均为空。Bridge Settlement 的两种声明分别为已兑付提款核销和同一 L1 交易中的双金库储备调拨。

```text
tx_id      = H("Stem transaction ID", network_id, canonical_raw_tx)
tx_commit  = H("Stem transaction commitment", network_id, canonical_full_tx)
cell_leaf  = PatriciaLeaf(network_id, cell_key, live_cell_record)
```

各域标识采用固定字节串并以零字节结尾。变长字段由 Strand 或显式长度前缀编码。普通 OutPoint 是 `(tx_id, output_index)`。Genesis 分配使用单独的创世承诺和确定性编号，主网资金来自认证存款，测试 genesis 可显式配置测试配额，测试配额只能存在于关闭真实 L1 兑付的独立网络。

### Witness、CoBuild 与 OTX

```text
ScriptGroupId  = (role, script_hash)
GroupMessage   = (group_index: u16, actions: [{ kind: u16, data: Bytes }])
GroupWitness   = (group_index: u16, proof_data, seal)
```

组编号按角色和完整规范 Script 字节排序，从零开始。每组最多一个 Witness，条目严格递增。解析器拒绝重复、乱序、空条目以及指向未推导组的编号。无 Witness 需求的组省略条目，内核继续执行该组。Group Messages 属于交易主体，按组编号严格递增。每条消息包含 1–64 个有序 Action，合约解释 Action 的类型及数据。签名者在收集 seal 前确定消息。内核拒绝不存在的组编号，所有实际推导出的组始终执行。普通支付可省略消息。

标准锁的签名摘要绑定网络、Lock 角色、完整 Script 哈希及原始交易 ID。它还承诺按顺序解析的输入 Cell 内容、每个 Cell Dep、Dep Group 容器及其实际展开成员、已声明 Header Dep 的完整编码，以及所有非空 `proof_data`。交易 ID 已覆盖输出、输入引用、依赖引用、排他有效高度及 Group Messages。独立签名人可分别填入各组的 seal，完整交易承诺覆盖全部 seal。其他 Lock 可以定义自己的验签逻辑。

CoBuild 保留 IDL、钱包展示、Action 构造与多方协调。每个合约独立核对 Action 与实际 Cell 差分。Stem 的配对 OTX 使用普通交易和专用 RISC-V Lock。签名绑定网络、完整 Lock 身份、输入 OutPoint 与 Cell、交付 Cell 的完整 Strand 哈希、实际解析的有序依赖、有效高度、费用上限及全体签名人认可的完整 Action 清单。Solver 可以移动一对输入与输出，脚本总是用已解析输入的下标匹配同下标输出。一项部分授权只消费一个 Cell，同一输出不能满足两个独立配对。配对模式要求零 Since 和相同有序依赖。持有人可以使用完整交易签名取消。 [CKB Open Transaction 场景](https://github.com/nervosnetwork/ckb/discussions/3670)

Action 清单通过实际推导出的组校验，按 `(role, script_hash)` 唯一排序。哈希从独立域 `Stem intent Group Messages` 开始，承诺小端 `u16` 的组数、每组的角色字节、完整 Script 哈希及小端 `u16` 的 Action 数。各 Action 按原顺序承诺 `kind: u16`、`len: u32` 和原始载荷，整数使用小端。Solver 可根据最终交易重新绑定组编号。新增、移除、改序、改变组身份或修改载荷都会改变摘要。每个实际 Lock 和 Type Group 仍按普通规则执行。

配对授权只接受同一个 Data 寻址 Intent Lock 程序的配对条款作为非空 Group Proof。跨链保留 Action、独立 L1 引用及 Lineage、Keeper 或其他系统控制需要各自的完整授权路径。配对签名和代付共用同一 Action 清单。

可重组代付使用交易位置 0 的一个纯 Capacity 输入及同锁找零。授权额外绑定位置 1 的指定 OutPoint 和完整交付 Cell。代付扣款必须为正、低于各自授权上限且不超过整笔交易实际消耗的手续费。已认证的 Lock 和 Type Groups 仍独立执行。手续费和多资产交付在同一次普通交易提交内生效。

### Lineage Cell

已实现的注册表使用普通四字段 Cell 和网络固定的 Lineage Type Script。其 32 字节参数是稳定的 `LineageId`。按顺序哈希 `"Stem Lineage ID\0"`、32 字节网络 ID、注册交易首个消费的普通 OutPoint 的 36 字节编码和小端 `u32` 输出下标。哈希使用 CKB 的 32 字节 Blake2b。注册和 Executor 更换用该 Type Group 的一个显式 Action 声明固定的 RISC-V 程序内容哈希。实际代码必须存在于解析后的依赖。Lineage 元数据在同一个 SystemRoot 下承诺，Live Head、版本和 Executor 与实际 UTXO 同步更新。Cell Data 和其他 Type Scripts 可由应用独立使用。

注册版本为零。后继消费唯一当前 Head，版本加一，并运行旧 Lock 与输入输出派生的全部 Type Groups。更换 Executor 或控制 Lock 经过当前 Head 的授权。显式退休消费 Head 并移除活动记录，注册锚点已经消费，原身份无法重新生成。普通网络可省略 `lineage_type` 配置。

`LineageRef` 由 32 字节 `LineageId` 和最短无符号 LEB128 选择器组成。`0` 选择当前 Head，正数 `v + 1` 选择精确的 `u64` 版本 `v`。选择器最高为 `2^64`，因此能表达完整的版本范围。引用的总长度为 33 至 42 字节。拒绝零 ID、超范围整数、非最短编码和多余字节。

`/stem/lineage` 接收相同的规范引用并返回查询高度的 SystemRoot 和 CellRoot 路径。精确引用必须匹配该高度的 live Head。`Latest` 在该高度没有 live Head 时可以返回认证的缺失证明，该证明无法判定某历史版本是否曾经存在。历史读取需同时持有一个该版本仍是 Head 时的已认证 Header 及其两条状态证明。节点的查询仅服务当前高度，客户端可以保存历史证明并在对应的 Header 下验证。

Lineage 命令是原生 Strand 交易的可选字段。命令包含 1 至 16 个严格按原始 LineageId 排序且互异的目标，每个目标包含 `LineageRef` 和最低后继 Capacity。`Latest` 可以附加非平凡的闭区间版本约束。完整范围采用无约束形式，单一版本采用精确引用。交易签名覆盖规范化后的目标、网络、固定 Executor 的 Data 哈希和直接 Cell Dep、完整依赖引用、唯一付款 OutPoint 及其 Cell 内容、排他的过期高度、应用载荷、确定费用和两项 cycles 上限。应用载荷最多 4 KiB。网络和签名域防止跨网重放，精确付款 Cell 防止同网重复提交。付款 Cell 只持有 CKB Capacity，扣除授权费用后的余额返回原 Lock。

节点在交易的规范区块位置查找当前 Head。`Latest` 和 `Exact(v)` 均从同一注册表解析，`Exact(v)` 仅在当前 Head 的版本等于 `v` 时可写入。选择器或 Executor 校验失败时跳过执行。付款 Lock 先验证授权，Executor 在有界的 RISC-V V2 VM 中读取已声明目标、应用载荷、候选高度、上一提交区块时间和付款 Lock 哈希。Executor 为每个目标返回至多 16 KiB Data 及新 Capacity。内核保留原 Head 的 Lock 和完整 Type Script 集合，检查后继最低 Capacity、目标间总 Capacity 守恒以及存储抵押。生成的完整交易仍须通过原有 Lock 和所有输入、输出 Type Groups。权限变更和退休保持普通精确 OutPoint 授权。

成功时原子消费付款 Cell 与所有 Head，在原命令交易 ID 下创建后继。输出 0 是付款余额，其余按目标顺序。Executor 拒绝、耗尽预算、给出不合规后继或后续 Script Group 拒绝时，已通过付款授权且覆盖预算的命令可以提交费用结果，全部 Head 保持原样。未认证、过期或签名匹配条件失效的命令没有费用结果。回执的协议结果单字节为 1（已应用）或 0（已拒绝业务）。已执行尝试的逻辑读取和 VM cycles 一并计费，失败阶段按签名上限计费。

同一 Head 的访问冲突按区块顺序执行。不同命令只有在当前 Head、费用输入、预期输出及依赖访问互不冲突且保留完整预算时才并行解析和运行 Executor，最终检查和提交按原交易顺序。使用 Dep Group 的命令按串行路径执行。

#### 独立业务授权与代付

Lineage Intent 允许最多八位业务参与者先签署同一组操作，再由代付方选择手续费 Cell。参与者 ID 由域分离的压缩 secp256k1 公钥哈希生成，按字节严格排序并去重。业务签名覆盖网络、参与者集合、费用上限、执行代码和有序依赖、全部目标和版本约束、载荷、执行上限、过期高度与可选预约触发条件。签名摘要只将实际费用和费用 OutPoint 规范化。所有业务签名按参与者顺序进入完整 Witness，随后费用 Lock 签署最终交易 ID、费用 Cell 内容及完整业务签名。

每项 Intent 至少将一个目标锁定在精确版本。原子更新成功时该版本前进，同一组业务签名即使更换付费 Cell 也无法再次更新。业务失败时保留原有目标版本，已签名并消费的费 Cell 仍只支付一次。换用另一枚费用 Cell 再尝试需要新的代付方签名，且仍受原业务签名的高度、版本和金额约束。节点验证全部业务签名后才运行 Executor。每份签名计入固定恢复 cycles，缓存命中计费相同。

Executor 通过 syscall 2126 的 source 2 按索引读取已验证参与者 ID。应用必须把业务参与者与实际输入状态中的权限绑定。在付费方可替换的 Intent 中，Executor 读取的付款身份字段为零，避免业务结果取决于参与者签名之后选定的代付方。业务后继经过原有所有 Lock 与 Type Groups。双边结算示例用两份业务签名匹配两枚储备的所有者，用两个 UDT Type Groups 分别验证两种资产的守恒。

#### 原子组合多条 Lineage Intent

Solver 可以组合 2–8 条各自授权的 Lineage Intent。按计划给定的顺序，将网络、成员数量及每个成员的规范化业务条款分域哈希。计算成员条款时，移除计划 ID 和付款 OutPoint，并将实际费用归零。每个业务签名把相同的计划 ID 写回自己的完整条款。每位代付方按原有路径签署自己的费用、付款 Cell 和业务签名。代付方仍可在业务签名之后选择。

提交封套携带所有已签成员。节点重算计划，拒绝拆开提交、改变顺序、替换成员、重复付款 OutPoint、目标 Head 重叠、嵌套批次及成员触发条件不一致。每位签名人可以单独检查成员和计划。执行仍使用当前 Head、各成员自己的 Executor、依赖、业务签名、费用 Lock 和全部原有 Lock 与 Type Groups。

节点在同一候选区块顺序验证成员，原子提交涉及的 Cell 和 Lineage 记录。任一成员未获授权、预算不足、应用失败或 Script Group 拒绝，整笔批次无效，先前成员的临时状态和手续费全部撤销。批次共用一个交易 cycles 上限，并按完整封套计量区块字节。封套额外字节由首位成员承担资源费，仍受其签署的费用上限约束。批次回执承诺实际费用和 cycles 合计。每个成员的退款及后继使用该成员的交易 ID 与原输出序号，客户端从批次交易证明取得完整成员，再验证对应 Live Cell 和 Lineage 路径。

### Native Keeper

Genesis 可配置 `keeper` 的预约窗口 `max_delay`、单高度上限 `per_height` 和正数预约费 `reservation_fee`。可选的 `keeper.time` 固定每块时间预约份额 `per_height` 和最长等待 `max_delay_ms`。两种执行份额合计不超过每块 16 笔，并为用户交易保留区块资源。Keeper 复用 Lineage 执行。签署包含 `execute_at` 或 `execute_after_ms` 的一条 Lineage 命令或原子批次，再由普通 Cell 交易提交执行体和预约费。批内每个成员在签名前采用完全相同的可选触发条件。批次的排他过期高度为所有成员的最早过期高度。Strand 编码承诺一种触发条件及可选重试策略，两种触发条件只能选其一。单条命令或完整批次的字节上限为 8 KiB。预约时验证各成员实际付款 Lock、执行费签名及业务签名。整个批次共享一个预约份额，各成员的签名和声明的 VM 预算合计须符合该份额。高度和时间预约都为每个付款 OutPoint 保存占用记录，承诺触发条件、首个成员的付款 OutPoint 和预约执行体的交易 ID。配对 OTX 签名无法授权新增预约或取消。

区块先处理当前高度桶的一次性任务，桶内按首个成员的付款 OutPoint 排序。然后在时间预约份额中优先清理已到达排他过期高度的命令，按过期高度、目标时间及 OutPoint 排序。其余就绪命令按目标时间及 OutPoint 排序。时间预约读取候选块父 Header 的共识时间，使用 Unix 毫秒。预约时要求目标时间晚于当前父时间，且同时处于时间窗口和高度窗口内。等待期间未达条件的命令留在 SystemRoot 的时间索引，索引最多 32 条，执行体按首个成员的付款 OutPoint 独立保存。停机后首次跨越时间阈值的 Header 只有进入父上下文时才触发预约。候选分支使用各自的父 Header，只有取得最终性的执行才可对外证明。超出本块时间份额的就绪命令继续等待，达到过期高度后只记录跳过。

每个已选预约拥有由区块上限计算出的字节、读取、写入和 cycles 份额。高度与时间共用同一执行覆盖层。单条命令遇到已授权的业务失败时，仍按原有 Lineage 规则收取执行费。批次内任一成员失败则整批回滚，回执结果为 2（跳过），只保留原预约费，所有执行费 Cell 保持原状。付款 Cell 已被消费、Head 版本失效、预约过期或份额不足也产生跳过回执。跳过的 cycles 记录保留份额。

完成、跳过及授权取消原子删除预约执行体、队列索引和全部付款占用。任一代付方均可通过其普通 Lock 消费自己的预约付款 Cell 取消整批。取消 Action 绑定原触发条件和具体预约交易 ID。未声明取消的普通付款 Cell 花费会使待执行命令届时跳过。

预约状态与普通交易使用同一次原子提交，快照包括预约索引、执行体和代付占用记录。执行列表和回执按相同顺序纳入 Header 承诺。用 `/stem/keeper` 查询高度预约，用 `/stem/keeper-time` 查询时间索引或按首个成员的付款 OutPoint 查询执行体，用 `/stem/keeper-reservation` 核验任一付款 OutPoint 的预约归属或不存在证明。完成的尝试通过 `/stem/execution` 验证。Jolteon 各副本独立从父状态推导预约执行。

#### 有界重试

签名人可在任务中选择 `retry = {max_attempts, delay_blocks}`。允许 2–4 次总尝试，间隔至少一个区块。原子批次的每个成员必须拥有相同的触发条件和策略。预约额为网络预约单价乘以总尝试次数，预约交易另须满足常规资源手续费。所有预约费在提交时支付。成功、取消、提前终止及未用次数均不返还预约费。

重试任务使用配置的时间预约份额和有限 SystemRoot 队列。启用重试时，签名高度是首次可以尝试的高度。一次性高度预约仍在所签高度桶执行。按时间触发的任务仍使用候选分支父 Header 的共识毫秒时间。到期任务优先清理，其他就绪任务按原触发条件（高度在时间之前）、目标值和首位代付 OutPoint 确定顺序。每个任务另有可验证的剩余次数和最早执行高度。排他过期高度及预约窗口限制全部尝试。

重试执行沿用原子 Lineage 路径。失败的尝试回滚业务更新，保留执行费 Cell，并计入一个保留份额。若仍有次数且下次高度早于过期高度，回执结果为 3（等待重试），剩余次数减一，最早高度推进到当前高度加签名间隔。成功时按已签额度扣费并清除剩余次数。遇到已经花费的执行费 Cell、耗尽次数或过期时终止并记录结果 2。单次任务的原有业务失败结算仍适用。状态读取错误会中止候选区块准备。

整个重试期间保持原交易 ID 和全部代付占用。预约状态、次数变更和实际状态提交保持原子性。代付方仍可绑定该 ID 取消。客户端按区块高度和交易位置核验各次尝试的回执。同一任务的交易 ID 可出现在多个区块的执行证明中，成功产出仍只发生一次。

## 3. 确定性执行与资源

对交易按区块顺序解析实际 UTXO。Lock Groups 来自全部输入的 Lock Script，Type Groups 来自输入与输出的 Type Scripts 并集。按完整 Script 身份分组，Lock 和 Type 角色分离。每组只执行一次，拥有自己的输入和输出索引集合，并读取同一份不可变交易。输入侧规则可以拒绝自身被删除、替换或迁移。

脚本寻址保留 `code_hash/hash_type/args`。原生编码收敛为 `Data` 内容哈希和 `Type` 完整代码 Type Script 哈希。Type 寻址只承认 genesis 固定的代码身份规则，例如一个 Type ID 约束下的唯一代码 Cell。交易必须明确列出实际代码 Cell Dep，升级后的代码对应新的 OutPoint。Dep Group 按其规范 OutPoint 向量单层展开，重复依赖、与消费输入重叠的依赖和超限展开直接拒绝。L1 Script 的原始哈希类型继续用于计算 CKB/xUDT 的 L1 身份。执行配置由网络固定为 CKB-VM V2 RISC-V 配置，复用 Assembly、分配复用及 Spawn/Exec。

原生 ABI 提供完整 Cell、完整 Type Script 集合、按索引及按哈希查询、当前组的角色和索引、组级消息及 Witness。保留 `load_script`、`load_script_hash`、全局及组相对的 Cell 读取语义。多个 Type Script 不扩大 Cell 写冲突域。确定性覆盖层记录消费、创建及依赖读取。可并行验证独立交易和各 Script Group。后续交易对同块输出、依赖和已消费 Cell 的可见性以串行重放定义。

缓存键覆盖网络、固定 VM 配置、完整 Witness、解析后的 Cells、代码、声明的 Header 及认证的 L1 引用。读取候选高度或已提交父 Header 的脚本还必须匹配原父区块，Spawn 与 Exec 的读取也计入。其余执行结果可跨父区块复用，每次仍执行引用解析、存活检查及时锁检查，并按已验证的 cycles 计费。区块同时限制编码字节、交易数、VM cycles、依赖读取字节和写入量。准备提案可以减少重复工作，验证提案与重放都以相同规则计算预算和结果。

标准锁在 RISC-V 内校验恢复的压缩公钥哈希，通过 syscall 2121 进行 secp256k1 恢复。恢复接受低 S 的 65 字节签名，计算结果与原 k256 锁进行差分验证。每次调用在读取 VM 内存前计入 1,900,000 cycles。节点可以本地复用相同完整签名和摘要对应的成功恢复结果。命中缓存时计费相同，Lock Script 仍验证公钥对应的授权。CKB L1 合约维持 CKB 原生 ABI。

创世配置可设置 `"vm_crypto_v1": true`，以系统记录 `vm/crypto-abi = 0x01` 将 ABI 版本绑定到 Genesis Header。省略或设为 false 使用原创世编码。启用时，Lock、Type Script、子 VM 及 Lineage Executor 可调用 RISC-V syscall 2131。a0 为操作号，a1/a2 为输入地址和字节数，a3 为哈希输出地址。返回 a0 为 0 表示成功，4 表示操作、长度或签名无效。非法内存或超出 cycles 预算按 VM 错误处理。

| a0 | 输入与输出 | 额外 cycles |
| --- | --- | --- |
| 0 | 0 至 1 MiB 原始字节，写入 32 字节 CKB 个性化 BLAKE2b-256 | 每输入字节 128 |
| 1 | 1 至 64 条定长 129 字节记录，每条依次为 33 字节压缩 secp256k1 公钥、32 字节摘要和 64 字节低 S 紧凑 ECDSA 签名 | 每条 2,200,000 |
| 2 | 1 至 64 条定长 128 字节记录，每条依次为 32 字节 Ed25519 公钥、32 字节消息和 64 字节签名，使用严格验证 | 每条 3,200,000 |

每次调用先计 25,000 cycles。长度合法的批次在读内存前按全批计费。每条签名独立验证，整批通过才返回成功。脚本可根据网络的交易或 Executor 预算使用更小的批次。计费标定、完整 ELF 验证与四副本创世重启实测见[原始记录](benchmarks/2026-11-27-crypto-v1.json.xz)。


### Stem Header 与时间

原生 Stem Header 编码高度、共识时间、父 Header ID、状态承诺和执行承诺。运行交易前，以高度、共识时间、父 Header ID 和交易根计算独立的区块上下文 ID，用于认证同块新建 Cell 的创建信息。完整 Header ID 在状态与执行承诺得出后计算。Jolteon 投票认证完整 Stem 执行 Header。Stem 原生 Header 没有 CKB PoW nonce、compact target、难度、Uncle 承诺或 DAO 字段。交易执行删除 NC-Max Proposal Window、Cellbase、Cellbase Maturity、一级与二级发行、矿工奖励和 Nervos DAO 的特殊验证、取款补偿与容量例外。CKB PoW、DAO 等 L1 字段可用于核验真正的 L1 数据，L1 协议继续独立运行。 [CKB L1 区块结构](https://github.com/nervosnetwork/rfcs/blob/master/rfcs/0027-block-structure/0027-block-structure.md)

合约可以读取候选高度、已提交父区块的 Unix 毫秒时间及父 Header。候选时间只在提交后供下一块的合约使用。CheckTx、提案准备与执行都提供相同的确定性接口，无需预先知道候选交易根或执行结果。

| 尺度 | 由什么确定 | 用途 |
| --- | --- | --- |
| Stem 高度 | 已确定的 BFT 区块序列及候选高度 | 绝对与相对高度锁、交易过期 |
| Stem epoch | 高度和 genesis 固定的区块数量 | 质押状态与委员会边界 |
| Stem 共识时间 | 已提交 Header 的 Unix 毫秒 | 绝对时间与相对时间锁 |
| CKB L1 高度与时间 | CKB 自己的 PoW 链和共识规则 | 解除质押、存款确认、桥兑付及检查点锚定 |

冻结的 CometBFT v0.38.26 对照以 genesis 时间启动，之后从上一高度提交中按该高度的实际验证者权重计算时间戳的中位数。Stem 以截断到 Unix 毫秒的共识时间入账。候选 Header 的毫秒时间允许相同，禁止倒退。Stem 内核读取**前一已提交 Header**的时间作为时间锁参考。它省去额外滚动窗口，安全假设仍是 CometBFT 的验证者权重与诚实时钟假设。 [CometBFT v0.38.26 BFT Time 规范](https://github.com/cometbft/cometbft/blob/v0.38.26/spec/consensus/bft-time.md) · [对应版本的时间实现](https://github.com/cometbft/cometbft/blob/v0.38.26/state/state.go#L234-L292)

Jolteon 验证者只给严格推进父时间的执行 Header 投票。投票时，提案签名绑定的时间须处于验证者实时钟的过去 30 秒与未来 1 秒之间，策略绑定到共识签名域。历史重放使用已认证 Header 原有的时间，epoch 交接 seal 重复边界 Header。已提交时间的准确性依赖诚实投票权重及其时钟同步，Pacemaker 超时使用本地单调时钟。合约和时间锁继续使用父执行 Header，不读取本地实时钟。

当已见的认证链超出 192 个候选块的重放范围，Jolteon 节点暂停投票，从首个缺失高度请求归档节点的最终性证明。每段执行以本地已提交头为起点，并使用当前委员会的两链证明或终结证明认证。节点使用同一执行与原子落盘流程逐段前进，保留同一 epoch 已签的投票、超时和锁定状态。遇到交接按正常规则切换委员会。归档需保留缺失段的载荷和证书。

新观察节点可从独立核验的 CKB 检查点恢复。快照恢复认证目标执行 Header 及创世以来的委员会交接，重算 CellRoot、SystemRoot 与历史前沿。恢复后的状态与永久只观察的共识日志原子安装。由快照启动的观察节点会独立执行随后收到的区块。已有签名节点使用自己的投票日志和认证归档重放。

```rust
enum Since {
    AbsoluteHeight(u64),
    RelativeBlocks(u64),
    AbsoluteTime(u64),
    RelativeTime(u64),
}
```

以上四种值使用同一 9 字节固定结构（1 字节标记和 8 字节小端数值），时间值均为 Unix 毫秒或以毫秒表示的时长。每个输入可省略 Since。相对零块或零毫秒用省略表示，显式的相对零编码拒绝。Stem 不编码 epoch 分数锁。SDK 用固定的 `1 + epoch * blocks_per_epoch` 把 epoch 边界换算成高度，乘法和加法检查溢出。CKB L1 相对 `since` 仍按 L1 自有语义验证。 [CKB RFC 0017](https://github.com/nervosnetwork/rfcs/blob/master/rfcs/0017-tx-valid-since/0017-tx-valid-since.md)

令 `H` 为候选块高度，`T` 为前一已提交 Header 的共识时间。首个候选块使用 `T = genesis_time_ms`。创世 Cell 的创建高度为 0、创建时间为 genesis 时间，并随其创建上下文 ID 一起认证。

| Since | 成熟条件 |
| --- | --- |
| `AbsoluteHeight(h)` | `H >= h` |
| `RelativeBlocks(n)` | `H >= creation_height + n` |
| `AbsoluteTime(t)` | `T >= t` |
| `RelativeTime(d)` | `T >= creation_time_ms + d` |

加法溢出、负的上游时间或时间换算溢出使对应数据无效。同块创建后允许按照交易顺序消费。只有未设置相对锁的输入能在同块完成此路径。过期条件独立于 Since，`valid_before_height` 为排他上限，`H >= valid_before_height` 时拒绝。执行读取已认证的上下文和确定性候选高度，本地时钟、实时 CKB RPC 与未提交候选时间均不参与锁成熟判断。

停摆期间，已提交的高度、epoch 和时间冻结。恢复后，新的提交时间可能跨越多个时间期限。按高度保护的争议窗口仍须经过相应数量的区块。需要保证可出块反应窗口的 HTLC 或通道规则使用高度差。贷款到期等墙钟相关条件按实际签入的共识时间生效。未来替换共识时，适配层必须证明能提供相同的最终性、单调时间、高度和停机语义。

### Capacity、存储抵押与资源费

```text
cell_bytes = 13 + len(lock) + len(data)
             + (type_scripts.is_empty() ? 0 : 4 + sum(len(type_script)))
storage_bond = (cell_bytes + state_overhead_bytes) × bond_per_byte

fee_paid = Σ input_capacity − Σ output_capacity
fee_required = p_b × B + p_c × C + p_r × R + p_d × D + p_w × W + p_x × X
```

13 字节覆盖 Cell 存在位、固定宽度 Capacity 和 Data 长度。Script 长度为 `37 + len(args)`。非空 Type Script 集合额外计入 4 字节数量字段。`state_overhead_bytes` 由网络固定。消费 Cell 时，抵押随输入 Capacity 重新可用。L1 质押与收款输出继续采用 CKB 的容量规则。

20 字节参数的普通 Lock 编码为 57 字节。空 Data、无 Type Script 的 Cell 占 70 字节。每字节 0.01 CKB 时，抵押为 `0.70 CKB + state_overhead_bytes × 0.01 CKB`。价格由网络配置指定。

| 费用量 | 共识计量 |
| --- | --- |
| `B` | 完整交易及 Witness 的字节数 |
| `C` | 全部 Script Group 的验证 cycles。缓存命中仍按实际 cycles 计费 |
| `R` | 为交易解析读取的 Cell 数，含 Dep Group 容器及其实际成员 |
| `D` | 这些 Cell 的规范编码字节量 |
| `W` | 新 Live Cell 记录的编码字节与每项消费的 36 字节 OutPoint |
| `X` | 消费的输入 Cell 数 |

`fees` 为每个资源单位配置整数微 shannons，每 1,000,000 微 shannons 等于 1 shannon。创世承诺精度和六个费率。以 `u128` 计算资源消耗总价，实际手续费仍以整数 shannons 支付。总价可以一次向上取整，计算预算时等价于用实付手续费换算后的微 shannons 减去固定成本，再按 cycle 费率向下求可用 cycles。区块和交易上限继续生效。省略费率配置的研究网使用全零价格。物理 RocksDB 读写只作观测。手续费冲销流通负债，产生等额的 L1 储备盈余。

**控制交易费用。** 合法的 Stake Snapshot 和 Bridge Settlement 支付零 Capacity，豁免普通费用公式。控制交易必须使用固定操作形状和超过三分之二的已固定签名人权重。每类操作独立限制每块操作数、规范交易字节、签名数及相应的状态读取与写入规模。协议先检查并占用自己的预算，再做签名验证。读取的 Registry 另有记录数量上限。两类控制交易的预算不互相借用，其字节亦计入整个区块的字节上限。普通交易无法使用豁免。Bridge Mint 继续按输入与认证入账增量核算容量。候选区块重复验证使用各自重新计算的预算。

### 零发行与手续费负债

普通支付的初始定价实验以同类 CKB 交易的最低中继费基准为参照，目标约为 30%。460 字节 CKB 交易计入 4 字节区块序列化开销，在 1,000 shannons/KB 的参考费率下为 464 shannons，按 30% 向上取整为 140 shannons。这个实验目标供校准 Stem 的多维费率。复杂交易继续按自身的带宽、执行和状态资源消耗计价。CKB 主网实际费率和 Stem 长期安全预算需用运营数据另行评估。[CKB 缺省交易池配置](https://github.com/nervosnetwork/ckb/blob/119598b0a1146a569068ac80bdba7eb5f6292fd3/resource/ckb.toml#L204-L211)

可用已对账的可分配手续费收入的 10% 模拟 PoW Security Fund，用于支付真实 CKB 检查点、证明、质押及结算交易。普通支付与合约执行可共享低频锚。提款可合并触发更新，退休与处罚需要足够新的检查点，每次实际签名 epoch 交接仍需验证。按 4 小时一笔、每笔 0.01 CKB 的场景，365 天定期认证费用为 21.9 CKB。每笔 140 shannons、按 10% 划拨时，单靠该项开支的成本平衡约为 4.96 TPS。这个预算只包括定期认证，交接、提款、质押、验证者和基础设施开销需单独计入。发布器可按固定间隔或最低目标高度请求更新，并优先提交尚未发布的签名 epoch 交接。实际费用由独立 CKB 资金 Cell 支付。

```text
Stem native issuance = 0
liquid L1 reserve >= redeemable circulating capacity
                    + pending withdrawals
                    + payable fees
stake assets ∩ bridge reserve = ∅
```

在可兑付网络，认证的真实存款才能增加净 CKB 兑付能力，每一笔 L1 入账只可使用一次。储备盈余不得直接用于生成新的 Stem CKB。普通交易容量守恒，手续费和抵押都通过已存在的 Capacity 核算。质押、冻结罚没款、金库底仓以及不可及时支出的资产均排除在可兑付储备之外。首轮金库使用能按提款流程及时支出的 L1 Cells，储备不投入 Nervos DAO。Stem 不提供增发、固定质押收益或 DAO 补偿。

手续费扣减流通负债，相应的 L1 资金留在桥储备。桥接网络在 SystemRoot 下保存 Strand `BridgeBacking`，包括认证观察到的可兑付 CKB、流通 Capacity、待兑 Capacity 和已累积质押收益。铸造增加储备和流通，普通提款申请把流通转为待兑，经认证的 L1 兑付同时减少储备与待兑。调拨与金库轮换保持总量。已确认储备扣除流通、待兑及已累积收益后的余额，等于累计冲销手续费扣除累计入队的基金提款与已累积收益。`/stem/bridge-backing` 提供聚合记录与 Patricia 证明，`/stem/bridge-lane` 认证指定通道的金额、金库和 nonce。CKB 观察依赖配置的 guardian 法定人数。

桥接网络在 `fees/vault-revenue` 承诺按通道排序的累计手续费归属。Capacity 可以在 Stem 交易中合并。同一块内冲销的手续费以父状态各金库的认证 CKB 液态储备为权重。父状态总储备为零时，使用本块结算后的各通道储备，覆盖同块铸造后支付的情况。按通道 ID 顺序计算前缀权重，某通道的归属额为本块手续费乘以当前前缀权重除以总权重后向下取整，再减去前一通道的累计分额。零权重获得零，总归属严格等于冲销额。调拨会改变后续区块的储备权重。轮换沿用通道 ID、权重和归属额。收入记录和聚合费额同一原子批次提交。实际基金兑付继续依据 `FeeFundPolicy` 和待付提款证明。 [审计命令](OPERATIONS.md#verify-per-vault-revenue)

`bridge.fee_funds` 可在创世固定验证者与委托人、DevFund、Ecosystem Fund、PoW Security Fund 四个账户的 CKB 收款 Lock 和份额，正整数基点合计 10,000。65/15/10/10 仅用于实验。各账户累计分配额为 `floor(累计冲销的 shannons × 该账户基点 / 10_000)`；累计取整最多留出 3 shannons，随费用累积可进入分配额。`/stem/fee-funds` 提供份额、收款 Lock、累计入队及确认出账并附 SystemRoot 证明。每笔原始交易的确认费用可从执行回执认证。

任何中继者都可提交 Strand `FeeClaim`，包含固定收款账户 ID、申领额和预期累计入队额。启用 CKB 质押后，验证者与委托人账户使用下面的逐人申领规则。交易没有输入、输出或证书。执行只计算前一已提交状态的费用。每账户每块至多一次，仍占用块交易数和字节预算。申领额须覆盖收款 Lock 的 CKB 最低容量，同时受累计分配额、可用聚合盈余和原生 CKB 金库未预留余额约束。合法申领使用与普通提款相同的通道 nonce 和 SystemRoot 路径，把盈余转换为待兑负债。L1 金库根据直接最终性检查点付款，独立 CKB 输入支付 L1 手续费。经认证的 L1 确认推进累计已兑余额。

### PoW Security Fund 检查点支出

可在创世为 `bridge.checkpoint_fee_fund_code` 固定 CKB `checkpoint-fee-fund` 的 Data2 代码哈希，并把 PoW Security Fund 的收款人设为对应 Lock。Lock 参数依次绑定检查点代码、Type ID、Stem 网络、CKB genesis、单笔 CKB 费用上限及例行发布间隔。Stem 核对该检查点与原生 CKB 金库固定的检查点相同。普通提款和其他基金继续使用已有收款规则。

该 Lock 允许两种交易。合并至少消耗两个同 Policy 的纯 CKB 资金 Cell，全部本金合并到一个同 Lock 的输出。外部输入负担合并费用。用于检查点时，交易恰好消费一个已存在的指定检查点和一个基金 Cell，并创建等额的检查点后继及同 Lock 找零。基金容量差额等于整笔 CKB 交易的矿工费，必须为正且不超出单笔上限。普通检查点使用已认证 Stem Header 的时间差检查例行发布间隔。经过原有 Jolteon 合约验证的 epoch 交接可以在间隔以内推进。初始检查点由独立钱包建立，检查点创建和更新仍使用 CKB Type ID 与 Jolteon 最终性验证。

兑付到账后，资金由 CKB Lock 执行用途约束。Stem 的累计已兑记录认证桥资金的转移，CKB 规范链上的检查点及其输入输出认证实际矿工费。检查点费用不再动用已经核销的 Stem 储备负债。合并与找零保持基金剩余容量，最低可用本金受该 Lock 的 CKB occupied Capacity 约束。合约验证路径不需要额外 guardian 费用审批。发布器可显式选择真实基金 OutPoint 并沿同一本地日志等待确认，之后可继续使用原钱包找零。 [运行说明](OPERATIONS.md#fund-ckb-checkpoint-fees)

### 可编程安全域与付费交付

应用可以在 CKB 创建使用同一验证程序的独立安全域。首个服务为指定 Live Cell 出具认证回执。每个域由 CKB Type ID 注册，绑定 CKB genesis、Stem 网络、运营者、罚没收款人、法定权重阈值和责任等待块数。注册交易实际锁定每个位置声明的 CKB 本金，并另留足 CKB occupied Capacity。名单按 Ed25519 公钥严格排序，成员位置与独立收款地址绑定。付费服务域、Stem 共识质押 Registry 和桥储备各自消费不同的真实 CKB 资产。该域的审批只在应用请求中产生效力。

买家把读取对象的完整 CKB OutPoint、退款高度、买家 Sighash Lock 参数、回执容量和域 Type Script 哈希写入请求 Lock。实际请求 OutPoint 是唯一签名标识。绑定域、请求和结果的 Ed25519 签名用有序位图表示。CKB 交付交易把读取对象置于首个直接 Cell Dep，并要求它不在本交易的消费输入中。合约重新计算完整 CKB CellOutput 及数据哈希的域分离承诺，核对结果和仍符合资格的签署者。签署权重须达到域的基点阈值，分母是注册时承诺的全部本金。罚没后若剩余权重不足，旧域暂停交付，应用可通过新的 Type ID 重建。域使用独立的请求授权和数据校验。

交付原子消费请求，保持域本金，并把请求扣除回执容量后的服务费按有效签署权重分给实际签署者。整数余数归第一位签署者。普通付款和返还使用标准 CKB Sighash Lock，每个支付输出必须是可构造的 CKB Cell。回执 Type Script 绑定域与已消费请求，数据存储认证结果。所有者可以消费回执并释放占用容量。历史证明继续认证原回执。达到退款高度后，请求仍存活时任何中继者都可按固定买家和全额 Capacity 执行退款。并发交付与退款由 CKB 的输入顺序和唯一消费解决。

同一成员对同一域和同一请求签署不同结果，构成可客观验证的罚没证据。两份完整签名经 CKB 合约验签后，移除该成员整个位置的本金和签署资格，罚款支付至固定罚没 Lock。成功罚没保留位置标记以拒绝重放。运营者认证退役后，域停止新交付。CKB 根据最近的退役状态 Cell 开始执行相对块等待，退役期间罚没更新会延长剩余位置的等待。到期后全额返还未罚本金，底仓返还给运营者，并移除域 Cell。此配置把权重绑定在链上位置。实际受益人集中度依赖运营披露。当前罚没判据为双重报告。

Stem 应用可以用已确认 CKB 回执的历史 L1 Cell Reference，验证其类型、请求身份与结果。Stem 仍通过现有 guardian 法定人数认证 CKB 观察与锚点，应用视野中的回执来源及该回执代表的业务规则必须另行固定。 [运行与复现](OPERATIONS.md#exercise-a-paid-ckb-security-domain)

安全域审计可以固定 CKB genesis、Stem 网络、合约代码、完整域 Type Script 和目标 Cell 或关闭交易，验证从 Type ID 创建到目标的连续消费链。审计验证每笔 CKB 交易和见证的包含证明，分别统计锁定、罚没、返还本金和已发生的服务付款。签署者身份、证书权重、付款分配及回执与真实交易绑定。客户端需独立认证同一条有效 CKB 链的区块哈希，历史包含证明只描述所选状态。 [查询命令与信任边界](OPERATIONS.md#verify-security-domain-capital-and-income)

### 任期质押收益

启用 CKB 质押的网络在 SystemRoot 记录当前任期的起点、累计费用基线、Registry 身份及承诺、实际投票委员会承诺。下一任期从高度 `A` 激活时，旧任期在执行完 `A−1` 后关闭，包含该块已收取的费用。该任期预算等于验证者与委托人基金对关闭时累计费用的分配额，减去该基金对期初累计费用的分配额。没有配置基金时预算为零。

每位验证者按实际委员会票权从预算计算份额，向下取整。以该任期 Registry 中的自有质押和有效委托本金划分份额，先向下取整计算自押收益，再以剩余部分计算委托收益和佣金。佣金基点固定于验证者注册交易。扣除佣金后，每个委托头寸按已承诺的有效本金比例向下取整。验证者与委托人的金额合计受任期预算约束。验证者收款 Lock 哈希来自历史 Stake Cell 承诺，委托人收款 Lock 哈希来自历史头寸身份。

`/stem/stake-reward-epoch` 提供已关闭记录与 Patricia 路径。验证时使用认证的 Header、该任期 Registry 和委员会，委托人另提供头寸本金的成员证明。收益记录与四个基金引用同一笔收入。

中继者可提交 Strand `RewardClaim`，按任期和头寸排序，一笔最多包含 8 个任期、32 个收益头寸。每个任期附对应的 Registry、委员会和必要的头寸证明。各头寸的认证收款哈希必须等于交易指定的 CKB Lock 哈希，且 Lock 必须符合网络提款规则。所列份额累积到该 Lock 的已认证余额。`payout` 为零时须提供至少一笔未领份额，操作只累积收益，无须达到 CKB 输出最低容量，也不分配提款 nonce。`payout` 为正时须等于既有余额加本笔新增份额的全额，可以提交空头寸集合。兑付清除该 Lock 的余额并创建普通待兑记录，继续检查 CKB 最低容量与原生金库未预留余额。任期取整余量留在储备盈余。

验证者与委托人共用每块一个申领名额。份额累积时为每个 `(epoch, kind, position_id)` 记录交易 ID 和已分配金额。后续改变申领总额、重组头寸组合或结清 L1 提款后，这些记录仍会拒绝再次申领。已累积余额全额计入桥负债，并与累计入队合计受验证者基金分配额约束。兑付把该负债转为待兑，无须再次扣减盈余。`/stem/stake-reward-claim` 认证某一个头寸份额是否已领取，`/stem/stake-reward-balance` 认证指定 Lock 当前余额。实际 L1 付款通过通道 nonce 和结算证明核对。

各通道独立核算储备，缺少可用原生 CKB 时经过认证调拨。质押本金、用户的流通与待兑负债以及金库底仓均排除在收益之外。检查点矿工费可以走受约束的 PoW 基金 Lock。首个可编程安全域的服务费由独立 CKB 请求直接支付给签署者，其他 L1 服务的支出认证仍待实现。

扩展到多条质押来源链时，抵押按来源链、资产、托管条件和可执行的罚没规则独立核算。可分配收入按已确认的可用储备与待付负债核算。未来的 CKB PoW 收益桶也须从同一笔可分配收入中划拨，并由独立规则认证矿工领取资格。增加收入受益方本身不授予 Stem 投票权、储备支配权或外部抵押资格。

## 4. 资产与组合标准

Stem UDT Type Script 负责标识、发行权限、数量解码和守恒。桥接资产只接受绑定其 L1 资产身份的认证增量。若资产发行规则要求监管，UDT 必须检查每个对应输出都包含指定政策 Type Script。可选抵押、交易限制和结算规则由各自的脚本执行。链上应用数据保持字节串。需要共享数据布局时，DataMap 作为合约层标准，采用稳定命名空间、规范编码、键和总字节上限，由各合约验证自己负责的字段。CKB L1 xUDT 的扩展和发行限制仍由实际部署的 L1 合约执行。 [CKB RFC 0052](https://github.com/nervosnetwork/rfcs/blob/master/rfcs/0052-extensible-udt/0052-extensible-udt.md)

| 用例 | Type Script 组合 | 验收 |
| --- | --- | --- |
| 可互换资产 | Asset | 多输入与多输出守恒 |
| 抵押资产 | Asset、Collateral | 资产组身份不变，容量不足或移除抵押约束时拒绝 |
| 受限资产 | Asset、TransferPolicy | 政策可跨资产复用，禁止绕过强制政策 |
| 可识别对象 | Identity、Lifecycle | 身份创建依赖唯一输入及真实输出索引，生命周期只能合法推进 |
| 多资产原子结算 | 多个 Asset Groups、同一 Settlement Group | 每种资产分别守恒，同一结算脚本验证全部交割 |

代码身份与应用身份可以共用一套 Type ID 规范。身份从网络、创建输入和创建时的实际输出索引导出。Type ID 查找索引属于 RPC 和 SDK。共享状态仍通过精确 OutPoint 更新，本轮的共识模型限定为基于已签名交易的验证式执行。

## 5. L1 质押、委员会与处罚

### Registry 与历史责任

CKB L1 Registry 用 Type ID 保证唯一性。Stake Cell Type Script 绑定 Registry 和验证者 Ed25519 公钥。该 Cell 的 L1 Capacity 等于实际 occupied Capacity 加自有质押。注册需要验证者签名、资金所有者授权以及公钥尚未退休的证明。增押、退出请求、罚没和提现都原子更新 Registry 与对应 Stake Cell。

| Registry 的承诺 | 状态内容和保留规则 |
| --- | --- |
| 当前质押 | 按公钥排列的已质押和退出中记录，绑定实际 Stake Cell、自有质押、委托总额和经认证的活跃高度区间。最多保留 256 个尚未结清的公钥 |
| 待结责任 | 退休后仍有余额可支取或处罚待执行时，保留完整 Stake Cell 和 Registry 记录 |
| 退休根 | 提现时从当前记录移除公钥，并在稀疏 Merkle 根中永久承诺该公钥已退休。原记录及证据可从 CKB 交易历史审计 |

退休证明编码一个 32 字节位图和按层号由低至高排列的非零兄弟哈希。合约验证当前根下公钥不存在。提现同时将该公钥插入退休根，释放 Registry 名额。历史公钥始终保留在认证根中。曾结清的质押公钥永久退休，后续验证者注册使用新公钥。仍在责任窗口内的 Stake Cell 无法通过更换 OutPoint 或重新注册解除责任。

### 开放委托

委托人用自己的 CKB 输入授权锁定本金。每个委托 Cell 使用独立 Type ID，绑定 Registry、验证者公钥、委托人的 CKB Lock 哈希以及创建输入与输出位置。Type Script 只保存这四项的 32 字节域隔离承诺，使用零参数质押 Lock。每次转换由 Registry input witness 提供承诺的原文，CKB 同时核验完整原文、Type ID 和 Registry 归属。Cell 容量等于 146 CKB 占用容量加活跃本金与待解锁本金。40 字节数据含五个小端 u64。未生效时，序号字段记录创建 Registry 序号。生效后，同一字段记录减押请求序号，未请求时为零。该 Cell 只能随同唯一的 Registry 状态转换创建或结清，合约逐笔核验本金变化等于目标验证者的委托总额变化。每个验证者条目还记录由合约更新的稀疏默克尔根，按头寸提交有效本金。改变有效本金时，用同一条路径认证旧本金和推导新根。验证者必须已激活、满足自押门槛且未冻结或请求退出。每个 Registry 更新至多处理一个委托 Cell。

每笔委托从首次覆盖其创建序号的 Registry 应用高度起承担责任。创世也产生应用记录。CKB 核验该应用已经生效并绑定实际创建区块。实际处罚还需证明验证者在相关高度拥有投票资格。增押创建独立头寸与新的责任起点，已有本金的起点保持原值。起点之前的双签不影响这笔本金。生效后，委托人可以在验证者任内授权部分或全额减押。Registry 原子地从参与选举的委托总额中扣除这笔本金，并把它记入待解锁金额。原 Cell 保留已证明的起点，继续托管剩余本金和待解锁本金。每个 Cell 同时容纳一笔待解锁金额，结清后可继续减押。重新委托使用已返还的 CKB 资金，在另一验证者下建立独立头寸。

每次投票委员会采用更高序号的 Registry 时，SystemRoot 记录前次采用序号、首次生效的 Stem 高度和认证的 CKB 区块锚。一笔减押的排除高度取第一个覆盖该请求序号的应用记录。确认减押时，CKB 校验直接检查点、Registry 身份、Patricia 路径和生效高度，并要求该快照锚及请求 Cell 的创建区块都在 CKB 主链上。该锚点不得早于请求上链。交接前收到证明、重组后引用孤块，以及使用更新的序号延长已明确的责任截止高度都会失败。

确认交易锁定排除高度并启动 CKB 相对区块等待期。之后任何人可将待解锁本金付回原 Lock。部分减押保留未减本金及原 Cell 的占用容量。全额退出同时返还占用容量。责任覆盖生效高度到首次排除高度的左闭右开区间，合法历史证据可在等待期提交。全局冻结记录保留两份签名投票的最小和最大高度。两份投票都落在责任区间时，可直接触发待解锁本金罚没。全局冻结采用区间外证据时，仍可提交区间内的有效证据。已结束委托不承担排除高度起的违规。

未请求减押的委托也可在验证者正常退役后直接提款。中继者依赖当前退休 Stake Cell 及其 CKB Header，等待自该 Cell 上链起的 L1 解锁期，再把本金和占用容量付回原 Lock。CKB 按绝对高度 `since` 验证这条路径。冻结验证者完成退役后，仍在委托的本金可独立罚没，占用容量返还原 Lock。罚没本金体现在实付 CKB 交易费用。Registry 保留验证者记录直至活跃和待解锁委托全部结清。

### 快照签署窗口

快照承诺 Stem 网络、CKB genesis、CKB 锚点高度与哈希、Registry 身份及序号、完整 Registry 状态、前一已接受 Stem 快照及 L1 锚点、目标 epoch、guardian 代次，以及签署窗口的 Stem 锚点。 原生控制交易将目标 epoch、签署锚点 Header ID 与证书正文承诺写入原始交易。正文包含规范快照及整个 Registry。签名绑定网络和原始声明。快照正文依次编码两字节小端快照长度、规范快照、两字节小端的新认证方数量、按公钥升序排列的新认证方条目和完整 Registry。每条为 32 字节公钥与 8 字节小端权重。平常快照的新认证方数量为零。通过验证后才允许替换候选覆盖层中的已接受快照。观察者从可信 CKB 全节点验证 UTXO、质押容量、确认深度和链历史，持久化签过的 L1 锚点和最近一次签署的目标 epoch 与消息摘要。同一 epoch 只允许再次签署完全相同的消息。Stem 共识依据已提交字节与签名验证快照。独立的快照签名域要求超过三分之二已授权 guardian 权重。

令 epoch 生效高度为 `A(e) = 1 + e × blocks_per_epoch`。对非 genesis epoch，网络固定整数 `W`，满足 `3 ≤ W < blocks_per_epoch`。签署锚点为已最终确定的 `A(e)−W−1` 区块。只能从下一块开始签发与提交，到 `A(e)−2` 截止，过期证书不得转用于其他 epoch。每个 epoch 最多接受一个快照。Registry 序号不可倒退，同序号只能对应相同承诺。后续快照必须延续前一已认证 L1 锚点。深重组跨过该锚点时停签新授权，并重新核验仍受锁定的质押和桥负债。已确定的 Stem 历史继续有效。

先排除未激活、已退休、已冻结、已到退出 epoch 和未满足自押门槛的记录。对其余合格记录按自押与委托总额从高到低排列，公钥字节序打破平局，取配置规模。票权为 `(bonded_shannons + delegated_shannons) / shannons_per_power` 的整数商。自押门槛、加法溢出、非空、正权重和总权重上限全部是确定性检查。genesis 委员会通过同一认证路径直接生效。Jolteon 在 `A−2` 认证下一届名单，在 `A−1` 执行后由旧委员会提交终结证明。新委员会自高度 `A` 开始。冻结的 CometBFT 对照在 `A−2` 发出 `ValidatorUpdate`，验证新旧委员会的签名 Header。缺少新快照时，只有旧委员会对应质押仍锁在 L1 的情况下，旧委员会才能继续按原权重执行。 [CometBFT v0.38.26 ABCI 方法](https://github.com/cometbft/cometbft/blob/v0.38.26/spec/abci/abci%2B%2B_methods.md#finalizeblock)

当入选委员的原始权重总和 `R` 超过上限 `M`，先为每位委员保留 1 票。入选人数为 `N`，每位原始权重为 `p` 的委员获得 `1 + floor((p−1) × (M−N) / (R−N))`。配置要求 `M` 至少等于委员会人数上限。总权重保持在上限内，法定人数按实际分配后的总权重计算。原始总和未超过上限时直接使用整数商。

### Jolteon 委员会交接

在 `A(e)−2` 提交的执行状态中，`stake/announced` 记录目标 epoch、生效高度和规范排序的加权委员名单。旧委员会在 `A(e)−1` 停止认证新的执行块，随后对绑定该 Header 的封闭提案投票。封闭提案沿用 QC、TC 和相邻视图的两链规则。视图跳过后可以用同一边界的两份相邻封闭证书确定最终性。只有执行块写入历史 MMR。

交接证明包含边界 Header、两个旧委员会 QC 及下一届委员记录的 Patricia 路径。边界执行、最终性证明和新委员会投票日志原子落盘。新委员会使用边界 Header ID 作为起点，视图编号从 1 开始，签名域绑定新 epoch 和起点。节点通过持久化的交接证明及候选载荷追赶旧历史。

原生 CKB 检查点单元信任固定的创世身份与委员承诺。更新验证当前成员、两链或交接证明，以及从实际 CKB 输入记录到新 Header 的 MMR 扩展。交接更新同时认证后继委员。客户端需从可信的 CKB 锚点确定当前检查点。CKB 合约验证最终性证书及连续性，执行有效性仍取决于 Stem 共识。质押退休与桥兑付需绑定同一个已认证检查点，才能把长期历史安全与资本锁定对应起来。[运行验证](BENCHMARKS.md#ckb-funded-jolteon-handoffs)

### 退休的不可逆性和快照签名人轮换

1. **请求退出。** 所有者在 L1 更新 Registry 和 Stake Cell。资金继续锁定。在快照中因排名下降离开委员会的记录仍可参加后续选举。
2. **登记终止。** Stem 接受包含到期退出、冻结或已退休记录的快照，同时在 SystemRoot 写入该公钥的退出生效高度。未来快照如果试图把该公钥重新加入委员会，则整份快照被拒绝。
3. **证明实际退出。** Registry 在 L1 固定直接检查点的代码哈希和 Type ID。中继者用该检查点证明已签署的 Stem Header，并提交 SystemRoot 下的退休标记 Patricia 路径。L1 验证检查点的 Stem 网络、CKB genesis、固定的检查点身份，以及由 Registry 标识和验证者公钥推导出的记录键。Path 必须证明标记中的退出生效高度等于新 Stake Cell 的终止高度，并且该高度不晚于检查点认证的原生高度。签署窗口提前写入的标记要等到实际生效才可使用。
4. **执行 L1 退出。** 任何付费中继者都可消费 Registry 和 Stake Cell 并提交该证明。所有者才能消费退休后的 Stake Cell 提款。CKB 执行相对区块锁，提现与释放 Registry 名额、更新永久退休根原子完成。仍在责任期的记录可以提交客观双签证据。已冻结的质押先执行罚没，再走剩余容量的取回流程。

L1 Registry 轮换由旧集合和新集合共同签署并在 CKB 确认。Stem 侧，第一份携带新轮换代次的快照必须在其目标 epoch 的签署窗口内提交，并由两个集合对同一消息各提供超过三分之二权重的签名。签名索引最高位标记新集合，其余位为该集合内按公钥排序的索引。快照提交后，Stem 原子更新协议状态中的认证方，后续快照只接受新集合。提议快照也须延续最近已接受的 CKB 锚点和 Registry 序号。已经退休的公钥继续受 Stem 标记和 L1 退休根约束。桥和独立检查点使用各自的认证方流程。

### 双签、罚没和停摆

冻结的 CometBFT 对照验证 canonical vote sign bytes。两票需对应同一 chain ID、公钥、高度、round、vote type，不同的完整 Block ID 和各自有效签名。证据高度必须落入该质押代次已认证的实际活跃区间，包含退出等待期内仍可追责的历史区间。证据键按投票位置去重。罚没额与去向由网络规则指定。证据通过后，L1 冻结罚没部分。Stem 认证冻结状态后将该代次排除出候选集合，并以与退休相同的机制提交终止记录。实际移除该代次前，资金继续作为 L1 受约束资产锁定。完成移除后才执行资产结算。开发链已实测双签冻结、提前结算拒绝和退休后全额罚没。 [CometBFT v0.38.26 投票签名源码](https://github.com/cometbft/cometbft/blob/v0.38.26/types/vote.go)

Jolteon 罚没使用同一 epoch、同一视图、同一成员的两份有效签名，签名绑定各自的候选点及其执行 Header。当前 CKB 检查点的 SystemRoot 认证历史 epoch 的结束高度，尚未公告后继的 epoch 使用不存在证明。创世名单由部署参数固定，后续历史名单由 epoch 启动 Header 的 Patricia 路径与检查点的 MMR 包含证明认证。两份投票的执行高度同时受实际委员任期和 Stake Cell 责任区间约束。冻结、认证移除、罚没以及等待后领取剩余容量均由 CKB 交易执行。[双签与退休实验](BENCHMARKS.md#jolteon-withdrawals-retirement-and-slashing)

活跃权重不足以形成超过三分之二的签名时，Stem 接受停摆。L1 可接收证据并冻结资产。停摆期间的 L1 处罚不会跳过委员会交接，也不能替 Stem 选择后续区块。原网络在当前委员会恢复合法法定人数后，按 Jolteon 规则从最后已确定区块继续。若长期无法恢复，另建网络需要新的网络身份和 genesis。新网络只能使用独立储备，原金库负债先按已验证的原历史核对，同一笔资产只能承担一条历史的兑付。首轮方案没有同身份紧急换委员会的应急路径。

## 6. 桥、储备与提款

每条提款通道由 `CKB genesis + L1 vault Type ID + 资产身份` 唯一标识。原生 CKB 和每种启用的 L1 xUDT 各有独立的金库状态与 nonce。xUDT 身份承诺其完整 L1 Type Script，覆盖 hash type 和所有 args。Stem 的 Capacity 在应用层统一流通。各通道可用的 L1 CKB 容量与已经预留的容量必须分别入账。

- **入桥。** L1 存款归集、确认之后，由存款 guardians 认证存款 OutPoint、金库、金额、Stem 收款 Lock、L1 锚点和网络。Stem 只对未使用过的证书授予准确的容量与资产增量。所有实际输入、输出与相关 Type Groups 仍按原生规则处理。
- **原子申请。** 一个 Burn Cell 对应一张票据。原生 CKB 的 Burn Cell 无桥接 UDT，xUDT Burn Cell 只承载一种已注册的桥接 UDT 和它自己的 CKB Capacity。多个资产用同笔交易中的多个 Burn Cells 提出申请。多个 Type Script 仍能约束同一个 Burn Cell。每个 Burn Cell 的全部 Capacity 只计入本票据的 L1 支付金额一次。合约将原生 CKB、UDT 数量和附带的 L1 Capacity 分开验证。票据沿交易输出索引顺序在各自通道分配连续 nonce。所有票据与通道预算共同提交，实际 L1 兑付分别进行。
- **小额聚合。** 多个小额 Stem Cell 可按同一资产通道和同一 L1 收款脚本合并为一个 Burn Cell。每个输入照常签名和验证。达到 L1 输出最低 Capacity 后才可受理并分配 nonce。聚合在 Stem 交易内完成。
- **入队校验。** 验证 L1 收款模板和参数、准确输出及其 occupied Capacity、原生资产配置和足够的通道可用余额。收款 Lock 的完整 Molecule 编码最多 4,096 字节，使待付款证明始终落在合约接收上限内。拒绝会被 L1 金库解释为存款或 escrow 的收款脚本。校验失败则整笔申请拒绝。若通道需要更多 CKB Capacity，先完成已认证的 L1 储备调拨，再受理新申请。
- **L1 兑付。** 中继者在 CKB 交易中引用金库预先绑定的直接检查点 Cell，并提交待付款记录的 SystemRoot Patricia 证明。金库合约从该已验证 Header 读取 SystemRoot，核验网络、通道、当前 nonce、收款 Lock、CKB Capacity、xUDT 数量和票据标识。交易消费原金库并创建同一 Type ID 的继任金库，推进 nonce。费用由独立 CKB 输入支付。轮换保持资金可兑付及原有 nonce。一个通道出现异常时保留其队首票据及完整负债，其他通道继续处理各自的 nonce。
- **对账。** 对 CKB 及各 UDT 分别维持 `可兑付 L1 储备 ≥ Stem 流通负债 + 未付的已提交提款 + 应付手续费`。此外，每个通道预留的 CKB Capacity 和 UDT 数量必须足以兑现该通道的待付款。申请把流通负债转成待付款，在同一提交中预留通道余额。经认证的 L1 兑付同步减记储备、待付款与预留。占用的金库底仓、外部交易费和质押资金分别计量。同一笔 CKB 不得计入多个通道可兑付余额。

**认证 L1 核销和调拨。** Bridge Settlement 使用独立签名域。认证方须核验指定 L1 genesis、金库 Type ID、实际确认的交易、相关金库输入和输出、余额变化，以及本次确认锚沿已认证锚延伸。签名绑定网络、认证方代次、前后 L1 锚点和操作所有字段。它承诺完整的 Stem 提款票据标识，认证方比对 L1 收款输出、资产和金额。Stem 共识只读取已提交的签名人、L1 锚点、金库 OutPoint 及相关票据。

- **已兑付提款核销。** 声明携带通道、当前 nonce、原票据标识及金库继任 OutPoint。必须匹配队首的已确认票据与先前金库输入。一次提交消耗待付款，推进 nonce，并按票据数值减记 L1 储备和预留余额。此动作不创建 Stem Capacity。
- **储备调拨。** 声明携带源通道、目标通道、确定的 CKB Capacity，以及两个金库前后 OutPoint。两个金库在同一笔已确认的 L1 交易里被替换。源额扣减和目标额增加等量原子完成，各通道仍须覆盖原有预留，资产单位、待付款和出款 nonce 保持原值。跨通道只调拨原生 CKB Capacity。
- **顺序与重放。** 认证以候选区块内先前操作形成的锚点及金库 OutPoint 为准。重复使用已经被核销的票据、旧金库输入或已被替换的认证代次均拒绝。跟踪全局认证 L1 锚点时，新结算声明需推进该锚点。

Stem 验证者质押、桥状态认证与金库托管分别授权。L1 归集、调拨和轮换必须消费一个由当前 CKB 多签锁授权的输入。L1 兑付依据 Stem 共识认证的待付款记录。Stem 对 L1 存款和已兑付记录的确认仍依赖 guardians 对确认链历史的核验。原生检查点沿历届 Jolteon 委员会验证 Stem 状态。

### CKB Cell 只读引用

普通交易可附一份引用集合证书。集合包含一至四条引用，按 CKB OutPoint 原始字节顺序排列，禁止重复，且使用同一 CKB genesis 和观测锚点。每条引用承诺 CKB genesis、OutPoint、创建区块、观测锚点、完整原始 CKB CellOutput 和 Data。编码中两个种类分别为历史包含（0）和锚点存活（1）。引用加入交易 ID 与完整 Lock 授权的承诺。部分授权和 Lineage 命令当前不接收此附件。引用只用于读取，不具备消费、代码依赖、发行或提款能力。

桥接 guardians 分别核验创建交易和见证 Merkle 路径及确认链历史。在稳定的 CKB tip 读取原始 Live Cell，核对它的创建高度不晚于观测锚点，并验证仍存活的原始输出与包含证明一致，才可签署锚点存活记录。这是保守的全节点观测。只要它在后续 tip 仍存活，就可证明它在同一链的更早锚点存活。已经消费的输出仍可证明历史包含，但无法由这条观测路径重新认证存活。

引用集合的签名域独立于入桥、结算与质押，并承诺 Stem 网络身份、guardian 代次和完全相同的规范集合。Stem 对整个集合校验一次超过三分之二权重的认证，并要求观测锚点精确匹配父执行状态的已认证 CKB 锚点。锚点存活还受该锚点在 Stem 采用的高度及 `header_dep_age` 限制。桥接铸造、结算与独立的 `L1AnchorUpdate` 都可推进已认证锚点。独立更新要求前驱精确匹配、后继高度严格递增，并由桥接 guardians 以单独签名域认证新旧锚点和 CKB genesis。更新只改动锚点记录及其 Stem 采用高度。此操作免交易费，独立的 `bridge.anchor_limits` 限制每块操作数、字节数和签名数。新锚点自下一个区块的父状态可用。先前签发的引用和金库操作若仍引用旧锚点，应重新采集证书。Stem 验证不读取本地 CKB RPC。CKB 重组改变守护者已记住的确认锚点时，观测程序停止签发。已最终确定的 Stem 历史仍绑定原观测区块。引用对 CKB 之后的消费不作承诺。

StemVM syscall 2130 按索引读取认证引用。选择器 0 返回完整记录，选择器 1 仅对带 Type Script 的存活认证返回 Type Script 哈希和 Cell Data。每条记录的 CellOutput 至多 4 KiB，Data 至多 16 KiB。交易按实际编码计费，解析的记录计入逻辑读取及字节预算，每份集合认证签名固定计入 4,000,000 cycles，集合内每条记录分别计入读取和字节预算。脚本加载字节另按正常 VM 规则计量。重复使用某条引用作为只读事实是允许的，业务合约负责自己要求的一次性操作。

### Intercell 双向消息

一条消息由四个 32 字节标识、一个小端 `u64` nonce 及最多 4,096 字节载荷组成。标识依次为来源网络、目的网络、发送端点、接收端点。CKB 网络标识为 genesis 哈希，Stem 使用原生 network ID。CKB 端点为 Mailbox 的完整 Type Script 哈希，Stem 端点为完整 Lock Script 哈希。消息 ID 是前 136 字节的域隔离哈希。载荷由完整消息记录认证。同一网络和端点组合的同一 nonce 只能发布并消费一份载荷。目的端可乱序消费，未送达的 nonce 不阻塞后续消息。

Genesis 的 `bridge.intercell_mailbox_code` 固定受支持的 CKB 程序。Stem 普通交易可在最多四个实际消费的 Lock Group 中各声明一个 Intercell Action，`0xff00` 为发送，`0xff01` 为接收。完整 Lock 授权覆盖消息。消息记录与交易的应用输出和防重放状态原子提交。Outbox 和 Inbox 分别使用永久的 SystemRoot 记录，键绑定方向和消息 ID，值为完整消息。回执按规范 Lock Group 顺序串联每条消息的 32 字节 ID。每条消息独立计入交易编码、读取、写入及验证资源。

Stem → CKB 的接收方消费其唯一 Mailbox。Mailbox 的 Type ID、所有者 Lock、双方网络身份和 Jolteon 检查点代码哈希及 Type ID 在创建时固定。创建交易引用 CKB genesis Header。合约读取固定检查点实例。Outbox 证明可以直接使用检查点的 SystemRoot，也可以使用该检查点历史 MMR 已承诺的较早 Header。历史路径验证 Stem 网络、Header 高度和 ID、检查点 MMR 的叶数与峰值，以及该 Header 的包含路径。历史路径可以跨 epoch，CKB 上的检查点必须先按顺序验证委员会交接和历史连续性。合约在对应 SystemRoot 下验证完整 Outbox 记录的 Patricia 路径，再以稀疏默克尔树不存在证明插入已消费 ID。后继 Mailbox 保留同一类型和所有者。任何中继者都可发布检查点和准备投递证明。Mailbox 的消费由接收者的 CKB Lock 授权。

投递见证首先用三个小端 `u32` 给出消息、Outbox 路径和历史证明的长度，接着依次编码这三段，最后为防重放路径。历史段为空时使用检查点本身。历史段非空时依次放入较早的 208 字节执行 Header、检查点的 MMR 叶数和峰值，以及目标叶到所属峰值的最短兄弟路径。检查点高度必须等于 MMR 叶数，待验证 Header 高度必须严格更早。检查点更新后可继续使用原 Outbox 证明，只需为仍存活的检查点 Cell 更新历史路径。防重放路径包含 32 字节位图及位图指定的非零同级节点。路径只在对应 Mailbox 的当前防重放根下有效。递增锚点或重新签发 CKB 历史引用不会清除 Stem 一侧已消费的消息 ID。

CKB → Stem 的发送方由 Mailbox 的原有 CKB Lock 授权。合约令发送序号逐笔递增，并把完整消息写在这一笔的后继 Cell。后续状态可覆盖正文，原 CKB 交易仍可证明历史包含。每条接收消息须有恰好一条与它完整匹配的历史引用。内核核对父状态接受的锚点、CKB genesis、实际 Type Script、Mailbox 的检查点与所有者、消息正文及实际接收 Lock Group。应用可在同一笔交易内附上其他 L1 Cell References，所有引用仍遵守原有数量上限、锚点、认证和计费规则。超过三分之二 guardian 权重认证 CKB 观测，签名前按网络确认深度核验创建交易和见证的包含证明。若确认锚重组，持久化观测日志的 guardians 停签。Stem 的已最终确定历史保持原记录。此前签发的证书在其锚点仍是父状态锚点时保持有效，因此确认深度与 guardian 观测属于实际安全假设。

CKB Mailbox 的防重放根是 CKB 规范链上的状态。中继者可从指定的 Live Head 回溯同一 Type Script 的唯一历史，从交易见证恢复已消费 ID，并逐步校验根变化。CKB 重组若移除一次投递，防重放根回到替代链对应的状态。同一消息能否再次投递取决于替代链是否已消费其 ID。Stem 已最终确定的消息和历史保持原记录。

跨网络通用的消息身份和载荷编码与具体证明验证分离。当前启用 CKB 与 Stem 两种端点及证明路径。业务合约读取同笔交易的消息并决定自身状态变化，资产铸造、储备和提款沿各自协议路径验证。 [运行说明](OPERATIONS.md#exercise-intercell-messaging)

### Stem 原生 UDT 往返结算

应用 UDT 的 Type Script 以发行者的完整 Lock 哈希和 32 字节资产序列标识资产。Cell Data 是非零小端 `u128`。发行交易须消费发行者 Lock 的输入，普通交易逐组守恒。开放给 CKB 结算的资产由独立的 Stem Escrow Lock 托管。Escrow 脚本承诺资产的完整 Type Script 哈希、CKB genesis、Mailbox Type ID 和检查点。CKB 映射 UDT 的 Type Script 以该 Mailbox 的完整 Script 哈希标识资产。

- **导出。** Stem 交易执行原资产的 Type Group，将精确金额锁入 Escrow，并通过该 Lock Group 发送 Intercell 消息。Escrow 脚本要求托管原币的增加额等于外部原币的减少额，两者均等于消息金额。Nonce 取当前组首个消费 OutPoint 的域隔离哈希前八字节，按小端解释。载荷是资产 ID、收款 Lock 哈希及小端 `u128` 金额，共 80 字节。CKB 的专用 Mailbox Lock 强制铸造指定数量的映射 UDT，且全部输出归消息指定的收款 Lock。Mailbox 同时打开原生 Jolteon 检查点中的消息，永久记录已投递 ID。
- **返回。** CKB 交易由映射 UDT 持有人授权消费，Mailbox Lock 强制销毁与消息一致的数量。Stem 以 guardian 认证的、匹配父状态锚点的 Mailbox 历史输出核验返回消息。Escrow 只按已认证的金额减少持仓，并把全部释放原币支付给该消息指定的 Stem Lock。同一消息只能释放一次。部分兑回保留剩余原币，全部兑回返还至零本金状态。
- **边界。** 双方托管 Cell 的 CKB Capacity 原额保留，独立输入支付交易费。结算交易中的该 UDT Cell 必须只携带该 UDT Type Script。存在附加规则的资产需等待明确的规则映射。直接向 Escrow 转账不会自动产生导出消息。已消费的 CKB 返回事务如果在确认深度外重组，其 Stem 兑付不会自动撤销。实际安全性依赖相同检查点、独立 guardian 法定人数和 CKB 确认策略。

[运行说明](OPERATIONS.md#exercise-stem-native-udt-settlement)

### CKB Spore V2 往返结算

Spore 的跨链身份包含 CKB genesis、原 Spore 的完整 Type Script 哈希，以及固定路由的 Stem 映射 Type Script。CKB 托管路径固定发布版 Spore V2 字节码和已有的 Jolteon 检查点。同一路由重复导入使用同一完整 Stem Type Script。反向操作释放原 Spore Cell，保持原来的 32 字节 Spore ID、Type Script、完整内容、可选 Cluster ID 和 CKB Capacity。

- **导入。** CKB 转移交易在同一笔交易内将真实原件转给专用 Mailbox Lock，并发布指定 Stem 收件人的通知。CKB 原 Spore Type Script 继续执行 CoBuild 转移动作。Stem 校验真实 Mailbox 输出的 guardian 认证，在一个普通交易中消费通知并向指定 Lock 创建唯一映射。载荷依次为原 Type Script 哈希、收件人 Lock 哈希和完整 Molecule `SporeData`。
- **流通。** 映射 Type Script 保护单一对象和原始字节。实际持有人通过 Stem Lock 授权转移。映射和原件之间的排他性由原件托管、消息去重和真实历史认证共同维持。
- **返回。** 普通 Stem 交易消费映射和端点押金 Cell。用户的 Lock 验证同笔交易，端点脚本要求恰好销毁一个匹配映射并发布绑定原件和 CKB 收件人的返回消息。返回的 nonce 从实际消费的端点 OutPoint 派生。CKB 的 Mailbox 验证 Jolteon 最终性、消息路径和消费状态，托管 Lock 仅释放原件给消息指定的收件人。原 Spore Type Script 再次核验 CoBuild 转移。
- **边界。** 路径接受至多 4,032 字节的规范 Molecule 数据，内容类型固定为 `text/plain` 或 `application/octet-stream`。`cluster_id` 为空或为一个承载非零 32 字节 ID 的 Molecule `BytesOpt`。发布版 CKB Spore 在铸造时检查 Cluster 与发行权限，在转移时检查 CoBuild 和全部原始数据。Cluster 本体及铸造权限留在 CKB。Stem 映射单独携带其 Type Script，逐字节保留成员关系。另行部署的 MIME 扩展合约需要专用适配。Stem 端点和 CKB Mailbox 的容量押金保留原额。CKB → Stem 依赖 CKB 确认和 guardian 法定人数，深度重组不会自动撤销此前形成的 Stem 映射。 [发布版 Spore V2 源码](https://github.com/sporeprotocol/spore-contract/blob/0.2.2-beta.1/contracts/spore/src/entry.rs)

[运行说明](OPERATIONS.md#exercise-ckb-spore-settlement)

### Stem 原生复合对象在 CKB 结算

原生对象使用两个独立 Type Script。身份组验证唯一性、规范的不可变内容，以及每次变更都携带绑定的批准组。对象 ID 按 Stem network ID、首个消费 OutPoint 和对象输出位置派生。批准组验证交易确实消费了指定批准 Lock 的输入。对象身份承诺 Stem 和 CKB 的完整批准 Lock 哈希及 Stem 批准程序哈希。实际持有人仍由对象的 Lock 授权。应用可为两条链绑定不同的 Lock，测试用同一批准密钥分别签署两条链的交易。

Stem 导出交易将原对象按原容量转入固定 Escrow Lock，保留两个 Type Script 和全部数据。CKB Mailbox 通过原生 Jolteon 检查点验证导出，创建一个内容相同的映射 Cell。映射 Type Script 承诺 Mailbox、其 Lock 和 CKB 批准 Lock。CKB Cell 只有一个 Type Script，因此映射程序在同一个 Type Group 中核验唯一性、不变性和 CKB 批准输入。后续转移也需要持有人和独立批准方授权。

返回交易在 CKB 消费并销毁映射，由 Mailbox 原子发布返回消息。Stem 验证 guardian 认证的 CKB 历史输出后，把仍在托管的原对象释放给指定 Lock。身份组和批准组再次执行，保持两个 Type Script、内容和原容量。每一方向均沿用 Intercell 的防重放约束。同一原对象的一次导出只能在 CKB 发行一次，销毁映射后也无法重播此前的发行证明。

当前路由映射的是明确的批准规则及基础不可变内容。CKB 发行具有独立的映射身份。通用 Cluster、扩展合约和任意附加 Type Script 仍需要相应的跨链验证规则。资产发送合约拒绝把消息收款人设为对端保留的托管 Lock，以确保该消息可按接收合约的规则交割。

[运行说明](OPERATIONS.md#exercise-stem-native-object-settlement)

## 7. 状态承诺、执行回执与锚定快照

五项逻辑承诺由三种认证数据结构产生，所有共识数据经同一个 RocksDB 提交路径持久化。

| 数据结构 | 承诺及职责 | 持久化内容 |
| --- | --- | --- |
| Patricia 树 | Cell 根与系统状态根 | 当前规范记录与当前树的受影响节点，共用节点存储和缓存 |
| 有序 Merkle 树 | 本块交易根与回执根 | 持久化规范交易和回执。树节点在计算或生成证明时构建 |
| MMR | 历史 Header 根 | 当前叶数与峰值。完整历史证明索引由归档节点按需维护 |

```rust
struct StateCommitment {
    cell_root: Hash32,
    system_root: Hash32,
    history_root: Hash32,
}

struct ExecutionCommitment {
    transaction_root: Hash32,
    receipt_root: Hash32,
}
```

| 承诺 | 认证对象 |
| --- | --- |
| `cell_root` | 当前 Live Cells 及其创建上下文 |
| `system_root` | 桥、质押、委员会、费用和协议上下文 |
| `history_root` | 此前已提交的原生 Stem Header ID |
| `transaction_root` | 按区块顺序排列的完整交易，包含全部 Witness |
| `receipt_root` | 同一顺序的确定性执行回执 |

### 共享 Patricia 存储

Cell 根与系统状态根各自认证一棵当前状态树。两棵树使用同一套二进制 Patricia 算法、节点缓存及 RocksDB 节点存储。节点键带树标识。协议功能通过系统状态树中的固定命名空间寻址，无须为各功能分配单独的树或存储引擎。

状态树按从最高位到最低位的顺序处理键。分支在共同前缀后的第一个相异位分叉，其哈希绑定前缀长度、规范前缀和有序子节点。空树、分支和叶使用独立的哈希域。同一规范键值集合产生相同的根。

```text
cell_key = H("Stem state key", network_id, 0, canonical_out_point)
record   = (canonical_out_point, creation_height, creation_time_ms, creation_context_id, canonical_cell)
value    = H("Stem state record", network_id, 0, record)
leaf     = H("Stem Patricia node", network_id, 0, 0, cell_key, value)
```

每条规范记录只存一份，按命名空间及键读取。Live Cell 记录附带其规范 OutPoint，快照恢复由此重算 Cell 键。持久化叶存键及叶哈希，生成证明时按键取记录并重算。创建高度和时间用于 `Since`，创建上下文 ID 绑定该块的高度、时间、父 Header ID 和交易根。每块批量更新发生变化的键及受影响路径，覆盖被替换的当前节点并移除失效路径。历史 Header 继续承诺各高度的状态根，历史状态证明由相应锚定快照或归档重放获得。

系统状态树统一使用有域分隔的键。桥接状态存入存款防重放标记、通道储备与 nonce、待兑付款和可认证的聚合负债。质押状态存入接受的快照、当前及排定委员会、退休标记、认证方代次及任期收益预算。按通道和 nonce 查找提款，按公钥查找永久退休记录。

### 永久防重放记录

SystemRoot 保留可查询的紧凑记录。存款键绑定 `(CKB genesis, L1 deposit OutPoint)`，覆盖认证方和金库轮换。退休键绑定 `(Stake Registry ID, 验证者公钥)`。已退休公钥永久失去注册资格。键均有独立哈希域并绑定 Stem 网络。值以固定 8 字节编码记录使用或终止高度。候选覆盖层检查同时涵盖已提交记录和本块新记录。快照必须包含全部防重放记录并重建其认证根。历史正文、回执与证明索引继续按节点窗口裁剪。MMR 认证已提交 Header 的历史，快照恢复仍需要当前状态下可检索的防重放记录。

存款与退休各有独立的每块新增记录上限，存款流量不会占用退出记录的预留份额。以 32 字节哈希键和 8 字节值计，`N` 个区块的防重放逻辑数据增长上限为 `40 × N × (D + R)` 字节，`D` 和 `R` 分别是两类独立上限。这一上限排除 Patricia 节点、数据库元数据、备份和物理写放大。部署按预期寿命及实测放大安排持久空间。同一原子提交写入标记和已认证的累积计数，快照安装时对导出记录核验计数，裁剪按命名空间跳过这些键。若未来压缩这些记录，需另行证明新状态提供等价的防重放查找或拒绝条件。

### 本块交易与极简回执

```rust
struct Receipt {
    fee: u64,
    cycles: u64,
    protocol_result: Bytes,
}
```

`fee` 为实际支付的 shannons，`cycles` 为按协议计费的 VM cycles，缓存命中沿用原计费。普通交易的 `protocol_result` 为空。提款、已接受的质押快照等协议效果才产生规范的额外结果。每个提交交易恰好对应一条回执，拒绝的交易不入块。

完整交易和回执分别按区块顺序构建有域分隔的有序 Merkle 树，交易叶包含全部 Witness。构树递归时，左子树包含小于当前叶数的最大二次幂个叶，右子树包含其余叶。根承诺叶数，树形和路径共同确定位置。验证结果时，客户端校验同一原生 Header 下两份证明具有相同的叶数和交易位置。交易证明认证交易身份，回执正文无需重复编码位置和交易哈希。树节点在提交期间临时计算，用保留的交易或回执正文按需重建证明。

### 历史 Header 累积器

历史 Header 累积器使用 Merkle Mountain Range（MMR）。创世高度 0 的历史根承诺空序列。提交高度 `H` 时追加 `HeaderID(H−1)`，叶哈希绑定网络身份和对应高度。高度 `H` 的历史根承诺创世至 `H−1` 的 Header。`H("Stem history root", network_id, leaf_count, ordered_peaks)` 按从左到右编码峰值，其数量与位置由叶数确定。

普通节点持久化叶数和峰值，通过合并同高度峰值完成追加。历史包含证明认证指定高度的 Header，追加一致性证明认证后续累积器延续同一前缀。归档索引提供证明所需的历史节点。 [MMR 包含与一致性证明](https://datatracker.ietf.org/doc/draft-ietf-scitt-receipts-mmr-profile/)

### Header 与 AppHash 的计算顺序

```text
HistoryRoot(H) = Append(History(H−1), HeaderID(H−1)).root
Context(H)     = (H, time_ms, HeaderID(H−1), TxRoot(H))
ContextID(H)   = H("Stem block context", network_id, Context(H))
HeaderID(H)    = H("Stem Header", network_id, Context(H), StateCommitment(H), ExecutionCommitment(H))
AppHash(H)     = H("Stem AppHash", network_id, H, HeaderID(H))
```

执行前先确定交易根与区块上下文。执行时新建 Cell 只引用这个上下文 ID。执行完成后计算 Cell 根、系统状态根和回执根，再构造完整 Header。历史累积器本块追加的 Header 属于前一高度。三者依赖方向固定。Jolteon 的两链证明直接认证已最终确定的原生 Header。验证者亲自执行候选分支，再签署其完整执行 Header ID。验证者变更由前一届委员会签署的终结证书及名单证明认证。状态、交易及回执证明对照已认证 Header 的承诺验证。 [CometBFT v0.38.26 ABCI 方法](https://github.com/cometbft/cometbft/blob/v0.38.26/spec/abci/abci%2B%2B_methods.md#finalizeblock)

### 原子提交、锚定快照与节点形态

提案、投票和历史追赶共用执行内核。如果候选块字节、父状态及上下文完全匹配，可复用该候选块的验证结果。每个最终确认区块同步执行一次 RocksDB WriteBatch。规范 Cell 与协议记录、Patricia 变更路径、Header、历史峰值、保留的执行记录、最终性证明和投票日志一起原子可见。签名前另行持久化投票水位及其载荷。

锚定快照从同一已提交高度的稳定读取视图导出规范 Cell 与协议记录、原生 Header、BFT 证明、MMR 叶数和峰值，以及恢复必需的委员会交接和检查点材料。快照导出规范状态记录，Patricia 节点由接收方重建。交易根和回执根随认证 Header 提供，旧区块的逐块 Merkle 证明由保存的交易与回执正文重建。客户端先验证 Header 和 AppHash，再重建 Cell 根与系统状态根，按峰值核验历史根，匹配后原子安装。创建元数据、待兑付负债、峰值或委员会材料被篡改都会使恢复失败。快照导出在提交路径之外运行。

| 运行形态 | 保留与服务 |
| --- | --- |
| 普通节点，验证者默认 | 当前 Live Cells、协议状态与 Patricia 节点、MMR 叶数及峰值、近期 Header、交易和回执。按保留窗口自动裁剪，持续保留未结提款、委员会交接与继续共识所需材料 |
| 归档节点 | 普通节点的全部能力，加完整 Header、交易和回执历史、锚定快照服务与可选历史证明索引。从快照起步时，使用已认证的归档历史补齐早期正文 |

历史证明索引从已提交归档记录异步构建或重建。归档索引、钱包扫描和快照导出独立于区块提交。普通节点在同一提交中按窗口裁剪旧 Header 索引、执行正文、候选载荷和逐高度最终性证明。签名日志保留当前 epoch 锚，交接证明与已发现的双签证据独立保留。交易只引用共识配置允许的近期 Header Deps，两种节点按同一规则验证。客户端通过归档索引查询更早的 Header 和历史包含证明。

钱包与 RPC 使用 Stem 原生交易格式。本地钱包可按完整 Lock 或 Type Script Hash 查询 Live Cells，原生查询按 OutPoint 提供 Cell 证明，并可查询指定交易位置的执行结果。提款和委员会的证明由相应工具验证。交易意图由 Lock 和 OTX 标准表达，内核继续验证精确 OutPoint、已签名输入和实际输出。

### 空块与直接最终性

空块依然追加父 Header，验证者按配置节奏提案。无交易时使用规范的空交易和空回执根。相邻视图的认证父子块最终确定父块及其祖先，终结 seal 可认证 epoch 边界。停摆期间最后一个已投票的候选可能等待合法后继证书。已最终确定的 Header 继续可证明。

### CKB PoW 检查点锚定

部署独立的 CKB Checkpoint Type ID Cell。检查点更新消费上一 Checkpoint Cell，并验证唯一继任者、严格递增的 Stem 高度、有效的两链或交接证明和 MMR 扩展。输出保留固定的网络绑定、当前 epoch 锚、委员会承诺和完整执行 Header。普通更新可跨越同一 epoch 多个区块，交接更新推进委员会承诺。发布器从创世种子创建检查点，按已认证的 Stem 时间间隔或目标高度推进，并按顺序提交必需的交接。发布前同步保存已签 CKB 交易，确认深度达到配置值后原子更新本地锚点和委员会。已确认锚点被重组时停止发布。 新发布器可从固定种子开始，已有发布器可从日志固定的 OutPoint 开始，按消费顺序接纳已确认的后继。每步核验真实输入、输出、CKB 见证位置、原生最终性和委员会交接。只有整段通过并达到确认深度后才更新日志。落败的竞争交易随确认后继一起从待提交日志清除，后续发布继续使用本机独立的手续费资金。

创世配置包含每条通道的完整金库 Type Script 和质押 Registry 的完整 Type Script。Registry 脚本哈希必须等于创世认证签名覆盖的 Registry ID。创世校验要求所有金库与质押 Registry 绑定同一检查点代码哈希及 Type ID，同时核对 CKB genesis 和 Stem 网络。Registry 同时绑定 Stem chain ID、最低自押和任期长度。这样，已解锁的旧委员会无法利用另一条落后检查点提交交接之前的提款。验证还依赖部署者固定正确的已审查 L1 程序，以及客户端和守护者核对已确认的 CKB 实例。

锚定认证方独立于金库。它核验从上一锚到新锚的 Stem 最终性、对应 AppHash、新旧历史根的追加一致性、旧锚的 Header 在新根中的包含性及期间的每次委员会交接后，按专门签名域对记录签名。CKB 合约验证当前固定锚定认证方超过三分之二签名与继任顺序。轮换由旧、新集合分别认证。该首轮合约由认证方证明 Stem 历史，签名责任之外的证明仍需客户端独立复核。

客户端从配置固定的 Checkpoint Type ID 与信任期内的 Stem 检查点起步。先用完整 CKB 节点或已验证的 CKB 轻客户端确认锚定交易包含在满足确认策略的有效 PoW 链上，再校验 Type ID 继任路径、Stem AppHash、新旧历史根的追加一致性、上一已接受锚的 Header 在新历史根中的包含性以及跨 epoch 的完整交接证明。证明缺失、历史与已接受锚冲突或超出信任期时，拒绝自动跟随。已确认的 CKB 记录给客户端一个可追溯的外部历史约束。Stem 最终性仍由 BFT 提交确定。历史包含证明和执行回执分别认证 Header 和该 Header 下的执行结果。CKB 深重组时冻结该锚定源的后续接受，保留先前已验证的 Stem 最终性判断。发布延迟只影响新外部锚点的可用时刻，Stem 仍按正常 BFT 流程提交区块。首轮委员会按已认证质押确定。 [CKB 轻客户端参考实现](https://github.com/nervosnetwork/ckb-light-client)

## 8. 接入顺序与交付验收

| 优先级 | 接入 | 实际验收 |
| --- | --- | --- |
| P0 | Cell、VM、时间、签名和共识切换 | 四副本使用同一原生交易格式。删除 CKB Header 旧字段与 DAO 容量例外，验证创世、同块消费、过期高度、时间跳跃、重启和跨 epoch 的边界。钱包与 RPC 支持该格式。串行重放和并行验证结果一致，拒绝冲突及恶意提案 |
| P0 | 质押终止、快照窗口和罚没 | 真实 L1 注册、增押、退出及双签。拒绝抢跑退休、先签后提交的旧快照、过期签名与退役认证方。在丧失法定人数的实网确认 L1 可冻结而 Stem 不发生强制换组 |
| P0 | 状态与执行承诺、桥接守恒 | 真实 CKB 与 xUDT 往返、储备对账、金库轮换。多资产原子入队。小额聚合后达到 L1 最低容量才入队。某通道暂停时其他 nonce 正常推进，拒绝重复使用 Capacity。验证空块两链最终性、历史包含及追加一致性、回执位置和锚定快照，拒绝被篡改的创建元数据与负债，并跨裁剪与恢复重测存款和退休代次重放 |
| P1 | 裁剪、归档证明与 Capacity 定价 | 在不同 Live Cell 总量下固定每块变更量，分别测量状态树、回执树、历史累积器、读写放大与提交延迟。验证裁剪后的 Header Dep、证明重建、重启和状态同步。用结果确定开发网抵押成本 |
| P1 | CKB Checkpoint Cell | 真实 L1 发布和轮换。验证客户端接受合法锚和交接证明，拒绝冲突、缺漏历史与失效分叉 |
| P2 | 多维费率及净盈余分配 | 对齐确定性计量、费用上限、应付费收与真实 L1 出账，验证分配后的储备覆盖 |

### 提交成本不变量

固定每块交易数、交易字节、已计费资源和 Live Cell 及协议状态变更量时，Stem 的提交成本不得随历史总长度线性增长。Patricia 成本可以随当前状态规模和实际变更路径增长。MMR 追加只处理峰值合并。提交只访问本块解析的键、更新路径和由键查出的协议记录。禁止在提交时扫描 Live Cells 全集、历史 Header、退休记录或提款记录。每块只有一次应用数据库的原子同步提交，历史证明索引、RPC 索引、快照导出和 L1 发布不阻塞最终性。

验收分别改变**当前状态规模**和**累计历史长度**。使用等规模、各自有效签名的交易负载，匹配状态变更量和缓存条件，测量 Patricia 触及节点、MMR 合并次数、逻辑读写、同步提交次数、提交延迟及全副本确认延迟。对照普通与归档节点、索引开启与关闭。快照全量重建与归档重放单独报告。

压测分别运行 CKB 支付、单 Type Script、双 Type Script、多 Type Script、跨资产结算及委员会变更。按能力逐步提高投递速率至 1,000、2,000、3,000、5,000 TPS，记录 offered、admitted、finalized 和全副本可见速率、p50/p95/p99、客户端丢弃、CPU、RSS、物理写入、fsync、网络流量及积压。历史账本单独对照。

Polytype 的边际成本采用控制实验。固定输入数量和数据、输出数量、Lock 分组、签名算法与密钥、依赖读取、数据总尺寸和 VM 配置，改变独立 Type Groups 的数量。每个变体按真实交易内容重新签名。等长夹具能做到时使用等长编码，其他差异应记录。分别记录 Lock 与 Type 的 cycles、缓存冷暖、上下文载入、状态根更新及完整四节点吞吐，保留原始数据、命令和构建指纹。

### CKB 直接最终性检查点

`direct-checkpoint` 使用独立的 CKB Type ID 状态 Cell。部署时固定 Stem 网络身份、CometBFT chain ID、创世委员会哈希、创世 AppHash 和目标 CKB 创世标识。首批证明从 CometBFT 高度 1 验证。后续每次消费当前检查点，提交最多八个连续的 CometBFT Header、区块 Precommit、实际验证者集合和对应的 Stem Header。

合约重新计算 CometBFT 的 SHA-256 Header 与委员会根，并核验超过三分之二权重的规范 Ed25519 Precommit。每个 Header 的当前委员会必须等于前一个已认证的 `NextValidatorsHash`。改变权重和成员时使用同一条验证规则。合约检查完整父 Block ID 以及原生父 Header ID，并用 CometBFT 高度 H+1 的 AppHash 验证原生高度 H 的完整 Header。这个 Header 包含执行与状态承诺。签名人、状态承诺与历史连续性均由合约直接检查。中继者只需支付 CKB 交易手续费。

安全依赖固定的创世信任参数、Stem 各历史委员会的安全阈值以及用户选择的 CKB 锚点。检查点验证提交证书和应用承诺。执行有效性由 Stem 共识负责。冻结对照合约逐高度验证。长期断更时，需要按信任假设重新获得可信 CKB 锚点。桥金库兑付和质押退休共用创世固定的直接检查点。独立权威检查点用于额外的历史锚定。

## 9. 当前实现与实测证据

| 项目 | 已验证 | 尚待集成 |
| --- | --- | --- |
| 原生执行 | 默认 `stem run` 使用 Strand 四字段 Cell、组级消息及 Witness、完整输入签名、已提交父时间和并行 RISC-V Script Groups。原生锁使用固定计费的 secp256k1 恢复。四节点拒绝移除输入侧 Transfer Policy 的交易，串行与并行回执及状态一致 | 执行与存储更高负载下的剖析 |
| CKB L1 质押 | 实际部署登记 Type ID、锁定并增押；未激活、已退休的记录无法进委员会；双签冻结 250 CKB、阻止提前罚没、退休后罚没；另有正常解除质押路径；替换已观察的 CKB 分支后守护者实际停止认证 | 生产环境的参数标定和运行部署 |
| 快照与委员会 | 实际 CKB 质押快照提供 genesis 和 H+2 更新；实测高度 11 加入新验证者，高度 21 移除，核验两次交接及后继签名，guardian 轮换后由新集合参与签发快照，CKB 直接验证退休并退还 250 CKB。实际增押在高度 31 将在任验证者权重从 400 提高到 450 | 高变更率下的长周期轮换验证 |
| 控制交易与桥 | 原生状态持久化存款防重放和通道储备；实际 CKB 与 1,200 xUDT 双向桥接，无许可兑付使用直接检查点和待付款状态证明，按独立通道核销；CKB devnet 实际调拨 500,000 shannons 并轮换金库，分支替换后桥守护者停止签发 | 连续运行中的长期储备对账 |
| 状态承诺与恢复 | RocksDB 同步提交 Patricia、MMR 和执行树；四节点重启追赶、第五节点实际 CometBFT 快照恢复；归档索引生成历史证明，CKB devnet 锚定高度 11 和 24 并完成认证方轮换 | 独立生产发布与锚定源的重组演练 |
| 性能 | 同机四节点以 120 TPS 投递并确认全部 1,200 笔支付，全副本确认速率 103.67 TPS，可见延迟 p99 为 2.06 秒。3 亿 block cycles 下，更快的短时投递对应约 116–119 的全副本确认 TPS | 更长时间运行、大状态与压缩尾延迟 |

本节保留 CometBFT v0.38.26 和本地 CKB devnet 的对照记录。Stem 的 L1 状态认证依赖各自配置的 guardian 法定人数与受信 CKB 完整节点。详见 [操作与复现](OPERATIONS.md) 和 [测量](BENCHMARKS.md)。

同一普通支付夹具使用内联 Strand 时为 312 B。三 Type Script 组合 Cell 的输出向量为 285 B，CKB 三个单独输出及 Data 向量为 590 B。标准锁的恢复调用计费 1,900,000 cycles，完整交易另计其余执行开销。早期 Molecule 编码、k256 锁与对应吞吐数据保存在[历史实验归档](HISTORICAL-BENCHMARKS.md)。[当前性能记录](BENCHMARKS.md)

已在 CKB devnet 对照 CKB 与 xUDT 的入账和兑付，核对聚合储备、流通负债及待兑余额。Patricia 批量编辑保持状态根、证明和单次原子提交语义。Jolteon 实际网络覆盖 4 至 128 副本，并在 CKB devnet 核验跨 epoch 交接、观察节点恢复与原生检查点发布。当前默认入口为加权 Jolteon，CometBFT 保留为冻结的实验对照。默认节点已在 CKB devnet 验证无许可兑付、退休、解押和双签罚没。完整研发状态及本轮新增方向见[研发记录](STEM-EVOLUTION.md)。

CKB 历史输出验证器从可信区块哈希重算 Header、交易及见证根，并按 OutPoint 提取原始输出。Rust 客户端与离线 CLI 可独立验证历史输出。StemVM 可读取 guardians 认证的历史输出或指定锚点存活记录。当前 CKB tip 的实时状态由独立查询处理。
