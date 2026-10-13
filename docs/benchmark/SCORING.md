# RecallLedger Benchmark v0.1：评分与统计方案

日期：2026-10-13，Asia/Shanghai。评估岗位：方衡之（AI 岗位）。

**H0 产品与实验设计提案，未冻结、未执行。** 本文与 `analysis-plan.json` 定义待实现评分器；没有成绩。本轮模型/API/GPU 调用预算均为 0。候选阈值没有经过方差校准，不构成后续执行或费用授权。

## 1. 固定的 pilot 与正式比较

受控 pilot：

`8 history clusters × 2 counterfactual branches × 4 configs × 2 lifecycles × 4 checkpoints = 512 task slots`

- 配置：`C_NONE`、`C_MD`、`C_RAW`、`C_RL_EVIDENCE`。
- 生命周期：`hot`、`fresh`。
- 每个配置 128 个 slots；每个历史/配置 16 个 slots。
- 独立单位最多为 8 个历史簇。两个反事实分支、两个生命周期、检查点和模型重试均不成为新独立样本。
- 512 是预定任务位置，不是模型调用数。嵌入、维护、工具循环、重试和读者可产生不同数量的调用；真实调用按账本另计。
- pilot 用于验证隔离、评分、账本、负控和估计方差，不作产品优越性、非劣性或真实宿主结论。
- pilot 固定 `input_mode=admitted_events`：各组获得同一已准入事件、identity、纠正映射及显式依赖；不评估从原话提取/准入。原话模式须另行通过 admission qualification 后启用。
- 公开 samples 中的自然语言事件仅演示原话输入合同，不是本 pilot 的完整 admitted-events 数据；不得把这些样例称为已具备八簇 pilot 数据或已执行实验。

正式主比较：

| 轨道 | 主比较（实验减对照） | 条件 |
| --- | --- | --- |
| 受控机制 | `C_RL_EVIDENCE − C_LIFECYCLE` | 同样结构化准入事件、可访问原文、条件/纠正/依赖、读者与预算；对照可检索合法当前原文并按需重建 |
| 真实宿主 | `H_RL_INTEGRATED − H_NATIVE_TUNED` | 同一宿主版本及原始历史；准入必须通过 qualification；明确长期事实读写所有权及刷新策略 |
| 只能叠加的宿主 | `H_NATIVE_PLUS_RL − H_NATIVE_TUNED` | 单独的增量价值比较，不能替代上一行或声称替换原生 |

主指标为**有充分合法依据任务的有效成功率**。无法回答任务的正确拒答率单列，避免通过大量容易拒答的任务稀释记忆应用失败。正式样本量是 `null`，须在开发方差与功效分析后冻结；不能把 pilot 的 512 slots 当作正式样本量。

## 2. 评分单位、资格和状态

一个 slot 的主键：

`cluster_id / history_id / counterfactual_branch / system_id / lifecycle_profile / checkpoint_id / task_event_id / replicate`

字段以 `INTERFACES.md` 为准：`cluster_id` 是独立历史簇，`history_id` 是具体分支历史，`counterfactual_branch` 标识分支；`task_event_id` 引用公开任务事件的 `event_id`。旧草案的 `history_cluster_id/branch_id/config_id/lifecycle/task_id/repeat_id` 分别映射为 `cluster_id/counterfactual_branch/system_id/lifecycle_profile/task_event_id/replicate`，禁止在最终产物中混用。

pilot 每个实验格点只有一个 task、一次规定尝试；`replicate=0`。内部重试按同一 slot 计费计时，不取最好答案。正式若增加重复，需在冻结清单中声明次数和嵌套结构；不得因失败自行追加独立评分机会。

资格由**系统不可见的公共事件真值及预先冻结任务清单**决定，不由某系统是否成功召回决定：

- `answerable`：任务时点有合法、当前、足够的依据，且在声明的任务/工具能力范围内可完成。
- `unanswerable`：缺乏所需合法依据，正确行为为拒答、澄清或报告缺失信息；允许的响应类别在 gold 预先规定。
- `applicability`：该任务是否属于某条已声明比较的能力范围。此项在运行前确定，不能看过失败后修改。
- 主比较要求配对各组的资格集合一致；开放闭环系统如导致资格不再相同，另作系统实验，不混入受控配对估计。

执行终态仅允许 `completed/failed/budget_exhausted/infra_invalid/unsupported/not_run`。另用 `failure_reason` 表达错误原因，`response_kind` 表达响应类别，`observation_status=complete/partial/unknown` 表达观测程度，不能混成一列：

| `terminal_state` | 主任务指标 | 配套字段 / 解释 |
| --- | --- | --- |
| `completed` | 独立评分后为 0 或 1 | Agent 自称完成不是得分 |
| `failed` | 有资格、实际尝试的 slot 成功记 0 | `failure_reason=timeout/system_error/task_budget_exhausted/...`；包括单任务上限耗尽 |
| `budget_exhausted` | 全局预算中断的 slot 成功值为 `null` | `failure_reason=global_budget_exhausted`；保留资源并停止完整矩阵结论 |
| `infra_invalid` | 盲化独立复核后使整个配对格点无效 | 例如测试机断电；公布对称重跑，不能单组剔除 |
| `unsupported` | 见下述预先资格规则 | 不制造一个不可能执行的平均分 |
| `not_run` | 成功、延迟和消耗均未观测，不填 0 | 尚未启动，含全局预算停止后的剩余 slots |

拒答通常是 `terminal_state=completed, response_kind=refusal`：answerable 任务成功记0，unanswerable 按 gold 判分；不能因拒答“安全”自动通过。`response_kind=substantive` 表示实质回答/行动；澄清使用 `clarification`，没有响应使用 `none`。

旧状态映射：`timeout→failed+failure_reason=timeout`；`system_error→failed+failure_reason=system_error`；`refused→completed+response_kind=refusal`；`observation_unknown→observation_status=unknown`（不改变已有执行终态）；`invalid_external→infra_invalid`。

`unsupported` 的规则：

1. qualification 前确认不支持的格式/宿主场景，列为能力缺口；在一致、预先声明的适用子集上比较，不能宣称覆盖未支持场景。
2. 已声明支持、已进入冻结任务范围，运行时才返回 unsupported，规范为 `failed+failure_reason=unsupported_declared_capability`，成功记0并计入分母；保留底层原始 unsupported 错误。
3. 若强主对手不能在目标范围完成 qualification，主比较标为 `not_evaluable`，而不是把对手全部记 0；可另做能力覆盖报告。

`infra_invalid` 不包括某个被测系统耗尽单任务资源、请求超时或不能修复；这些是系统失败。若需要重跑，所有配对配置从同一干净检查点重跑；原始账与无效原因保留在研究成本中。

**全局预算与单任务上限严格区分。** 单任务上限耗尽为 failed，保留分母；全局预算停止后，未启动 slots 为 not_run，被中断 slots 为 budget_exhausted，不填0。报告 planned/started/completed/failed/not_run 计数和截断位置；整体正式比较为 `inconclusive`，完整矩阵主效应/CI 为 `null`。已执行部分只能作明确标注的描述结果，不能通过删除未运行项取得正式结论。

## 3. 独立评分与精确定义

优先采用沙箱实际动作、最终工件隐藏检查、时间/作用域真值。开放语义答案仅能使用预定量表和盲化裁判；正式执行前冻结裁判版本、校准样本与人工复核规则。裁判不能是被测 Agent 的自我评价。

对 slot `i`，令：

- `A_i`：任务 answerable。
- `U_i`：任务 unanswerable。
- `V_i`：正确完成、在截止时间内、所有必要条件满足，且无记忆相关版本/撤回/作用域违规。
- `R_i`：正确拒答/澄清类别与必要缺失说明符合 gold，且没有无依据的确定性答复或动作。
- `S_i`：送入读者的当前合法证据足够完成任务，保留该任务所需条件；相关但不充分不算。
- `T_i`：实际作出实质答案或动作；纯拒答/仅澄清不算。
- `E_i`：该实质答案/动作错误或不受当前合法依据支持。

| ID / 指标 | 分子 | 分母 / 不适用 |
| --- | --- | --- |
| `answerable_valid_success`（主） | `Σ A_i V_i` | `Σ A_i`，分母为 0 则 `null` |
| `unanswerable_correct_abstention` | `Σ U_i R_i` | `Σ U_i`，与主指标分开 |
| `overall_valid_success` | `Σ(A_i V_i + U_i R_i)` | `Σ(A_i + U_i)`，仅辅助，不用它排名 |
| `sufficient_evidence_coverage` | `Σ A_i S_i` | answerable 且有完整上下文观测的任务数；另报观测比例 |
| `response_coverage` | `Σ A_i T_i` | `Σ A_i`；不等同于证据充分覆盖 |
| `selective_action_risk` | `Σ T_i E_i` | `Σ T_i`；零动作时为 `null`，不是零风险 |
| `unnecessary_refusal` | answerable 中明确拒答数 | `Σ A_i`；超时/崩溃另报，不能冒充拒答 |
| `stale_adoption` | 使用过时值作为当前值的答案/动作数 | gold 预先标识的 `stale_challenge` slots |
| `withdrawal_exposure` | 当前读取包含已禁止正文的读取数 | 撤回提交确认后开始、有完整响应观测的当前读取数 |
| `benign_retention` | 未受撤回/纠正影响的 answerable 任务中有效成功数 | 预定 `benign_unaffected` answerable slots |
| `qualified_source_preservation` | 完整正文、条件、作用域、有效期和来源身份均符合预定契约的输出数 | 有完整可检查来源/输出的预定表示操作数 |
| `unsupported_claim_rate` | 无当前合法来源支持的实质断言数 | 可独立分解和判定的全部实质断言；同时报每 slot 发生率 |

主指标 V 的判分不要求所有系统暴露内部来源图，但必须能独立验证实际产物/行动及必要条件。实际运行到任务截止仍拿不到主任务评分所需的工件，是该 slot 未证明完成，按失败记；只有测试设施本身的异常才可走 `infra_invalid`。not_run 与全局预算中断不是普通任务失败，不适用此0分规则。

内部证据 unknown 的输出规则：

- 返回 `value=null`、`known_count`、`unknown_count`、`observed_fraction`。
- 对二元任务级诊断，可同时给识别区间 `[known_success/N, (known_success+unknown)/N]`，它不是统计置信区间。
- 不对观测缺失严重且不对称的内部指标做跨系统优越性结论。
- `qualified_source_preservation` 是工程指标；即使全通过，也不能替代主任务效果。
- 指标表默认适用于完整可评分矩阵；出现全局预算截断时，按上一节输出完整矩阵指标 `null`，不能把表中分母规则用于强行插补未运行结果。

## 4. 聚合：保留历史簇与层次

主要 estimand 是“等权项目历史下的效果”，不让长历史或高频用户主导。

对每个历史 `j`、配置 `s`、分支 `b`、生命周期 `l`、检查点 `k`：

1. 在预定 answerable tasks 内计算成功比例 `q[j,s,b,l,k]`。
2. 先等权平均预定分支与 lifecycle，得到 `q[j,s,k]`。空单元不凭空记为 0；报告资格计数，并按冻结任务分布提供足够 answerable 单元。
3. 最后检查点的 `q[j,s,final]` 是主 endpoint。主比较差为 `d_j=q[j,experimental,final]−q[j,comparator,final]`。
4. 总效应为所有独立历史簇 `d_j` 的算术平均；每任务等权结果作为次指标。

若某历史在最后检查点无 answerable task，正式主设计不合格，须在冻结前修订任务分布。运行后不能重造任务来替代失败；若是清单/评分器缺陷，则该验证不能通过冻结门槛，修订后用新封存材料重新开始。

次 endpoint：

- `longitudinal_degradation_difference`：每历史计算 `(RL_final−RL_first)−(Baseline_final−Baseline_first)`。
- `checkpoint_auc`：在冻结的逻辑检查点轴上，以固定权重平均质量；非等距轴使用预定梯形面积并标准化。
- `hot`、`fresh` 以及事实/条件/纠正/撤回任务族分层；不事后选择最有利层作为主结果。
- 反事实响应敏感度：有关事实变化时动作相应变化，无关事实变化时动作保持；按成对分支判定，不能把两分支作为独立历史。

同一历史内因随机种子增加重复时，先在历史内按预定规则平均，仍以历史簇作推断单位。原生与 RecallLedger 都必须在相同 seed/repeat 网格上评估。

## 5. 置信区间、多重性与样本量

正式主分析使用历史簇配对 bootstrap：

- 重采样独立 `cluster_id`，同时带走该簇全部配置、分支、lifecycle、检查点与重复，保持配对。
- 10,000 次重采样；随机种子 20261013；报告 percentile 区间与绝对百分点差。
- 主比较族的总比较数 `m` 冻结前为 `null`。采用 Bonferroni 控制 family-wise α=0.05，每个主比较用 `1−0.05/m` 双侧区间。
- 主比较族包含预先选定的 controlled 和 real-host 主比较；单一宿主资格合格后可以只选择相应轨道，但必须在封存前决定。多宿主不能事后只报告显著的那一个。
- 次指标默认报告未校正 95% 区间并注明探索；若要用它作正式声明或门槛，必须预先加入相应比较族。

pilot 只有 8 个历史簇；bootstrap 可以作评分器开发输出，但**不用于正式 pass/fail 或稳健 CI 声明**。特别要检查有效簇数、方差估计不稳定和配对资格。

正式 `sample_size=null`。冻结前必须：

1. 从开发历史获得主配对差标准差、不一致结果比例、未知率和实际费用；开发集不能再次成为封存集。
2. 以最小有意义差异、α、至少 80% 目标功效和所有主要分层需求做功效计算/模拟。
3. 使用方差不确定性的保守上界或敏感性区间，不用 8 簇的偶然低方差宣称大幅节省样本。
4. 冻结独立历史簇数、总 slots、重复次数、预算和停止规则；预算耗尽时不得改称成功。

未校准的算量示例：配对历史差 SD=0.25、单一两侧 5% 检验、80% 功效、目标差 5pp，正态近似需约 196 个独立历史簇：

`n ≈ ((1.96+0.84)×0.25/0.05)^2`

这不是样本量决定。按同一 SD、真实差为 0 粗估，3pp 非劣边界约需 545 簇，1pp 约需 4,900 簇；多重比较和真实分布会改变结果。pilot 的反事实、检查点和重复不能拿来充足这类样本数。

零违规不等于零风险。只有事件可合理视为独立同分布时，0/n 的单侧 95% 上界才可粗略用 3/n；本设计的同一历史多个 slots 不满足默认独立，禁止用 512 作此分母宣传。

## 6. 候选门槛与结果状态

以下是**产品候选值**，尚未校准或得到执行授权：

- 主任务提升规划目标：绝对 5pp，用于样本量规划和产品意义判断。
- 主任务非劣边界：绝对 3pp。
- 无法回答任务的正确拒答非劣边界：绝对 3pp。
- 有效成功任务单位成本候选上限比：1.25。
- 恢复和终端延迟截止时间：`null`，须结合目标场景在冻结前填写。

必须区分不同主张，不能把任一 pass 解释为总体胜出：

| 声明 | `pass` | `fail` | `inconclusive` |
| --- | --- | --- | --- |
| 主任务实用改进 | 校正区间下界 >0，且点估计 ≥5pp | 区间上界 ≤0，或上界 <5pp | 其他情况 |
| 主任务非劣 | 校正区间下界 >−3pp | 区间上界 <−3pp | 其他情况 |
| 长期自身不明显退化 | 自身末段−初段的预定区间下界 >−3pp | 上界 <−3pp | 其他情况 |
| 迁移行为连续性 | 同一目的端迁移−continuous 的预定区间下界 >−3pp | 上界 <−3pp | 其他情况 |
| 单位有效成功成本可接受 | 预定成本比区间上界 ≤1.25 | 下界 >1.25 | 区间跨界、成功为0或不可估计 |

“主任务实用改进 pass”仅支持“存在正向提升，点估计达到 5pp 规划目标”，**不能写成提升下界至少 5pp**。要宣称至少提升 5pp，需要区间下界也 ≥5pp，并用相应 null/alternative 重新规划功效。

上述正式声明只有冻结且所有资格/统计门槛通过才计算。当前状态为 `not_evaluated`。若主对手或完整观测/独立评分无法建立，则为 `not_evaluable`。没有显著差异不能判非劣；预算不足或区间太宽应为 `inconclusive`。

工程阻断与产品判断分开：

- 已复核的跨作用域泄露、撤回内容复活、未经准入自动提升等，记为对应 declared contract 的 `fail` 并阻断“可靠性验收通过”。
- 零观测违规只说明本覆盖内未发现；不能标无限范围工程安全已证明。
- 相对原生质量更好、但正确拒答明显恶化或成本门槛失败，应报告具体取舍；不能只给总冠军标签。

## 7. 超时、恢复与性能

端到端截止从任务到达开始，包含排队、召回、模型、工具和允许重试；每任务deadline必须冻结。已启动任务耗尽自身deadline/cap仍未完成，任务成功为0，即使后台之后才恢复。全局预算中断按第2节保留缺失，不伪装成跑到了单任务deadline。

修复事件单独计两个时钟：

1. 原始纠正到达→首次提供合法充分证据/完成受影响任务。
2. 准入提交确认→旧内容不可见，以及→再次提供合法充分证据。

统一观察窗 `τ` 内，报告恢复完成比例随时间 `F(t)`，及限制平均未恢复时间 `∫₀^τ [1−F(t)]dt`。未恢复或终端失败的事件贡献整个观察窗；不要把失败当作与成功无关的普通删失后套 Kaplan–Meier，制造乐观恢复率。观察提前终止须单列原因；正式比较必须有相同预定观察窗。

存储请求与整个 Agent 回合分别计时，不能比较原生端到端与 RecallLedger 进程内 SQLite 耗时。成功条件一致时报告有效吞吐、p50/p95；超时、错误与成功任务的延迟分别显示。失败/超时的“到有效成功时间”在固定窗汇总时取截止上限，不能因快速错误而表现为低延迟。

pilot 不以 p99 宣称尾延迟稳定。性能重跑需独立运行/历史簇，固定冷热缓存、并发、读写比、维护竞争与数据规模；总体测试耗时不作为产品请求延迟。

## 8. 成本账与资源

每笔实际资源事件以 `cost_event_id` 为唯一去重键（替换旧草案 `cost_entry_id`），归属 `run_id/cluster_id/history_id/system_id`，以 `operation` 标记 reader/repair 等实际操作，并带正交的 `stage` 与 `ledger`；部分归属可通过 run manifest 解析。记录实际 request ID、token、CPU/GPU 时间、模型/价格版本、墙钟时间、峰值 RSS 与磁盘增量。

`stage` 只说明研究执行阶段：`D0` 设计、`P0` Harness 自检、`P1` Host qualification、`P2` 受控 pilot、`P3` 校准/消融、`P4` 封存验证、`P5` 真实 Host/迁移。它不决定账本类别；例如 pilot 内被测模型调用是 **`stage=P2, ledger=system_runtime`**，不可因为在开发阶段就再算进 development。

| 账本 | 内容 | 主要用途 |
| --- | --- | --- |
| `system_runtime` | 初建/原生导入、提取准入、嵌入、检索、压缩、失效修复、读者、动作、失败重试、迁移重建、人力 | 用户完整运行成本与成本比 |
| `evaluation` | 评分器、裁判、盲化复核 | 独立报告，计入项目总预算 |
| `development` | 数据构造、调优/参数搜索、故障调试 | 独立报告，禁止替某组免费调优；不包含按冻结 pilot 配置进行的被测调用 |
| `infrastructure` | runner、额外 replay setup、测试快照复制、外部无效运行、测量设施 | 记录并说明；真实业务所需 ingestion/repair 仍属 system_runtime |

ledger 四类互斥，每笔事件只能归一个 ledger。stage 可与各 ledger 组合，不再把“pilot”当费用类别。旧术语 mapping：online→system_runtime；tuning→development；judge→evaluation；research replay setup/harness overhead→infrastructure。P3 参数搜索调用归 development；P3 冻结消融配置的被测调用归 system_runtime，二者须有不同 run purpose 并可审计。

admitted-events 轨将共同结构化输入作为控制条件；它不包含被测系统自主提取/准入的能力或费用。制作该输入的研究费用仍需记账，但不能将 A 轨的条件内成本冒称真实宿主完整 raw-conversation 成本。

`cost_per_valid_answerable_success = Σ system_runtime / answerable_valid_success_count`，包含全部指定 answerable/unanswerable slots 及共同前史的运行开销；两组任务混合比例须相同。成功数为0时 `null` 且 `reason=no_valid_success`；总费用仍显示。

共享前史/缓存如实际只执行一次，只入账一次，并用冻结权重分摊到同配置内后继分支；分摊权重和为1。单独报告实验实际总成本与部署场景成本，不能重复计费，也不能把某组必须的初建/维护从部署成本中抹去。跨配置不得共享被测记忆、模型缓存结果或先验答案。

货币金额未取得可靠定价时为 `null`，同时保留原始资源单位；canonical 原始字段 `human_seconds` 可派生人力分钟，但不擅自换算 token；磁盘字段为 `disk_delta_bytes`。cost ratio 只有相同单位、系统边界和可审计归属时可比较；未知核心成本或全局预算截断使完整运行成本声明 `inconclusive`，同时显示已发生费用。

迁移中的本地 recipe、嵌入重建、绑定、目标端证据再验证和人工重新解释属于 `system_runtime.migration`。导入胶囊 `verified=false` 不能继承源端证明；不能事先免费补齐目的端后才启动迁移计时。

## 9. 冻结清单和机器输出

冻结需要全部非空：代码/宿主/模型/基线版本，qualification 记录，独立数据切分hash，gold隔离审计，主比较族及m，最终样本与功效依据，任务/修复deadline，候选阈值决定，预算与停止规则，评分器校准和独立审阅。

每份报告同时保存：

- 数值或 `null`、分子、分母、有效历史簇数、未知数、infra_invalid/unsupported/not_run数及planned/started计数。
- 聚合权重、区间方法、置信度、多重校正族、bootstrap seed。
- `pass/fail/inconclusive/not_evaluated/not_evaluable` 及机器可读原因。
- 全量失败与费用，不能只公布成功样本。
- 分开的持久化正确性、实际 delivered context、行动结果，不能用某一层代替全部效果。

`analysis-plan.json` 是本文件的机器可读配置；`results` 不包含任何运行成绩。工程师应先实现拒绝未冻结正式比较、拒绝缺失成本归属和拒绝把未知写为零的验证器，再执行获授权的实验。
