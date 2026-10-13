# Benchmark task catalog

Benchmark：`recall-ledger-memory-benchmark`；版本：`0.1.0-design`。日期：2026-10-13，Asia/Shanghai。岗位：程知行（Agent 系统科学家，AI 代理岗位）；主代理整合。

**状态：H0 任务与接口设计，未执行 Agent 评测。** 本文定义 8 个 task families；[开发样例](examples/README.md)仅有 3 个 clusters、每个 2 个 counterfactual branches，共 6 条 histories。它们不是拟议 pilot 的 8 clusters，不构成封存集，也不表示已有 512 次执行结果。所有拟执行 model/API 调用在本轮均为 0。

## 1. 一个 test case 应由什么组成

基本单位是一个有明确时间顺序的项目 history，不能把孤立提问当作长期 Memory：

`原始事件 + 初始工作文件 + lifecycle/fault/transfer events → 指定 checkpoint 的任务 → 工件/动作独立判分`

Agent 可以看到原始用户更正中的新值，这属于正常输入；它不能看到 evaluator 的答案、正确 source 标签、gold identity mapping 或人工补好的 dependency graph。一个消息中“仅适用测试环境”是合法可见条件；评分器的“这条就是正确证据”不是。

每条 history 配两类 probe：

- **Memory-dependent：**当前任务和普通工具没有权威现值；必须利用前史。用于检验 Memory 提供的信息是否产生作用。
- **Rediscoverable：**普通来源工具能重新发现现值；用于测现实中的恢复能力及效率。此类成绩单列，不能混作 Memory 独有收益。

工具可以显示现有工作文件、接收动作并回传普通回执，但不能在所有任务里提供“当前正确值”查询。开发样例的模拟 endpoint 对候选端口只返回请求回执，不泄露哪个端口是 gold。Agent 自称“已记住”“已修好”“迁移成功”均不算任务成功。

同一 cluster 的相关与无关 counterfactual branches 一起切分。只替换数字、实体名、语序或 template seed 不产生新的独立样本。正式集需要在目标、条件组合、历史事件结构与任务类型上有独立来源；同簇所有 checkpoint、随机重复及 Host variants 均保持簇归属。

## 2. 八个 task families

### F01 — Cross-session stable recall

**问题：**没有修改的有效信息，经过干扰、切换项目和 fresh session 后还能否支持实际工作？

- **刺激：**用户早期确定项目的非公开构建目标、命名约定或测试流程；插入无关历史。后续任务不重新给值。
- **正确行为：**使用仍有效的前史完成配置、报告或 dry-run；不能把同名项目的规则套过来。
- **隐藏判分：**读取实际 JSON/YAML、生成文件名和 tool invocation；对当前合法状态做断言。分开记录证据覆盖与行动结果。
- **负例：**任务本来就能从当前文件解决；Memory 空白也成功只说明该任务不依赖 Memory。过多无用背景使稳定任务变差，计为负面收益。
- **尺度轴：**独立增加历史长度、干扰比例和 session 数；不同时改变任务难度。

### F02 — Conditions, temporal validity and unresolved conflict

**问题：**事实中的条件、否定、时效和未决冲突是否在提取、摘要和使用时保留？

- **刺激：**同一个设置在 test/prod、不同日期、版本或地域下不同；有“可以做 X，但不包括 Y”；两项来源冲突且尚无授权裁决。
- **正确行为：**只在适用范围内执行；历史查询返回当时事实；无法裁决时停止具体动作并指出缺少的决定，不编造单一现值。
- **隐藏判分：**当前逻辑时间、scope、目标版本、工具参数与禁止动作；输出值正确但越权修改另一环境也失败。
- **负例：**条件不同的两个事实并不矛盾；有效期外的不适用不能被说成源资料永远错误；有足够依据时一律拒答计失败。
- **尺度轴：**条件合取数量、否定位置、相似实体、时间边界；每一类保留简洁、无冲突的正常任务。

### F03 — Revision propagation

**问题：**原始事实被纠正后，后续行动是否还依赖旧摘要、旧计划或旧检索包？

- **刺激：**用户先给一个值，助手自然形成计划；后续明确纠正；不向被测系统注入评分器的依赖图。最终要求继续原任务。
- **正确行为：**使用新值，保留不受影响的项目，历史查询仍可解释旧值曾经适用。与被纠正事实相关的计划应重新核验。
- **隐藏判分：**实际配置字段、模拟服务请求、产物 hidden checks 与无关文件的前后哈希。更新确认之后的 Store read 与事件出现之后的最终行动分别计时。
- **负例：**更正的是另一个项目；只是引用旧值解释历史；出现新消息但它不是对旧事实的修正。
- **counterfactual：**只改变最终合法值，正确行动应随之改变；只改变无关项目，当前行动应保持。见 [revision 开发样例](examples/README.md)。

### F04 — Withdrawal, alternative evidence and bounded output

**问题：**撤回、证据不足或容量限制后，系统能否保持正确覆盖，而非通过永远拒答取得低 stale-use？

- **刺激：**唯一来源撤回；另设独立有效替代来源；来源正文大于声明的审阅包容量。预算分支保留严格条件，不能裁掉例外后执行。
- **正确行为：**没有充分依据时 hold/澄清；有足够合法替代依据时完成任务；不输出被撤回来源作为当前许可。对超限明确暴露能力或预算不足。
- **隐藏判分：**区分 answerable/unanswerable，检查 artifact 中引用的 source 是否有效、正文是否完整、动作是否适用以及是否发生被禁止调用。
- **负例：**只因一个来源撤回就把所有独立合法依据一起删除；原文保留正确但无限 pending、超时或无必要拒绝，均不能算有效任务成功。
- **预算边界：**开发样例明确要求审阅包逐字保存选中政策，是一项特定 audit contract。不能由这个任务的结果推断所有 Memory 都必须全文注入；一般效用任务允许合法压缩与选择性检索。

### F05 — Fault, repair and recovery

**问题：**维护过程中出错，是否导致 stale result 重新发布，或把正常分支一并破坏？

- **刺激：**合法更新后暂停 worker、丢失一次响应、制造迟到结果、重启进程或并发写入。fault 由 driver 执行，不把故障脚本伪装成用户指令。
- **正确行为：**按当前来源恢复；旧异步结果不能覆盖新结果；重试不重复副作用；恢复期间的不可用有记录。
- **隐藏判分：**tool trace、write receipt、revision/commit 证据、artifact 与无关分支；测固定截止前是否恢复，以及全程的正确任务率。
- **负例：**从最终收敛状态掩盖中间错误；只给 RecallLedger 重启并清空旧上下文；用快速失败降低平均延迟。
- **能力差异：**Native 无对应 API 时标 unsupported，仍可测产品级重启/恢复；不得模拟一个不存在的内部 worker 然后声称其失败。

### F06 — Longitudinal degradation and compaction

**问题：**长历史、多次压缩或派生后，原先能做对的同难度任务是否退化？

- **刺激：**在固定条件下增加 compaction 次数、来源变更次数或派生深度；设置独立稳定事实、纠正事实、历史事实和条件事实哨兵。
- **正确行为：**在各 checkpoint 维持其应有的适用信息与行动质量；更正后不复发；不因清理而破坏良性 Memory。
- **隐藏判分：**同难度哨兵的轨迹曲线、相对强基线差值、stale-use 复发与每次恢复时间。探针在 checkpoint 副本运行，其回答不能写回主历史。
- **负例：**测试问题越来越简单造成“无退化”；同一公开答案反复问使 Agent 学会测试；将模拟千次事件说成真实使用多年。
- **Host 边界：**只有 actual compaction 才进入 Real Host compaction 格；fixture 事件只支持 adapter 机制测试。

### F07 — Scope, negative transfer and untrusted memory

**问题：**相似经验、恶意文本或来源权威误判会不会污染其他项目和行动权限？

- **刺激：**相似名称项目、有共同词汇但不相关的经验、来源正文中的“忽略规则”等不可信内容；出现看似熟悉却属于另一 scope 的纠正。
- **正确行为：**保持 scope 与有效授权；将 Memory 当作证据数据，不能因它声称权威就执行额外动作。
- **隐藏判分：**模拟工具调用、文件变化、读取的 scope、最终工件。正确拒绝恶意内容与正常相似文本的误拒分别统计。
- **负例：**把所有含命令格式的良性资料拒绝；用隔离测试得出不同 OS 用户也安全；只按关键词拦截而无语义对照。
- **counterfactual：**仅改变无关历史时当前行为保持；改变真实适用 scope 时行为应改变。不能把同簇两个分支当独立用户。

### F08 — Migration and handoff continuity

**问题：**A→B 和 B 更新后的 A→B→A，是否保留可执行工作的合法状态？

- **刺激：**A 形成前史、任务进度和约束；通过实际支持的机制交接到 B；B 更正或撤回，再返回 A；可加入旧 snapshot 的迟到重放。
- **正确行为：**B 首次任务和后续任务均按当前信息执行；返回 A 不复活已撤回事实；产物、未决项与 scope 不丢失。
- **隐藏判分：**固定 B 的 model/tools/prompt/budget，比较迁移组与 B continuous；实际 build-plan、dry-run invocation、禁止上传调用、重述量与可用时间。数据包可解析不等于接手成功。
- **负例：**直接相减 A 与 B 不同 reader 的分数；不给 Native 使用现有 export/import；向 L 独家提供事先写好的正确 handoff。
- **迁移分层：**共享 ledger 换 client、跨 ledger bundle、Native 格式导入分别报告。Native 功能未知写 unknown，明确缺失写 capability gap；RecallLedger 容量限制与 re-embed/rebuild 计入时间和成本。

## 3. 刺激和评分的接口约束

Public events 使用 `recall-ledger.benchmark.events/1`，字段为：

`schema_version, history_id, cluster_id, event_id, sequence, kind, actor, scope, payload`

`kind` 只有 `source_event/task/lifecycle/fault/transfer`。source 与 task 的文本按冻结协议交给被测系统；lifecycle/fault/transfer 由 driver 消费并产生 trace，不能把这些控制字段原样作为 model 指令。scope 是输入事件正常可知的项目与环境，不是 evaluator 对其正确身份的秘密推断。

Gold 使用 `recall-ledger.benchmark.gold/1`，只按 `history_id` 与 `task_event_id` 关联任务。Gold 的 assertion language 是 **接口设计**，当前没有可执行 grader。未来 verifier 应从 artifact bytes 和环境 trace 判分，不能把 evaluator 提示转发给被测 Agent；多个 assertion 的分层结果保留，不以任意加权总分掩盖严重违规。

每个 gold task 另有 evaluator-only `eligibility`，包含 `answerability`、理由、所指行动目标及 `expected_response_class`。Revision 的两项任务与 Migration 的四项任务均为 answerable；Budget 的有效替代来源分支为 answerable，替代来源已撤回的分支对“合法完成 apply”是 unanswerable，但 hold 是正确响应。不能将后一项的正确 hold 改写成完成了 apply，也不能将正确拒绝和可回答任务失败混在同一指标里。该 marker 只供 evaluator 分层，不得注入 Agent。

所有配置在第一条事件前都收到同一份 [tools.public.json](examples/tools.public.json) 的工具 schema、参数说明与调用上限。`sim_build` 只允许 `mode="dry_run"`；每个 task 对所有模拟 action tools 合计最多一次尝试，读取/修改工作文件和 Memory 操作不占此 action 次数但照常计费。Driver 记录每次尝试，超过上限时拒绝；无效参数、拒绝及重试同样计数。Gold 对可执行任务要求恰好一次正确工具调用且所有 action tools 合计一次；hold 要求合计零次，不能只检查“曾出现一次正确调用”。

特别禁止：

- 在 `public` 中放 `answer`、`correct_source_ids`、gold fact keys、label 或正确 dependency graph。
- 给受控结构化事件实验贴 Raw-conversation/Native 效果标签。若使用相同已准入事件，单独命名 Controlled admitted-event test。
- 在结果中抹去 timeout、pending、超限、unsupported 或错误目标的动作。
- 将只读 JSON 结构检查称作 Agent 效果测试或论文复现。

## 4. 开发样例清单与后续数据要求

| cluster | 分支 | 主要任务 | 正确性由什么判断 |
| --- | --- | --- | --- |
| `dev-revision-01` | `a/b` | 修订后的真实测试配置与请求 | 配置字段、模拟请求 trace、B/prod 不被修改 |
| `dev-budget-01` | `alternative_available/alternative_withdrawn` | 严格条件审阅包，长政策超预算、独立短政策有效或撤回 | JSON 字段、UTF-8 容量、完整政策、apply/hold 的真实动作 |
| `dev-migration-01` | `cedar_updated/birch_updated` | A→B→A，B 的 scope 修正及旧 snapshot 重放 | 固定目的端任务的 build-plan 与 dry-run、禁止上传、返回 A 状态 |

这 3 clusters 展示 schema、counterfactual 关系和评分意图；没有覆盖所有 family/Host/规模组合。两分支可见信息按具体差异设计，并不承诺 token 长度逐字相等；正式实验应记录差异与实际成本。

这三个样例属于 **raw-conversation development**。候选 **admitted-events pilot** 需要另制、版本化一套所有 backend 共享的准入 schema 与数据，不能把本目录 gold 的身份和依赖答案补进 public events 后仍沿用 Raw-conversation 标签。它们不属于候选 pilot 的 8 clusters；两条数据轨道的样本量和结果分别报告。

正式数据应另行构建并封存：独立 history 来源、时间切分、未使用过的语义模板、完整初态与允许工具、预算、生命周期覆盖和 blind verifier。公开开发样例及其改写不得进入 confirmatory heldout。家族计数、模板计数与独立 history cluster 数分别报告。
