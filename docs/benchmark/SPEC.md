# Benchmark 规格 v0.1

`benchmark_id=recall-ledger-memory-benchmark`；`version=0.1.0-design`。

状态：设计可供实现，尚未 preregister、执行或建立排行榜。RecallLedger 对照源码为 `6ff7f5824aa235046d27c18c971fe9f3b1dfa8ef`。修复产品后必须记录新 SHA，不能沿用该 SHA 的测试标签。

## 1. 研究问题与可证伪假设

在固定信息权限、Agent 能力和总资源条件下，Memory 是否帮助 Agent 持续正确行动？

| 主张 | 支持需要什么结果 | 会推翻或收窄主张的结果 |
| --- | --- | --- |
| 任务有效性改善 | 同 Host、同前史下有效任务成功提高，或质量非劣且成本更低 | 只对比未开启 Memory 的 Native；强 Native 同样好且更便宜 |
| 长期变化可控 | 相同难度任务的退化、旧错误复发减少且有效覆盖维持 | 拒答或 pending 增多导致“零错误”；只保住旧事实却不能吸收新知识 |
| 修复有用 | 原始纠正到正确行动恢复更快，未损伤无关任务 | Store 状态更新正确，但旧 prompt 继续导致错误行动 |
| 跨品牌连续性 | 同一目的端 B 的迁移损失、恢复等待/重述成本改善 | Native 最佳可行迁移同样好；数据导入成功而 B 无法继续任务 |

不设单一综合总分。不同 Suite、Track、Host、预算档位各自报告。

## 2. 四个 Suite 与一组前置合同检查

| ID | Suite | 主要输入/刺激 | 主要输出 |
| --- | --- | --- | --- |
| S1 | Task utility | 稳定历史、跨 session、条件/时间/冲突、隐式偏好、负面迁移 | answerable task success、条件违规、充分依据覆盖、correct abstention |
| S2 | Longitudinal & repair | 固定 checkpoint，纠正/撤回、压缩、迟到任务、重启与修复竞争 | checkpoint 曲线、复发、退化差、恢复时间和良性任务保留 |
| S3 | Migration & handoff | A→B、B continuous、Native 最佳迁移、B 更新后回 A | 目的端绝对质量、migration gap、恢复/重述成本和撤回复活 |
| S4 | Performance & cost | 同语义 service operations 和完整 Agent 任务 | service 分位数/吞吐/资源、端到端延迟、完整成本曲线 |

P0 为 CPU/offline 合同及 Harness 自检，不是第五个产品效果 Suite。既有 Rust/daemon 回归可以作为 P0 输入证据，不能计为 S1–S4 成绩。

## 3. Track 与配置身份

### Controlled Harness

固定 reader model/version/tokenizer、runtime、prompt role/位置、工具、可见历史、到达日程和资源上限；采用统一格式注入 Memory，避免包装成更高优先级指令。

| ID | 规格 | 地位 |
| --- | --- | --- |
| C_NONE | 禁用长期持久 Memory；仍有同样当前 session 与普通工具 | 诊断负控 |
| C_MD | 维护型 MD + 搜索 + 正常更新刷新；原始来源仍可访问 | 工程基线，不等于厂商 Native |
| C_RAW | 同样原文、当前版本/撤回过滤、词法/Hybrid retrieval、按需重建 | 强信息基线 |
| C_LIFECYCLE | C_RAW 加同等已声明 dependency、version、condition 合同，采用简单按需/full rebuild | 正式主要对手；必须实现或不提出机制净收益结论 |
| C_RL_EVIDENCE | 固定 RecallLedger evidence policy、adapter、调度、retrieval、预算 | 被测主要配置 |
| C_RL_MODEL | 显式模型摘要；固定模型/prompt，并计入完整重建成本 | 预定次要消融 |

两种输入模式分表：

- `admitted_events`：所有组得到相同已准入事件、identity、修正映射与显式依赖；只检验治理/检索/使用，排除 extraction。
- `raw_conversation`：只有相同原话/文件/工具观察，各方案自行提取准入。需要可执行 admission policy；gold 不参与。

### Real Host

| ID | 规格 |
| --- | --- |
| H_NATIVE_DEFAULT | 实际默认配置，包括默认 off；只测开箱体验 |
| H_NATIVE_TUNED | 正常启用的官方支持能力、开发集同等调优机会；正式主要对手 |
| H_RL_INTEGRATED | 已核验动态长期事实的读写/刷新所有权，完整自主 ingestion/admission；保留 Host 正常对话/执行状态 |
| H_NATIVE_PLUS_RL | 不能或不选择独占原生路径的组合配置；只能证明增量配置价值 |
| H_NONE | 可验证关闭长期 Memory 的诊断组，仍保留当前 session 正常上下文 |

H_RL_INTEGRATED 的资格不能由核心 API 通过替代。缺 autonomous admission 时使用独立 assisted 配置并计人力；不能用它填主要自主对照格。Host 的 native long-term 路径不能被安装副作用悄悄关闭后仍标为 H_NATIVE_TUNED。

首个 Host 为待资格检查的 OpenCode V1 候选，其他目标包括 Codex、本地 ChatGPT App 与 KiroCrew；不预先宣布兼容。具体版本、模型及 effective config 尚待锁定。无法固定模型或获得 actual input 的真实 App 可做黑盒效果测试，但不得输出未观察的模块归因/精确隐藏成本。

## 4. 数据集设计

### 数据单位与划分

一个 `cluster_id` 对应独立项目历史及其 counterfactual 分支；数值替换、措辞改写、随机 seed 和重复采样不增加独立 cluster 数。同源 template family 必须整体进入同一数据划分。

分为：

1. `examples`：本包公开的三个开发 cluster，仅用于格式/判分讨论。
2. `pilot`：候选八个开发 cluster，用于管线区分力、方差和成本校准；目前尚未构造。
3. `development`：用于 baseline 调优、overflow policy 和 ablation；设置对称调优上限，数据数量随预算冻结。
4. `sealed_validation`：独立项目/来源/template families、未用于开发；sample size 与数据均未供应。

S1/S2 采用 `challenge` 与 `everyday` 两个 reporting strata，独立结果表。Challenge 均衡覆盖八类困难；everyday 要有独立来源与分布依据，未取得时记未覆盖。不得将均衡困难集解释为日常事件发生比例。

八类任务以 [TASKS.md](TASKS.md) 为准：稳定保留、条件/时间/未决冲突、纠正传播、撤回与替代证据、故障修复、长期退化与压缩、作用域/负面迁移/不可信内容、跨 Host 交接。隐式偏好可作为稳定保留与条件任务的子类型，目前没有独立样例或覆盖成绩。

### 历史负载

设计档位为短/中/长三档，按 reader tokenizer 统计完整可见前史：8k、32k、128k tokens 为候选目标；精确事件数、正文长度分布、CJK/英文比例须在数据 manifest 中保留。这些是 generator 参数，不是实际已有数据规模。

分别改变历史长度、session/compaction 数、修订数、dependency depth/fanout；先单轴再组合，避免全部变大后仅归因“使用更久”。

每段历史四个共同 checkpoint：

- K0：初始记忆获得正常整理机会后。
- K1：干扰与 session 转换后。
- K2：更正/撤回后的固定事件时钟点。
- K3：修复/恢复后的固定截止点。

每个 checkpoint 在隔离 clone 上执行任务；评分答案和诊断材料不回写时间主线。真实 Host 无法 clone 时，从干净状态重放同一 prefix；额外重放的研究成本单列，且需确认等价状态/运行随机性。无法可靠做到的格标未覆盖，不能偷偷采用不同 reset 方式。

Counterfactual 一支改变相关事实，另一类额外负控只改变无关事实；冻结 `counterfactual_kind`，不能要求所有分支的正确动作都不同。正确关系由隐藏判分器检查。

## 5. 任务执行、时钟与故障

事件按 `sequence` 提供，只在其可见时点挂载；run lock 固定 monotonic arrival schedule。后台积压不得暂停 deadline。历史日期是语义输入，实际执行延迟用 monotonic clock，两者不能混用。

S1/S2 主生命周期为 hot/fresh；after-compaction/process-restart 在开发扩展及正式协议预定格中加入。真实 compaction 必须有可核实事件；adapter fixture 不能冒充 Host compaction。

同时设置：

- `settled_quality`：预定、双方对等且有上限的正常整理窗口后评测。
- `deadline_recovery`：从原始用户纠正可见起固定计时，记录 Store commit、合法充分 evidence ready、正确行动四个时点。

故障点包括：reader 已见旧值后更新、检索后行动前更新、worker 在修复时退出、旧结果迟到、重复事件、导入前目的端已变化。受控故障必须对双方设置同样外部时机；不可控 Host 格只做自然观测并标明限制。

即使提交前校验成功，也不能据此宣称外部行动与 Store 更新原子一致。行动层必须由独立环境状态判分。

无法回答的任务期待澄清/停止，存在替代证据的任务则期待继续正确工作；正确拒绝与无谓拒绝分开。

## 6. S3 Migration 矩阵

每个 A→B 方向，固定 B 的 model/tools/task/environment，至少：

| Memory 方案 | 无迁移参照 | 实际迁移 |
| --- | --- | --- |
| RecallLedger | B continuous + 相同配置/前史 | A→B，共享 ledger 与 bundle 分开 |
| Native best feasible | B Native continuous | A→B，真实官方导入/合法文件转移/交接或来源重放 |

B empty 与 B sufficient evidence 是独立诊断组。不能直接用 A 老成绩减 B 新成绩。每方案给绝对成功率、自己的 migration gap，之后再比较两个 gap。

A→B→A 的终点是 A，因此终点比较另用 A continuous：将同样的中途更正/撤回与任务输入提供给持续留在 A 的参照，固定 A 的模型、工具和预算。不能拿最终 A 与 B continuous 相减，也不能让返回组少经历或多获得一轮纠正。

Native 不支持的方向是 capability gap，应报告该方向的用户恢复流程/人工量；不能制造不存在的 API 得分，也不能仅删除失败方向后给总体迁移冠军。

共享 ledger、独立部署 bundle、Native format import、A→B→A 是不同测试。当前 RecallLedger bundle 有 512 KiB/1,000 objects 上限，vectors/repair recipes 不迁移；目标重建费用、验证等待、重新绑定和任务引用检查入账。超限用例只说明容量限制，不冒充大规模迁移实现。

相同合法项目工件、权限条件、工具环境提供给双方；handoff 文本由各方案正常形成，不能给 L 独家人工 gold 摘要。权限迁移与知识迁移分开，不自动授权。

## 7. S4 Performance 配置

### Service

候选规模 1k/10k/100k records；并发 1/4/16；更新比例 0%/1%/10%。先探索选出负载点，正式配置不做无法承担的全笛卡尔积。query、write、repair、transfer 分别计时。

固定 source 大小/分布、query selectivity、返回有效证据量、vector model/dimension、dependency depth/fanout、durability、fsync 与硬件。Release build 与调试 build 不混比；包含 IPC 时两边都包含。

三个独立过程重启作为候选重复，按 run 分块报告不确定性。候选每点 warmup 100 requests、measurement 2,000 requests，只作起始设计；尾部精度与预算校准后冻结。预先固定到达率与排队时钟，记录计划/实际发出时间，避免 coordinated omission。负载生成器跟不上则报告 generator lag，不能宣称服务已承受目标负载。

报告 process-cold 和 warm 状态；没有实际控制 OS page cache 时写未知，不用重启进程假装清空操作系统缓存。Native 无独立 Memory API 时没有 service 对比格，只报告端到端。

operation 每种分别列：成功/失败、p50/p95、达到冻结 SLO 的有效吞吐、CPU/RSS、磁盘/WAL/索引、队列峰值。p99 仅在足够精度证据下发布。Embedding 预计算成绩明确只含索引/检索，生成费用另列并计总账。

### End-to-end

完整 input/output tokens、费用、wall-clock、人工分钟，从历史构建至任务验收和后台结算计量；失败、取消、重试与不必要拒绝都保留。模型 price sheet、cache pricing/命中信息和币种记录快照，隐藏账单为 unknown，不用字符数猜费用。

Context 候选档位 2,048/8,192/32,768 Memory tokens；主要候选 8,192。Controlled 每次 reader 完整输入上限候选 32,768、输出 4,096；32,768 Memory 档位须另扩完整输入上限并为普通任务/tool内容预留空间，配置未锁定前不得比较。所有后续 file/tool 回溯及 metadata 进入累计预算。

全任务输入+输出 token、reader calls、deadline 均有独立硬上限；先触顶即按预定状态退出，不重启重置额度。候选值见 protocol JSON，不代表已经批准资源。

## 8. 评分、统计及判胜

[SCORING.md](SCORING.md) 为计分权威，[analysis-plan.json](analysis-plan.json) 为机器可读统计草案。主指标在可回答任务上计算有效任务成功；不能靠大量无需行动的拒答题稀释失败。

主要单位是 history cluster；同 cluster 的分支/checkpoints/seeds 嵌套。主要对手与预算档位在验证前选定，不能跑完选最差 Native 配置。正式 n 按配对方差、cluster 相关性、power 与预设差异确定；目前 `null`。

建议业务门槛候选为 5 percentage points 的最小有意义提升、3 pp non-inferiority margin，以及单位有效成功成本比不高于 1.25。这些是待审定设计值，不是数据推导或效果承诺。成本比门槛只判断增加的成本是否可接受，并不证明成本节省。按 SCORING 的区间与幅度条件给每条主张分别判定 pass / fail / inconclusive；没有显著下降不等于 non-inferior。

任何已复核的严重跨 scope 副作用/撤回复活均单独阻断相应产品合同主张，不被平均成功率抵消。观测零次不表示真实风险为零。

任务超时后仍要按冻结的 cancel/drain policy 结算 background jobs；未结算成本标 unknown/lower_bound。一个 job 只计费一次，并可引用多个 checkpoint，不能把重建推到窗外免费完成。

### 预定的机制消融

以下在 P3 开发集选择有区分力且可承担的格，若用于正式机制主张须在封存前加入比较族。它们不自动扩展512格 pilot，也不允许看过正式结果再挑有利配置。

| 问题 | 只改变什么 | 必须固定什么 |
| --- | --- | --- |
| 增益是否来自 retrieval？ | lexical / dense / hybrid，含 reranking 策略 | 来源、当前版本过滤、embedding 模型、reader 与 Memory 预算；各策略完整计费 |
| dependency repair 是否值得？ | 精确依赖失效/增量修复，对比简单按需或 full rebuild | 相同依赖输入、纠正与撤回语义、目标正确性合同；测质量与维护竞争成本 |
| evidence 与模型摘要各适合什么？ | C_RL_EVIDENCE 与 C_RL_MODEL 的表示方式 | 来源、条件、版本、模型配置；按预算画质量—成本曲线，不能让任一组无限全文 |
| 是否只是 Host delivery 的改善？ | 同一 backend 的固定文件投影与动态 packet 刷新 | 同 Host/reader/来源/预算，记录实际进入模型的内容；内部不可观测时不作归因 |

raw-conversation 的 extraction/admission 是另一层系统能力：待链路实现后，先固定同一 admission 结果测机制，再分别测真实原话端到端。两张表共同解释收益，不用强制统一 extractor 掩盖生产链路的漏记或错记。

## 9. 阶段、预算和停止门槛

| 阶段 | 交付 | 模型使用与推进条件 |
| --- | --- | --- |
| D0 本轮 design | 本资料包、结构检查、反方审阅 | 本轮 model/API/GPU=0 |
| P0 Harness self-check | 隔离、event replay、verifier正负控、token/cost计量、failed/unknown状态 | 小型 CPU 合同检查；不是 Agent 比较 |
| P1 Host qualification | 上轮 H01–H13，尤其原话准入、actual input、Native真启用、gold不可读 | 模型版本/资源/授权先落实；不访问封存集 |
| P2 Controlled pilot | 八 cluster 的 512 task slots + 开发成本/方差报告 | 所有 token/model/后台调用计总预算，不能拿512当API上限 |
| P3 Calibration/ablations | C_LIFECYCLE、overflow policy、模型重复、边界负载 | 与正式集隔离；补齐effect margin与sample size依据 |
| P4 Sealed validation | 锁版本/新数据/预算/判分/统计，独立运行及复核 | 冻结检查通过且授权预算充足；否则不启动 |
| P5 Real Host/migration | 真实同Host对照、第二Host、逐方向A→B→A | 每Host先资格检查；受控成绩不代替此阶段 |

P2 采用 `admitted_events`，所有组获得相同结构化准入事件；用于机制和测量管线探索，不能得出原话自主提取或真实 Host 的结论。矩阵：8 clusters × 2 branches × 4 systems × 2 lifecycle profiles × 4 checkpoints = 512 task slots。只有八个 cluster，正式检验样本量不是512。八簇 pilot 数据尚未构造；examples 是另供接口开发的 raw-conversation 样例。完整 ingestion/后台调用及失败重跑另计，checkpoint reset/replay属于研究开销，不免费。

本轮以后资金/服务预算未授权。候选 P2 上限：25 million aggregate model tokens、12 小时 active wall-clock、10 GiB 临时数据；还须填实际货币上限、price snapshot、model lock与授权记录。若本地模型没有货币请求费用，仍要 GPU/CPU/内存/时间预算。到达任何已冻结上限即 stop，不边看结果边追加预算直到显著。

动态预算账使用一个 `budget_id`，父子作业先 reserve 后 reconcile；失败、重试、judge、调优均计项目账。`stage`（如 P2）与 `ledger`（如 system_runtime）正交：pilot 内被测 Agent 的调用仍是运行成本，不能再重复列入 development 账。额外验证重放的设施开销与真实任务必需的初建/维护开销分开，保留实际总账与部署场景分摊账。

单条任务候选上限：120,000累计input+output tokens、20次reader调用、300秒deadline；纯预测或模型速度不同会触发不同失败，开发集校准后统一冻结。不能把P2全局预算除以任务数后宣称实际单价。

执行终态严格区分：completed / failed / budget_exhausted / infra_invalid / unsupported / not_run。超时、系统错误与单任务额度耗尽记 failed，并保留 failure_reason；拒答作为 completed 的 response_kind 独立判分；内部不可观测由 observation_status 表达，不能改变已经可观察的任务结果。环境事故只有预定规则和证据才能 infra_invalid；保留原run，配对重跑对称，不能将难题失败重新分类。

单任务上限耗尽是该任务失败，保留分母与费用。项目总预算停止时，尚未开始的 slots 为 not_run，中断的 slots 为 budget_exhausted，success 为 null；不能将未执行填0或只保留已完成格宣布胜出。完整矩阵正式结论为 inconclusive。候选 25M 全局 tokens 小于 512×120,000 的逐任务上限总和，也要承担后台调用，因此不保证完成全部512格；先校准预计费用再冻结可负担矩阵。

## 10. 防泄漏、复现与实现路线

评分器与未来事件必须在被测 Agent 不可访问的执行边界；仅把该时刻公开材料传给 runner/Host。不同系统、历史、counterfactual 分支和诊断 trace 隔离，先用 canary 验证。用户本人的真实 Memory 和全局配置不作为测试状态。

封存模板、seed、答案和判分器不能随公开开发包一起放进可读工作目录。当前 examples 的 gold 是公开示意，不能用于确认性结论。数据来源、许可证、语言、构造/审查者、泄漏检查与缺失覆盖写入 data card。

工程建议：Python 实现 orchestration、数据与统计；使用已有 Rust CLI/daemon 测真实 RecallLedger；Node bridge 仅用于实际 Host driver。Benchmark 的测量开销要单独校准，不用不同语言制造不对等 timed boundary。

实现顺序：

1. 按 [INTERFACES.md](INTERFACES.md) 做隔离状态、事件时钟、trace/cost和独立verifier。
2. 实现 C_NONE/C_MD/C_RAW，建立强基线维护政策；随后 C_LIFECYCLE。
3. 接真实 RecallLedger API，记录实际策略/limits/qualification；补自主 ingestion 或限定 admitted-events Track。
4. 运行开发 canary/正负控；所有 fallback 和预算事件可重算。
5. 达到条件后运行 pilot，冻结新 validation protocol；随后 Real Host 和 migration。

拟产物：systems lock、数据manifest/public input hashes、私有gold版本、run manifest、trace、cost events、实际工件、每task判分、每cluster结果、统计报告、失败案例、unsupported矩阵及全部source/config hashes。

设计完成不等于 runner 已实现。当前产品的 pending admission、Host加载/刷新、统一trace计量和真实Native migration driver仍是明确工程工作项。
