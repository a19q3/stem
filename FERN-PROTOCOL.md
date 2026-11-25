# Fern 协议方案

## 1. 网络定位

Fern 是一条以 CKB 计价、由 CometBFT 最终确认的 Cell 网络。Fern 定义唯一的原生 Cell 与交易格式，以 Many-to-Many Type Script 执行资产和应用规则。CKB L1 的真实质押决定验证者权重，独立桥金库负责 CKB 和 xUDT 的兑付。Fern 不发行独立原生代币。可兑付 CKB 的唯一增量来自已经确认并可兑付的 L1 存款。

每个网络用全新的 genesis、数据库、签名域和桥实例启动。网络身份由 canonical network spec 和一次性网络盐导出。该预部署 spec 包含 CKB genesis、初始权限与经济配置，排除依赖网络身份才能部署的 Registry Type ID、金库 Type ID、Checkpoint Type ID 及初始化签名。部署完成后，Fern genesis 再固定这些 L1 合约身份和第一份已认证的完整质押快照。CKB Checkpoint Cell 的身份与初始认证方也由 genesis 固定。CometBFT chain ID 由网络身份确定性导出，并符合 CometBFT 的长度约束。所有交易、证明和链间认证都明确绑定该网络。

| 组件 | 权责 |
| --- | --- |
| Fern Kernel | Cell 编码、交易验证、状态转换、资源计量、状态与执行承诺 |
| CKB-VM | 固定 RISC-V 执行配置和只读交易上下文 |
| CometBFT | 交易顺序、最终性、历史委员会及 ABCI 交接 |
| CKB Stake Registry | 质押保管、唯一记录、退出延时、证据与处罚 |
| L1 状态认证 guardians | 认证 CKB 上的 Registry、桥储备和历史。各用途分别固定权限与签名域 |
| L1 金库 | 独立的托管权限与兑付顺序，执行实际跨链交割 |
| L1 Checkpoint Cell | 在 CKB PoW 历史中固定已认证的 Fern 状态与委员会承诺 |

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

使用 Molecule 实现。Type Script 集合按完整规范编码做字节序排序。解码拒绝乱序、重复及超限编码，排序位置仅用于读取。代码标识、寻址方式、参数、所有 Type Script 和 Cell Data 都在输出原始字节中，资产身份因此独立于同一个 Cell 的抵押、监管或结算脚本。Cell 是消费和创建的原子单位。

交易使用一张 schema。原始部分含 Cell Inputs（OutPoint 与可选 Fern Since）、完整输出 Cells、Cell Deps、Header Deps、可选的截止高度、按 Script Group 寻址的应用消息，以及可选的协议操作。Witness 区域承载按组寻址的验证材料与签名，并为协议操作保留证书字段。普通交易至少消费一个可用 Cell。Bridge Mint、Stake Snapshot 和 Bridge Settlement 使用同一套编码、哈希和提案路径。原始交易中只允许一个操作，证书位于独立的 Witness 字段。Bridge Mint 的所有实际输入与输出仍由常规脚本规则验证。Stake Snapshot 和 Bridge Settlement 是控制交易，Cell 输入输出、Cell Deps、Header Deps 与普通 Witness 均为空。Bridge Settlement 的两种声明分别为已兑付提款核销和同一 L1 交易中的双金库储备调拨。

```text
tx_id      = H("Fern transaction ID", network_id, canonical_raw_tx)
tx_commit  = H("Fern transaction commitment", network_id, canonical_full_tx)
cell_leaf  = PatriciaLeaf(network_id, cell_key, live_cell_record)
```

各域标识采用固定字节串并以零字节结尾。变长字段经 Molecule 或显式长度前缀编码。普通 OutPoint 是 `(tx_id, output_index)`。Genesis 分配使用单独的创世承诺和确定性编号，主网资金来自认证存款，测试 genesis 可显式配置测试配额，测试配额只能存在于关闭真实 L1 兑付的独立网络。

### Witness、CoBuild 与 OTX

```text
ScriptGroupId  = (role, script_hash)
GroupWitness   = (group_index: u16, proof_data, seal)
```

组编号按角色和完整规范 Script 字节排序，从零开始。每组最多一个 Witness，条目严格递增。解析器拒绝重复、乱序、空条目以及指向未推导组的编号。无 Witness 需求的组省略条目，内核继续执行该组。应用操作消息与多方签署使用独立的 CoBuild 标准，普通支付无 Action 负载。

标准锁的签名摘要绑定网络、Lock 角色、完整 Script 哈希、原始交易 ID 和所有非空 `proof_data` 的有序承诺。签名材料由完整交易承诺覆盖，独立签名人可分别填入各组的 seal。输入和依赖由原始交易中的 OutPoint 绑定。其他 Lock 可以定义自己的验签逻辑。

CoBuild 保留 IDL、钱包展示、Action 构造与多方协调。每个合约独立核对 Action 与实际 Cell 差分。OTX 留作独立 Lock 与 SDK 签名标准，双方最终构造同一种 Fern 交易。OTX 的签名范围必须锁定可用输入、受保护输出、Action、依赖、费用限额和有效高度。结算脚本必须为每个承诺分配唯一的交付权益，避免同一输出重复满足多个订单。首轮共识接入使用完整交易签名，OTX 激活取决于独立签名与结算规范。 [CKB Transaction Co-Build 参考实现](https://github.com/cryptape/ckb-transaction-cobuild-poc)

## 3. 确定性执行与资源

对交易按区块顺序解析实际 UTXO。Lock Groups 来自全部输入的 Lock Script，Type Groups 来自输入与输出的 Type Scripts 并集。按完整 Script 身份分组，Lock 和 Type 角色分离。每组只执行一次，拥有自己的输入和输出索引集合，并读取同一份不可变交易。输入侧规则可以拒绝自身被删除、替换或迁移。

脚本寻址保留 `code_hash/hash_type/args`。原生编码收敛为 `Data` 内容哈希和 `Type` 完整代码 Type Script 哈希。Type 寻址只承认 genesis 固定的代码身份规则，例如一个 Type ID 约束下的唯一代码 Cell。交易必须明确列出实际代码 Cell Dep，升级后的代码对应新的 OutPoint。Dep Group 按其规范 OutPoint 向量单层展开，重复依赖、与消费输入重叠的依赖和超限展开直接拒绝。L1 Script 的原始哈希类型继续用于计算 CKB/xUDT 的 L1 身份。执行配置由网络固定为 CKB-VM V2 RISC-V 配置，复用 Assembly、分配复用及 Spawn/Exec。

原生 ABI 提供完整 Cell、完整 Type Script 集合、按索引及按哈希查询、当前组的角色和索引、组级消息及 Witness。保留 `load_script`、`load_script_hash`、全局及组相对的 Cell 读取语义。多个 Type Script 不扩大 Cell 写冲突域。确定性覆盖层记录消费、创建及依赖读取。可并行验证独立交易和各 Script Group。后续交易对同块输出、依赖和已消费 Cell 的可见性以串行重放定义。

缓存键覆盖网络、固定 VM 配置、完整 Witness、解析后的 Cells、代码和可见 Header。缓存命中按此前确认的 cycles 计入协议预算。区块同时限制编码字节、交易数、VM cycles、依赖读取字节和写入量。准备提案可以减少重复工作，验证提案与重放都以相同规则计算预算和结果。

### Fern Header 与时间

原生 Fern Header 编码高度、共识时间、父 Header ID、状态承诺和执行承诺。运行交易前，以高度、共识时间、父 Header ID 和交易根计算独立的区块上下文 ID，用于认证同块新建 Cell 的创建信息。完整 Header ID 在状态与执行承诺得出后计算。CometBFT 的签名 Header 与 Fern 的执行 Header 分别承担共识证明和应用上下文。Fern 原生 Header 没有 CKB PoW nonce、compact target、难度、Uncle 承诺或 DAO 字段。交易执行删除 NC-Max Proposal Window、Cellbase、Cellbase Maturity、一级与二级发行、矿工奖励和 Nervos DAO 的特殊验证、取款补偿与容量例外。CKB PoW、DAO 等 L1 字段可用于核验真正的 L1 数据，L1 协议继续独立运行。 [CKB L1 区块结构](https://github.com/nervosnetwork/rfcs/blob/master/rfcs/0027-block-structure/0027-block-structure.md)

| 尺度 | 由什么确定 | 用途 |
| --- | --- | --- |
| Fern 高度 | 已确定的 BFT 区块序列及候选高度 | 绝对与相对高度锁、交易过期 |
| Fern epoch | 高度和 genesis 固定的区块数量 | 质押状态与委员会边界 |
| Fern 共识时间 | 已提交 Header 的 Unix 毫秒 | 绝对时间与相对时间锁 |
| CKB L1 高度与时间 | CKB 自己的 PoW 链和共识规则 | 解除质押、存款确认、桥兑付及检查点锚定 |

CometBFT v0.38.26 首块使用 genesis 时间，之后从上一高度提交中按该高度的实际验证者权重计算时间戳的中位数。Fern 以截断到 Unix 毫秒的共识时间入账。候选 Header 的毫秒时间允许相同，禁止倒退。Fern 内核读取**前一已提交 Header**的时间作为时间锁参考。它省去额外滚动窗口，安全假设仍是 CometBFT 的验证者权重与诚实时钟假设。 [CometBFT v0.38.26 BFT Time 规范](https://github.com/cometbft/cometbft/blob/v0.38.26/spec/consensus/bft-time.md) · [对应版本的时间实现](https://github.com/cometbft/cometbft/blob/v0.38.26/state/state.go#L234-L292)

```rust
enum Since {
    AbsoluteHeight(u64),
    RelativeBlocks(u64),
    AbsoluteTime(u64),
    RelativeTime(u64),
}
```

以上四种值使用同一 9 字节 Molecule 结构（1 字节标记和 8 字节小端数值），时间值均为 Unix 毫秒或以毫秒表示的时长。每个输入可省略 Since。相对零块或零毫秒用省略表示，显式的相对零编码拒绝。Fern 不编码 epoch 分数锁。SDK 用固定的 `1 + epoch * blocks_per_epoch` 把 epoch 边界换算成高度，乘法和加法检查溢出。CKB L1 相对 `since` 仍按 L1 自有语义验证。 [CKB RFC 0017](https://github.com/nervosnetwork/rfcs/blob/master/rfcs/0017-tx-valid-since/0017-tx-valid-since.md)

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
cell_bytes     = 36 + len(lock) + sum(4 + len(type_script)) + len(data)
occupied_bytes = cell_bytes + state_overhead_bytes
storage_bond   = occupied_bytes × bond_per_byte

fee_paid       = Σ input_capacity + authenticated_mint_credit − Σ output_capacity
fee_required   = p_b × B + p_c × C + p_r × R + p_w × W
```

36 字节覆盖 Cell 表头、Capacity 字段、Type Script 向量头与 Cell Data 长度前缀。Script 长度采用完整规范编码。`state_overhead_bytes` 按网络固定规则计量标准 Live Cell 创建元数据和索引预算，排除已计入的 Cell 字节。`bond_per_byte` 用整数 shannons 表示，由 genesis 固定。每个输出至少提供规定的 Capacity。消费 Cell 时，抵押随输入 Capacity 重新可用。抵押给长期存储带来资金占用成本，节点持续收入来自实际手续费。L1 质押与收款输出继续采用 CKB 自身最低容量。 [CKB Capacity 经济规则](https://docs.nervos.org/docs/basics/concepts/economics/)

20 字节参数的普通 Lock 编码为 73 字节，空 Data、无 Type Script 的 Cell 共 109 字节。示例价格每字节 0.01 CKB 时，抵押为 `1.09 CKB + state_overhead_bytes × 0.01 CKB`。开发与生产参数分别由状态增长和读写测量确定。

| 费用量 | 共识计量 |
| --- | --- |
| `B` | 完整规范交易及 Witness 的字节数 |
| `C` | 所有 Lock 与 Type Groups 的已验证 cycles，缓存命中按原周期计费 |
| `R` | 按标准记录编码计量的逻辑读取字节，解析时按交易内唯一的输入、依赖、Header 及协议记录去重 |
| `W` | 规范状态差分字节，包含键及新记录或删除标记，涵盖相应的协议状态变化 |

逻辑读写量按交易解析后的规范记录与状态差分计量。VM 重复读取已解析的记录时，相应执行开销计入 cycles。本地 RocksDB、树节点压缩与磁盘写入作为观测指标。费率和各独立资源限额由网络配置固定，所有计算使用 checked 整数。接受交易需满足 `fee_paid >= fee_required`，并同时通过所有独立预算。完整交易签名覆盖输入、输出及 Witness 材料，固定实际付费差额。代付方用同一交易的 Capacity 输入承担费用，并在签名前校验差额和自己的授权上限。

**控制交易费用。** 合法的 Stake Snapshot 和 Bridge Settlement 支付零 Capacity，豁免普通费用公式。控制交易必须使用固定操作形状和超过三分之二的已固定签名人权重。每类操作独立限制每块操作数、规范交易字节、签名数及相应的状态读取与写入规模。协议先检查并占用自己的预算，再做签名验证。读取的 Registry 另有记录数量上限。两类控制交易的预算不互相借用，其字节亦计入整个区块的字节上限。普通交易无法使用豁免。Bridge Mint 继续按输入与认证入账增量核算容量。候选区块重复验证使用各自重新计算的预算。

### 零发行与手续费负债

```text
Fern native issuance = 0
liquid L1 reserve >= redeemable circulating capacity
                    + pending withdrawals
                    + payable fees
stake assets ∩ bridge reserve = ∅
```

在可兑付网络，认证的真实存款才能增加净 CKB 兑付能力，每一笔 L1 入账只可使用一次。储备盈余不得直接用于生成新的 Fern CKB。普通交易容量守恒，手续费和抵押都通过已存在的 Capacity 核算。质押、冻结罚没款、金库底仓以及不可及时支出的资产均排除在可兑付储备之外。首轮金库使用能按提款流程及时支出的 L1 Cells，储备不投入 Nervos DAO。Fern 不提供增发、固定质押收益或 DAO 补偿。

研究网手续费扣减流通负债，尚未分配的真实 L1 盈余留在桥储备。生产分配候选是把可用净手续费盈余分给验证者、DevFund 和 Treasury。比例、支付权限和安全留存由生产规则确定，80/15/5 只是一组示例。归属一旦确认，未付款项从盈余重分类为 `payable_fees`，若费用落在可兑付的 Fern 资金池 Cell 中，其余额已经属于流通负债。转为应付手续费时须等额移出流通负债。支付时同时减记 L1 储备和相应负债。分配不得超过已确认且未分配的费收盈余。确认应付款和执行 L1 出账时都要检查覆盖，出账预算包含 L1 交易费。首轮没有自动分配的支出路径。

## 4. 资产与组合标准

Fern UDT Type Script 负责标识、发行权限、数量解码和守恒。桥接资产只接受绑定其 L1 资产身份的认证增量。若资产发行规则要求监管，UDT 必须检查每个对应输出都包含指定政策 Type Script。可选抵押、交易限制和结算规则由各自的脚本执行。链上应用数据保持字节串。需要共享数据布局时，DataMap 作为合约层标准，采用稳定命名空间、规范编码、键和总字节上限，由各合约验证自己负责的字段。CKB L1 xUDT 的扩展和发行限制仍由实际部署的 L1 合约执行。 [CKB RFC 0052](https://github.com/nervosnetwork/rfcs/blob/master/rfcs/0052-extensible-udt/0052-extensible-udt.md)

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

CKB L1 Registry 用 Type ID 保证唯一性。Stake Cell Type Script 绑定 Registry 和验证者 Ed25519 公钥。该 Cell 的 L1 Capacity 等于实际 occupied Capacity 加声明质押。注册需要验证者签名、资金所有者授权以及公钥尚未退休的证明。增押、退出请求、罚没和提现都原子更新 Registry 与对应 Stake Cell。

| Registry 的承诺 | 状态内容和保留规则 |
| --- | --- |
| 当前质押 | 按公钥排列的已质押和退出中记录，绑定实际 Stake Cell、金额和经认证的活跃高度区间。最多保留 256 个尚未结清的公钥 |
| 待结责任 | 退休后仍有余额可支取或处罚待执行时，保留完整 Stake Cell 和 Registry 记录 |
| 退休根 | 提现时从当前记录移除公钥，并在稀疏 Merkle 根中永久承诺该公钥已退休。原记录及证据可从 CKB 交易历史审计 |

退休证明编码一个 32 字节位图和按层号由低至高排列的非零兄弟哈希。合约验证当前根下公钥不存在。提现同时将该公钥插入退休根，释放 Registry 名额。历史公钥始终保留在认证根中。曾结清的质押公钥永久退休，后续验证者注册使用新公钥。仍在责任窗口内的 Stake Cell 无法通过更换 OutPoint 或重新注册解除责任。

### 快照窗口与 H+2

快照承诺 Fern 网络、CKB genesis、CKB 锚点高度与哈希、Registry 身份及序号、完整 Registry 状态、前一已接受 Fern 快照及 L1 锚点、目标 epoch、guardian 代次，以及签署窗口的 Fern 锚点。 原生控制交易将目标 epoch、签署锚点 Header ID 与证书正文承诺写入原始交易。正文包含规范快照及整个 Registry。签名绑定网络和原始声明。快照正文依次编码两字节小端快照长度、规范快照、两字节小端的新认证方数量、按公钥升序排列的新认证方条目和完整 Registry。每条为 32 字节公钥与 8 字节小端权重。平常快照的新认证方数量为零。通过验证后才允许替换候选覆盖层中的已接受快照。观察者从可信 CKB 全节点验证 UTXO、质押容量、确认深度和链历史，持久化签过的 L1 锚点和最近一次签署的目标 epoch 与消息摘要。同一 epoch 只允许再次签署完全相同的消息。Fern 共识依据已提交字节与签名验证快照。独立的快照签名域要求超过三分之二已授权 guardian 权重。

令 epoch 生效高度为 `A(e) = 1 + e × blocks_per_epoch`。对非 genesis epoch，网络固定整数 `W`，满足 `3 ≤ W < blocks_per_epoch`。签署锚点为已最终确定的 `A(e)−W−1` 区块。只能从下一块开始签发与提交，到 `A(e)−2` 截止，过期证书不得转用于其他 epoch。每个 epoch 最多接受一个快照。Registry 序号不可倒退，同序号只能对应相同承诺。后续快照必须延续前一已认证 L1 锚点。深重组跨过该锚点时停签新授权，并重新核验仍受锁定的质押和桥负债。已确定的 Fern 历史继续有效。

先排除未激活、已退休、已冻结和已到退出 epoch 的记录。对其余合格记录按质押从高到低排列，公钥字节序打破平局，取配置规模。票权为 `bonded_shannons / shannons_per_power` 的整数商。非空、最小质押、正权重和总权重上限全部是确定性检查。genesis 委员会通过同一认证路径直接生效。正常交接在 `FinalizeBlock(A−2)` 发出 `ValidatorUpdate`。旧委员会签名的 `A−1` Header 绑定新 `NextValidatorsHash`，新委员会签名的 `A` Header 绑定实际新 `ValidatorsHash`。交接证明包含这两个 Header 和签名，以及快照与更新执行证明。缺少新快照时，只有旧委员会对应质押仍锁在 L1 的情况下，旧委员会才能继续按原权重执行。 [CometBFT v0.38.26 ABCI 方法](https://github.com/cometbft/cometbft/blob/v0.38.26/spec/abci/abci%2B%2B_methods.md#finalizeblock)

### 退休的不可逆性和签名人轮换

1. **请求退出。** 所有者在 L1 更新 Registry 和 Stake Cell。资金继续锁定。在快照中因排名下降离开委员会的记录仍可参加后续选举。
2. **登记终止。** Fern 接受包含到期退出、冻结或已退休记录的快照，同时在 SystemRoot 写入该公钥的退出生效高度。未来快照如果试图把该公钥重新加入委员会，则整份快照被拒绝。
3. **验证实际退出。** 认证方验证 Fern 的退休标记 Patricia 证明和实际生效高度之后的 CometBFT `H+1` 提交。签署集合必须通过从可信 genesis 开始的交接证明认证，并确认该公钥缺席。认证方还核对已确认的 CKB Registry、真实 Stake Cell、容量和当前签名委员会，再按退休域签署确定的 L1 状态转换。
4. **执行 L1 退出。** CKB 合约验证现任认证方超过三分之二权重的签名。提款消耗退休后的 Stake Cell，执行相对 CKB 区块锁，并在同一笔交易中释放 Registry 名额及更新退休根。仍处于责任期的记录可以提交客观双签证据。

L1 Registry 轮换由旧集合和新集合共同签署并在 CKB 确认。Fern 侧，第一份携带新轮换代次的快照必须在其目标 epoch 的签署窗口内提交，并由两个集合对同一消息各提供超过三分之二权重的签名。签名索引最高位标记新集合，其余位为该集合内按公钥排序的索引。快照提交后，Fern 原子更新协议状态中的认证方，后续快照只接受新集合。提议快照也须延续最近已接受的 CKB 锚点和 Registry 序号。已经退休的公钥继续受 Fern 标记和 L1 退休根约束。桥和独立检查点使用各自的认证方流程。

### 双签、罚没和停摆

L1 验证真实 CometBFT canonical vote sign bytes。两票需对应同一 chain ID、公钥、高度、round、vote type，不同的完整 Block ID 和各自有效签名。证据高度必须落入该质押代次已认证的实际活跃区间，包含退出等待期内仍可追责的历史区间。证据键按投票位置去重。罚没额与去向由网络规则指定。证据通过后，L1 冻结罚没部分。Fern 认证冻结状态后将该代次排除出候选集合，并以与退休相同的机制提交终止记录。实际移除该代次前，资金继续作为 L1 受约束资产锁定。完成移除后才执行资产结算。开发链已实测双签冻结、提前结算拒绝和退休后全额罚没。 [CometBFT v0.38.26 投票签名源码](https://github.com/cometbft/cometbft/blob/v0.38.26/types/vote.go)

活跃权重不足以形成超过三分之二的签名时，Fern 接受停摆。L1 可接收证据并冻结资产。停摆期间的 L1 处罚不会跳过委员会交接，也不能替 Fern 选择后续区块。原网络在当前委员会恢复合法法定人数后，按 CometBFT 的正常规则从最后已确定区块继续。若长期无法恢复，另建网络需要新的网络身份和 genesis。新网络只能使用独立储备，原金库负债先按已验证的原历史核对，同一笔资产只能承担一条历史的兑付。首轮方案没有同身份紧急换委员会的应急路径。

## 6. 桥、储备与提款

每条提款通道由 `CKB genesis + L1 vault Type ID + 资产身份` 唯一标识。原生 CKB 和每种启用的 L1 xUDT 各有独立的金库状态与 nonce。xUDT 身份承诺其完整 L1 Type Script，覆盖 hash type 和所有 args。Fern 的 Capacity 在应用层统一流通。各通道可用的 L1 CKB 容量与已经预留的容量必须分别入账。

- **入桥。** L1 存款归集、确认之后，由存款 guardians 认证存款 OutPoint、金库、金额、Fern 收款 Lock、L1 锚点和网络。Fern 只对未使用过的证书授予准确的容量与资产增量。所有实际输入、输出与相关 Type Groups 仍按原生规则处理。
- **原子申请。** 一个 Burn Cell 对应一张票据。原生 CKB 的 Burn Cell 无桥接 UDT，xUDT Burn Cell 只承载一种已注册的桥接 UDT 和它自己的 CKB Capacity。多个资产用同笔交易中的多个 Burn Cells 提出申请。多个 Type Script 仍能约束同一个 Burn Cell。每个 Burn Cell 的全部 Capacity 只计入本票据的 L1 支付金额一次。合约将原生 CKB、UDT 数量和附带的 L1 Capacity 分开验证。票据沿交易输出索引顺序在各自通道分配连续 nonce。所有票据与通道预算共同提交，实际 L1 兑付分别进行。
- **小额聚合。** 多个小额 Fern Cell 可按同一资产通道和同一 L1 收款脚本合并为一个 Burn Cell。每个输入照常签名和验证。达到 L1 输出最低 Capacity 后才可受理并分配 nonce。聚合在 Fern 交易内完成。
- **入队校验。** 验证 L1 收款模板和参数、准确输出及其 occupied Capacity、原生资产配置和足够的通道可用余额。拒绝会被 L1 金库解释为存款或 escrow 的收款脚本。校验失败则整笔申请拒绝。若通道需要更多 CKB Capacity，先完成已认证的 L1 储备调拨，再受理新申请。
- **L1 兑付。** 金库保管人核对 Fern 最终性、执行结果及票据，合约约束通道的 nonce、实际收款输出与金额。金库轮换保持票据与资金可兑付。一个通道出现异常时停止该通道的新申请，并保留其队首票据及完整负债。其他通道继续处理各自的 nonce。
- **对账。** 对 CKB 及各 UDT 分别维持 `可兑付 L1 储备 ≥ Fern 流通负债 + 未付的已提交提款 + 应付手续费`。此外，每个通道预留的 CKB Capacity 和 UDT 数量必须足以兑现该通道的待付款。申请把流通负债转成待付款，在同一提交中预留通道余额。经认证的 L1 兑付同步减记储备、待付款与预留。占用的金库底仓、外部交易费和质押资金分别计量。同一笔 CKB 不得计入多个通道可兑付余额。

**认证 L1 核销和调拨。** Bridge Settlement 使用独立签名域。认证方须核验指定 L1 genesis、金库 Type ID、实际确认的交易、相关金库输入和输出、余额变化，以及本次确认锚沿已认证锚延伸。签名绑定网络、认证方代次、前后 L1 锚点和操作所有字段。它承诺完整的 Fern 提款票据标识，认证方比对 L1 收款输出、资产和金额。Fern 共识只读取已提交的签名人、L1 锚点、金库 OutPoint 及相关票据。

- **已兑付提款核销。** 声明携带通道、当前 nonce、原票据标识及金库继任 OutPoint。必须匹配队首的已确认票据与先前金库输入。一次提交消耗待付款，推进 nonce，并按票据数值减记 L1 储备和预留余额。此动作不创建 Fern Capacity。
- **储备调拨。** 声明携带源通道、目标通道、确定的 CKB Capacity，以及两个金库前后 OutPoint。两个金库在同一笔已确认的 L1 交易里被替换。源额扣减和目标额增加等量原子完成，各通道仍须覆盖原有预留，资产单位、待付款和出款 nonce 保持原值。跨通道只调拨原生 CKB Capacity。
- **顺序与重放。** 认证以候选区块内先前操作形成的锚点及金库 OutPoint 为准。重复使用已经被核销的票据、旧金库输入或已被替换的认证代次均拒绝。跟踪全局认证 L1 锚点时，新结算声明需推进该锚点。

Fern 验证者质押、存款认证、检查点认证和金库托管各自拥有独立权限。共识安全依赖各高度的 CometBFT 权重假设。L1 状态认证依赖 guardians 对 CKB 全节点的核验。实际提现仍遵守 L1 金库权限。

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
| `history_root` | 此前已提交的原生 Fern Header ID |
| `transaction_root` | 按区块顺序排列的完整交易，包含全部 Witness |
| `receipt_root` | 同一顺序的确定性执行回执 |

### 共享 Patricia 存储

Cell 根与系统状态根各自认证一棵当前状态树。两棵树使用同一套二进制 Patricia 算法、节点缓存及 RocksDB 节点存储。节点键带树标识。协议功能通过系统状态树中的固定命名空间寻址，无须为各功能分配单独的树或存储引擎。

状态树按从最高位到最低位的顺序处理键。分支在共同前缀后的第一个相异位分叉，其哈希绑定前缀长度、规范前缀和有序子节点。空树、分支和叶使用独立的哈希域。同一规范键值集合产生相同的根。

```text
cell_key = H("Fern state key", network_id, 0, canonical_out_point)
record   = (canonical_out_point, creation_height, creation_time_ms, creation_context_id, canonical_cell)
value    = H("Fern state record", network_id, 0, record)
leaf     = H("Fern Patricia node", network_id, 0, 0, cell_key, value)
```

每条规范记录只存一份，按命名空间及键读取。Live Cell 记录附带其规范 OutPoint，快照恢复由此重算 Cell 键。持久化叶存键及叶哈希，生成证明时按键取记录并重算。创建高度和时间用于 `Since`，创建上下文 ID 绑定该块的高度、时间、父 Header ID 和交易根。每块批量更新发生变化的键及受影响路径，覆盖被替换的当前节点并移除失效路径。历史 Header 继续承诺各高度的状态根，历史状态证明由相应锚定快照或归档重放获得。

系统状态树统一使用有域分隔的键，记录存款防重放标记和认证方代次、桥储备、通道 nonce、待兑付负债、应付手续费、接受的质押快照、当前及排定的委员会、退休终止标记、认证方轮换、委员会历史和可引用检查点。按通道和 nonce 查找提款，按公钥和质押代次查找退休记录。历史检查点只引用此前已提交的 Header。

### 永久防重放记录

SystemRoot 保留可查询的紧凑记录。存款键绑定 `(CKB genesis, L1 deposit OutPoint)`，覆盖认证方和金库轮换。退休键绑定 `(Stake Registry ID, 验证者公钥)`。已退休公钥永久失去注册资格。键均有独立哈希域并绑定 Fern 网络。值以固定 8 字节编码记录使用或终止高度。候选覆盖层检查同时涵盖已提交记录和本块新记录。快照必须包含全部防重放记录并重建其认证根。历史正文、回执与证明索引继续按节点窗口裁剪。MMR 认证已提交 Header 的历史，快照恢复仍需要当前状态下可检索的防重放记录。

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

历史 Header 累积器使用 Merkle Mountain Range（MMR）。创世高度 0 的历史根承诺空序列。提交高度 `H` 时追加 `HeaderID(H−1)`，叶哈希绑定网络身份和对应高度。高度 `H` 的历史根承诺创世至 `H−1` 的 Header。`H("Fern history root", network_id, leaf_count, ordered_peaks)` 按从左到右编码峰值，其数量与位置由叶数确定。

普通节点持久化叶数和峰值，通过合并同高度峰值完成追加。历史包含证明认证指定高度的 Header，追加一致性证明认证后续累积器延续同一前缀。归档索引提供证明所需的历史节点。 [MMR 包含与一致性证明](https://datatracker.ietf.org/doc/draft-ietf-scitt-receipts-mmr-profile/)

### Header 与 AppHash 的计算顺序

```text
HistoryRoot(H) = Append(History(H−1), HeaderID(H−1)).root
Context(H)     = (H, time_ms, HeaderID(H−1), TxRoot(H))
ContextID(H)   = H("Fern block context", network_id, Context(H))
HeaderID(H)    = H("Fern Header", network_id, Context(H), StateCommitment(H), ExecutionCommitment(H))
AppHash(H)     = H("Fern AppHash", network_id, H, HeaderID(H))
```

执行前先确定交易根与区块上下文。执行时新建 Cell 只引用这个上下文 ID。执行完成后计算 Cell 根、系统状态根和回执根，再构造完整 Header。历史累积器本块追加的 Header 属于前一高度。三者依赖方向固定。CometBFT 的 `H+1` Header 认证高度 `H` 的 AppHash。轻客户端验证 `H+1` 的合法提交和相应委员会，然后用提供的 Fern Header 重算 AppHash 并比对。状态、交易及回执证明对照该 Fern Header 的承诺验证。 [CometBFT v0.38.26 ABCI 方法](https://github.com/cometbft/cometbft/blob/v0.38.26/spec/abci/abci%2B%2B_methods.md#finalizeblock)

### 原子提交、锚定快照与节点形态

`ProcessProposal` 与 `FinalizeBlock` 共享执行内核。如果候选块字节、父状态及共识上下文完全匹配，可复用该候选块的验证结果。`Commit` 为每个区块执行一次同步 RocksDB WriteBatch。规范 Cell 与协议记录、Patricia 变更路径、Header、累积器叶数及峰值、保留的交易与回执、提交点一起原子可见。该约束适用于 Fern 应用提交，CometBFT 保持自己的共识持久化路径。

锚定快照从同一已提交高度的稳定读取视图导出规范 Cell 与协议记录、原生 Header、BFT 证明、MMR 叶数和峰值，以及恢复必需的委员会交接和检查点材料。快照导出规范状态记录，Patricia 节点由接收方重建。交易根和回执根随认证 Header 提供，旧区块的逐块 Merkle 证明由保存的交易与回执正文重建。客户端先验证 Header 和 AppHash，再重建 Cell 根与系统状态根，按峰值核验历史根，匹配后原子安装。创建元数据、待兑付负债、峰值或委员会材料被篡改都会使恢复失败。快照导出在提交路径之外运行。

| 运行形态 | 保留与服务 |
| --- | --- |
| 普通节点，验证者默认 | 当前 Live Cells、协议状态与 Patricia 节点、MMR 叶数及峰值、近期 Header、交易和回执。按保留窗口自动裁剪，持续保留未结提款、委员会交接与继续共识所需材料 |
| 归档节点 | 普通节点的全部能力，加完整 Header、交易和回执历史、锚定快照服务与可选历史证明索引。从快照起步时，使用已认证的归档历史补齐早期正文 |

历史证明索引从已提交归档记录异步构建或重建。归档索引、钱包扫描和快照导出独立于区块提交。普通节点按保留窗口裁剪应用状态正文，并向 CometBFT 请求裁剪对应历史区块。交易只引用共识配置允许的近期 Header Deps，两种节点按同一规则验证。客户端通过归档索引查询更早的 Header 和历史包含证明。

钱包与 RPC 使用 Fern 原生交易格式。本地钱包可按完整 Lock 或 Type Script Hash 查询 Live Cells，ABCI 按 OutPoint 提供 Cell 证明，并可查询指定交易位置的执行结果。提款和委员会的证明由相应工具验证。交易意图由 Lock 和 OTX 标准表达，内核继续验证精确 OutPoint、已签名输入和实际输出。

### 空块与 H+1 证明

空块依然追加父 Header，AppHash 因而继续变化。CometBFT v0.38.26 会在应用哈希改变后主动提议证明块，`create_empty_blocks=false` 或非零的空闲出块间隔都不能将这些证明块暂停。原生集成采用明确的常规出块配置，允许空块并关闭 `skip_timeout_commit`。空载节奏由部署的共识提交等待参数测量。区块头、历史峰值和一次原子提交仍是每个空块的工作内容。无交易时使用规范的空交易和空回执根。节点只有在已取得签名的后继 Header 时才向客户端提供高度 `H` 的 H+1 证明。停摆时，最后一个高度的应用根可能仍在等待后继 Header。 [CometBFT v0.38.26 证明块判定](https://github.com/cometbft/cometbft/blob/v0.38.26/consensus/state.go#L1098-L1126)

### CKB PoW 检查点锚定

部署独立的 CKB Checkpoint Type ID Cell。独立发布进程按网络配置的低频间隔收集已经提交的 Fern 证明并提交 L1 交易。每次更新消费上一 Checkpoint Cell，验证唯一继任者、严格递增的序号与 Fern 高度，以及上一检查点的 OutPoint 与承诺。新记录绑定 Fern 网络和 CKB genesis、Fern 高度及最终区块完整 Block ID、原生 Header ID、AppHash、历史根及叶数、承载该 AppHash 的后续 CometBFT Header 身份、当前和下一委员会承诺、质押快照承诺及委员会交接证明的摘要。

锚定认证方独立于金库。它核验从上一锚到新锚的 Fern 最终性、对应 AppHash、新旧历史根的追加一致性、旧锚的 Header 在新根中的包含性及期间的每次委员会交接后，按专门签名域对记录签名。CKB 合约验证当前固定锚定认证方超过三分之二签名与继任顺序。轮换由旧、新集合分别认证。该首轮合约由认证方证明 Fern 历史，签名责任之外的证明仍需客户端独立复核。

客户端从配置固定的 Checkpoint Type ID 与信任期内的 Fern 检查点起步。先用完整 CKB 节点或已验证的 CKB 轻客户端确认锚定交易包含在满足确认策略的有效 PoW 链上，再校验 Type ID 继任路径、Fern AppHash、新旧历史根的追加一致性、上一已接受锚的 Header 在新历史根中的包含性以及跨 epoch 的完整交接证明。证明缺失、历史与已接受锚冲突或超出信任期时，拒绝自动跟随。已确认的 CKB 记录给客户端一个可追溯的外部历史约束。Fern 最终性仍由 BFT 提交确定。历史包含证明和执行回执分别认证 Header 和该 Header 下的执行结果。CKB 深重组时冻结该锚定源的后续接受，保留先前已验证的 Fern 最终性判断。发布延迟只影响新外部锚点的可用时刻，Fern 仍按正常 BFT 流程提交区块。首轮委员会按已认证质押确定。 [CKB 轻客户端参考实现](https://github.com/nervosnetwork/ckb-light-client)

## 8. 接入顺序与交付验收

| 优先级 | 接入 | 实际验收 |
| --- | --- | --- |
| P0 | Cell、VM、时间、签名和共识切换 | 四副本使用同一原生交易格式。删除 CKB Header 旧字段与 DAO 容量例外，验证创世、同块消费、过期高度、时间跳跃、重启和跨 epoch 的边界。钱包与 RPC 支持该格式。串行重放和并行验证结果一致，拒绝冲突及恶意提案 |
| P0 | 质押终止、快照窗口和罚没 | 真实 L1 注册、增押、退出及双签。拒绝抢跑退休、先签后提交的旧快照、过期签名与退役认证方。在丧失法定人数的实网确认 L1 可冻结而 Fern 不发生强制换组 |
| P0 | 状态与执行承诺、桥接守恒 | 真实 CKB 与 xUDT 往返、储备对账、金库轮换。多资产原子入队。小额聚合后达到 L1 最低容量才入队。某通道暂停时其他 nonce 正常推进，拒绝重复使用 Capacity。验证空块 H+1 认证、历史包含及追加一致性、回执位置和锚定快照，拒绝被篡改的创建元数据与负债，并跨裁剪与恢复重测存款和退休代次重放 |
| P1 | 裁剪、归档证明与 Capacity 定价 | 在不同 Live Cell 总量下固定每块变更量，分别测量状态树、回执树、历史累积器、读写放大与提交延迟。验证裁剪后的 Header Dep、证明重建、重启和状态同步。用结果确定开发网抵押成本 |
| P1 | CKB Checkpoint Cell | 真实 L1 发布和轮换。验证客户端接受合法锚和交接证明，拒绝冲突、缺漏历史与失效分叉 |
| P2 | 多维费率及净盈余分配 | 对齐确定性计量、费用上限、应付费收与真实 L1 出账，验证分配后的储备覆盖 |

### 提交成本不变量

固定每块交易数、交易字节、已计费资源和 Live Cell 及协议状态变更量时，Fern 的提交成本不得随历史总长度线性增长。Patricia 成本可以随当前状态规模和实际变更路径增长。MMR 追加只处理峰值合并。提交只访问本块解析的键、更新路径和由键查出的协议记录。禁止在提交时扫描 Live Cells 全集、历史 Header、退休记录或提款记录。每块只有一次应用数据库的原子同步提交，历史证明索引、RPC 索引、快照导出和 L1 发布不阻塞最终性。

验收分别改变**当前状态规模**和**累计历史长度**。使用等规模、各自有效签名的交易负载，匹配状态变更量和缓存条件，测量 Patricia 触及节点、MMR 合并次数、逻辑读写、同步提交次数、提交延迟及全副本确认延迟。对照普通与归档节点、索引开启与关闭。快照全量重建与归档重放单独报告。

压测分别运行 CKB 支付、单 Type Script、双 Type Script、多 Type Script、跨资产结算及委员会变更。按能力逐步提高投递速率至 1,000、2,000、3,000、5,000 TPS，记录 offered、admitted、finalized 和全副本可见速率、p50/p95/p99、客户端丢弃、CPU、RSS、物理写入、fsync、网络流量及积压。历史账本单独对照。

Many-to-Many 的边际成本采用控制实验。固定输入数量和数据、输出数量、Lock 分组、签名算法与密钥、依赖读取、数据总尺寸和 VM 配置，改变独立 Type Groups 的数量。每个变体按真实交易内容重新签名。等长夹具能做到时使用等长编码，其他差异应记录。分别记录 Lock 与 Type 的 cycles、缓存冷暖、上下文载入、状态根更新及完整四节点吞吐，保留原始数据、命令和构建指纹。

## 9. 当前实现与实测证据

| 项目 | 已验证 | 尚待集成 |
| --- | --- | --- |
| 原生执行 | 默认 `fern run` 使用四字段 Cell、组级 Witness、BFT 时间锁、可定价抵押及并行 RISC-V Script Groups；四节点拒绝移除输入侧 Transfer Policy 的交易；统一池并行执行交易和组，串行与并行回执及状态一致 | 同机四 worker 暂未提高带预算的整链吞吐 |
| CKB L1 质押 | 实际部署登记 Type ID、锁定并增押；未激活、已退休的记录无法进委员会；双签冻结 250 CKB、阻止提前罚没、退休后罚没；另有正常解除质押路径；替换已观察的 CKB 分支后守护者实际停止认证 | 生产环境的参数标定和运行部署 |
| 快照与委员会 | 实际 CKB 质押快照提供 genesis 和 H+2 更新；实测高度 11 加入新验证者，高度 21 移除，核验两次交接及后继签名，两层 guardian 轮换后由新集合签发快照和退休证书，CKB 退还 250 CKB。实际增押在高度 31 将在任验证者权重从 400 提高到 450 | 高变更率下的长周期轮换验证 |
| 控制交易与桥 | 原生状态持久化存款防重放和通道储备；实际 CKB 与 1,200 xUDT 双向桥接，按独立通道核销兑付；CKB devnet 实际调拨 500,000 shannons 并轮换金库，分支替换后桥守护者停止签发 | 连续运行中的长期储备对账 |
| 状态承诺与恢复 | RocksDB 同步提交 Patricia、MMR 和执行树；四节点重启追赶、第五节点实际 CometBFT 快照恢复；归档索引生成历史证明，CKB devnet 锚定高度 11 和 24 并完成认证方轮换 | 独立生产发布与锚定源的重组演练 |
| 性能 | 同机四节点的五类负载均提交 512 笔，约 34 TPS；三分钟支付提交 4,320 笔、23.88 TPS；另实测 8、16、32 副本和质押变更期间的付款 | 持续高负载和跨主机基线 |

实际运行由 CometBFT v0.38.26 和本地 CKB devnet 完成。Fern 的 L1 状态认证依赖各自配置的 guardian 法定人数与受信 CKB 完整节点。详见 [操作与复现](OPERATIONS.md) 和 [测量](BENCHMARKS.md)。

原生组级签名夹具实测单资产 5,769,207 cycles，身份和生命周期 5,760,590 cycles，五个 Type Groups 的跨资产结算 5,835,417 cycles。普通支付体积为 Fern 467 B、CKB 460 B，三个组合 Type Script 的输出向量为 Fern 384 B、CKB 590 B。夹具测量涵盖序列化和 VM 执行，整链吞吐指标单独采集。 [性能记录](BENCHMARKS.md)
