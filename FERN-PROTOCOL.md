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

交易使用一张 schema。原始部分含 Cell Inputs（OutPoint 与可选 Fern Since）、完整输出 Cells、Cell Deps、Header Deps、可选的截止高度、按 Script Group 寻址的应用消息，以及可选的协议操作。Witness 区域承载按组寻址的验证材料与签名，并为协议操作保留证书字段。普通交易至少消费一个可用 Cell。Bridge Mint 和 Stake Snapshot 是唯一预定义的协议操作，使用同一套编码、哈希和提案路径。操作类型与公共参数进入原始交易，证书进入 Witness。快照操作的输入及输出必须为空。Bridge Mint 的所有实际输入与输出仍由常规脚本规则验证。

```text
tx_id      = H("Fern transaction ID", network_id, canonical_raw_tx)
tx_commit  = H("Fern witnessed transaction", network_id, canonical_full_tx)
cell_leaf  = H("Fern Live Cell leaf", network_id, cell_key, out_point, canonical_live_cell_record)
```

各域标识采用固定字节串。变长字段经 Molecule 或显式长度前缀编码。普通 OutPoint 是 `(tx_id, output_index)`。Genesis 分配使用单独的创世承诺和确定性编号，主网资金来自认证存款，测试 genesis 可显式配置测试配额，测试配额只能存在于关闭真实 L1 兑付的独立网络。

### Witness、CoBuild 与 OTX

```text
ScriptGroupId  = (role, script_hash)
GroupMessage   = (group_id, ordered_actions)
GroupWitness   = (group_id, proof_data, seal)
```

原始交易最多为每个 Script Group 编码一个消息条目，条目可含多条 Action。每个 Witness 条目同样以角色和完整 Script 哈希唯一寻址，并按该键排序。条目只能指向实际推导的组。同一角色出现相同哈希但不同规范 Script 字节时，拒绝交易。无消息的交易依然执行全部组。完整交易签名绑定网络、组身份、原始交易哈希、解析后的依赖承诺和所有非签名见证材料。所有 seal 统一留空再计算签名摘要，签名加入后由全交易承诺覆盖。不同锁仍可定义自己的验签逻辑。

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

以上四种值使用同一 Molecule 规范联合类型，时间值均为 Unix 毫秒或以毫秒表示的时长。每个输入可省略 Since。相对零块或零毫秒用省略表示，显式的相对零编码拒绝。Fern 不编码 epoch 分数锁。SDK 用固定的 `1 + epoch * blocks_per_epoch` 把 epoch 边界换算成高度，乘法和加法检查溢出。CKB L1 相对 `since` 仍按 L1 自有语义验证。 [CKB RFC 0017](https://github.com/nervosnetwork/rfcs/blob/master/rfcs/0017-tx-valid-since/0017-tx-valid-since.md)

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

CKB L1 Registry 用 Type ID 保证唯一性。Stake Cell 绑定 Registry、验证者 Ed25519 公钥与 `stake_generation`，其中代次由注册完成时的 Registry 序号分配，严格递增且永不复用。该 Cell 的 L1 Capacity 等于实际 occupied Capacity 加声明质押。注册需要验证者签名与资金所有者授权。同一公钥最多有一个未终结代次。新代次需等待前一代的责任窗口关闭且资金流程结清。增押、退出请求、罚没和提现都原子更新 Registry 与对应 Stake Cell。

| Registry 的承诺 | 状态内容和保留规则 |
| --- | --- |
| Active Stakes | 按公钥排列的已质押和退出中记录，绑定代次、实际 Stake Cell 及金额。完整快照可从这部分验证所有候选人 |
| Historical Liability | 退出后仍可追责的代次、实际活跃高度区间、退休证书、历史最大责任和证据状态。存在未结处罚或可支取余额时保留其所需记录 |
| Historical Commitment | 归档记录摘要按终结顺序追加到 Merkle 累积器。完整记录与证明由归档者提供 |

只有在 L1 责任期限届满、支出已核销且不存在挂起处罚时，才可移除相应的完整历史记录。清理同时承诺不可逆的终结摘要，归档叶包含 Registry、验证者公钥、代次、活跃区间、结算状态及证据摘要。Registry 的容量限制针对尚未终结的状态。注册时预留对应责任记录的空间，满足退休条件的旧代次在高周转时仍能写入历史。单一 Registry Cell 继续原子承载更新。实现不得把永久历史实体计入同一固定名额上限。

### 快照窗口与 H+2

快照承诺 Fern 网络、CKB genesis、CKB 锚点高度与哈希、Registry 身份及序号、完整 Registry 状态、前一已接受 Fern 快照及 L1 锚点、目标 epoch、guardian 代次，以及签署窗口的 Fern 锚点。观察者从可信 CKB 全节点验证 UTXO、质押容量、确认深度和链历史，持久化签过的 L1 锚点。Fern 共识依据已提交字节与签名验证快照。独立的快照签名域要求超过三分之二已授权 guardian 权重。

令 epoch 生效高度为 `A(e) = 1 + e × blocks_per_epoch`。对非 genesis epoch，网络固定整数 `W`，满足 `3 ≤ W < blocks_per_epoch`。签署锚点为已最终确定的 `A(e)−W−1` 区块。只能从下一块开始签发与提交，到 `A(e)−2` 截止，过期证书不得转用于其他 epoch。每个 epoch 最多接受一个快照。Registry 序号不可倒退，同序号只能对应相同承诺。后续快照必须延续前一已认证 L1 锚点。深重组跨过该锚点时停签新授权，并重新核验仍受锁定的质押和桥负债。已确定的 Fern 历史继续有效。

对实际合格的代次，按质押从高到低排列，公钥字节序打破平局，取配置规模。票权为 `bonded_shannons / shannons_per_power` 的整数商。非空、最小质押、正权重和总权重上限全部是确定性检查。genesis 委员会通过同一认证路径直接生效。正常交接在 `FinalizeBlock(A−2)` 发出 `ValidatorUpdate`。旧委员会签名的 `A−1` Header 绑定新 `NextValidatorsHash`，新委员会签名的 `A` Header 绑定实际新 `ValidatorsHash`。交接证明包含这两个 Header 和签名，以及快照与更新执行证明。缺少新快照时，只有旧委员会对应质押仍锁在 L1 的情况下，旧委员会才能继续按原权重执行。 [CometBFT v0.38.26 ABCI 方法](https://github.com/cometbft/cometbft/blob/v0.38.26/spec/abci/abci%2B%2B_methods.md#finalizeblock)

### 退休的不可逆性和签名人轮换

1. **请求退出。** 所有者在 L1 更新指定代次，资金继续锁定。单纯因排名下降而离开委员会，只结算当期活跃区间。正式退休还需要认证过的 L1 退出请求。
2. **提交终止记录。** Fern 通过满足签署和提交窗口的快照认证该退出请求。在排定该代次退出的区块中，原子写入 `(registry_id, validator_pubkey, stake_generation, removal_height)` 终止记录。所有后续提案检查这一累积承诺，在计算委员会前排除已排定退休的代次。已提交的终止记录永远禁止该代次重新进入委员会。Fern 保留可查询的紧凑终止标记，L1 清理完整历史记录时无需删除此标记。
3. **认证实际退休。** Guardians 验证终止记录、`A−1` 与 `A` 的交接证明及实际移除后的委员会。原来签发但尚未提交的候选快照，即使 Registry 序号满足条件，也无法恢复该代次的权重。没有完成 H+2 交接时不得签发退休证书。对从未入选的代次，需证明其既不在活跃集合也不在未来集合，并在同一条 Fern 终止记录中禁用该代次。
4. **执行 L1 退出。** CKB 合约接受当前退休认证方对完整证书的签名。证书绑定上述终止记录、Fern 最终区块及 Registry 代次。合约用相对区块锁等待解除质押期。等待时间覆盖发现、提交和在 L1 纳入证据的最低窗口。提现和清理均不得越过仍开放的责任。

轮换记录绑定旧代次、新代次、授权集合、目标 Fern epoch 和允许在 CKB 生效的最早高度。旧、新双方分别超过三分之二签名，登记待生效的 L1 轮换计划。较早 epoch 的已认证 Registry 快照把轮换计划提交到 Fern，必须赶在目标签署窗口之前。快照签名权限以该已提交计划的**目标 epoch**为准，旧代次签名不适用于生效 epoch 及以后。CKB 通过消费 Registry 执行单独的生效操作，以 CKB `since` 约束最早生效高度。退休证书以该 L1 操作确认后合约**执行时**授权的代次为准。未执行的旧证书需由新集合对同一终止记录重新认证。若两侧授权暂时不同步，退休签发暂停，已有终止记录和锁定约束继续生效。桥认证和金库权限凭各自流程轮换。

### 双签、罚没和停摆

L1 验证真实 CometBFT canonical vote sign bytes。两票需对应同一 chain ID、公钥、高度、round、vote type，不同的完整 Block ID 和各自有效签名。证据高度必须落入该质押代次已认证的实际活跃区间，包含退出等待期内仍可追责的历史区间。证据键按投票位置去重。罚没额与去向由网络规则指定。证据通过后，L1 冻结罚没部分。Fern 认证冻结状态后将该代次排除出候选集合，并以与退休相同的机制提交终止记录。实际移除该代次前，资金继续作为 L1 受约束资产锁定。完成移除后才执行资产结算。现有开发链已实测立即全额罚没流程，冻结与延迟结算仍需接入。 [CometBFT v0.38.26 投票签名源码](https://github.com/cometbft/cometbft/blob/v0.38.26/types/vote.go)

活跃权重不足以形成超过三分之二的签名时，Fern 接受停摆。L1 可接收证据并冻结资产。停摆期间的 L1 处罚不会跳过委员会交接，也不能替 Fern 选择后续区块。原网络在当前委员会恢复合法法定人数后，按 CometBFT 的正常规则从最后已确定区块继续。若长期无法恢复，另建网络需要新的网络身份和 genesis。新网络只能使用独立储备，原金库负债先按已验证的原历史核对，同一笔资产只能承担一条历史的兑付。首轮方案没有同身份紧急换委员会的应急路径。

## 6. 桥、储备与提款

每条提款通道由 `CKB genesis + L1 vault Type ID + 资产身份` 唯一标识。原生 CKB 和每种启用的 L1 xUDT 各有独立的金库状态与 nonce。xUDT 身份承诺其完整 L1 Type Script，覆盖 hash type 和所有 args。Fern 的 Capacity 在应用层统一流通。各通道可用的 L1 CKB 容量与已经预留的容量必须分别入账。

- **入桥。** L1 存款归集、确认之后，由存款 guardians 认证存款 OutPoint、金库、金额、Fern 收款 Lock、L1 锚点和网络。Fern 只对未使用过的证书授予准确的容量与资产增量。所有实际输入、输出与相关 Type Groups 仍按原生规则处理。
- **原子申请。** 一个 Burn Cell 对应一张票据。原生 CKB 的 Burn Cell 无桥接 UDT，xUDT Burn Cell 只承载一种已注册的桥接 UDT 和它自己的 CKB Capacity。多个资产用同笔交易中的多个 Burn Cells 提出申请。多个 Type Script 仍能约束同一个 Burn Cell。每个 Burn Cell 的全部 Capacity 只计入本票据的 L1 支付金额一次。合约将原生 CKB、UDT 数量和附带的 L1 Capacity 分开验证。票据沿交易输出索引顺序在各自通道分配连续 nonce。所有票据与通道预算共同提交，实际 L1 兑付分别进行。
- **小额聚合。** 多个小额 Fern Cell 可按同一资产通道和同一 L1 收款脚本合并为一个 Burn Cell。每个输入照常签名和验证。达到 L1 输出最低 Capacity 后才可受理并分配 nonce。聚合在 Fern 交易内完成。
- **入队校验。** 验证 L1 收款模板和参数、准确输出及其 occupied Capacity、原生资产配置和足够的通道可用余额。拒绝会被 L1 金库解释为存款或 escrow 的收款脚本。校验失败则整笔申请拒绝。若通道需要更多 CKB Capacity，先完成已认证的 L1 储备调拨，再受理新申请。
- **L1 兑付。** 金库保管人核对 Fern 最终性、执行结果及票据，合约约束通道的 nonce、实际收款输出与金额。金库轮换保持票据与资金可兑付。一个通道出现异常时停止该通道的新申请，并保留其队首票据及完整负债。其他通道继续处理各自的 nonce。
- **对账。** 对 CKB 及各 UDT 分别维持 `可兑付 L1 储备 ≥ Fern 流通负债 + 未付的已提交提款 + 应付手续费`。此外，每个通道预留的 CKB Capacity 和 UDT 数量必须足以兑现该通道的待付款。申请把流通负债转成待付款，在同一提交中预留通道余额。经认证的 L1 兑付同步减记储备、待付款与预留。占用的金库底仓、外部交易费和质押资金分别计量。同一笔 CKB 不得计入多个通道可兑付余额。

Fern 验证者质押、存款认证、检查点认证和金库托管各自拥有独立权限。共识安全依赖各高度的 CometBFT 权重假设。L1 状态认证依赖 guardians 对 CKB 全节点的核验。实际提现仍遵守 L1 金库权限。

## 7. 状态承诺、执行回执与锚定快照

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

### 增量状态树和系统状态根

Live Cells 用规范的二进制 Patricia 树。键为 `H("Fern Live Cell key", network_id, canonical_out_point)`，按最高位至最低位判定分支。分支在共同前缀之后第一个相异位分叉。节点哈希承诺前缀长度、规范前缀和有序子节点。叶子绑定完整键与规范记录。空树、分支和叶各用独立哈希域。给定同一键值集合，插入顺序与存储布局产生相同的根。

```text
record = (canonical_cell, creation_height, creation_time_ms, creation_context_id)
leaf   = H("Fern Live Cell leaf", network_id, cell_key, canonical_out_point, canonical_record)
```

创建高度和时间直接用于 `Since`。创建上下文 ID 绑定产生该 Cell 的高度、共识时间、父 Header ID 与交易根。每块只更新实际消费、创建的键及其受影响路径。快照安装时允许顺序重建整棵树，并用全集重建作为增量更新的测试对照。

系统状态根绑定独立的命名空间根，覆盖已使用的存款证书与认证方代次、桥储备、各通道 nonce、待兑付负债、应付手续费、质押快照、当前及已排定委员会、退休终止记录、认证方轮换、委员会历史和可引用的历史检查点。新记录的可引用检查点只指向此前已提交的 Header。协议状态记录与 Live Cells 共享一次原子提交。

### 历史 Header 累积器和执行回执

历史 Header 累积器使用 Merkle Mountain Range（MMR）。创世高度 0 的历史根承诺空序列。提交高度 `H` 时追加 `HeaderID(H−1)`，叶哈希同时绑定网络身份和叶对应的高度。高度 `H` 的历史根承诺从创世到 `H−1` 的 Header。历史根为 `H("Fern history root", network_id, leaf_count, ordered_peaks)`，峰值按从左到右编码。峰值数量和位置由叶数唯一确定，证明只接受规范顺序和指定高度。历史包含证明验证单个 Header，追加一致性证明验证后续历史完整延续已接受的前缀。 [MMR 包含与一致性证明](https://datatracker.ietf.org/doc/draft-ietf-scitt-receipts-mmr-profile/)

交易根承诺包含 Witness 的规范交易。执行回执记录区块内交易位置、交易 ID、完整交易哈希、实际收取的费用、已计费资源与协议结果。提款等结果写入规范回执数据。交易与回执各使用有域分隔的有序 Merkle 树。递归构树时，左子树包含小于当前叶数的最大二次幂个叶，右子树包含其余叶。回执叶绑定高度和位置，根绑定叶数。证明某笔交易的提款或执行结果时，验证同一 Header 下对应位置的交易与回执。拒绝且未提交的交易没有链上回执。

### Header 与 AppHash 的计算顺序

```text
HistoryRoot(H) = Append(History(H−1), HeaderID(H−1)).root
Context(H)     = (H, time_ms, HeaderID(H−1), TxRoot(H))
ContextID(H)   = H("Fern block context", network_id, Context(H))
HeaderID(H)    = H("Fern Header", network_id, Context(H), StateCommitment(H), ExecutionCommitment(H))
AppHash(H)     = H("Fern AppHash", network_id, H, HeaderID(H))
```

执行前先确定交易根与区块上下文。执行时新建 Cell 只引用这个上下文 ID。执行完成后计算 Cell 根、系统状态根和回执根，再构造完整 Header。历史累积器本块追加的 Header 属于前一高度。三者依赖方向固定。CometBFT 的 `H+1` Header 认证高度 `H` 的 AppHash。轻客户端验证 `H+1` 的合法提交和相应委员会，然后用提供的 Fern Header 重算 AppHash 并比对。状态、交易及回执证明对照该 Fern Header 的承诺验证。 [CometBFT v0.38.26 ABCI 方法](https://github.com/cometbft/cometbft/blob/v0.38.26/spec/abci/abci%2B%2B_methods.md#finalizeblock)

### 锚定快照、历史证明索引与裁剪

锚定快照提供高度 `H` 的原生 Header、对应的 BFT 认证、规范 Cell 与协议状态记录、历史累积器的叶数和峰值，以及继续验证所需的委员会交接及历史检查点。客户端从可信锚点核验 Header 和 AppHash，重建 Cell 根与系统状态根，用叶数和峰值核验历史根，全部匹配后原子安装。篡改 Cell 创建时间、通道负债、峰值或委员会材料均使恢复失败。已认证的回执根随 Header 保留。历史回执正文来自归档证明。

`ProcessProposal` 与 `FinalizeBlock` 共享执行内核。`Commit` 同步提交 Cells、更新的树路径、协议状态、Header、累积器叶数和峰值及提交点。普通节点按已验证的检查点及保留策略自动裁剪历史区块、回执和 Header 正文，保留当前及近期 Header、未结负债、委员会交接及继续验证所需的数据。归档节点维护完整历史与证明索引，索引可异步重建，节点出块无须等待归档索引写入。

历史 Header Dep 以仍可引用的已认证检查点为锚，随交易提供 Header 与包含证明。最近已提交的父 Header 可直接从一致的当前状态读取。引用规则取决于协议定义的锚点和高度，各节点的本地裁剪进度不影响验证结果。钱包和 RPC 使用 Fern 原生地址与交易格式，支持按 Lock、Type、OutPoint 查询以及获取状态、交易、回执、提款和委员会证明。

### CKB PoW 检查点锚定

部署独立的 CKB Checkpoint Type ID Cell。每次更新消费上一 Checkpoint Cell，验证唯一继任者、严格递增的序号与 Fern 高度，以及上一检查点的 OutPoint 与承诺。新记录绑定 Fern 网络和 CKB genesis、Fern 高度及最终区块完整 Block ID、原生 Header ID、AppHash、历史根及叶数、承载该 AppHash 的后续 CometBFT Header 身份、当前和下一委员会承诺、质押快照承诺及委员会交接证明的摘要。

锚定认证方独立于金库。它核验从上一锚到新锚的 Fern 最终性、对应 AppHash、新旧历史根的追加一致性、旧锚的 Header 在新根中的包含性及期间的每次委员会交接后，按专门签名域对记录签名。CKB 合约验证当前固定锚定认证方超过三分之二签名与继任顺序。轮换由旧、新集合分别认证。该首轮合约由认证方证明 Fern 历史，签名责任之外的证明仍需客户端独立复核。

客户端从配置固定的 Checkpoint Type ID 与信任期内的 Fern 检查点起步。先用完整 CKB 节点或已验证的 CKB 轻客户端确认锚定交易包含在满足确认策略的有效 PoW 链上，再校验 Type ID 继任路径、Fern AppHash、新旧历史根的追加一致性、上一已接受锚的 Header 在新历史根中的包含性以及跨 epoch 的完整交接证明。证明缺失、历史与已接受锚冲突或超出信任期时，拒绝自动跟随。已确认的 CKB 记录给客户端一个可追溯的外部历史约束。Fern 最终性仍由 BFT 提交确定。历史包含证明和执行回执分别认证 Header 和该 Header 下的执行结果。CKB 深重组时冻结该锚定源的后续接受，保留先前已验证的 Fern 最终性判断。首轮委员会按已认证质押确定，PoW 熵抽样留作后续研究。 [CKB 轻客户端参考实现](https://github.com/nervosnetwork/ckb-light-client)

## 8. 接入顺序与交付验收

| 优先级 | 接入 | 实际验收 |
| --- | --- | --- |
| P0 | Cell、VM、时间、签名和共识切换 | 四副本使用同一原生交易格式。删除 CKB Header 旧字段与 DAO 容量例外，验证创世、同块消费、过期高度、时间跳跃、重启和跨 epoch 的边界。钱包与 RPC 支持该格式。串行重放和并行验证结果一致，拒绝冲突及恶意提案 |
| P0 | 质押终止、快照窗口和罚没 | 真实 L1 注册、增押、退出及双签。拒绝抢跑退休、先签后提交的旧快照、过期签名与退役认证方。在丧失法定人数的实网确认 L1 可冻结而 Fern 不发生强制换组 |
| P0 | 状态与执行承诺、桥接守恒 | 真实 CKB 与 xUDT 往返、储备对账、金库轮换。多资产原子入队。小额聚合后达到 L1 最低容量才入队。某通道暂停时其他 nonce 正常推进，拒绝重复使用 Capacity。验证历史包含及追加一致性、回执位置、H+1 认证和锚定快照，拒绝被篡改的创建元数据与负债 |
| P1 | 裁剪、归档证明与 Capacity 定价 | 在不同 Live Cell 总量下固定每块变更量，分别测量状态树、回执树、历史累积器、读写放大与提交延迟。验证裁剪后的 Header Dep、证明重建、重启和状态同步。用结果确定开发网抵押成本 |
| P1 | CKB Checkpoint Cell | 真实 L1 发布和轮换。验证客户端接受合法锚和交接证明，拒绝冲突、缺漏历史与失效分叉 |
| P2 | 多维费率及净盈余分配 | 对齐确定性计量、费用上限、应付费收与真实 L1 出账，验证分配后的储备覆盖 |

压测分别运行 CKB 支付、单 Type Script、双 Type Script、多 Type Script、跨资产结算及委员会变更。按能力逐步提高投递速率至 1,000、2,000、3,000、5,000 TPS，记录 offered、admitted、finalized 和全副本可见速率、p50/p95/p99、客户端丢弃、CPU、RSS、物理写入、fsync、网络流量及积压。历史账本单独对照。

Many-to-Many 的边际成本采用控制实验。固定输入数量和数据、输出数量、Lock 分组、签名算法与密钥、依赖读取、数据总尺寸和 VM 配置，改变独立 Type Groups 的数量。每个变体按真实交易内容重新签名。等长夹具能做到时使用等长编码，其他差异应记录。分别记录 Lock 与 Type 的 cycles、缓存冷暖、上下文载入、状态根更新及完整四节点吞吐，保留原始数据、命令和构建指纹。

## 9. 当前实现与实测证据

| 项目 | 已验证 | 尚待集成 |
| --- | --- | --- |
| 原生执行 | `fern-ledger` 的 Molecule、组推导、当前按字节 Capacity、承诺和 Merkle 证明。独立 RISC-V 脚本与真实 secp256k1 签名通过，串行与四 worker 周期一致，Spawn/Exec 可读取原生组上下文 | `fern run` 的完整切换、组级 Witness、原生 RPC、简化 Since、可定价抵押、Patricia 状态树和统一承诺 |
| CKB L1 质押 | 已部署 Type ID Registry。真实锁定和增押、拒绝过早提现，已执行 250 CKB 罚没及另一笔 200 CKB 未处罚质押的提现。监护方已完成轮换 | 代次与责任窗口清理、实际 CometBFT 历史认证、罚没延迟结算、网络级交接 |
| 快照认证 | 三个真实 guardian 签名通过完整 Registry 校验，两个签名被拒，离线导出权重 250 | 快照窗口、退休终止记录、轮换生效、ABCI 更新及原生 genesis |
| 历史证明与回执 | 当前独立账本已有交易及 Live Cell Merkle 包含证明 | 历史 Header 累积器、执行回执树、锚定快照、裁剪和历史证明索引 |
| 四节点和桥 | 原账本四节点 CKB/xUDT 往返、金库轮换、重启、执行证明与状态同步。已拒绝会把提款重新归入 xUDT escrow 的收款人 | 在原生账本验证以上行为，接入独立提款通道与 CKB Checkpoint Cell |

本轮核对到的代码边界如下。`fern run` 仍由 CKB `HeaderBuilder` 构造 Header，并向 CKB `CapacityVerifier` 传入 DAO Type Hash。独立 `fern-ledger` 的 `Since` 仍解析 CKB 位字段，包含 epoch 分数和秒转毫秒逻辑。简化时间、原生 Header、移除 DAO 容量例外和新费用规则均为待接入的协议变更。 [现行 Header 构造](src/genesis.rs#L293) · [现行容量验证路径](src/engine.rs#L400) · [当前独立账本的 Since](ledger/src/lib.rs#L571)

三个原生交易夹具的实测 VM 成本分别为单资产 1,938,965 cycles、身份与生命周期 1,931,200 cycles、五个 Type Groups 的跨资产结算 2,020,121 cycles。第三个夹具比第一个高约 4.2%。夹具还有业务、输入和输出结构等差异，边际开销仍待控制实验。原账本四节点控制组投递 64 TPS，768 笔均在四副本可见，实测最终确认为 56 TPS。全副本可见延迟 p50/p95/p99 分别为 2.86、6.47、6.97 秒，包含一次节点重启。尚无原生四节点或 1,000–5,000 TPS 的确认记录。 [架构和 ABI 实现](ARCHITECTURE.md) · [性能记录](BENCHMARKS.md) · [质押复现命令](OPERATIONS.md#exercise-the-ckb-stake-registry)
