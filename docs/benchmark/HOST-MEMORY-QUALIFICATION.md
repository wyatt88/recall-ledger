# Host Memory 替代资格检查

日期：2026-10-18。**当前不能宣称 RecallLedger 默认替代了 Claude Code、ChatGPT 本地 Agent App 或 KiroCrew 的 native Memory。** 本轮检查先验证真实 RecallLedger daemon 与三个 adapter 的机制；真实宿主接线、自主准入和 Native 配对效果另设验收门。

这里的“替代”限于指定项目/member 的动态长期事实：写入、准入、纠正、撤回与跨会话读取。不包含替换宿主 session、执行 checkpoint、人工规则，或抹除已经进入 transcript 的内容。

## 当前能力与缺口

| 对象 | 现有实现 | 仍缺少的替代资格 |
| --- | --- | --- |
| Claude Code | 项目 hooks、capture、fresh recall，以及关闭 auto memory 的配置 | 真实宿主有效配置与 hook 执行；自主 admission；resume/compaction 等生命周期覆盖 |
| ChatGPT 本地 Agent App | `chatgpt.py` 复用 Codex adapter，安装本地配置与 hooks | 实际 App 的信任、有效 hooks、native suppression 与真实会话；自主 admission |
| KiroCrew | 绑定 member/store/session/privacy 的 callable Gateway bridge | 安装器仍返回 `gateway_wired=false`、`coexists_no_v2_replacement`；真实 Gateway 接线与 Native V2 所有权切换；自主 admission |

三个 adapter 的 capture/flush 目前提交 `pending` candidate。默认路径没有把原话自动抽取、识别为已有事实的更正并准入的完整 pipeline。测试显式调用 `accept/revise/retract` 后的成功属于 **assisted mechanism**，不能计为 autonomous replacement。

这个缺口来自当前实现审阅，不是因为 probe 使用 `--no-worker`：maintenance worker 负责摘要、修复和 embedding，当前不会自动接受 ingestion candidates。本轮关闭它以保持无模型、lexical 的检查范围；没有评估这些 maintenance 能力的质量、速度或费用。

ChatGPT App 必须在实际 App 的独立测试宿主上验收，平台按当时官方支持情况选择。Linux Codex CLI、app-server、嵌入式可执行文件或 Python alias 都不能换名充当实际 App。KiroCrew 当前固定审阅版本为 [`330812e688d595ec225006097656d7d2a01cccdc`](https://github.com/kirodotdev/KiroCrew/commit/330812e688d595ec225006097656d7d2a01cccdc)；其官方无模型 Gateway harness 存在，但本轮未执行。

## 版本与检查范围

- RecallLedger core 基线：`3c26a4dd7d617efe37ceb34116eaeb0adf43339d`，版本 `0.4.0`。
- 新增 probe：[`tools/host_memory_probe.py`](../../tools/host_memory_probe.py)，SHA-256 `4202f1beb98e0dd6efa81603b9104927dfaa8fbf4c2aa9d8cfec240178a141ef`。该文件指纹单独固定，不声称它已经包含在上述 core commit 中。
- 回归测试：[`tools/tests/test_host_memory_probe.py`](../../tools/tests/test_host_memory_probe.py)。独立复验完成后，仅修正测试自身 SQLite 连接的及时关闭，再由整合者重跑11项，全部通过、零 skipped、无 ResourceWarning；Probe 指纹保持不变。
- EKS 实跑 daemon SHA-256：`14243a284b493ed925413d0ece18848719e6b494027ab2b8dcb2a463cb076e34`；Python SDK fingerprint：`0e8568595103df35b6f9d0cbcd591b4ed12c6a1c7133d9f2db60b948e3085224`。SDK 指纹按 probe 实现，将排序后的包内 `.py` 相对路径与文件内容共同计算。
- EKS 条件：单台专用 `m7i.large`、gVisor、串行执行三个 adapter；真实 Rust daemon 是 **同容器 subprocess**，不据此验证跨容器 daemon sidecar。
- 数据与检索：随机合成事实、无真实用户 profile、无模型配置，probe 模型请求数为0，仅 lexical recall。云资源费用另计。

源码与方法审阅为 E2。本地执行属于原创工程检查，不是论文复现；少量 synthetic 机制断言也不构成真实 Agent 场景 E4。

## 结果登记

脱敏机器可读回执：[HOST-MEMORY-CHECK.json](HOST-MEMORY-CHECK.json)，包含54项断言、10项 Controller 持久化核对、前序失败和各宿主未通过的 Gate。

| 检查 | 状态 | 能支持的结论 |
| --- | --- | --- |
| 本地 probe 回归，修复后独立复验 | **11/11 passed，0 skipped** | 所选 adapter/daemon 合同与失败路径通过；另有7项独立故障反例正确拒绝 |
| `tools/tests` 全集回归 | **37/37 passed，0 skipped** | 整合者执行，4.218秒，无 warnings；这是本地测试耗时，不是 Agent 性能指标 |
| 既有 Sandbox 测试连接收尾复查 | **13/13 passed，无 ResourceWarning** | 整合者修正测试自身 SQLite 连接关闭后重跑；属于既有测试子集，不与全集重复累计 |
| 本轮 EKS 基础 Sandbox probe | **passed** | Controller 已报告基础隔离检查通过；不等于宿主 Memory 通过 |
| EKS 前序部署/启动尝试 | **3次失败，保留** | 2次 staging 失败：控制连接传输 timeout、CSI 挂载未及时可见；1次 SDK 路径布局预检失败，均未执行 Memory DB 操作或模型调用 |
| EKS adapter fixture — initial | **passed：42/42** | 三个 adapter 各14项机制断言通过；显式准入、纠正、撤回逐项标记 assisted |
| EKS adapter fixture — resume | **passed：12/12** | 三个 adapter 各4项通过；不同 Pod UID、同一 PVC 引用，同 run ID 与指纹，原 history/pending/稳定事实与撤回状态检查通过 |
| Claude Code 真实宿主 | **not_run** | 没有真实 Claude 会话或 model input 证据 |
| ChatGPT 真实本地 Agent App | **not_run** | 没有实际 App 测试会话证据 |
| KiroCrew 真实 Gateway | **not_run** | 未证明 Gateway 调用 bridge，更未证明 Native V2 替换 |
| 默认自主 Memory 替代 | **blocked_autonomous_admission** | 尚无默认自主准入链路；上述机制检查成功不会改变该状态 |
| Native/RL 效果与性能配对 | **not_run** | 没有长期退化、迁移成功率、延迟或吞吐的比较结论 |
| EKS 临时资源收尾 | **passed** | 本轮 owned Kubernetes 资源为0；测试 EC2 terminated；两块测试 EBS 均不存在；仅剩原6节点，全部 Ready 且 UID 保持 |

本轮合计54项机制断言通过；它们不是54个独立任务样本。两阶段 JSON 均为 `status=pass`，全部逐项 checks 通过，`all_daemons_stopped=true`；对应 Pod 均为 `Succeeded`，主容器退出0。Initial 的容器结束时间早于 resume Pod 启动时间，两份 Pod spec 引用同一 PVC；核对前后库存，PVC UID、绑定的 PV 身份保持一致，状态均为 `Bound`。两阶段 run ID、binary/SDK/probe 指纹一致，SDK/probe 还与本地审阅源码指纹一致。公开文档不披露这些资源的身份值。

上述证据支持“在本次不同 Pod、同一持久状态条件下恢复”的限定结论，不扩大为 EBS snapshot restore、跨节点故障或真实 Agent 会话恢复。不得把进程退出0、Pod 成功或“空 recall 没有错答案”解释为自主替代通过。

清理核查时间为 `2026-10-18T14:56:49Z`。Controller 检查本轮创建的 Kubernetes 资源、专用 EC2 与两块测试 EBS；EBS 查询均返回 `InvalidVolume.NotFound`。公开 JSON 与私有清理汇总一致。这是上述资源范围内的清理确认，不是对整个 AWS 账户的库存或费用审计。

前两次 staging 失败的最终症状均为 wrapper timeout，具体原因分别为控制连接传输 timeout、CSI 挂载未及时可见。第三次失败为 `sdk_import_does_not_match_requested_root`：ConfigMap atomic writer 的 symlink 使解析后的模块位置不在 probe 要求的 SDK package 实目录下。Controller 保留了失败证据，改为启动前复制 SDK 到稳定实目录，再使用新的 work-dir 重跑；没有修改冻结 probe 放宽校验。该次失败只留下 fixture 根目录与锁文件，未操作 Memory DB。最终成功也不得从记录中删除这三次尝试。

### 首次提交后的 CI 发现

资格检查提交 [`a54c5fb`](https://github.com/wyatt88/recall-ledger/commit/a54c5fb181b69e3eaacf563f06320533dc6c42e2) 的[首次 CI](https://github.com/wyatt88/recall-ledger/actions/runs/39138412470)中，macOS 全部通过；Linux stable 与 Rust 1.89.0 均编译成功，但既有预算账本的八进程首次初始化用例失败。失败方有时返回 `database_error`，未到达预期的 `initialization_conflict`。

单独复现确认：SQLite 的 DELETE → WAL 模式切换可能立即返回 `SQLITE_BUSY`，不等待已设置的 busy timeout；并发启动还捕获过 `invalid_path`，具体失败元数据未确定。这属于 `recall-bench-budget` 的本地并发缺陷，保留原失败记录。EKS 的54项机制结果来自上述固定 daemon 基线，不包含之后的预算工具修复，也不能作为该修复在 EKS 上通过的证据。

后续修复对单个本地账本使用独立的 owner-only 文件锁，覆盖 DB/辅助文件检查到 SQLite 连接关闭；不删除锁文件，不降低既有权限或预算上限校验。WAL 切换仅对 `SQLITE_BUSY` 有界重试，其他错误直接保留。4项单元测试及16项预算集成测试通过，原八进程唯一赢家断言保留；新增覆盖锁等待、异常释放、不安全锁文件拒绝及 WAL 超时。源码已交叉审阅，跨平台结果以对应提交的 CI 为准。

## Probe 检查什么

Initial 覆盖重复事件幂等、pending 不可召回、显式准入后可读、显式 revision/retraction、旧 manifest 失效、无关稳定事实保留、daemon 中断后的 outbox、同库重启、错误 namespace binding 拒绝及恢复原 binding。测试直接写入的 foreign-namespace canary 也是辅助 fixture 数据。

Resume 在启动可写 daemon **之前**，以 SQLite `mode=ro` 验证已有 DB 的 header、schema、完整性、history/namespace，以及 initial 保存的原 pending candidate ID、event ID 和 body hash。随后先核对原候选，再调用可能产生新 candidate 的 adapter read；新读取不能掩盖旧 pending 丢失。最后检查稳定 admitted fact 仍可读、撤回正文没有复活。

独立审阅曾发现“空 DB 被重新初始化”和“新 read 生成的 pending 掩盖旧候选丢失”两项假恢复风险；上述固定 probe 已修复，并通过相关负例回归。恢复缺失、损坏或身份不符的状态必须失败，不能重新 seed 成初态后继续。

## 复现

在已准备好的独立 Linux 测试容器中，从仓库根执行。预装 Python ≥3.11、相应架构的 daemon 与 SDK；不挂载用户真实配置。下面 `/data` 与 `/evidence` 是预先分配的可写目录，`--sdk-root` 必须直接包含 `recall_ledger` 包，并使解析后的模块文件位于该包实目录下。ConfigMap 投影需先复制到稳定实目录；不要把 atomic-writer symlink 布局当作已安装 Python package。

```sh
umask 077
python3 -B tools/host_memory_probe.py \
  --binary /opt/recall-ledger/recall-ledger \
  --sdk-root /opt/recall-ledger/sdk/python \
  --work-dir /data/host-memory-probe \
  --adapter all --phase initial > /evidence/initial.json
```

Initial 要求 work-dir 不存在。确认 initial 成功后，保留全部 DB、outbox、adapter 状态和 `fixture-state.json`；用完全相同的 binary、SDK、probe 和 work-dir 执行：

```sh
python3 -B tools/host_memory_probe.py \
  --binary /opt/recall-ledger/recall-ledger \
  --sdk-root /opt/recall-ledger/sdk/python \
  --work-dir /data/host-memory-probe \
  --adapter all --phase resume > /evidence/resume.json
```

`--adapter` 也可选 `claude-code`、`chatgpt` 或 `kirocrew`；resume 必须保持原选择。跨 Pod 恢复时，Controller 另行记录 Pod 更换、原持久卷重新挂载及文件指纹；脚本自身只验证提供给它的文件系统状态。保持 probe 所有 daemon 已退出，再销毁或转移资源。

汇总必须一起读取：

```json
{
  "test_kind": "adapter_fixture",
  "live_host_executed": false,
  "replacement_status": "blocked_autonomous_admission",
  "native_memory_replacement_verified": false,
  "retrieval_mode": "lexical",
  "model_provider_requests_by_probe": 0
}
```

同时检查逐 adapter `checks`、`assisted_actions`、指纹和 `cleanup.all_daemons_stopped`。该 cleanup 字段只覆盖 probe 管理的 daemon，不证明真实 Host、sidecar 或 EKS 资源全部收尾。公共记录仅保留脱敏结果与指纹，不发布原始库存、账户、节点/Pod 名称、地址、卷 ID、kubeconfig 或认证信息。

## 下一验收门

| Gate | 进入下一阶段必须补齐 |
| --- | --- |
| 自主 admission | 冻结原话 → 提取 → 事实身份/更正 → 准入策略、来源与失败期限；verifier 不能向 autonomous 组填正确答案或调用 accept/revise/retract |
| 真实宿主接线 | 关联真实事件、hook/Gateway callback、candidate、admitted revision、fresh recall 与宿主实际接收的 context；验证 native 长期读写的有效控制 |
| 生命周期与交接 | fresh/warm/resume/真实 compaction、隐私模式、member/project 隔离、daemon 故障和跨品牌交接分别验收；无法观测记 unknown，未实现记 blocked |
| 公平效果比较 | 同一 Host 的独立 Native、RL-assisted、RL-autonomous 初态；固定模型、工具、硬件、任务、预算，使用封存测试集与配对指标，完整计算提取、judge、重试和辅助动作成本 |

详细接口见 [Claude Code](../claude-code.md)、[ChatGPT](../chatgpt.md)、[KiroCrew](../kirocrew.md) 与 [ingestion](../ingestion.md)；测试平台及既有基础隔离边界见 [EKS](EKS.md)、[Sandbox qualification](SANDBOX-QUALIFICATION.md) 和 [Agent matrix](AGENT-MATRIX.md)。
