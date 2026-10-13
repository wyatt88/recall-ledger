# Benchmark v0.1 设计包：独立评审

日期：2026-10-13，Asia/Shanghai。岗位：魏慎言，独立审稿人 / 反方专家（AI 岗位）。

**性质：设计审查，未执行实验。** 本文判断文件是否能共同定义公平、可解释的实现目标；不证明 runner 已实现、数据已隔离、Agent 已接入或 RecallLedger 更优。岗位分工也不保证模型偏差独立。

## 审阅范围与结论边界

初审读取：

- `README.md`、`SPEC.md`、`INTERFACES.md` 全文；
- `SCORING.md`、`analysis-plan.json` 全文；
- `TASKS.md`、`examples/README.md`、`examples/manifest.json`；
- revision、budget、migration 三组 public/gold JSONL 的全部内容。

本轮唯一写入为此 `REVIEW.md`；没有调用模型/API/GPU、运行被测 Agent、修改产品代码、原生 Memory 或 Host 配置。JSON/JSONL 解析、引用和哈希由负责人另做结构检查；不能把结构检查解释为数据隔离或 Agent 通过。

**初审判断：研究对象与主要比较设计合理，但接口字段、终态和成本判据需统一后再交给工程实现。** 下述问题已经发给负责人处理，最终状态见文末复核。正式协议及执行预算尚未冻结，即使文字关项也不等于允许启动正式验证。

## 问题与修订要求

严重度 P1 表示会导致比较/评分合同不可一致实现；P2 表示限制解释或设计文件不一致。这不是线上缺陷分级，也不意味着已经观察到错误结果。

### R01 — P1：执行终态与评分状态缺乏明确映射

**初稿位置：**

- SPEC §9：`completed / failed / budget_exhausted / infra_invalid / unsupported / unknown`。
- SCORING §2 与 analysis-plan：`completed / timeout / system_error / refused / observation_unknown / invalid_external`。

**问题：**单个 runner 的预算耗尽可能在一份合同中成为失败，在另一份中没有合法状态。可观测性 unknown 与任务是否完成也不应是同一个互斥维度。全局预算提前停止后的未启动 slots 若被抹去、全部当任务失败或只挑已完成子集，三种做法对应不同估计量。

**修订要求：**

1. 明确执行终态、独立任务评分和可观测性三个维度；给字段与映射表。
2. 已启动且有资格的任务若自身预算耗尽、超时或系统错误，主指标记失败并保留成本。
3. 全局预算停止后未启动 slots 保留为 `not_run`；整矩阵/正式验证标不完整，不能用有利已完成子集判胜。
4. infra invalid 保留独立复核与整个配对格点对称重跑，不让系统失败变成外部事故。

**最低通过条件：**相同原始 run 状态能唯一映射到同一分母和 outcome，不存在未知状态默认成功/默认丢弃。

### R02 — P1：成本去重键、归属字段与账本分类不一致

**初稿位置：**

- INTERFACES §6 使用 `cost_event_id`，字段包含 `history_id`，但未直接列统计合同需要的所有配置/簇/账本字段。
- SCORING §8 与 analysis-plan 使用 `cost_entry_id`，归属要求 `history_cluster_id / config_id / ledger`。
- INTERFACES 的 online/tuning/replay setup/judge/harness overhead 分类，与评分的四账本没有映射。

**问题：**字段别名未固定会使同一收费事件无法去重或归属。pilot 是研究阶段，不应自动把其中所有读者和 Memory 请求既记 `development` 又记 `system_runtime`。否则项目总成本和部署成本都可能重复或遗漏。

**修订要求：**

- 选一套 canonical ID 与字段命名；跨文件显式一致，或给唯一映射。
- `stage`（pilot/development/validation）与 `ledger`（system runtime/evaluation/development/infrastructure）正交。
- 一个资源事件只进入一个原子账本；项目总账计一次。部署场景成本是带规则的派生视图，不复制收费事件。
- 共同前史、checkpoint replay、失败尝试、后台 tail 分别有归属；未知成本保留 `null`/原因。

**最低通过条件：**从逐事件账本能重算每配置成本和项目合计，同一 provider request/job 不重复收费。

### R03 — P2：20% 降本目标与 1.25 倍可接受成本不是同一判据

**初稿位置：**SPEC §8 提出“20% 成本降低”，SCORING/JSON 却仅定义 `cost_acceptability` 的成本比上限 1.25。

**问题：**可接受最多增加 25% 与至少节省 20% 对产品含义不同。候选阈值虽然都不是结果，但不能让工程师自行挑其中一个解释 `pass`。

**修订要求：**删除未定义的 20% 降本，或定义独立 cost-reduction 主张及其统计判据；保留质量非劣条件和相同单位/完整成本边界，不从“可接受”推出“更便宜”。

### R04 — P2：八类任务目录名称需与规格一致

**初稿位置：**SPEC §4 的八类清单包含“隐式偏好”，未列独立 longitudinal；TASKS 中 F06 是 Longitudinal degradation and compaction，隐式偏好没有单独 family。

**问题：**这会使数据 generator、覆盖清单和统计 strata 对同一“八类”产生不同理解。任务家族数量不等于独立样本数量。

**修订要求：**以 F01—F08 为唯一目录；偏好可以作为稳定/条件任务的子例或未来扩展，明确没有单独实现/覆盖。

### R05 — P2：示例需明确资格及一次动作的断言语义

**初稿位置：**开发 gold 使用 `contains_exact_request`，原始输出未见明确的 answerability 字段；任务却要求“执行一次”，预算撤回分支期待 hold。

**问题：**“存在一条正确请求”不足以排除先错后对或重复副作用；评分主指标要区分可答成功和正确拒答，不能让 verifier 根据被测输出猜资格。

**修订要求：**

- gold 为每个 task 明确冻结 answerable/unanswerable 与预期行为类别；
- 模拟工具参数/允许值向 Agent 公开，不用隐藏 gold 中独有的参数值判断失败；
- 成功分支检查恰好一次合法动作和无禁止副作用；hold 分支零动作；
- 每次 task 独立划定 trace 范围，不能把 earlier checkpoint 的正确调用挪来满足 later task。

本项已由任务设计岗位主动补充，最终复核将检查实际文件，不以口头完成替代。

## 已检验的关键设计点

| 主题 | 文本中可确认的设计 | 尚不能据此声称 |
| --- | --- | --- |
| Native 与 MD | C_MD 明确是工程基线；正式受控主对手 C_LIFECYCLE，真实 Host 主对手 H_NATIVE_TUNED；组合配置单独报告 | 已经测过某品牌 Native 或关闭原生后的配置仍是 Native |
| 512 slots / 8 clusters | `8×2×4×2×4=512`；每配置 128，独立簇最多 8；pilot 不作正式判胜 | 512 个独立样本、512 次 API 调用或足以验证 3pp 非劣 |
| 正式样本量 | 保留 `null`；按簇配对、方差不确定性、功效、多重比较和预算冻结 | 196/545 等算量示例是实际样本要求或预算承诺 |
| 主分母 | answerable valid success 为主；正确拒答单独；超时、崩溃和不必要拒答不能排除 | 只愿意回答子集上的低错误率代表整体更好 |
| unknown | 未观察内部状态保留 null 与观测比例；主工件可独立判分；缺必需产物不能靠自述通过 | 未知等于无错误、黑盒费用为零 |
| 反事实 | 分支归同簇；相关事实变化与无关事实变化有不同预期关系 | 每个改数字分支是新用户/独立项目 |
| gold 隔离 | 当前公开示例明确没有物理隔离；正式数据/未来事件/他组 trace 要不可访问并做 canary | 文件分开、hash 正确就已实现隔离 |
| 迁移 | 固定目的端，两套实际 A→B 及各自 B continuous；分共享 ledger/bundle/原生导入 | 自身数据包正确就是比原生迁移好 |
| source preservation | 360 UTF-8-byte 示例明确为特定 audit 产物合同；一般 Memory 允许合法压缩/选择性检索 | 全文保真是所有任务的必需能力，或 bytes 等于 model tokens |
| 自主准入 | 缺 autonomous admission 时不得填 H_RL_INTEGRATED 主比较格；admitted mode 排除提取结论 | 已捕获 candidate 就已经形成可用长期 Memory |
| 成本与总预算 | 任务与全局硬上限分开；前史、维护、失败/重试/裁判计账；本轮 model/API/GPU=0 | 候选上限是已授权资源，或 pilot 数量保证能在该预算完成 |
| 长期不退化 | 独立压力轴、固定难度/检查点、hot/fresh 以及观察窗内的非劣；不把加速回放称多年 | 有限测试证明永久不失真、永不退化 |

## 仍须由未来实现完成的门槛

无论本次文字审阅是否关项，以下均未完成：

1. Backend/Host driver、数据生成器、admitted-event schema/编译器、实际 verifier 与统计 runner。
2. 可观测真实 Host 资格检查，包含 Native 正常开启和 RecallLedger 自主准入链路。
3. 封存数据、物理访问隔离、只公开当前 prefix 的实际测试。
4. 资源账本与 overflow/fallback 的可重算验证。
5. 正式模型版本、主比较族、统计容差、样本量、预算与停止规则的冻结和执行授权。

在这些条件满足之前，允许的交付描述是“工程实现规格与公开开发样例”；不是“Benchmark 已跑通”“比较通过”“产品可靠性已验收”。

## 修订复核记录

初审问题已发回负责人。此处将在读取最终修订后追加逐项处理状态，保留上述原始问题，不把原问题静默删除。

### 同日最终设计复核

已补读 `protocol.draft.json`、新增 `examples/tools.public.json`，重读修改后的 SPEC/INTERFACES/SCORING、analysis-plan 的配置、终态与成本部分，以及三组更新后的 gold 和 examples 说明。

| 编号 | 修订后事实 | 设计层状态 |
| --- | --- | --- |
| R01 | 使用共同的六个 terminal states；response kind 与 observability 正交；单任务 cap 为 failed；全局中断为 budget_exhausted/null、未开始为 not_run；正式完整矩阵结论 inconclusive、效应/CI null | **已解决** |
| R02 | 共同采用 cost_event_id、cluster_id/history_id/system_id；原成本 phase 字段统一为 operation；stage 与唯一 ledger 正交，pilot 被测调用仍为 system_runtime，研究/部署视图不重复收费 | **已解决** |
| R03 | 删除未定义 20% 节省主张；统一为 1.25 成本可接受候选门槛，明确不证明节省 | **已解决** |
| R04 | SPEC 与 protocol JSON 对应 F01—F08；隐式偏好仅为稳定/条件任务子类型且未有独立覆盖成绩 | **已解决** |
| R05 | gold 各任务新增 evaluator-only eligibility；新增各组预先可见的工具参数、dry_run、一次动作预算；成功任务同时检查正确请求、恰好一次工具及总模拟动作，hold 为零动作，包含被拒绝尝试；task_event_id 绑定评分任务，工具预算按下一 task 重置 | **已解决**；实际 verifier 仍须按 task/run 的 trace 边界实现和负控验证 |

额外核对：

- protocol 与 analysis-plan 的 pilot 同为 `admitted_events`、512 slots、8 个簇；当前 3 簇/6 历史 raw-conversation 样例独立，不冒充 pilot 输入。共享结构化输入的研究制作费用与实际产品自主准入费用已区分。
- protocol 当前 `protocol_frozen=false`、`execution_enabled=false`；analysis-plan `executed=false`、正式门槛未通过，当前调用/执行授权均为 0。
- 候选 25 million aggregate tokens、12 小时和 10 GiB 为未来全局上限；单任务另有 120,000 tokens / 20 reader calls / 300 秒。它们不保证 512 slots 全部完成，预算耗尽不允许取剩余有利子集判胜。
- 主要 5pp、3pp 和成本比 1.25 均标未冻结候选；正式样本量、比较族大小、模型/价格/时间表/资源授权仍为空。主任务实用改进规则只要求正向区间和 5pp 点估计，已明确不得表述为“提升下界达到 5pp”。
- 迁移对照文本与机器配置保留固定目的端、各自 continuous、绝对质量、重建成本和真实 Native 最佳可行路径的要求；没有新的迁移实测。

**最终结论：本次发现的问题已在设计文件层关项，资料包可以交给工程师实现。** 没有剩余阻断这一设计交付的文本问题。这不是未来正式冻结审阅的完成记录，也不是 runner、Host qualification、gold 隔离、统计实现或真实实验的验收。正式运行仍须满足上节全部未完成门槛；新实现、数据、版本或主要判据变化后应重新审阅。

### 最终补充的窄范围复核

再次读取 SPEC 的 S3 往返定义、§8“预定的机制消融”，protocol JSON 的 `migration_comparison` / `mechanism_ablations`，以及 analysis-plan 最终落盘的 migration 字段。

1. **往返迁移：**A→B→A 的最终读者是 A，比较对象明确为同样接收中途更正/撤回的 A continuous。analysis-plan 已加入 `A_TO_B_TO_A`、`A_CONTINUOUS_WITH_SAME_INTERMEDIATE_UPDATES` 和对应差值、`round_trip_destination=A`；单程仍与 B continuous 比较。该补充消除了最终 A 减 B 分数的混杂，与已有固定目的端原则一致。
2. **P3 消融：**检索方式、增量修复与按需/full rebuild、evidence 与模型摘要、同一 backend 的文件投影与动态交付四个轴，均说明固定条件和完整成本。选格/预算尚待开发校准，不能自动增加 512-slot pilot；正式机制结论须在封存前确定比较族。没有发现与主要对照、统计家族或未执行边界的新冲突。

该窄审只确认文字与机器配置的设计一致性；未评估实际消融实现、往返执行结果或统计功效。此前的“可交工程实现、正式验证尚未验收”结论保持不变。
