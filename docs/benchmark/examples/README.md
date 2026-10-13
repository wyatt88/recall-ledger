# Development examples

Benchmark：`recall-ledger-memory-benchmark`，`0.1.0-design`。

这是 **3 个公开开发 clusters × 2 branches = 6 histories**。目录不是安全隔离区：public events 与 evaluator-only gold 虽分文件，却处于同一目录，任何可读这个目录的 Agent 都能看到答案。所有文件标记为 `not_for_confirmatory_evaluation: true`，不能用于正式效果结论。

本轮只交付设计数据；没有调用 model/API、运行 Host、实现产品 admission pipeline 或执行 grader。Gold assertion language 只是未来 verifier 的接口设计，JSON 能解析不表示任务已经通过。

| 文件 | 内容 |
| --- | --- |
| `manifest.json` | 数据用途、分支关系、模拟环境与运行约束 |
| `tools.public.json` | 所有配置在首条事件前收到的固定工具 schema、`dry_run` 语义和 action-call budget |
| `revision.public.jsonl` / `revision.gold.jsonl` | Atlas 修正端口，实际修改测试配置并发出模拟 probe |
| `budget.public.jsonl` / `budget.gold.jsonl` | 证据包硬容量限制下，分别使用有效替代政策或 hold |
| `migration.public.jsonl` / `migration.gold.jsonl` | A→B→A，修正属于 Cedar 或 Birch；检查 scope 与撤回复活 |

## 加载与隔离规则

1. manifest、gold、隐藏 verifier 和初态哈希由 evaluator 持有。将 public event stream 按 history 分别复制到新 sandbox，并在首条事件前向所有配置交付 `tools.public.json` 的 instructions 与 tool schemas；Agent 不能读取本目录或 evaluator 的父目录。
2. 只按 `sequence` 逐条交付，不给未来事件。公开源消息里自然出现正确新值属于正常用户输入，不能删除后指责 Agent 没记住。
3. `source_event.payload.text` 表示实际来源内容；`task.payload.prompt` 表示任务；其余 kind 由 driver 操作 Host，只有 Host 正常产生的可见结果才进入会话。
4. lifecycle/fault/transfer 未实现时记录 unsupported，不能把 JSON 中写了 `compact` 当 actual compaction 成功。
5. `gold.jsonl` 必须由隔离 verifier 根据 artifacts 与 tool trace 执行；不得给被测模型或 extraction/admission 模型读取。
   所有 `tool_trace`、`forbidden_side_effect` 与次数断言仅读取该 `gold.task_event_id` 对应 run 的动作范围。迁移样例 e04 与 e09 分别判分，不能借用前一个任务的正确调用，也不能把两个任务的次数相加。
6. counterfactual branches 使用完全独立状态，归同一 cluster；正式 split 按 cluster 整体切分。
7. SDK/daemon 测试可使用这些结构做开发；如果提供 gold fact identity/dependencies，那必须另列为 Controlled admitted-event test，不能沿用 Raw-conversation 对照名。

这三组是 raw-conversation development examples，不是候选 admitted-events pilot 的数据。后者应另制共享准入 schema 和独立 pilot clusters；不能把本目录的 gold 转成训练/准入答案而继续声称测原始会话处理能力。

## 模拟环境不是答案查询服务

所有写入和调用都在一次性 sandbox，路径为 sandbox-relative。`sim_probe` 与 `sim_build` 只记录调用参数，并给普通请求回执，不返回当前合法配置；不访问真实网络或执行外部构建。文件工具仅允许读写提供的项目文件，禁止读取 gold、driver 状态或未来 source。

为了检验 Memory-dependent 行为，revision 样例的磁盘配置保留初始旧值；tool trace 的隐藏 verifier 才知道新值。迁移样例的工具接受多种 target 并记录，不能通过工具错误消息枚举到 gold target。任务成功要求正确产物和对应动作都满足，不接受 model 自报成功。

工具 schema 与限制对 Agent 可见：`sim_build` 的 `mode` 只允许 `dry_run`。每个 task 对所有模拟 action tools 总计最多一次调用尝试；driver 记录并拒绝超额调用，参数错误、被拒绝和重试都计入尝试。文件与 Memory 操作另行计量，不占此 action 上限。Gold 除一次正确 request，还检查该工具调用数与总 action 尝试数；出现额外错误 request 不能被最后一次正确 request 抵消。需要 hold 时预期没有 action 尝试。

budget 样例针对明确的审阅包 contract：每份选中政策必须逐字保存，`evidence_text` 合计最多 360 UTF-8 bytes。长政策超过此 cap；有效且独立的短政策可以被选用。该 cap 是任务产物限制，**不是模型 context/token 上限**。更一般的 Memory 比较仍允许压缩、选择性检索和其他合法表示。

迁移的 `from_host/to_host` 为 runner 配置别名，不预设品牌。必须固定目的端的 reader/tools/budget，在同一 B 上另建 continuous/Native 对照。`configured_memory_path` 表示按该配置真实支持的方法迁移，不是声称已实现统一 Native 导入。

Gold 中的 `eligibility` 是 evaluator-only marker。它针对行动目标标记 answerable/unanswerable，并说明预期响应类型。Budget-withdrawn 分支不能合法完成 apply，因而是 unanswerable；写正确 hold 是该分支正确响应，不代表完成了 apply。其余七个 gold tasks 可完成指定行动。这个分层信息不能交给 Agent，也不能用来丢掉失败样本。

## 何时可以说“样例通过”

- 当前可说：文件可解析、ID 与引用一致、counterfactual 分支及断言已设计。
- runner 实现后可说：某一次固定配置在某个开发样例的工件断言通过。
- 不能说：3 clusters 证明长期不退化、Native 优劣、真实 Host 接入通过或正式显著优势。
