# Stem 协议方案

## 1. 网络定位

Stem 是一条以 CKB 计价、由加权 Jolteon 最终确认的 Cell 网络。Stem 使用唯一的原生 Polytype Cell 模型与交易格式。一个 Cell 可同时绑定多个 Type Scripts，内核按完整 Script 身份派生并执行所有验证组。CKB L1 的真实质押决定验证者权重，独立桥金库负责 CKB 和 xUDT 的兑付。Stem 不发行独立原生代币。可兑付 CKB 的唯一增量来自已经确认并可兑付的 L1 存款。

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

CoBuild 保留 IDL、钱包展示、Action 构造与多方协调。每个合约独立核对 Action 与实际 Cell 差分。Stem 的配对 OTX 使用普通交易和专用 RISC-V Lock。签名绑定网络、完整 Lock 身份、输入 OutPoint 与 Cell、交付 Cell 的完整 Strand 哈希、实际解析的有序依赖、有效高度及费用上限。Solver 可以移动一对输入与输出，脚本总是用已解析输入的下标匹配同下标输出。一项部分授权只消费一个 Cell，同一输出不能满足两个独立配对。当前配对模式要求零 Since、相同有序依赖及空 Group Messages，并排除 Keeper 控制。持有人可以使用完整交易签名取消。 [CKB Open Transaction 场景](https://github.com/nervosnetwork/ckb/discussions/3670)

可重组代付使用交易位置 0 的一个纯 Capacity 输入及同锁找零。授权额外绑定位置 1 的指定 OutPoint 和完整交付 Cell。代付扣款必须为正、低于各自授权上限且不超过整笔交易实际消耗的手续费。已认证的 Lock 和 Type Groups 仍独立执行。手续费和多资产交付在同一次普通交易提交内生效。

### Lineage Cell

已实现的注册表使用普通四字段 Cell 和网络固定的 Lineage Type Script。其 32 字节参数是稳定的 `LineageId`，由网络、注册交易首个消费的普通 OutPoint 和输出下标推导。注册和 Executor 更换用该 Type Group 的一个显式 Action 声明固定的 RISC-V 程序内容哈希。实际代码必须存在于解析后的依赖。Lineage 元数据在同一个 SystemRoot 下承诺，Live Head、版本和 Executor 与实际 UTXO 同步更新。Cell Data 和其他 Type Scripts 可由应用独立使用。

注册版本为零。后继消费唯一当前 Head，版本加一，并运行旧 Lock 与输入输出派生的全部 Type Groups。更换 Executor 或控制 Lock 经过当前 Head 的授权。显式退休消费 Head 并移除活动记录，注册锚点已经消费，原身份无法重新生成。`/stem/lineage` 在同一可信 Header 下提供 SystemRoot 路径与 CellRoot 路径。普通网络可省略 `lineage_type` 配置。

Lineage 命令是原生 Strand 交易的可选字段。命令包含 1 至 16 个严格按 LineageId 排序且互异的目标，每个目标有闭区间版本约束和最低后继 Capacity。交易签名还覆盖网络、固定 Executor 的 Data 哈希和直接 Cell Dep、完整依赖引用、唯一付款 OutPoint 及其 Cell 内容、排他的过期高度、应用载荷、确定费用和两项 cycles 上限。应用载荷最多 4 KiB。网络和签名域防止跨网重放，精确付款 Cell 防止同网重复提交。付款 Cell 只持有 CKB Capacity，扣除授权费用后的余额返回原 Lock。

节点在交易的规范区块位置查找当前 Head。只有注册版本和 Executor 满足签名约束才开始执行。付款 Lock 先验证授权，Executor 在有界的 RISC-V V2 VM 中读取已声明目标、应用载荷、候选高度、上一提交区块时间和付款 Lock 哈希。Executor 为每个目标返回至多 16 KiB Data 及新 Capacity。内核保留原 Head 的 Lock 和完整 Type Script 集合，检查后继最低 Capacity、目标间总 Capacity 守恒以及存储抵押。生成的完整交易仍须通过原有 Lock 和所有输入、输出 Type Groups。权限变更和退休保持普通精确 OutPoint 授权。

成功时原子消费付款 Cell 与所有 Head，在原命令交易 ID 下创建后继。输出 0 是付款余额，其余按目标顺序。Executor 拒绝、耗尽预算、给出不合规后继或后续 Script Group 拒绝时，已通过付款授权且覆盖预算的命令可以提交费用结果，全部 Head 保持原样。未认证、过期或签名匹配条件失效的命令没有费用结果。回执的协议结果单字节为 1（已应用）或 0（已拒绝业务）。已执行尝试的逻辑读取和 VM cycles 一并计费，失败阶段按签名上限计费。

同一 Head 的访问冲突按区块顺序执行。不同命令只有在当前 Head、费用输入、预期输出及依赖访问互不冲突且保留完整预算时才并行解析和运行 Executor，最终检查和提交按原交易顺序。使用 Dep Group 的命令按串行路径执行。

#### 独立业务授权与代付

Lineage Intent 允许最多八位业务参与者先签署同一组操作，再由代付方选择手续费 Cell。参与者 ID 由域分离的压缩 secp256k1 公钥哈希生成，按字节严格排序并去重。业务签名覆盖网络、参与者集合、费用上限、执行代码和有序依赖、全部目标和版本约束、载荷、执行上限、过期高度与可选预约高度。签名摘要只将实际费用和费用 OutPoint 规范化。所有业务签名按参与者顺序进入完整 Witness，随后费用 Lock 签署最终交易 ID、费用 Cell 内容及完整业务签名。

每项 Intent 至少将一个目标锁定在精确版本。原子更新成功时该版本前进，同一组业务签名即使更换付费 Cell 也无法再次更新。业务失败时保留原有目标版本，已签名并消费的费 Cell 仍只支付一次。换用另一枚费用 Cell 再尝试需要新的代付方签名，且仍受原业务签名的高度、版本和金额约束。节点验证全部业务签名后才运行 Executor。每份签名计入固定恢复 cycles，缓存命中计费相同。

Executor 通过 syscall 2126 的 source 2 按索引读取已验证参与者 ID。应用必须把业务参与者与实际输入状态中的权限绑定。在付费方可替换的 Intent 中，Executor 读取的付款身份字段为零，避免业务结果取决于参与者签名之后选定的代付方。业务后继经过原有所有 Lock 与 Type Groups。双边结算示例用两份业务签名匹配两枚储备的所有者，用两个 UDT Type Groups 分别验证两种资产的守恒。

### Native Keeper

Genesis 可配置 `keeper` 的预约窗口 `max_delay`、单高度上限 `per_height` 和正数预约费 `reservation_fee`。Keeper 使用原有 Lineage 执行路径。先签一条包含确定执行高度 `execute_at` 的 Lineage 命令，再由普通 Cell 交易提交封存命令和预约费。命令字节上限为 8 KiB。两笔签名分别授权执行费和整个预约交易，付款 Cell 与预约交易输入分开。预约时验证实际付款 Lock 和签名，若携带业务参与者也逐一验签，付款 OutPoint 在所有待执行高度间只能占用一个预约。配对 OTX 签名无法授权新增预约或取消。

待处理队列按执行高度写入 SystemRoot，队内按付款 OutPoint 的规范字节排序。每块先读取当前高度队列，依次解析 Head 并执行一次，再处理用户提交的交易。带 `execute_at` 的命令只能由已提交队列触发。相同高度的写入共享同一串行覆盖层。预约、执行与结束均随同一次 RocksDB 提交持久化。执行列表和回执按相同顺序纳入 Header 承诺并可通过 `/stem/execution` 证明。Jolteon 提案正文携带纳入块的用户交易，预约触发由所有节点确定性执行。

每个预约单独拥有由区块上限计算出的字节、读取、写入和 cycles 份额。提交预约时另付正常交易资源费用。执行成功或遇到已授权的业务失败时，按 Lineage 规则扣除已签名执行费。付款 Cell 已被消费、Head 版本失效或份额不足时，到期结果为 2（跳过），只结束预约，不产生 Cell 或新增手续费。跳过回执的 cycles 记录保留份额，供区块资源计账。一次性尝试结束后删除队列和占用记录。直接花费付款 Cell 会使后续尝试跳过。到期之前，也可用经正常 Lock 授权并消费该付款 Cell 的取消交易立即腾出预约名额。此规则使用共识高度，网络停摆期间预约高度不会推进。

## 3. 确定性执行与资源

对交易按区块顺序解析实际 UTXO。Lock Groups 来自全部输入的 Lock Script，Type Groups 来自输入与输出的 Type Scripts 并集。按完整 Script 身份分组，Lock 和 Type 角色分离。每组只执行一次，拥有自己的输入和输出索引集合，并读取同一份不可变交易。输入侧规则可以拒绝自身被删除、替换或迁移。

脚本寻址保留 `code_hash/hash_type/args`。原生编码收敛为 `Data` 内容哈希和 `Type` 完整代码 Type Script 哈希。Type 寻址只承认 genesis 固定的代码身份规则，例如一个 Type ID 约束下的唯一代码 Cell。交易必须明确列出实际代码 Cell Dep，升级后的代码对应新的 OutPoint。Dep Group 按其规范 OutPoint 向量单层展开，重复依赖、与消费输入重叠的依赖和超限展开直接拒绝。L1 Script 的原始哈希类型继续用于计算 CKB/xUDT 的 L1 身份。执行配置由网络固定为 CKB-VM V2 RISC-V 配置，复用 Assembly、分配复用及 Spawn/Exec。

原生 ABI 提供完整 Cell、完整 Type Script 集合、按索引及按哈希查询、当前组的角色和索引、组级消息及 Witness。保留 `load_script`、`load_script_hash`、全局及组相对的 Cell 读取语义。多个 Type Script 不扩大 Cell 写冲突域。确定性覆盖层记录消费、创建及依赖读取。可并行验证独立交易和各 Script Group。后续交易对同块输出、依赖和已消费 Cell 的可见性以串行重放定义。

缓存键覆盖网络、固定 VM 配置、完整 Witness、解析后的 Cells、代码和可见 Header。缓存命中按此前确认的 cycles 计入协议预算。区块同时限制编码字节、交易数、VM cycles、依赖读取字节和写入量。准备提案可以减少重复工作，验证提案与重放都以相同规则计算预算和结果。

标准锁在 RISC-V 内校验恢复的压缩公钥哈希，通过 syscall 2121 进行 secp256k1 恢复。恢复接受低 S 的 65 字节签名，计算结果与原 k256 锁进行差分验证。每次调用在读取 VM 内存前计入 1,900,000 cycles。节点可以本地复用相同完整签名和摘要对应的成功恢复结果。命中缓存时计费相同，Lock Script 仍验证公钥对应的授权。CKB L1 合约维持 CKB 原生 ABI。

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

可用已对账的可分配手续费收入的 10% 模拟 PoW Security Fund，用于支付真实 CKB 检查点、证明、质押及结算交易。普通支付与合约执行可共享低频锚。提款可合并触发更新，退休与处罚需要足够新的检查点，每次实际签名 epoch 交接仍需验证。按 4 小时一笔、每笔 0.01 CKB 的场景，365 天定期认证费用为 21.9 CKB。每笔 140 shannons、按 10% 划拨时，单靠该项开支的成本平衡约为 4.96 TPS。这个预算只包括定期认证，交接、提款、质押、验证者和基础设施开销需单独计入。自动发布仍待实现。

```text
Stem native issuance = 0
liquid L1 reserve >= redeemable circulating capacity
                    + pending withdrawals
                    + payable fees
stake assets ∩ bridge reserve = ∅
```

在可兑付网络，认证的真实存款才能增加净 CKB 兑付能力，每一笔 L1 入账只可使用一次。储备盈余不得直接用于生成新的 Stem CKB。普通交易容量守恒，手续费和抵押都通过已存在的 Capacity 核算。质押、冻结罚没款、金库底仓以及不可及时支出的资产均排除在可兑付储备之外。首轮金库使用能按提款流程及时支出的 L1 Cells，储备不投入 Nervos DAO。Stem 不提供增发、固定质押收益或 DAO 补偿。

手续费扣减流通负债，相应的 L1 资金留在桥储备。桥接网络在 SystemRoot 下保存 Strand `BridgeBacking`，包括认证观察到的可兑付 CKB、流通 Capacity、待兑 Capacity 和已累积质押收益。铸造增加储备和流通，普通提款申请把流通转为待兑，经认证的 L1 兑付同时减少储备与待兑。调拨与金库轮换保持总量。已确认储备扣除流通、待兑及已累积收益后的余额，等于累计冲销手续费扣除累计入队的基金提款与已累积收益。`/stem/bridge-backing` 提供聚合记录与 Patricia 证明，`/stem/bridge-lane` 认证指定通道的金额、金库和 nonce。CKB 观察依赖配置的 guardian 法定人数。

`bridge.fee_funds` 可在创世固定验证者与委托人、DevFund、Ecosystem Fund、PoW Security Fund 四个账户的 CKB 收款 Lock 和份额，正整数基点合计 10,000。65/15/10/10 仅用于实验。各账户累计分配额为 `floor(累计冲销的 shannons × 该账户基点 / 10_000)`；累计取整最多留出 3 shannons，随费用累积可进入分配额。`/stem/fee-funds` 提供份额、收款 Lock、累计入队及确认出账并附 SystemRoot 证明。每笔原始交易的确认费用可从执行回执认证。

任何中继者都可提交 Strand `FeeClaim`，包含固定收款账户 ID、申领额和预期累计入队额。启用 CKB 质押后，验证者与委托人账户使用下面的逐人申领规则。交易没有输入、输出或证书。执行只计算前一已提交状态的费用。每账户每块至多一次，仍占用块交易数和字节预算。申领额须覆盖收款 Lock 的 CKB 最低容量，同时受累计分配额、可用聚合盈余和原生 CKB 金库未预留余额约束。合法申领使用与普通提款相同的通道 nonce 和 SystemRoot 路径，把盈余转换为待兑负债。L1 金库根据直接最终性检查点付款，独立 CKB 输入支付 L1 手续费。经认证的 L1 确认推进累计已兑余额。

### 任期质押收益

启用 CKB 质押的网络在 SystemRoot 记录当前任期的起点、累计费用基线、Registry 身份及承诺、实际投票委员会承诺。下一任期从高度 `A` 激活时，旧任期在执行完 `A−1` 后关闭，包含该块已收取的费用。该任期预算等于验证者与委托人基金对关闭时累计费用的分配额，减去该基金对期初累计费用的分配额。没有配置基金时预算为零。

每位验证者按实际委员会票权从预算计算份额，向下取整。以该任期 Registry 中的自有质押和有效委托本金划分份额，先向下取整计算自押收益，再以剩余部分计算委托收益和佣金。佣金基点固定于验证者注册交易。扣除佣金后，每个委托头寸按已承诺的有效本金比例向下取整。验证者与委托人的金额合计受任期预算约束。验证者收款 Lock 哈希来自历史 Stake Cell 承诺，委托人收款 Lock 哈希来自历史头寸身份。

`/stem/stake-reward-epoch` 提供已关闭记录与 Patricia 路径。验证时使用认证的 Header、该任期 Registry 和委员会，委托人另提供头寸本金的成员证明。收益记录与四个基金引用同一笔收入。

中继者可提交 Strand `RewardClaim`，按任期和头寸排序，一笔最多包含 8 个任期、32 个收益头寸。每个任期附对应的 Registry、委员会和必要的头寸证明。各头寸的认证收款哈希必须等于交易指定的 CKB Lock 哈希，且 Lock 必须符合网络提款规则。所列份额累积到该 Lock 的已认证余额。`payout` 为零时须提供至少一笔未领份额，操作只累积收益，无须达到 CKB 输出最低容量，也不分配提款 nonce。`payout` 为正时须等于既有余额加本笔新增份额的全额，可以提交空头寸集合。兑付清除该 Lock 的余额并创建普通待兑记录，继续检查 CKB 最低容量与原生金库未预留余额。任期取整余量留在储备盈余。

验证者与委托人共用每块一个申领名额。份额累积时为每个 `(epoch, kind, position_id)` 记录交易 ID 和已分配金额。后续改变申领总额、重组头寸组合或结清 L1 提款后，这些记录仍会拒绝再次申领。已累积余额全额计入桥负债，并与累计入队合计受验证者基金分配额约束。兑付把该负债转为待兑，无须再次扣减盈余。`/stem/stake-reward-claim` 认证某一个头寸份额是否已领取，`/stem/stake-reward-balance` 认证指定 Lock 当前余额。实际 L1 付款通过通道 nonce 和结算证明核对。

各通道独立核算储备，缺少可用原生 CKB 时经过认证调拨。质押本金、用户的流通与待兑负债以及金库底仓均排除在收益之外。安全域收入和 PoW 基金运营支出认证仍待实现。

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

普通交易可附最多四个按 CKB OutPoint 原始字节顺序排列的引用，禁止重复。每条引用承诺 CKB genesis、OutPoint、创建区块、观测锚点、完整原始 CKB CellOutput 和 Data。编码中两个种类分别为历史包含（0）和锚点存活（1）。引用加入交易 ID 与完整 Lock 授权的承诺。部分授权和 Lineage 命令当前不接收此附件。引用只用于读取，不具备消费、代码依赖、发行或提款能力。

桥接 guardians 分别核验创建交易和见证 Merkle 路径及确认链历史。在稳定的 CKB tip 读取原始 Live Cell，核对它的创建高度不晚于观测锚点，并验证仍存活的原始输出与包含证明一致，才可签署锚点存活记录。这是保守的全节点观测。只要它在后续 tip 仍存活，就可证明它在同一链的更早锚点存活。已经消费的输出仍可证明历史包含，但无法由这条观测路径重新认证存活。

签名域独立于入桥、结算与质押。Stem 校验配置 guardians 超过三分之二权重的认证，并要求观测锚点精确匹配父执行状态的已认证 CKB 锚点。锚点存活还受该锚点在 Stem 采用的高度及 `header_dep_age` 限制。桥接铸造、结算与独立的 `L1AnchorUpdate` 都可推进已认证锚点。独立更新要求前驱精确匹配、后继高度严格递增，并由桥接 guardians 以单独签名域认证新旧锚点和 CKB genesis。更新只改动锚点记录及其 Stem 采用高度。此操作免交易费，独立的 `bridge.anchor_limits` 限制每块操作数、字节数和签名数。新锚点自下一个区块的父状态可用。先前签发的引用和金库操作若仍引用旧锚点，应重新采集证书。Stem 验证不读取本地 CKB RPC。CKB 重组改变守护者已记住的确认锚点时，观测程序停止签发。已最终确定的 Stem 历史仍绑定原观测区块。引用对 CKB 之后的消费不作承诺。

StemVM syscall 2130 按索引返回认证记录。每条记录的 CellOutput 至多 4 KiB，Data 至多 16 KiB。交易按实际编码计费，解析的记录计入逻辑读取及字节预算，每份认证签名固定计入 4,000,000 cycles。脚本加载字节另按正常 VM 规则计量。重复使用某条引用作为只读事实是允许的，业务合约负责自己要求的一次性操作。

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

部署独立的 CKB Checkpoint Type ID Cell。检查点更新消费上一 Checkpoint Cell，并验证唯一继任者、严格递增的 Stem 高度、有效的两链或交接证明和 MMR 扩展。输出保留固定的网络绑定、当前 epoch 锚、委员会承诺和完整执行 Header。普通更新可跨越同一 epoch 多个区块，交接更新推进委员会承诺。自动发布策略仍待部署。

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

同一普通支付夹具使用内联 Strand 时为 312 B。三 Type Script 组合 Cell 的输出向量为 285 B，CKB 三个单独输出及 Data 向量为 590 B。标准锁的恢复调用计费 1,900,000 cycles，完整交易另计其余执行开销。早期 Molecule 编码、k256 锁与对应吞吐数据保存在[历史实验归档](FERN-BENCHMARKS.md)。[当前性能记录](BENCHMARKS.md)

已在 CKB devnet 对照 CKB 与 xUDT 的入账和兑付，核对聚合储备、流通负债及待兑余额。Patricia 批量编辑保持状态根、证明和单次原子提交语义。Jolteon 实际网络覆盖 4 至 128 副本，并在 CKB devnet 核验跨 epoch 交接、观察节点恢复与原生检查点发布。当前默认入口为加权 Jolteon，CometBFT 保留为冻结的实验对照。默认节点已在 CKB devnet 验证无许可兑付、退休、解押和双签罚没。完整研发状态及本轮新增方向见[研发记录](STEM-EVOLUTION.md)。

CKB 历史输出验证器从可信区块哈希重算 Header、交易及见证根，并按 OutPoint 提取原始输出。Rust 客户端与离线 CLI 可独立验证历史输出。StemVM 可读取 guardians 认证的历史输出或指定锚点存活记录。当前 CKB tip 的实时状态由独立查询处理。
