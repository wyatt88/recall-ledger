# Harness 接口与数据合同

版本 `0.1.0-design`。以下任务执行和评分接口待实现，不存在端到端 Benchmark runner。[环境发现与费用账本](PREFLIGHT.md)是已开始实现的独立基础工具，不实现下面的完整Controller/Worker合同。示例 assertion 是判分 DSL 设计，没有已实现语义解释器。

## 1. 信任与进程边界

```text
private evaluator: sealed history + future events + gold + grader
        │ publish only currently visible input
        ▼
replay controller ── isolated backend / Real Host ── sandbox tools / artifacts
        │                       │
        └─ timing/cost events ───┴── trace collector
                                     │
                                     ▼
                           private verifier → reports
```

文件名、namespace 和提示语不构成隔离。正式 executor 只能读取当前公开 prefix、任务及自己的状态，不能访问其他组 trace、gold、未来事件、Judge错误详情。controller 的时间表与 gold 分离，避免 driver 自行选择最有利 checkpoint。

## 2. 配置与 manifest

`systems.lock.json` 每项至少包含：

`configuration_id, agent_id, system_id, track, input_mode, host_name, host_version, model_id, model_revision, tokenizer_id, adapter_revision, backend_revision, effective_config_sha256, native_memory_enabled, ownership_policy, admission_policy, retrieval_policy, consolidation_policy, overflow_policy, timeout_policy, observability, qualification_record`。

未知值保留 `null` 和原因，不填 `latest` 代替版本。H_NATIVE_* 只能来自真实Host；C_* 不包含真实Native label。claim所需字段无法固定时 downgrade scope 或阻止冻结。

2026-10-14 多 Agent 身份约束：`system_id` 保留 H_* / C_* 的模式含义，**不是跨 Agent 唯一配置主键**。冻结时对单项完整配置锁（不含 configuration_id 本身，以 RFC 8785 规范 JSON 编码）取 SHA-256 作为 `configuration_id`；锁包含 Agent/backend/model/adapter 版本、子 Agent 拓扑及资源/运行时指纹。`agent_id` 是可读 Agent 身份，Controlled 使用明确的 reader 身份。未冻结候选仅有唯一 `candidate_id`，configuration_id 留 null，不能启动。

run、initial state、trace、cost、artifact 路径、migration source/target 和 report join 都必须携带 configuration_id 或唯一指向它的受约束 run_id，不能仅用 H_* 分组。Native/RL 配对按显式配置 ID 列表建立；不同 Agent、模型、版本的配置不得被模式名静默合并。既有 `config_id → system_id` 只是旧字段别名，不等于此新增主键；旧数据仅在 systems lock 可唯一解析时迁入，否则拒绝作为多 Agent 结果。

`run manifest`：

`run_id, attempt, protocol_sha256, analysis_plan_sha256, systems_lock_sha256, data_manifest_sha256, evaluator_revision, cluster_id, history_id, counterfactual_branch, configuration_id, agent_id, system_id, lifecycle_profile, checkpoint_id, task_event_id, replicate, seed, budget_id, initial_state_id, start_time, terminal_state, failure_reason, response_kind, observation_status, invalidation_reason`。

每个 initial state 与 configuration/history 绑定；重试生成新 attempt 并引用原run，不能覆盖原日志。

`terminal_state` 为 completed/failed/budget_exhausted/infra_invalid/unsupported/not_run；`response_kind` 可为 substantive/refusal/clarification/none；`observation_status` 为 complete/partial/unknown。任务级 timeout/system_error/task_budget_exhausted 是 failed 的原因，不是新的终态。budget_exhausted 专指全局预算停止导致的中断；未开始的任务用 not_run。具体分母处理以 SCORING 为准。

## 3. Public event contract

开发样例 `schema_version=recall-ledger.benchmark.events/1`，每行字段：

| 字段 | 语义 |
| --- | --- |
| history_id / cluster_id | 一条分支与其上层独立历史簇 |
| event_id / sequence | 分支内唯一事件与严格递增顺序 |
| kind | source_event / task / lifecycle / fault / transfer |
| actor / scope | 原始说话者/事件产生者与合法项目范围，不据此直接授予操作权限 |
| payload | 该时刻原始可见内容或工具可执行刺激；不包含评分答案标签 |

source_event 的自然语言更正本来可以含新值，这是合法输入；不能额外填 `correct_record_id`、gold dependency或预选答案段落。Mechanism admitted-events 是独立输入schema/mode，须明确所有组得到相同结构化映射。

任务下发只含目标、公开约束与工具权限。标准答案、期待端口/费用/是否可答、hidden tests仅在gold；Agent读到历史中的原始目标值与读到答案标签不是同一件事。

正式schedule由run lock另存 `visible_at_monotonic_offset_ms`、deadline及fault barrier；历史中的语义日期不驱动系统时钟。示例只有逻辑顺序，尚无真实时间schedule。

### Admitted-event contract

`recall-ledger.benchmark.admitted-events/1` 是另一个待实现 schema，仅用于机制对照；不能把 raw event stream 改个标签就声称完成准入。

每项保留 `cluster_id, history_id, event_id, sequence, operation, record_id, revision, scope, content, provenance_refs, qualifiers, supersedes, dependency_ids`。`operation` 为 create/correct/withdraw；qualifiers 包括适用条件和有效期，未知值显式为空并有说明。provenance_refs 必须指向双方同样可访问的原始事件及内容哈希。dependency_ids/supersedes 为机制实验的共同输入，所有组都得到，不由 gold task answer 生成。

准入数据由独立数据构造流程产生并审核，记录作者、映射规则、源版本和切分指纹；任务答案与判分标签不随准入输入交付。C_LIFECYCLE 必须实际使用同等版本、依赖和撤回合同。`withdraw` 的来源仍可按被测系统声明的历史审计权限保留，但不能作为当前许可或 current-read 结果；不据此假定完成物理删除。当前设计包没有供应八簇 admitted pilot 数据。

## 4. Backend / Host driver

每个operation返回 `operation_id, started_ns, finished_ns, status, output_ref, evidence_refs, cost_event_ids, observability`；时钟为 monotonic，必须声明跨进程clock映射。

| 接口 | 输入 | 必须返回或记录 |
| --- | --- | --- |
| reset | 隔离profile及初态快照 | state_id、scope绑定、是否成功清空、用户配置未变证据 |
| ingest | 一个原始事件或已声明 admitted event | captured/pending/admitted/omitted，stable IDs，截断/准入原因 |
| query | query、scope、时点与budget | 实际正文/metadata、record revisions、策略、omission/error原因 |
| transition | hot/fresh/compaction/restart | 实际Host事件/新session ID，unsupported须显式 |
| snapshot | 状态ID与checkpoint | snapshot ID及一致性语义；不能clone时记录重放方案 |
| transfer | source/target、format、截止与预算 | preview/apply/verify状态、native人工步骤、重建job IDs、gap |
| collect | bounded日志游标 | model input可见性、tool/action/artifact、后台状态与计量 |
| close | 成功截止和结算deadline | cancellation/drain结果、残留job、未知费用 |

这些接口不是额外提供给Agent的“正确答案工具”。C_RAW、C_LIFECYCLE自己处理metadata；RealHost保留其正常机制，测试driver不代替它完成admission、检索或迁移。

每task的工具环境必须相同。正常业务环境可被工具发现的信息不隐藏来人为抬高Memory价值；Memory-dependent与rediscoverable任务分层。Mock部署、邮件发送等副作用只在sandbox日志，不接真实外部收件人或生产服务。

## 5. Trace contract

每行 `trace_id, run_id, configuration_id, event_id, span_id, parent_span_id, component, operation, scheduled_ns, started_ns, finished_ns, status, input_ref, output_ref, evidence_refs, error_code, budget_snapshot`。

component 可为 capture/extract/admit/store/retrieve/project/model/tool/repair/transfer/evaluator。

model trace优先记录实际provider input/output及usage；若只能看到hook stdout，标 `observability=host_delivery_only`，不能冒称已进入model input。正文是实验材料，正式发布前按数据卡权限去敏；hash保留溯源。

边界事件分别编码：

- `capture_truncated` / `capture_skipped`
- `pending_timeout` / `conflict_unresolved`
- `retrieval_miss` / `insufficient_evidence`
- `source_closure_budget_exceeded`
- `packet_overflow` / `context_overflow`
- `host_not_refreshed`（仅有实际证据时）
- `reader_action_failed` / `tool_failed`
- `unsupported` / `unobservable`

没有观测不能推定为正常或归因失败。Gold替换诊断单独run，不参与主成功率。

## 6. Cost event contract

唯一收费/资源事件 `cost_event_id`，包括：

`budget_id, run_id, cluster_id, history_id, configuration_id, agent_id, system_id, originating_event_id, job_id, stage, ledger, operation, provider_request_id, started_ns, finished_ns, input_tokens, cached_input_tokens, output_tokens, embedding_tokens, cpu_seconds, gpu_seconds, peak_rss_bytes, disk_delta_bytes, wall_seconds, human_seconds, billed_amount, currency, price_snapshot_id, accounting_status, linked_checkpoint_ids`。

Controller、EKS 控制面等共享 infrastructure 费用的 configuration_id 可为空，但必须标记 shared 并保留分摊合同；单一被测配置的费用不得用 null 绕过归属。迁移费用同时引用 source/target configuration_id，不能因被两端观察而重复入账。

计量注意：

- 不同单位分别保存；CPU/GPU/墙钟/人工不能简单相加作为elapsed time。
- cached_input是input的子项，防止重复计token；money若有实际账单按账单，否则标估算及价表。
- 一个后台job引用多个checkpoint只记一次；汇总可做分摊，但保留未摊销总量。
- input tokens包含所有reader轮次的实际完整输入；同一source多轮重复进入模型，每次计相应usage。
- stage 为 D0/P0/P1/P2/P3/P4/P5，与四个互斥 ledger（system_runtime/evaluation/development/infrastructure）正交。online 为 system_runtime，judge 为 evaluation，调优/数据构造为 development，额外 replay setup 与 harness overhead 为 infrastructure；被测系统真正需要的 ingestion/修复不能挪入 infrastructure。
- pilot 内 reader、embedding、维护仍为 stage=P2、ledger=system_runtime，不再因为发生于开发期而重复计费。报告可按 stage 或 ledger 分组，同一 cost_event_id 只能入总账一次；部署场景成本分摊需另存权重与假设。
- accounting_status 为 measured/estimated/unknown/unsettled；unknown不填0。

reservation按project预算原子登记。一次请求的实际金额若可能超预估，保留余量与停止条件；budget freeze必须选定计量方法，不能把provider剩余额度当本项目预算。

## 7. Gold / verifier / grader contract

gold schema `recall-ledger.benchmark.gold/1`；具体开发断言见examples。gold与task事件关联，保持当前合法状态与受影响scope。正式verifier必须自己实现判据，不能从被测输出生成expected value。

评分包括：

1. artifact assertions：结构化文件/输出路径中的字段、允许修改范围、完整限定条件。
2. tool trace assertions：实际调用目标、次数、顺序、禁止副作用。
3. answerability：合法证据是否足够；可答失败与正确拒绝分开。
4. evidence diagnostics：selected/delivered的支持关系，仅在可观测时评分。
5. migration invariants：identity、scope、pending、tombstone、来源重新验证和实际工作接手。

正负控必须在正式运行前通过：正确产物通过；仅说“完成了”但没产物、旧值、错scope、删掉全部记忆或全拒绝不能通过。

开放文本需要冻结rubric及盲化Judge prompt/model。候选人工复核范围为所有严重违规/争议，加其余结果固定seed随机10%（至少20项，少于20则全量）；这些是拟执行审阅量，需计预算并在开发后冻结。分歧按预定裁决流程解决，不按系统名选择有利裁判。

trace缺失时 outcome可有artifact证据支持，但模块归因unknown；artifact也缺失时不得靠自述打成功。broken evaluator属于预定义infra_invalid，保留原证据并对受影响配对对称重跑。

## 8. Report contract

至少输出：

- configuration/support matrix：版本、模式、实际能力、未覆盖格；
- per_task.jsonl：终态、answerability、success、stale-use、scope违规、成本/延迟与unknown；
- per_cluster.jsonl：配对聚合，不能丢弃失败分支；
- suite_results.json：primary contrast、effect/CI、guardrails、pass/fail/inconclusive/not_evaluated/not_evaluable；
- longitudinal checkpoint曲线及成本/覆盖、migration矩阵、performance负载表；
- failure gallery：可复现输入prefix、artifact、trace、已知/未知归因；
- reproduce manifest：全部配置/数据/代码指纹与protocol frozen标记。

未运行格显示 `not_run`，不渲染为0分、0延迟或100%通过。不生成虚构示例成绩；设计包仅含空结果字段/判据。
