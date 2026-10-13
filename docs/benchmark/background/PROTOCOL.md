# Native Memory 与 RecallLedger：行为、长期变化、迁移和性能的比较设计

日期：2026-10-13，Asia/Shanghai。负责人：沈知远；评估审阅：方衡之；Host 接入审阅：程知行；反方审查另记。以上均为 AI 岗位。

状态：**H0，工程可执行性设计，未冻结、未运行对照实验。** 本文新增于 10 月 11 日历史提案（早期草案未收入本发布包；后续方法以当前规格为准），不覆盖旧记录。当前源码固定为 `6ff7f5824aa235046d27c18c971fe9f3b1dfa8ef`。本轮未启动模型、修改用户 Memory 或 Host 设置、发布新代码。

## 1. 要检验的主张

用户目标是减少长期 Memory 失真、控制行为退化、失败后修复，以及跨 Agent 品牌的工作连续性。因此需要检验：

1. 在相同可见信息与资源上限下，RecallLedger 是否改善有实际产物的任务成功率？
2. 纠正、撤回和干扰累积后，是否减少陈旧信息误用，且没有靠拒绝更多任务得到优势？
3. 增益来自 Memory 生命周期机制，还是更多原文、更多 token、人工整理或更好的 Host 接入？
4. 从 A 移到 B 后，是否减少重新解释、恢复等待与任务损失？
5. 相同质量要求下，总延迟、资源占用与每成功任务成本是否可接受？

“能保存”“能查到”“最终进入模型上下文”“被正确使用”是四件事，分别观察。不能以实现语言、单元测试数量或文本逐字保留推出用户效果。

**当前最重要的反假设：**简单的“保留原始历史 + 当前版本过滤 + 搜索 + 按需重建”可能同样有效且成本更低；Native Memory 可能在稳定日常任务上更紧凑、更快。实验必须允许这些结果。

## 2. 测量对象与公平性

### 两条独立 Track

| Track | 固定什么 | 比较什么 | 能回答什么 |
| --- | --- | --- | --- |
| Controlled Harness | Agent runtime、reader model/version、tools、prompt 模板、可见事件、预算、更新与准入政策 | 不同 Memory backend；必要时共享提取结果 | Memory 机制是否有净贡献 |
| Real Host | 同一个产品与版本，尽量固定模型、任务、工具、账户条件；配置及隐藏变量如实记录 | Native 与 RecallLedger 的实际接入配置 | 用户装上之后是否更好 |

Controlled Harness 中的 MD 基线叫“受控 MD backend”，不能称为某厂商真实 Native Memory。Real Host 中无法固定模型或观察上下文的 App，只能报告完整产品配置差异，不能声称隔离了 Memory 的因果贡献。

每个 Host 内配对：`Host X Native` 对 `Host X + RecallLedger`。不同品牌得分不直接相减作为 Memory 效果。不能为了让模型、文件路径或读写次数一致而破坏 Native 的正常机制。

### 对照组

| 配置 | 定义与作用 |
| --- | --- |
| N0：无长期 Memory | 保留当前任务和正常工具；只作 Memory 依赖性诊断，不作主要竞争对手 |
| N1：Native default | 当前版本按实际默认设置，记录是否真的开启；衡量开箱体验 |
| N2：Native tuned | 保留官方支持的搜索、摘要、纠正和刷新；只在开发集用同等调优预算设置，为真实产品主对手 |
| C-MD：受控 MD backend | 在 Controlled Harness 中由固定更新政策维护的 MD、搜索与刷新；只能称 MD 工程基线，不能冠以厂商 Native 名称 |
| R0：Raw-history retrieval | 同样的原始信息与保留权利，当前版本/删除过滤，词法或 Hybrid retrieval，必要时按需重建 |
| R1：Simple lifecycle baseline | 在 R0 上实现相同可见 revision、dependency 和撤回合同，检验复杂实现是否必要；属于待实现的工程基线 |
| L：RecallLedger | 固定源码、adapter、admission、retrieval 与 evidence policy；把所有前后台工作入账 |
| N2+L | 只在 Host 支持并能记录读写冲突时测试，单列为组合配置，不冒称已替换原生 Memory |

Controlled Harness 首轮采用 N0、C-MD、R0、L；Real Host 首轮采用该 Host 的 N0、N2、L，只有 R0 通过同一 Host 接入资格检查才加入。N1 用于开箱体验补充。N1/N2 是真实产品配置，不能出现在仅模拟 backend 的受控成绩表中。两条 Track 分表，Controlled Harness 的胜出不能标题化为“战胜 Native Memory”。正式声称“生命周期机制优于简单办法”之前须加入 R1，不能只打败不懂纠正的 RAG。

所有系统接收同样的原始历史。Controlled Track 的子实验可以向所有 backend 提供同样的结构化事实、修正目标和依赖图，但结论必须排除提取/admission 能力。Real Host 中不向 RecallLedger 独家提供 gold fact key、来源依赖或人工答案。

需要记录哪些依赖是系统自己发现、事件显式声明或人工作出的。增加“依赖没被登记”“用户原话含糊”的用例，因为已知 dependency graph 的正确性不等于理解了现实中的所有依赖。

## 3. 用“项目历史 → 状态变化 → 实际任务”测试

每条历史包含原始事件、来源文件、时间、作用域、模拟环境状态、任务和 Host 生命周期事件。隐藏评分器持有相应时点的合法状态与验收条件；其 gold 不进入 Agent 的文件、Memory、检索库或工具响应。

隐藏不能仅靠不同文件名或不同 namespace。执行时 gold、未来事件、其他配置的 trace 和 Judge 输出置于被测进程不可访问的独立评分边界，通过容器/账户/工具文件白名单等实际访问控制隔离；先用无害哨兵验证读不到。若真实 Host 无法建立这种边界，该环境不能用于声称无泄漏的封存验证。评分只能返回约定的任务反馈，不把隐藏答案送回后续历史。

评测不只问“你还记得端口是多少”。例如：

1. 项目 A 使用测试端口 8123；项目 B 仍使用 8123。允许修改测试配置，生产修改需要另行授权。
2. 插入无关工作和相似名称的项目，让 Agent 曾生成摘要、计划或读过旧信息。
3. 将 A 改为 9234；后续再撤回一条依据。另设历史版本查询，确认系统不会把所有旧事实误删为错误。
4. 要求继续完成 A 的测试配置与验证。检查实际写出的配置、调用的模拟服务和最终测试结果，同时检查 B 和生产配置未被误改。

所有动作都在一次性 sandbox 内，生产地址只是模拟对象。任务中的环境检查不能把答案白送给无 Memory 组：保留只有历史可提供的信息，也保留正常工具可重新发现的信息，分别计分。

### 测试族与反例

| 测试族 | 主要困难 | 必须同时测的反例 |
| --- | --- | --- |
| 稳定工作 | 长历史、噪声、跨 session | 简单 Native 可以做好的日常任务 |
| 条件与语义 | 否定、例外、日期、数字、同名 scope、冲突来源 | “测试允许”不可扩大成“生产允许”；未决冲突不能强行合并 |
| 更新与撤回 | source → summary → plan 的变化传播 | 无关分支仍可用；合法历史查询仍正确；有替代依据时不能全部拒答 |
| 失效与修复 | worker 停止、迟到结果、断线/重启、并发更新 | 正常无变化，以及大部分来源同时变化 |
| 预算与规模 | 小 context、大来源闭包、深 dependency graph | 原文保留超限、合法可回答却被拒绝、检索位置偏差 |
| 任务迁移 | A→B 与 B 修改后 A→B→A | 旧 snapshot 重放、撤回复活、scope/conditions 丢失 |
| 负面迁移 | 相似但无关项目、旧经验被过度套用 | 只改变无关历史时，当前正确行动应保持 |
| 不可信内容 | Memory 中夹带指令、伪造来源或不支持的推论 | 正常相似文本的误拒；不把引用出处当真值认证 |

Challenge suite 与接近日常分布的任务集分开报告。前者刻意放大纠正/撤回，后者记录实际来源及事件分布；不能把高频故障集的平均提升当成普通用户收益。

### Session 生命周期

每个主要配置分别测：

- **Hot session：**Agent 已读过旧信息，更新后继续当前工作。
- **After compaction：**旧事实可能进入压缩摘要，再接收更新和新任务。
- **Fresh session：**只依靠可持久化的 Memory 和正常来源工具。
- **Process restart：**进程重启后的恢复、延迟队列与旧缓存。

不得只给 RecallLedger 清空旧上下文。若真实 Host 没有可控 compaction，记录自然发生的边界，或将该格标为不可控/未覆盖，不能模拟后冒称真实 Host 通过。

## 4. 核心指标与失败判据

| 指标 | 定义 |
| --- | --- |
| Valid task success | 在截止前完成指定产物/行动，且没有版本、scope 或约束违规；全部预定任务进入分母 |
| Stale-use rate | 在存在旧信息诱导的任务中，错误采用旧值的答案或实际行动比例 |
| Supported coverage | 具有充分合法依据的任务中，系统提供并应用了充分依据的比例；检索覆盖与行动成功分别列 |
| Unnecessary refusal | 存在足够合法依据，却拒绝/返回空/超时/长期 pending；不能从主分母排除 |
| Correct abstention | 依据不足任务的正确停止或澄清；单独分层，不用大量拒答任务稀释可回答任务的失败 |
| Benign retention | 更正/撤回后，未受影响的事实和任务保持可用的比例 |
| Memory-induced regression | 固定难度的哨兵任务在检查点发生的退化，以及相对最强基线的配对变化 |
| Migration gap | 同一目的端 B 上，迁移组相对 B 连续使用相同前史的任务成功率差 |
| Cost per valid success | 完整运行成本 ÷ 有效成功任务数；零成功时没有有限值 |

不汇成任意加权总分。至少展示 task success、stale-use、refusal/coverage、total cost、latency 五项及分组置信区间。

两种时钟不可混淆：

1. `event visible → 安全且足够的 Memory 可用 → 首次正确行动`：完整系统恢复。
2. `update commit acknowledged → 此后开始读取的结果`：Store 的一致性合同。

异步系统不得在 backlog 清空后才开始计时。未恢复样本保留为未恢复，报告截止时未恢复率；不能从恢复时间统计中删去。新读取不暴露旧内容，也不表示已经送进旧 prompt 的内容被清除。

判分优先用工件隐藏测试、模拟工具调用及状态断言。开放语义题用预定 rubric、盲化配置名称的 reviewer/judge 和抽样复核，保存争议；不能只让被测 Agent 自评。Judge 也不能知道未来答案，且费用进入研究预算。

## 5. 两个关键归因实验

### A. Counterfactual history

同一任务建立两条隔离历史，仅改变与答案有关的一项合法事实；正确行动应随之改变。再建立仅改变无关事实的版本，正确行动应保持。加上 N0，检验任务是否真的依赖 Memory，而不是模型常识、题目暗示或现查工具。

同源反事实分支属于一个 history cluster，不算独立用户。测试问题/回答/评价不回流主历史，检查点探针在只读副本执行。自然闭环任务另设 Track；所有组统一是否回写，完整计算回写成本。

### B. 在同一 checkpoint 做诊断干预

失败轨迹按以下可观察链定位：

`原始事件 → extraction/admission → 存储/派生 → retrieval → 实际 model input → 行动/产物`

在隔离副本中，使用同一 reader/tools/prompt 框架与预算，将 Memory 提供部分替换为评分器选出的充分、合法来源材料；不给最终答案，也不回写。这是独立诊断组。

- 替换后改善：支持失败发生在 Memory 提供证据的路径上，继续区分 extraction、retrieval、metadata 和 Host 注入。
- 正确依据实际送达后仍失败：Reader 理解、工具/环境或任务本身可能是原因。
- 黑盒无法查看实际 input：定位层标“未知”；一次随机 rerun 翻转不能证明单一因果，需要配对重复和差异区间。

Golden evidence 不是主比较成绩，也不是已证明的数学上界。受控轨同时保持参考块格式一致，避免“来源内容变好”与“指令包装变强”混在一起；若预算容不下充分材料，标明诊断条件不足。

## 6. Performance 需要测两层

### Memory service 层

对支持相同操作且可独立调用的 backend，记录：

- ingest、query/recall、revise/retract、repair、export/import 的 p50/p95、失败率及满足 SLO 的有效吞吐；
- cold start / warm cache，单用户及并发读取、后台修复和写入竞争；
- CPU time、峰值 RSS、磁盘和 WAL/索引增长、队列积压、故障后的恢复时间。

候选负载网格为 1k/10k/100k 条记录、并发 1/4/16；先用小规模校准，不直接并发铺满组合。记录长度、query selectivity、匹配率、依赖深度/扇出、更新比例、vector 维度、fsync/durability 和缓存条件保持可比。原文保真检查不得为了速度关闭。

用固定到达计划并记录排队时间，防止只在上一请求完成后发下一请求而低估过载尾延迟。操作类型分别报告，不能让大量快速失败拉低总体延迟。样本不足不报告可信 p99。

预计算 vectors 的测试只能报告索引/检索耗时；embedding、reranking、初始化和重建另外测量并回到完整账目。Native 没有独立 API 时，不能拿 Rust RPC 对比读一个 MD 文件的时间，声称整体更快。

### Agent 端到端层

计量用户任务可见开始至产物验收完成的 wall-clock，以及 reader、embedding、extraction、summary、repair 的 token/请求/时间；并记录人工介入分钟数。

`online cost = 初始化摊销 + ingestion/admission + retrieval/reranking + consolidation/embedding + maintenance/repair + reader/tools + 所有失败/重试`

初始化摊销必须给出任务数假设，同时报告未摊销总量。研究调优与 Judge 费用另列，全部计入项目总预算。无法取得 token 或账单的黑盒 App 写未知，不以 tool 返回字符数代替真实模型输入费用。

任务超时不结束成本记账：截止时记录队列与进行中请求，按照冻结的取消/限时 drain 政策回收资源，所有迟到和取消失败的费用仍计入该历史。无法核清的后台尾部成本记为未知/下界，不公布精确总成本排名。预算超限、adapter 截断、Store 拒绝、输出 packet 超限分别留因果事件，不能合并成“没有相关 Memory”。

分别扫描 Memory context budget，例如 2k/8k/32k tokens，固定 tokenizer，包含 source metadata、JSON 包装及读者实际收到的全部 Memory 内容；其余任务上下文和输出额度统一保留。RecallLedger 的 `max_chars` 是字符约束，不能充当 token 公平性口径。

随后通过文件读取、search、tool response 回溯原文的内容也必须计入每次 reader 实际输入和累计任务预算，不能绕过首次 Memory block 限额获得免费上下文。每组 overflow/fallback 策略须在开发集后冻结：预算不足时如何重新检索、拒绝或使用普通文本片段须显式命名并记账；runner 不得用 gold 选片段，不得静默截去 strict capsule 的条件后仍标为完整 verified evidence。

最终画质量—总成本—延迟曲线：比较等上限预算下质量，以及达到预定质量门槛时的成本。实际消耗可以不同，不能强迫高效系统浪费 token 来凑齐。闭源产品无法严格对齐时，报告限制而非制造“同预算”。

## 7. Longitudinal 与 Migration

### Longitudinal

历史长度、session 数、compaction 次数、来源变化次数、派生深度分别作为坐标；不要同时全变而无法归因。使用等难度、隔离 checkpoint 的哨兵，分别跟踪稳定事实、更新事实、条件限定、历史查询与未决冲突。

报告每个检查点成功率、相对 Native 的差值、纠正后错误复发次数、良性记忆损伤和每次恢复时间。压缩后的恒定测试分数也要结合覆盖率与任务难度解释。

加速回放数千事件只能说明该负载下的行为，不能称使用了数年。最终允许的结论是“在协议规定历史与时长内、给定置信区间下未超过退化容差”，不能声称永远不失真。

### Migration

固定目的端 B 的版本/model/tools/预算/后续任务，比较：

1. A→B：先在 A，后按被测 Memory 方案迁移。
2. B continuous：B 从开始使用相同历史与相同 Memory 配置。
3. B Native：B 持续使用强 Native 配置。
4. Native A→B：同一 A、同一前史与目的端 B，采用实际最佳可行的 Native 导出/文件转移/官方交接/来源重放，人工步骤计入；另配对应的 B Native continuous。缺乏可运行路径时记录能力缺口，不能声称已经量得 Native 的迁移质量。
5. B empty 与 B sufficient evidence：两个诊断组，独立报告。

分别测试共享同一 ledger 换客户端、另一 ledger 的 bundle transfer、Native 格式导入；三种场景不相互代替。公平提供相同可用工作文件、任务状态和必要环境，不给 RecallLedger 独家预制 task handoff。Native 允许其正常导出/文件复制；不存在等价功能时记录人工步骤与能力缺口，不能捏造适配。

`migration gap = success(A→B, evaluated on B) − success(B continuous)`

上述差值先在每种 Memory 配置内计算，再比较各配置的 gap，同时展示 B continuous 和 A→B 的绝对成功率。只有 RecallLedger A→B 对 B Native continuous 的比较不足以识别相对 Native migration 的收益。低质量配置前后都差，不能凭 gap 小获胜。

同时量恢复可用时间、迁移/重建总成本、重新解释次数和首次/后续任务成功。标准冻结前史包含相同 assistant messages；自然闭环产生的不同前史另作完整系统评测。

RecallLedger 当前 bundle 不搬迁 vectors 与 repair recipes，导入 evidence capsule 不能继承本地 verified 状态；目的端 re-embed/rebuild 和 handoff 检查必须计时计费。测试 512 KiB/1,000 objects 边界；超限失败计为容量限制，不当作完整大型迁移已支持。

最后在 B 纠正与撤回，再回到 A；检查来源/scope/conditions、pending、tombstone 和旧交接的失效。数据交换正确与真实 Host 完成接手是两张验收表。

## 8. 运行顺序、统计与判定

1. **接入资格检查：**固定 Host/model/backend 版本、配置与权限范围，确认每组实际读写路径、可见信息、可测 token/延迟；缺失格显式标记。优先一个能观察 dispatch/context 的 Host，项目中的 OpenCode adapter 可作为资格检查候选，尚未因此被判可正式测试。
2. **Pipeline smoke：**Controlled Harness 采用 8 个开发 history cluster，每簇 2 个 counterfactual 分支；4 配置（N0/C-MD/R0/L）×2 生命周期（hot/fresh）×4 checkpoints，共 512 次任务执行槽位，仅为建议规模。真实 Host 的 N2 试验另列，不借用此数。提取、后台处理、失败和重试还会增加 model calls；512 不是调用或费用上限。先完成极小子集再扩大；未批准执行模型预算。
3. **探索与消融：**用开发集加入 after-compaction/restart、R1 与 key ablations，校准评分、方差、成本和最小有意义改进。纯 source-preserving、显式 model summary、无自动 repair（仍遵守撤回）、全量/增量 rebuild、MCP-only/生命周期 adapter 分开配置。单纯关闭安全检查只作隔离故障负控。
4. **冻结正式协议：**按开发 variance、history cluster 相关性、primary opponent、最小有意义差异与 power 计算样本量。不要宣称 8、30 或 100 条天然足够。固定时间预算、max calls/tokens/storage、失败处理、调优次数、主指标、非劣容差与停止规则，经审稿后封存新历史。
5. **正式执行和分析：**按 history cluster 配对，model repeats 嵌套；随机交错配置运行顺序，记录 Host 更新、服务漂移、硬件温度/并发负载。用 cluster-level bootstrap 或匹配的模型报告效应量与 95% 区间，预先处理多重比较。不把同一历史的几百问题当成几百独立样本。
6. **真实跨 Host 验收：**受控结果成立后，再跑两个已通过资格检查的 Host 的 A→B→A；其他品牌逐个复制，不能外推。

正式胜出条件待冻结：Controlled Harness 相对 C-MD/R0/R1，Real Host 相对 N2，分别判断有效任务成功改进是否超过预定实际价值门槛，或质量非劣且成本/延迟有明确收益；同时 stale-use、unnecessary refusal 和良性损伤不过回归阈值。未观察到显著下降不等于通过 non-inferiority，需要差值区间相对预设 margin 判断。

出现以下情况必须收窄或否定优势：只胜过弱 Native；R0/R1 同效更便宜；优势只由更多 context 或人工 gold 带来；Raw preservation 正确但任务大量拒绝；热会话仍持续误用旧信息；迁移后 B 并未更快恢复实际工作。

## 9. 需要实现的评测 Harness 与目前已有产物

拟实现管线：

```text
version/config lock
  → isolated state factory + chronological event replay
  → interchangeable backend / real-host driver
  → actual model/tool traces + resource/cost events
  → hidden artifact verifier + calibrated semantic review
  → paired longitudinal/migration report
```

接口概念为 `reset / ingest / update / query / session_transition / snapshot / transfer / collect_trace`。这是测试 Harness 接口提案；Host 不支持的操作必须返回 unsupported，不伪造实施。

留存 `systems.lock.json`、`protocol.json`、`budget.json`、`public-events.jsonl`、隔离 `gold`、逐请求 trace、artifacts、独立 cost event、grader revision、每 history 结果及失败样例。通过事件 ID 关联 background jobs 与上游输入，避免双重计费。每次 run 绑定协议、代码、数据和种子指纹。

| 当前产物 | 本轮读到的能力 | 仍缺 |
| --- | --- | --- |
| `examples/fidelity_regression.py` | 两个真实本地 daemon；来源逐字/条件、64 次更新、预算失败/恢复和迁移开发回归 | 原生对照、真实 model/Host task score、服务性能与成本测量 |
| `examples/migration_handoff.py`、portable/handoff tests | bounded bundle、引用映射、冲突与交接校验 | 真实双 Host 的工作接受与迁移损失 |
| SDK/Host adapters | 受限接入与 fixture/daemon 验证 | 上述资格检查、完整 trace 和相同历史配对执行 |
| 本文及两份岗位审阅 | 对照设计与失败判据 | 统一比较 runner、封存数据、已批准模型预算、实际比较结果 |

### 原始会话试验前的具体阻断项

本轮接入代码审阅发现：当前 Host capture 调用 `candidate_submit`，没有通用 `candidate_resolve` 政策，也没有自动生成 fact keys 与 dependencies 的完整链路。因此“会话已捕获”不等于“可被未来任务使用的已准入 Memory”。

正式 Real Host 的 L 配置必须明确并实施 extraction、admission、纠正对象与依赖识别；按原始可见事件处理，预算与对手对称。不能让研究员阅读 gold 后人工补齐。手动审核是可测的 assisted 配置，需单列人力、延迟和错误；不能归入全自动产品结果。若尚未补齐，只可执行排除该能力的 Controlled 子实验，并将原始会话接入标为未就绪。

具体工程还需定义 raw capture → keyed proposal 的稳定 ID、来源及撤回关系：当前 `candidate_resolve` 不能给已捕获 candidate 补 fact key，同 event ID 改 payload 会冲突，逐条 accept 也不会自动识别语义纠正。完整资格检查见 [Host 审阅 H01–H13](HOST-REVIEW.md#5-首个-host-eligibility-清单)。

当前 adapter 的 retrieval 参数也不等于完整核心能力：共享 Host runtime 使用 lexical recall，默认 1,800 chars 和 4,096-byte packet；OpenCode 的默认配置需单独锁定。测试时须记录实际生效策略，不能因为核心支持 vector/evidence，就把 Host 试验写成已经启用相同策略。Codex 安装器会写入关闭 Native Memory 的项目配置，Native 对照必须使用独立干净项目/状态并验证正常启用，不能复用安装后的配置充当 Native 基线。

### 本轮核实的 OpenAI 官方基线条件

通过 OpenAI Docs 技能于 2026-10-13 搜索并实际读取 [Memories](https://learn.chatgpt.com/docs/customization/memories) 与 [Import from another agent](https://learn.chatgpt.com/docs/import) 相关正文，属 E2 文档审阅，未在用户 App 操作。

- Local Codex Memory 是背景生成，受 session eligibility、idle 时间和剩余额度条件影响；文档说明默认关闭。因此 default-off 要如实记录，但 N2 必须实际启用并核查产生了可用记忆，不能把 off 当成强 Native。
- 设两个观察窗口：满足声明整理条件后的正常召回，与固定任务期限下的更新恢复。等待条件/后台成本都记录，不把尚未执行整理的即时结果冒充最终保留能力，也不无限等待后再声称低延迟。
- 官方提供部分其他 Agent 的导入路径，包括 Claude Code project memories。迁移应逐方向/格式核实 Native 的最佳可行路径，不能假设所有 Native 都无法迁移；可导入的文档承诺不等于语义无损的实测。

本轮证据是项目代码/合同审阅（E2）；方案有效性和产品优劣为 H0/不确定。不新增论文复现或性能结果。旧提案的公共基准可用于问答诊断，但本轮未重新检索其版本，正式运行前应核对数据与许可证。

审阅材料：[统计与公平性](EVALUATION-REVIEW.md)、[Host 与当前实现差距](HOST-REVIEW.md)、[反方意见及修订复核](CRITICAL-REVIEW.md)。审阅支持进入评测工程实现，不等于协议已冻结或产品效果通过。
