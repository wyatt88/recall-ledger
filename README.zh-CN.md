# RecallLedger

用 Rust 编写、随 Agent 本地部署的记忆基础设施。

RecallLedger 保存带版本的记录，并追踪每条派生记录使用过的输入。来源修改后，
依赖它的旧结果停止提供读取；后台任务即使晚到，也必须重新核对输入版本才能发布。
配置本地模型后，系统还提供向量检索、自动摘要，以及来源变化后的托管摘要修复。

[English](README.md) · [架构](docs/architecture.md) ·
[接口契约](docs/CONTRACT.md) · [运行说明](docs/OPERATIONS.md) · [研究依据](docs/research.md) ·
[原生记忆接管设计](docs/native-memory.md) · [技术路线与验收](docs/roadmap.md) ·
[参与贡献](CONTRIBUTING.md)

## 解决什么问题

Agent 记住“项目语言尚未决定”，后台开始生成摘要。随后，用户明确选择 Rust。
只修改原记录并不够：旧摘要和仍在处理旧输入的任务，也需要失效。

本项目把这个要求落实为存储契约：

```text
记下来源 → 固定输入版本 → 外部处理器加工 → 核验版本后发布
                │                               │
                └─ 来源变化 → 撤下派生结果 ────────┘
                               拒绝旧任务发布
```

适配器可以将这套契约接入框架长期记忆接口；0.2 尚未提供可直接替换宿主原生
记忆的适配器。执行检查点、模型上下文和模型权重仍由原系统管理。

## 相比原生记忆，技术价值在哪里

**RecallLedger 把记忆之间已记录的依赖关系，纳入写入、读取和修复契约。**
用户纠正一条信息后，系统负责让依赖旧版本的受管摘要和向量停止参与后续读取。

“原生记忆”包含多种实现。有些宿主已经具备文件搜索、语义检索、Git 历史、
跨 Agent 共享和后台整理。Markdown 本身适合阅读与编辑；换成 SQLite、
使用 Rust 或增加 MCP，都不足以构成替换理由。

真正需要比较的是：一条记忆发生变化后，系统如何处理已有结果和正在进行的工作。

| 场景 | RecallLedger 0.2 的实现机制 | 受管范围内的作用 | 可检查依据 |
| --- | --- | --- | --- |
| 来源被纠正，摘要又被用于生成其他摘要 | 带来源版本的依赖边；修改事务内传递失效 | 相关派生结果停止作为当前记忆返回，不只修改最初那条笔记 | [核心契约测试](tests/core_contract.rs) |
| 旧任务在纠正后才完成 | `prepare`/`publish` 核验输入；托管 worker 另核租约与输出版本 | 晚到结果不能覆盖新结果，也不能复活已撤回结果 | [维护测试](tests/maintenance.rs) |
| 过期向量仍有很高的相似度 | 向量绑定记录 revision 与 model key；召回核验可读状态 | 检索分数不能让失效记录重新获得使用资格 | [向量契约测试](tests/vector_contract.rs) |
| 写入成功，但调用方没收到响应 | 修改与幂等回执同事务；编辑要求 `expected_revision` | 原键重试恢复结果，并发修改冲突明确返回 | [核心契约测试](tests/core_contract.rs) |
| 进程重启后仍需修复摘要 | 持久化 recipe、任务代次、租约、有限重试及稳定 output_id | 托管修复可以恢复；修复失败期间旧结果保持不可读 | [运行时测试](tests/runtime_contract.rs) |
| 向量未建好，或查询模型不可用 | 返回索引覆盖率、字符预算及词法降级标记 | 调用方能识别缺失与降级，决定是否继续任务 | [召回契约](docs/CONTRACT.md#recall) |

例如把“4 月发布”改为“8 月发布”，系统会撤下依赖旧值的托管摘要，拒绝仍使用
旧输入的生成任务发布，再按新来源修复。修复保留摘要 ID，并增加 revision。
[v0.2 本地模型验证](docs/VALIDATION-v0.2.md#real-local-model-smoke)实际覆盖了
这条链路；它不证明所有摘要都忠实，也不能让已将“4 月”读入 prompt 的 Agent
自动忘记旧值。

这些是已经实现的生命周期机制，**尚未证明回答质量、延迟或成本优于原生记忆**。
原生系统也可以实现相同契约。对于规模小、变化少的任务，文件加良好搜索可能
更简单。[技术路线](docs/roadmap.md)列明了证明实际优势所需的强基线和验收。

## 现有 MEMORY.md 怎么办

仅添加 MCP 工具，宿主原有记忆机制仍会继续运行。要让 RecallLedger 成为权威
来源，适配器必须接管采集、读取，以及纠正或上下文压缩后的刷新。

| 接入层次 | 当前状态 | 记忆归属 |
| --- | --- | --- |
| MCP / SDK 与原生记忆并存 | 接口已有；具体宿主仍需验收 | 未明确改写应用流程时，两套记忆同时存在 |
| 托管 Markdown 兼容 | 待实现 | 动态记忆由 RecallLedger 管理；MD 是带版本、可重建的视图 |
| 框架原生记忆后端 | 适配器待实现 | 长期记忆读写经过 RecallLedger；checkpoint 继续由框架管理 |

用户维护的 `AGENTS.md`、`CLAUDE.md` 和项目规则应保留。自动积累的事实通过
带来源和版本的参考数据通道加载，不应自动变成行为指令。导出的 MD 需要版本
清单和鲜度检查，不能同时成为另一份可独立修改的权威记录。
[接管设计](docs/native-memory.md)说明了迁移、并发编辑、会话刷新和副本撤回边界。

## 当前实现范围

0.2 是面向单机的源码预览版：

| 接口或机制 | 当前行为 |
| --- | --- |
| Rust 库与 CLI | SQLite 记录、修订版本、显式 namespace、有效时间 |
| 派生协议 | `prepare` 固定输入；`publish` 核验已记录的依赖 |
| 修改与撤回 | 传递撤下派生记录；历史读取和任务读取也检查状态 |
| 重试恢复 | 持久化幂等回执，可按原请求键查回 |
| 检索 | 字面词检索、exact cosine 向量检索、RRF 混合排序（k=60）、字符预算 |
| 本地模型 | 显式配置 loopback embedding/chat endpoint、模型名和修订标识 |
| 自动摘要 | 持久化 recipe、分组输入预算、固定来源版本后生成 |
| 自动修复 | 撤下旧结果；带租约与版本核验的 worker 重建，保留 output_id |
| 本地服务 | macOS/Linux Unix-domain socket，不监听 HTTP |
| MCP | stdio 记录操作、摘要入队、维护任务状态 |
| Python SDK | 连接本地服务 |
| 实验性分组 | 可执行的确定性规则，用于后续整理实验 |

记录存储和字面检索无需模型。语义检索与生成摘要需要自行运行并配置本地模型服务；
项目不自动下载权重，也不自动选择云服务。向量检索使用精确余弦计算，扫描当前
可读向量；尚无 ANN 索引或大规模性能承诺。

代码环境校验、学习式选择、最优成本修复调度和论文基准成绩仍未实现。生成摘要
尚未经过事实校验；语言流畅不能证明它忠实于来源。

下一步优先补齐宿主记忆接管与 MD 迁移、针对当前任务和环境的适用性校验，
以及同预算的行为评测，见[阶段验收](docs/roadmap.md#delivery-gates)。

## 构建与运行

需要 Rust 1.89 或更新版本、Cargo，以及编译内置 SQLite 所需的 C 编译器。
演示脚本还需要 Python 3；本地服务与脚本面向 macOS/Linux。

在项目目录执行：

```sh
git clone https://github.com/wyatt88/recall-ledger.git
cd recall-ledger
cargo build --release --locked
./target/release/recall-ledger --help
bash examples/workflow.sh
```

也可以安装当前源码预览版：

```sh
cargo install --git https://github.com/wyatt88/recall-ledger.git --tag v0.2.0 --locked
```

当前没有发布到 crates.io 或 PyPI，Python SDK 随源码提供。发布物为源码，
由使用者在本机编译。需要复现安装版本时，选择已发布的 tag 或 commit。

演示只使用新建临时目录和合成内容：先固定来源输入，再修改来源，确认旧任务
发布被拒绝；然后用新输入发布摘要，最后撤回来源并检查摘要不可读。

手工使用一份数据库：

```sh
mkdir -p .local-state
chmod 700 .local-state

./target/release/recall-ledger \
  --db .local-state/memory.sqlite3 --namespace demo \
  call remember --params \
  '{"body":"项目使用 Rust。","kind":"commitment","idempotency_key":"decision-1"}'

./target/release/recall-ledger \
  --db .local-state/memory.sqlite3 --namespace demo \
  call recall --params '{"query":"Rust","max_chars":1200}'
```

记录与队列修改请求必须带非空 `idempotency_key`。结果不确定时，使用原键和相同参数
重试；同一个键配不同参数会返回冲突。`operation` 可以查回原回执。
维护批次在内部为各项事务管理幂等键。

## 与 Agent 一起部署

先把 [models.local.json](examples/models.local.json) 中的占位值替换为实际本地
服务的完整 endpoint、返回的模型名、权重修订标识和向量维度，再启动服务：

```sh
./target/release/recall-ledger \
  --db .local-state/memory.sqlite3 --namespace demo \
  --models examples/models.local.json \
  serve --socket .local-state/memory.sock
```

配置 embedding 后，非空查询默认使用 hybrid；没有配置模型时默认 lexical。
`mode="vector"` 要求语义查询成功。hybrid 的模型调用失败时，会明确返回词法
降级标记；检查 `coverage`，可以看到索引缺失和降级情况。

worker 自动索引当前记录，并按来源、类型和分组预算整理未托管的根记录；一组
达到四条时生成摘要。来源修订后，旧摘要立即不可读，再异步修复；新版本保留
原 `output_id`。撤回来源或摘要会取消相应 recipe。手工 `prepare`/`publish`
产生的派生结果仍会失效，但不会被自动重建。

省略 `--models` 可按原方式运行。`--no-worker` 关闭后台维护；
`--auto-summary-min 0` 只关闭自动分组，保留显式摘要任务与索引维护。
预算、重试、手动维护和模型兼容说明见[运行说明](docs/OPERATIONS.md)。

namespace 由可信服务配置指定，调用参数不能覆盖它。MCP 宿主可使用以下配置，
将两个路径改成实际绝对路径：

```json
{
  "mcpServers": {
    "recall-ledger": {
      "command": "/absolute/path/to/recall-ledger",
      "args": ["mcp", "--socket", "/absolute/path/to/memory.sock"]
    }
  }
}
```

先启动服务，再启动 MCP adapter。完整派生协议通过本地服务、CLI 和 Rust 库使用。
不同宿主的配置格式与工具批准方式需要分别检查。
adapter 固定支持 MCP `2025-11-25`，只提供 tools，不提供 sampling 或 tasks。

Python SDK 不需要第三方运行时依赖：

```sh
PYTHONPATH=sdk/python python3 examples/python_agent.py \
  --socket .local-state/memory.sock
```

```python
from recall_ledger import Client

memory = Client("/absolute/path/to/memory.sock")
receipt = memory.remember(
    "项目使用 Rust。",
    kind="commitment",
    idempotency_key="decision-2",
)
result = memory.recall("Rust", max_chars=1200)
# 需要服务配置 summary 模型；此处返回入队回执。
queued = memory.summarize(
    [receipt["id"]],
    max_chars=800,
    idempotency_key="summary-2",
)
tasks = memory.maintenance_status()
```

`summarize` 返回 task 和预留 output_id，尚不是生成正文。任务成功后使用
`inspect(queued["output_id"])` 读取结果。SDK 默认超时 330 秒，适应有上限的
本地推理等待；不会自动重试。遇到 `TransportError.outcome_unknown == True` 时，使用原
method/key 查询 `operation`，或以相同参数和原键重试，避免产生重复操作。

长期记忆应选择一个权威来源。宿主如果把文本另外复制进自己的画像或上下文，
本服务无法撤销已经交付的副本。

## 保证与边界

- 修改与对应幂等回执在同一 SQLite 事务提交。
- 发布派生结果时，在事务内核验固定输入修订、祖先依赖和有效期。
- 来源修改或撤回，会传递撤下依赖它的派生结果；普通检索只返回当前可读记录。
- 托管修复发布前核对租约、来源版本与输出版本。修复失败时，过期结果仍不可读。
- 撤回阻止后续受管读取，并清空受管正文；**不保证 SQLite 数据页、WAL、
  备份和已交付副本的安全擦除**。
- 依赖表示处理器接收过某份输入，不等于该输入证明输出中的每句话。模型隐藏
  上下文、未上报工具读取和训练数据不在此契约内。
- namespace 不是同一系统用户下的恶意进程隔离。直接 CLI 和 Rust 库调用属于
  可信管理接口。
- `export` 是诊断快照，不是备份恢复格式。提交标记不覆盖数据库文件恢复
  或克隆后的历史识别。打开 v0.1 数据库会升级到 schema 2，旧二进制不能再打开。
- 模型 `revision` 是操作方对权重版本的声明；接口协议不能证明服务实际加载的
  权重。权重或编码方式变化时，必须同步修改配置修订标识。
- 有效时间由调用者提供；未上报的现实变化不会自动被发现。

更多说明见[架构](docs/architecture.md)与[安全策略](SECURITY.md)。

## 验证与研究

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
PYTHONPATH=sdk/python python3 -m unittest discover -s sdk/python/tests
```

CI 在 Ubuntu 和 macOS 执行 Rust 检查。检查通过只能支持实际覆盖的测试用例，
不等于生产可用性、Agent 任务增益或论文复现。

[v0.2 验证记录](docs/VALIDATION-v0.2.md)分开列出确定性测试、HTTP 模拟服务与
真实本地模型验证；[独立审查](docs/REVIEW-v0.2.md)记录反例与已修问题。

[研究说明](docs/research.md)列出六篇固定版本论文、实现映射及尚待验证的分组
假设。本项目为独立工程实现，不是这些论文的官方实现，也不继承作者报告的成绩。

## 许可

Apache-2.0，见 [LICENSE](LICENSE)、[NOTICE](NOTICE) 和
[第三方说明](THIRD_PARTY_NOTICES.md)。
