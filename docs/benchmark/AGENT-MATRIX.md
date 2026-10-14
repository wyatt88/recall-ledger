# EKS Agent 测试矩阵

日期：2026-10-14。负责人：Agent 系统岗位。状态：设计与源码审阅，未执行 Agent、模型/API、GPU 或云实验。
产品实现审阅基于 RecallLedger `7ccad29ebdd8eee476cdfdf06ed813502ca5e412`；能力判断为 E2，效果与可运行性仍待验证。
本文件补充 [SPEC](SPEC.md) 和 [INTERFACES](INTERFACES.md)；EKS 是主测试平台，桌面 App 通过独立 external worker 接入同一 Controller。

## 1. 三种模式与资格

| 模式 | Benchmark ID | 必须满足的含义 |
| --- | --- | --- |
| Native-only | `H_NATIVE_TUNED` | 保留并正常启用该版本官方支持的 Memory、session、compaction 和工具；开发集调优机会与对手相同 |
| RL-integrated | `H_RL_INTEGRATED` | RL 拥有已声明动态长期事实的写入、准入、修订、撤回和刷新；保留 Host 正常对话/执行状态 |
| Native+RL | `H_NATIVE_PLUS_RL` | Native 正常运行，同时增加 RL；显式声明冲突/双写策略，只支持组合配置的增量价值主张 |

`H_NATIVE_DEFAULT` 另记开箱配置，包括默认关闭的能力，不能冒充 `H_NATIVE_TUNED`；`H_NONE` 只作诊断负控。
没有官方自主长期写入能力的版本，把该能力记为 `unsupported`；不能外加自研 MD 管理器后称为品牌 Native。
以下 `pending_qualification` 表示存在候选运行入口但尚未验收；`unsupported` 指当前指定实现不能满足该模式，不代表产品永久不可能支持。
只有资格证据齐全后才能记 `qualified`。文档、MCP 注册、hook 安装成功、pending capture 和模拟测试均不能代替资格。

## 2. 首批 Linux 候选：9 个配置，尚未冻结执行矩阵

| Agent / 固定范围 | Native-only | RL-integrated | Native+RL |
| --- | --- | --- | --- |
| OpenCode V1 `v1.18.35`，顶层 session | `pending_qualification`：核验实际 Native 写入/读取范围及 session 继承 | `unsupported`：缺自主 admission 与独占长期事实路径的验收 | `unsupported`：插件保留 Native，但完整自主 RL 写入链路缺失；delivery 可先单独验收 |
| Codex CLI，版本待锁定 | `pending_qualification`：在干净测试 profile 核验有效原生配置与实际读写 | `unsupported`：已有接管安装器，缺自主 admission、CLI hook 生效与真实生命周期验收 | `unsupported`：当前安装器关闭原生开关；缺保留 Native 的独立模式及 admission |
| KiroCrew Gateway，审阅提交 `950be416840623da0cda74be10e3acfee94e434f` | `pending_qualification`：固定 Gateway、ACP backend、Memory V2 与 member/store | `unsupported`：桥接不替换 V2，未接入 Gateway，缺准入及排他所有权 | `unsupported`：共存方向成立，但 Gateway wiring、admission 和冲突策略未实现/验收 |

这只是 **3 Agents × 3 modes = 9 个候选配置**。未支持格保留 `unsupported` 及原因，不填零分、不换一个弱系统冒名补齐。
需要人审才可写入的配置另命名为 assisted，记录人力；预填共同 `admitted_events` 的工程 smoke 单独归入机制测试。
两者都不能填自主 `raw_conversation` 格。旧方案的 8 clusters / 512 task slots 不因本表自动扩大；新矩阵、数据量和预算另行冻结。

## 3. 已有接口与具体工程缺口

### OpenCode

- [产品指南](../opencode.md)、[Python bridge](../../sdk/python/recall_ledger/opencode.py) 与 [JS runtime](../../sdk/python/recall_ledger/resources/opencode_runtime.mjs) 已有插件安装、待审候选队列、每次模型请求前 fresh recall 和 compaction 后刷新提醒。
- 当前读取为 lexical / 8 records / 6,000 characters；不替换 session DB、compaction 或 instruction files。Child session 被排除，首批不能宣称覆盖 subagents。
- 官方 CLI 提供 non-interactive `run`、headless `serve`、session 选择和 JSON events；这些入口仍须在锁定的 V1 binary 上验收，浮动文档不证明该版本所有 flags 可用。
- V1 插件不能直接用于 V2；V2 应新增 adapter 与独立 qualification，不能悄悄升级首批镜像。
- 下一步：headless driver、实际插件加载/模型输入证据、session/reset 实现及自主 admission；Native-only 必须从未安装 RL 的干净状态启动。

### Codex CLI

- [产品指南](../codex.md) 与 [安装器](../../sdk/python/recall_ledger/codex.py) 已接 `UserPromptSubmit`、compact `SessionStart` 和 `Stop`；安装时将项目 `memories.generate_memories` / `use_memories` 设置为 false。
- 因此安装后的状态不能标 Native-only 或 Native+RL。Native+RL 需要单独可审计的安装策略，不能随意改配置后忽略安装/卸载 journal 的一致性要求。
- 官方 `codex exec --json` 提供 JSONL，`codex exec resume <SESSION_ID>` 可继续指定会话；driver 应保存明确 ID，禁止并行运行时用 `--last` 猜会话。
- 官方 app-server 提供 threads、turns、events 和审批接口，是另一候选 driver 路线；CLI 与 app-server 必须记录不同 runtime 身份，结果不能直接当作桌面 App 验收。
- 下一步：锁定版本、核验 CLI 的有效 hooks/信任/原生设置、warm/fresh/真实 compaction、自主 admission；JSONL 输出不自动等于完整 provider request。

### KiroCrew

- [产品指南](../kirocrew.md)、[GatewayBridge](../../sdk/python/recall_ledger/kirocrew.py) 和 [helper](../../integrations/kirocrew/gateway_bridge.py) 已实现受信上下文下的 capture/read/refresh；安装回执明确 `gateway_wired: false`，不改原生 V2。
- 上游固定提交提供 Docker Gateway 路线；必须同时锁定 `agent.acp_backend` 及其 harness/model。若选择 Codex backend，就不是与 Codex 完全独立的底层执行系统。
- 下一步：在可信 Gateway dispatch 路径接入 refresh/capture，固定 member/store/session/privacy，记录原生与 RL 的双写/冲突策略，并实现独立 admission。
- RL-integrated 还需证明动态长期事实所有权切换，不可删除 V2 后假设 Gateway 的恢复、privacy、provenance 仍等价。

### 三者共同缺口

[host_runtime](../../sdk/python/recall_ledger/host_runtime.py) 的 capture 只提交 pending 候选；OpenCode bridge 同样不自动准入。
现有 capture 没有通用 semantic extraction、fact-key/修订对应和 dependency 识别管线；接受整段文字不能冒充完成这些能力。
自主模式必须先冻结提取/准入模型或规则、输入、权限、prompt、失败行为、审计记录及成本，再通过原话更正/撤回任务；不读取 evaluator gold。
默认 hook 读取也不是 evidence capsule 注入：Codex/KiroCrew runtime 当前为 lexical / 8 records / 1,800 characters，默认最终包上限 4,096 UTF-8 bytes。
若改成 evidence policy、模型摘要或 hybrid retrieval，要发布不同配置 ID 并验收；不能把产品核心支持某能力解释为所有 Host adapter 已使用它。

## 4. 每格进入执行前的 qualification

| Gate | 必须保存的证据；不满足时的处理 |
| --- | --- |
| Q1 身份 | binary/image digest、OS/架构、Agent/adapter commit、backend/model revision、权限与有效配置；未知字段不猜 |
| Q2 隔离 | 独立 profile/session/store；gold、未来分支、其他组产物不可读取；干净初态与用户配置未变 |
| Q3 Native | 默认与调优配置分开；用原话观察实际写入、后续读取及生命周期；缺能力记 unsupported |
| Q4 RL 写入 | 原话→提取→准入→修订/撤回完整 trace、所有权和耗时；pending receipt 只证明捕获 |
| Q5 Delivery | warm turn、新 session、resume、实际 compaction、进程重启分别检查；不能用换 session 冒充 compaction |
| Q6 可观察性 | task 工件、工具请求/结果、错误、后台 drain、完整成本；actual input 不可见则标 partial，限制模块归因 |
| Q7 故障 | daemon 中断、失配 binding、迟到队列、重复 delivery；不得用缓存旧正文补成功或静默重绑 |

通过该模式适用的 Q1–Q7 后才冻结可执行格；Native-only 的 Q4 及 RL 专属 Q7 标为 not_applicable，仍验证其自身恢复行为。不能测的 lifecycle/子能力逐项列出，不靠忽略失败取得资格。
同 Host 的 Native/RL 配对固定 model、任务、工具、预算和硬件；跨 Host 无法固定模型时只报告产品组合差异。
EKS worker 的短期 run token/provider gateway 是待实现控制；不将真实 API key、用户 auth/profile 全量放入可被任务代码读取的环境。
本轮未启动任何 Agent，所有运行资格均未实测；模型/API/GPU/云写入预算为 0。

## 5. 迁移矩阵：固定目的端

记 O=OpenCode、C=Codex CLI、K=KiroCrew。候选有向边为 **O→C、C→O、O→K、K→O、C→K、K→C**，每边单独做 transfer qualification。
每个 A→B 对同版本、同模式、同模型、同工具的 **B continuous**；不能把更强目的端的提升算作迁移效果。
Roundtrip 对每个选定 A→B 增加 B→A：B 期间发生更正/撤回，最终 A 对 **A continuous**，后者收到同样事件与时间安排。
共享 ledger 切换 Agent、独立 bundle 迁移、Native 导出导入/自然 handoff 分表；前三种不相互替代，roundtrip 也不是新的独立 history cluster。
Native 使用实际最佳支持方式；导出可用则检验内容和目的端兼容，缺失则该格式边 unsupported。自然 handoff 可另测，不能由评估者补 gold 摘要。
RL bundle 有 512 KiB / 1,000 objects 上限且不包含 vectors/repair recipes；显式绑定、目标重建与等待费用须入账。
六条边及 roundtrip 只是候选覆盖，不自动乘进旧 512 slots，也不代表已有六条可用迁移 driver。

## 6. 桌面 App

本地 ChatGPT App 使用 [chatgpt alias](../../sdk/python/recall_ledger/chatgpt.py)，共享 Codex adapter，不是另一套独立 Memory 实现。
现有目标 macOS App 由 external worker 运行，EKS Controller 下发同一任务合同并收集产物；CLI 或 Gateway 成功不能替代实际 App 的配置、信任与 UI/session 验收。
不推断桌面 App 仅支持 macOS；Linux GUI worker 可作为另一个候选，但需单独验证支持条件、窗口/session 生命周期与自动化入口。

## 来源与读取范围

- [OpenAI non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode)：2026-10-14 阅读 exec、JSONL、resume 和凭据边界；未运行。
- [Codex as a platform](https://developers.openai.com/blog/codex-as-a-platform)：同日阅读 SDK/app-server 与 harness 边界；不据此推断 Native Memory 已验收。
- [OpenCode CLI](https://docs.opencode.ai/docs/cli/) 与 [V1→V2 plugin migration](https://opencode.ai/v2/docs/build/plugins/migrate-v1)：同日阅读 run/serve/session 及插件不兼容说明；锁定版本复核待做。
- [KiroCrew 固定提交 README](https://github.com/kirodotdev/KiroCrew/blob/950be416840623da0cda74be10e3acfee94e434f/README.md)：同日阅读 Gateway Docker、ACP backend 和 session 架构；不是运行证据。
- 本文链接的四份产品指南与 Python/JS adapter：审阅安装副作用、capture/admission、刷新、绑定、privacy 及 Host 支持范围；没有修改 SDK 或用户 Host。
