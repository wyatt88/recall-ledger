# Real Host 对照审查：Native Memory 与 RecallLedger

日期：2026-10-13，Asia/Shanghai。岗位：程知行（Agent 系统科学家，AI 代理岗位）；由主代理整合审阅。源码：`6ff7f5824aa235046d27c18c971fe9f3b1dfa8ef`。配套：[主协议](PROTOCOL.md)。

**证据状态：项目实现与官方接口的 E2 审阅；实验设计为 H0、效果不确定。** 本轮只读审查代码和文档，没有运行真实 Host、模型或新的基准，没有修改 Host 配置或用户 Memory。本文的“可运行”仅指仓库已有相应代码入口，不表示本轮执行成功，更不表示 Real Host 验收通过。

## 1. 结论

比较需要两张独立结果表：

1. **Controlled Harness：**固定同一 reader、tools、prompt role、完整可见历史和总预算，替换 Memory backend。其 MD backend 是实验基线，不能代表某品牌 Native Memory。
2. **Real Host：**在同一产品、版本、模型与项目条件下，比较 Native default、Native tuned 与实际可安装的 RecallLedger 配置。产品隐藏行为无法固定时，只报告完整配置的差异。

现在最现实的前置问题不是挑 benchmark，而是确认 **RecallLedger 的 raw conversation 从哪里变成可召回的合法 Memory**。目前自动 capture 只产生 pending candidate；通用的 extraction、fact identity、纠正识别、admission 和 dependency discovery 没有由这些 Host adapter 实现。如果人工替 RecallLedger 补好 fact key 和依赖，却让 Native 从原话自行理解，结果不公平；如果不准入任何 candidate，则测到的是未接通的写入闭环。

Native 基线必须在独立状态中建立。不能先安装 RecallLedger，再把插件关掉就把剩余配置称为 Native：Codex/本地 ChatGPT 安装器会在项目 scope 关闭 Native Memory generation/use，默认卸载仍保留关闭状态。

## 2. 当前实现给比较带来的实际约束

| 接入 | 代码实际执行什么 | 会影响结果的差异 | 当前状态 |
| --- | --- | --- | --- |
| Codex / 本地 ChatGPT App | 共用 Codex 实现；安装三个 lifecycle hooks；项目 Native generation/use 关闭；`UserPromptSubmit` capture、flush、read；compact 后 refresh；Stop capture | 并非每个 tool continuation 都重新 recall；compact refresh 使用空 query；4000-character capture 会标记截断；lexical、最多 8 条、1800-character core budget，最终默认 4096-byte JSON cap | adapter/daemon fixture 入口可运行；真实 trust、有效配置、warm continuation 和 actual compaction 需 Host eligibility |
| KiroCrew | `GatewayBridge` 与 subprocess helper；从 frozen context 绑定 member/store/session/privacy；capture/read/refresh | 没有自动注册 Gateway lifecycle；`gateway_wired: false`；保留 Native V2，双写不是事务；同用 4000-character capture、lexical 与 1800-character core budget | bridge 入口可运行；真实 Gateway 接线需工程，不能称已替换 Native V2 |
| OpenCode V1 | plugin 在 system transform 请求 fresh recall；message/idle capture；compaction 只加刷新提示 | 固定 V1 接口，文档 pin 为 v1.18.35；lexical、最多 8 条、6000-character core budget；超过 capture limit 跳过；只读最近 32 条消息；排除 child sessions | JavaScript/Python/daemon fixture 入口可运行；真实进程、Bun、SDK 调度、compaction 需 eligibility；不能外推 V2 |
| MCP-only | 提供显式 Memory tools，由 Agent 决定是否调用 | 工具发现、调用意愿、审批和额外轮次是配置的一部分；没有自动写入/刷新保证 | 工具入口可运行；不能与 lifecycle 接入混称一个 L 配置 |
| Markdown projection | 提供受管 snapshot、manifest 与修改检测 | 文件内容写对，不等于 Host 已重读；同一文件作为 instruction 加载和作为 retrieved data 加载具有不同 prompt authority | projection 入口可运行；自动加载、刷新、缓存清除必须单独观测 |

实现锚点：

- [codex.py](../../../sdk/python/recall_ledger/codex.py)：`disable_native`、`install`、`run_hook`。
- [chatgpt.py](../../../sdk/python/recall_ledger/chatgpt.py)：仅复用 `.codex.main`，不是另一套独立 backend。
- [host_runtime.py](../../../sdk/python/recall_ledger/host_runtime.py)：`capture` 只提交正文和 origin/source；`read` 固定 lexical；`refresh` 总是重新读取。
- [kirocrew.py](../../../sdk/python/recall_ledger/kirocrew.py)：`FrozenContext`、`GatewayBridge` 与生成接线样例。
- [opencode.py](../../../sdk/python/recall_ledger/opencode.py)、[opencode_runtime.mjs](../../../sdk/python/recall_ledger/resources/opencode_runtime.mjs)：capture、recall、system transform 与 session 过滤。
- [projection.rs](../../../src/projection.rs)、[ingestion.rs](../../../src/ingestion.rs)：受管 snapshot 与 pending/accept 语义。

因此要将 L 配置写完整，例如 `L-host-default-lexical`、`L-harness-hybrid-evidence`。一个 adapter 的默认 lexical 成绩不能被宣传为已经测完 vector/hybrid retrieval；核心 evidence/repair 已实现，也不能推出该 Host 每次自动注入的都是 verified evidence capsule。需要在 trace 中保留实际 operation、record kind、verification state、query、budget 和返回内容。

## 3. Raw conversation 的 admission 阻断

### 实际缺口

`host_runtime.capture` 和 OpenCode `capture` 都不填写 `fact_key`，也不登记依赖；flush 只调用 `candidate_submit`。所有 origin 包括 user explicit/tool observation 都从 pending 开始，pending 不进入 recall。当前四元组 `fact_key` 是严格相等身份，不做 fuzzy entity resolution、指代消解、否定或日期推断；无 key 的不同 accepted events 会成为不同 records。参见 [intake contract](../../../docs/ingestion.md)。

直接对每条原话做 `accept` 也不是“完成纠正处理”：系统可能同时保留两个冲突、无关联的 records，不能靠已登记 dependency 的失效机制自动发现所有未知语义副本。

现有 `candidate_resolve` 只接受 decision 与 revision 条件，不能顺手给已捕获的无 key candidate 补 fact key；同一 event ID 改 payload 又会触发冲突。因此后续 pipeline 需明确原始 capture 与提取后 keyed proposal 的关系、独立稳定 ID、来源引用和撤回传播。不能把“给模型一个 accept tool”当作已经补齐这条链。

### 正式 L 必须冻结的 pipeline

`raw event → source-span capture → proposal extraction → identity/correction/dependency decision → admission → retrieval/view maintenance → Host injection`

至少记录：

| 决策 | 必须声明 |
| --- | --- |
| extraction | 是 Host Agent、规则、外部模型还是人工；可见输入与 prompt/version |
| identity / correction | 如何产生 fact key，如何定位旧 revision；不确定冲突如何保持未决 |
| dependency | 显式 source span、模型推断、调用关系分别记录；未知依赖不能伪装成已覆盖 |
| admission | 谁有权 accept/reject、何时运行、失败/积压策略、人工介入分钟 |
| consolidation / repair | 具体策略、worker 配置、模型或 model-free、运行时点与资源上限 |

三种实验配置必须分别命名：

- **Controlled admitted-event test：**给所有 backend 相同已准入结构化事件。可立即基于既有接口建设 runner，衡量 lifecycle 正确性；不测自然语言理解。
- **Raw-conversation autonomous test：**从相同原话进入各自完整 pipeline。L 目前需补齐并冻结可执行 admission policy，再做正式对照；本轮不实现或运行模型。
- **Human-reviewed test：**准入由人审，Native 允许同等信息与人工预算；单列为人机协作，不与全自动成本混算。

如果选择通用 Agent 通过 MCP 来负责准入，需固定 tools、提示、模型、可见事件与操作预算；这可以是合法的产品配置，但其 extraction、冲突决策、依赖漏记与失败都属于 L 的端到端成绩，不能归为“外部错误”而剔除。检索未暴露 stale record 却因所有新事实长期 pending 而无法完成任务，仍属于失败。

## 4. Native default 与 Native tuned

**Native default** 是安装后实际默认设置，包括 Memory 可能默认未开启；这衡量开箱体验。**Native tuned** 才是主要竞争对象，需使用官方支持的 Memory、搜索、维护、纠正与导入能力，并给与 L 相同调优机会。不能把关闭 Native Memory 的 N0 当 N2。

2026-10-13 实际打开 [OpenAI Memories 官方文档](https://learn.chatgpt.com/docs/customization/memories)：本地 Native Memory 采用后台生成，可能跳过活跃/短 session，并受 idle 与 quota 门槛影响。其测试需要两种时钟：

1. 固定用户事件到任务截止的真实时间，衡量实际更新延迟及可用性。
2. 预定、双方对等的 settling window 后测质量，记录后台工作和等待成本。

不能立即开新 session、在 Native 尚未完成正常生成时宣布其没有长期记忆；同样不能等 RecallLedger 后台完全处理后才开始计时。Native 若没有可观测 completion signal，记录未知并使用预定上限，不伪造“已完成整理”。

实际 Host 还必须核实 inherited/global/managed 配置、模型版本、Memory 开关及作用域、eligible session 条件和后台任务状态。官方文档只是接口依据，不认证本机账号、构建或 chat 的实际行为。使用独立测试 profile/目录或可验证隔离的临时部署，禁止修改用户现有全局 Memory 来制造干净基线。

## 5. 首个 Host eligibility 清单

优先审查能导出实际 model dispatch/context 的 Host。OpenCode V1 是代码可观察性候选，不是已验收结论；若真实可安装版本只有 V2，应记录 unsupported 并另做适配，不能运行旧 fixture 替代新 Host。

| ID | 测项与通过证据 | 现有代码支持 | 还需要什么 |
| --- | --- | --- | --- |
| H01 | 固定 Host/模型/adapter/backend 版本及配置哈希；确认 Native tuned 真启用 | adapter 配置、binding 与源码 pin 可读取 | isolated state factory；真实 Host effective-config readback |
| H02 | 两个独立 history A/B 放不同随机 canary，验证跨组不串；原配置/用户 Memory 未改变 | namespace/history/member binding 可检查 | Host/profile/session 隔离 driver；文件与配置前后指纹 |
| H03 | hook/plugin 被 Host 实际加载且受信；一次事件只进入 intended handler | installer receipt 与 fixture | Host load/trust 日志；不能以 `installed=true` 或 CLI exit 0 代替 |
| H04 | 用户原话与 assistant 来源分别 capture；重试不重复；oversize/skipped 可见 | pending、event identity、outbox 现成 | capture 到原始 source span 的映射 trace；Host scheduling 测试 |
| H05 | 一条普通原话经过冻结 policy 成为可检索记忆；纠正关联到正确 identity；模糊冲突不乱合并 | candidate/resolve/fact/revise API 现成 | 当前阻断：通用 extraction/admission/dependency pipeline 或明确人审配置 |
| H06 | 实际 model input 中能找到返回的 canary、revision、scope/conditions，且没有静默删改 | RecallLedger manifest 可给出 returned evidence | dispatch input recorder；仅 hook stdout 只能证明交给 Host，不能证明模型收到 |
| H07 | 同一 hot session 见过旧值后更新；后续真正执行正确动作 | revision/retract/fresh read 现成 | Host turn driver 与 artifact/tool verifier；不得只重置 L context |
| H08 | 自然或官方支持的 actual compaction 后刷新，记录旧 summary 与新 evidence 的并存 | compact fixture/refresh 入口现成 | actual compaction detector/trigger；不可控制时标未覆盖 |
| H09 | fresh session 与 process restart 各自恢复；已有 session resume 另测 | 持久化 manifest/outbox/binding 现成 | Host restart/new-session driver，启动成本与既存 transcript 判定 |
| H10 | daemon 停机、迟到结果、rebind 时保留可解释失败，不注入缓存旧包；错误仍入分母 | 既有错误路径、绑定与队列入口 | 真实 Host fault timing、观测 unavailable 后行为 |
| H11 | 所有模型 input/output、维护、embedding、修复及人工成本能关联上游 event | 部分 API metadata，尚无统一成本账 | trace/cost collector；Host 隐藏计量标 unknown |
| H12 | 架构与产品范围一致：root/subagent、privacy、background jobs 等缺失明确 | 当前 adapter 有限制与显式 privacy policy | 按真实 Host 测覆盖；不能用 root 通过外推 child |
| H13 | Native export/import 的方向、格式和界限，以及 RecallLedger bundle/handoff 可完成任务 | RecallLedger transfer API/示例现成 | 真实 native transfer driver 与固定 B 的接手验收 |

H05 未落实时不进入“全自动 Native vs RecallLedger”正式效果比较。H06 不可观测时仍可做黑盒端到端任务测试，但失败定位停留在“证据提供/Host/reader 未分解”，不能写出 Memory 机制因果优势。其他 Host 不支持的格写 unsupported/unknown/未覆盖，不能给零延迟或满分。

最小 eligibility smoke 用开发 canary，不接触封存测试：两条隔离 history、一次原话→准入、一次纠正、一次撤回、hot/fresh 各一次真实任务，再扩 compaction/restart。canary 同时有 Native 应该正常成功的稳定事实；只测撤回困难不能说明 everyday utility。

## 6. 输入、projection 与 token 公平

Controlled Harness 保持 Memory 块的 role、位置、模板和 tokenizer 相同；source metadata、JSON wrapper、manifest、事实限定及工具反馈都进入实际 token 预算。不能将 L 包装成更高优先级指令，同时把 Native 原文放在低优先级工具结果中，再把差异归因 Memory。

Real Host 保留该产品的正常加载方式，记录不同 role/位置/自动注入策略作为完整配置差异；强行把各 Host prompt 拼成一样，反而不再代表产品。Codex hook 的 developer context 和 OpenCode 的 system transform 都需要单列，untrusted 标签不是强制隔离。

至少存下四个大小：原始 source bytes、已储存 bytes、retrieval returned bytes/tokens、实际 model-visible tokens。中文、英文及 JSON 转义使 `max_chars`/byte cap 不能替代 token 计量。长文本截断/跳过、整条 budget omission、source closure 超限分别计数；来源全文保存并不保证每次注入全文。

Markdown projection 需记录 generation、manifest hash、生成/改写时间、实际文件读取时点、Host caching 和 model dispatch。把 20 KiB 文件写在磁盘却只加载前 2 KiB，应该归为 Host projection/预算丢失；Agent 无需读取该文件也完成的任务不能证明 Memory 生效。要求所有组在同样合法来源中自行检索，不向 L 独家提供预制“正确 handoff.md”。

## 7. Session 生命周期与失效定位

四个格互不替代：

- **Hot：**旧值实际送达过 reader，更新后继续原 session；记录没有 re-read 的 tool continuation。
- **After compaction：**旧值可能进入 Host summary；必须确认真实发生过 compaction。仅调用 compact fixture 只能验证 adapter。
- **Fresh：**新 session，只继承产品正常持久化状态和同样来源工具；不能在用户任务中重述答案。
- **Restart：**重启进程但保留规定持久化状态；与 resume 原 session、创建 fresh session 交叉标记。

每个指定 checkpoint 的探针在只读 clone 或隔离分支上运行，避免回答自身变成未来 Memory；自然回写轨单独报告。

推荐观测链：

`source visible → captured/omitted → candidate pending/resolved → record/revision → view/jobs → recall packet → Host inserted context → actual dispatch input → tool action/artifact`

每个箭头保存 event ID、wall-clock/monotonic 时间、状态和关联 ID。保留以下区分：

1. 原话未被捕获或被截断。
2. 原话已有，但事实提取/准入/纠正识别失败。
3. 合法来源存在，但 retrieval、derivation 或 metadata 丢失。
4. Store 返回正确包，但 Host 未加载、未刷新或保留旧 summary。
5. 正确且充分依据已送达，reader/tool 执行仍失败。

Store 内已 retract 只能证明未来受管读取不返回对应正文，不能证明既有 prompt/summary、外部副本或模型行为已经“忘记”。验证最终行动必须独立于 `validate_context` 的布尔结果。

## 8. 跨品牌 Migration 必须固定目的端 B

在相同 B、同一模型/tools/预算/后续任务中比較 A→B、B continuous、B Native tuned，以及 B empty/sufficient-evidence 诊断组。迁移损失是 B 上 A→B 相对 B continuous 的差异，不能用不同 A/B reader 的原始成功率相减。

按方向分别登记：

| 迁移情形 | 应如何比较 |
| --- | --- |
| Native 有官方 export/import | 允许使用，固定版本、支持的内容和必需步骤；算上格式转换、重建与人工时间；不能为了简化而禁用 |
| Native 没有通用 transfer，但可合法复制项目文件/Memory 或生成交接材料 | 使用该产品正常支持的最强方法，记录不支持的状态与重述量；相同源信息、工具与人工预算 |
| Native transfer 能力无法核实 | 标 unknown；不推断品牌无此能力，不计伪造性能失败 |
| 该方向明确没有所需功能 | 标 capability gap；用户恢复成本可测，但不称已跑过不存在的 API |
| 两品牌共享同一 RecallLedger | 这是 client handoff；单独测 Host 使用和 context 接管，不代表数据库或 Native 格式迁移 |
| RecallLedger A→另一 ledger B | 测 bundle 与目标 rebuild；目前 vectors/repair recipes 不随 bundle 搬迁，重建计时计费 |
| 旧品牌 Native Memory→RecallLedger→新品牌 | 需要真实 parser/format mapping；当前 generic bundle 不是任意 Native 格式 importer |

本轮打开 [OpenAI import 官方文档](https://learn.chatgpt.com/docs/import)，其明确提供若干产品到 ChatGPT desktop/Codex 的导入，包含特定 project memories；这是不能假定“所有 Native 都无法迁移”的直接反例。具体 A→B 覆盖仍需固定安装版本逐格验证。

除数据完整性，还要测试 B 的第一次真实任务、后续任务、恢复时间、人工重述、B 更正后的 A→B→A、旧 snapshot/旧 handoff 重放。恢复到来源存在而没有完成工作，应分别判为数据迁移成功、任务接手失败。

## 9. 性能记录不能偷换口径

服务层只比较等语义的 operation。Native 没有独立 Memory API 时，不得拿 Rust IPC 的毫秒数对比整个 Native 模型回合；也不得拿静态文件 read 时间代表 Native 的检索/维护。

端到端时间以用户任务可见开始到独立工件验收为准；后台 extraction/consolidation/repair、子进程启动、IPC、Python bridge、工具审批、queue、重试及人工都保留。重启、cold cache、warm cache 分开；主进程外的 daemon/worker 资源必须计入 total。

至少给出 quality–cost–latency 曲线与 task success、stale-use、coverage/refusal。无法取得真实 tokens 或账单的产品标 unknown，仅给可观测 latency/任务结果。请求回包大小不能推算隐藏 model cost。

## 10. 建议交付顺序

1. 为一个 Host 填完 H01–H13，先确定实际可控、可观察及 unsupported 部分。
2. 完成隔离 replay/dispatch/cost/verifier runner；Controlled admitted-event baseline 可先建设。
3. 给 L 固定真实 extraction/admission/dependency pipeline；给 N2 同等调优预算，保留 source-only 强基线。
4. 先运行开发 canary，再做主协议中的 smoke；费用、模型调用与总预算另行明确，本轮没有执行授权扩张。
5. 封存 fresh history 后运行正式效果比较；再扩第二个 Host 和 A→B→A。

最强可证伪判断：若 N2 在相同预算下同样少犯 stale-use，稳定任务更准且更省，RecallLedger 的收益应收窄为可审计 lifecycle 与 portability；若 L 必須靠人工 gold 才能维持质量，则不能声称全自动优于 Native。当前实现支持开始建设这些对照，但现有 fixture 与 daemon 测试不能替代最终结果。

## 本轮来源与读取深度

- 本项目四份 Host 文档、SDK adapter 主路径、candidate intake 与 projection 实现：E2，实际读取实现片段与接口合同；未运行 Host。
- [Memories](https://learn.chatgpt.com/docs/customization/memories)：2026-10-13 实际打开并读取相关正文；用于 Native generation timing/配置边界。
- [Hooks](https://learn.chatgpt.com/docs/hooks)：同日实际打开并读取 trust、context delivery、compact 相关正文；用于说明 installer 与 actual Host admission 的差别。
- [Import from another agent](https://learn.chatgpt.com/docs/import)：同日实际打开并读取支持范围；仅证明官方声明存在部分迁移入口，不证明某一迁移方向无损或本机已可用。
- 研究方法延续 2026-10-11 的早期协议，并以本轮源码与主协议收窄到当前缺口。早期草案未收入本发布包，后续实现以 Benchmark 当前规格为准；本轮未新增论文 benchmark 检索。
