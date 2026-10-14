# EKS 主测试平台设计

日期：2026-10-14，Asia/Shanghai。**H0 实施设计，采用 EKS 作为 Linux 测试主平台；未构建测试镜像或运行 Benchmark。** 已开始实现只读环境发现与本地模型费用账本，使用方式和边界见 [PREFLIGHT.md](PREFLIGHT.md)。本文没有提供已验证可执行的 runner、operator 或 Kubernetes manifests。

## 1. 平台职责与信任边界

EKS 承接不同 Agent 的批量试验、状态保留、故障注入和资源计量。每个 Agent 都分别测试其真实 Native 配置及 RecallLedger 集成配置；共同模型可控时固定模型，无法固定时列为产品组合比较。对照定义仍遵循 [SPEC](SPEC.md)。

```text
可信控制域：sealed data / gold / grader、Controller、预算账、模型出口
                    │ 当前公开事件、试验租约；无答案或未来事件
                    ▼
EKS Worker 域：每 trial 一台同规格专用实例 → Job → 单 trial Pod
                         [Agent + 按模式启用的 RecallLedger daemon]
                    │ 独占状态卷；仅交付当前 prefix
                    ▼
本 trial 产物入口 → 不可覆盖的 trace / 文件证据 → 私有 verifier

同一 Controller → 外部 Mac Worker → 相同证据合同，独立平台分层
```

Controller 与 grader 使用独立身份和存储，可运行在独立管理集群或 EKS 外部。EKS Worker 不拥有 sealed bucket、gold、未来事件、其他 trial 产物的读取/list 权限。Worker 只取得按 trial 限定、短期有效的模型出口与产物上传权限；云存储密钥、模型供应商主凭据和 Kubernetes 管理凭据不进入 Agent。

可信预算状态建议采用支持条件写入/事务的独立数据库；队列只传试验标识和可执行引用，不放 gold。模型出口和产物入口逐次校验 trial、attempt、租约 generation 与预算状态；不能只靠 namespace、S3 路径名字或 prompt 限制访问。

## 2. 试验单位与公平对照

一个 trial 绑定 `cluster_id / history_id / counterfactual_branch / configuration_id / lifecycle_profile / replicate`，由一个 Job 连续推进整段历史；不是每条消息新建空 Pod。configuration_id 按 [INTERFACES](INTERFACES.md#2-配置与-manifest) 唯一锁定 Agent/模式/版本；每个 checkpoint/task 保留独立 run/trace 记录，并引用所属 trial。队列、卷、费用与迁移两端都使用该身份，不能只用 H_* 模式名。

同一 Agent 的 Native 与 RecallLedger 配对使用同节点规格、架构、AZ、存储档位、模型服务位置和时间边界；分别从合法初态开始，不共享 Home、session、DB、缓存或远端账号 Memory。运行顺序随机化或交错分块。不同 Agent 的配置及集成能力另行锁定，不能因同在 EKS 就宣称 Native 语义相同。

**一个 trial 独占一台小实例**，Native 和 RL 两侧使用不同实例但固定相同规格，不在大节点内并排放多个测试 Job。Controller、模型出口、评分器和产物服务放在试验节点之外。一个 Job 推进完整历史，不能把历史中的每条消息拆成无状态的新实例。

512 task slots 只是既有 pilot 的逻辑任务数量，不是512个并发 Pod，也没有为新增 Agent 自动批准资源预算。扩展系统矩阵必须重新计算完整执行量；独立历史簇数量不随 Pod 或 checkpoint 数增加。

## 3. Pod、daemon 与收尾

- 每个 trial 使用自己的 Agent 容器与工具工作区，仅 RL 模式启用 RecallLedger daemon；Native-only 不安装 RL hook、不挂其状态卷，也不启动其维护任务。各模式保持相同总 Worker 资源额度，RL 的额外开销纳入该额度及成本。共享 Unix socket 目录可用 `emptyDir`；DB 路径只挂载到 daemon，Agent 工作区/profile 使用该 trial 的持久状态目录。
- 当前 [socket 源码](../../src/transport.rs)采用 `0600`、新建目录 `0700`，两端需匹配的有效 UID 和可访问父目录；`fsGroup` 单独不能修复 socket 的 owner-only 权限。共享 UID 不替代跨 trial 隔离。
- 启动先检查卷身份与 trial 绑定，再启动 daemon，确认 socket 和真实协议请求可用后启动 Agent。探活与状态检查本身不得泄露 gold 或改变被测 Memory。
- 若选定的 EKS/Kubernetes 版本和运行时通过 qualification，可采用 `initContainers` 中 `restartPolicy: Always` 的 native sidecar；它不会在主容器结束后无限阻塞 Job 完成。普通长驻 sidecar 必须有明确的退出协调，不能等待永不退出的 daemon。
- 主执行器退出前执行合同化收尾：停止接收事件，取消或 drain 后台任务，保存终态和资源事件，等待产物入口确认。所有阶段有预注册 deadline；超时记录未完成/未知费用，不能记为成功。
- daemon 的自动重启属于可观测生命周期事件；若不在该试验允许的 lifecycle 中，按冻结协议判定失败或环境无效，不能隐去后继续报正常完成。保留 Agent、daemon 和收尾阶段各自退出原因。
- 只有 Controller 核对 trace、费用与终态后才进入回收。Job TTL 与 PVC/snapshot 清理独立管理；Job 已完成不表示后台账目已结算，也不表示 EBS 已释放。

## 4. 存储与 checkpoint

每个 trial 独占加密 EBS gp3 PVC，基线采用 RWO 并由 Controller 确保只有一个有效 writer；**RWO 本身不保证同节点只有一个 Pod 可写**。必要时验证驱动支持的 `ReadWriteOncePod`，仍保留应用租约与 fencing。

重试复用持久卷前必须确认旧 Pod 的 writer 已停止、旧挂载不再可写；租约过期不会自动阻止已运行进程写 SQLite。节点失联且无法确认 fencing 时，隔离该卷并停止续跑，不让新旧 attempt 同时修改状态。

使用经核验版本的 EBS CSI driver；`WaitForFirstConsumer` 配合 node affinity/StorageClass topology 把卷和 Worker 固定在指定 AZ。不用 `nodeName` 绕过调度。配对固定文件系统、容量、IOPS、吞吐和挂载选项，记录 EBS/node I/O 指标。

当前 [Store 源码](../../src/lib.rs)为 SQLite WAL、`synchronous=FULL`；各组保持同等 durability。活跃 SQLite 不放 EFS/NFS，也不让多个 trial 共用 live DB。`emptyDir` 只作可丢弃数据，不能承担跨 Pod 重建后的长期记忆。

checkpoint 必须覆盖 Agent profile/session、Memory、索引、候选队列及合法后台状态。首选预先定义的静止 barrier：停止新写入，结算或记录 in-flight 操作，建立一致性状态，再生成快照；记录 barrier 对时间和生命周期的影响。EBS snapshot 完成不自动证明整个应用状态一致，多个卷/远端账号状态更不能假定原子一致。

SQLite 一致性备份只解决 DB 一致性，不等于整个 Agent clone。现有 [restore](../../src/recovery.rs)重建 storage identity、清理 receipts/change events、失效 prepared jobs，并改变维护任务 generation；因此恢复实验与普通 probe clone 分开。clone 不得复用有效远端租约/请求凭据：应用状态按协议保留，外部执行身份重新绑定且禁止回写主历史；语义无法验证时从干净初态重放 prefix 并单列成本。

EBS 卷限制在 AZ 内；单 AZ 第一阶段不证明跨 AZ 灾难恢复。快照恢复、Pod 重建、容器崩溃与 Memory 逻辑修复各自定义故障模型。

## 5. 队列、租约与预算幂等

Controller 是试验重试的唯一决策者。候选 Job 配置为 `parallelism=1`、`completions=1`、主容器 `restartPolicy=Never`、`backoffLimit=0`；即使如此，Kubernetes 仍可能重复启动程序，不能依靠这些字段取得 exactly-once 语义。

1. 创建不可变 trial specification 和 fingerprint；条件写入获得唯一有效 lease/generation，再以稳定标识创建 Job。创建请求超时后按标识查已有 Job，不直接再建。
2. Worker 启动、每次领取新事件及每次有副作用请求时校验 fencing token。新 attempt 撤销旧租约；旧 Worker 不能领取事件、发模型请求或提交终态。收集旧 attempt 已发生费用，不因结果无效而免账。
3. 模型调用先以稳定 operation ID 原子 reserve，供应商请求/实际 usage 回到同一 reservation reconcile。Controller 与 Worker 的观察合并为同一个 `cost_event_id`；重放上报去重，确实发生的新调用单独记费。
4. 供应商不支持请求幂等时，超时不能盲目重发；保留 unknown/unsettled 和预留额度，按供应商回查或冻结的失败规则处理。不能同时承诺“绝不重复调用”和透明无限重试。
5. 全局预算与每 trial 配额同时校验。重试、后台维护、embedding、judge、失败尝试共用原 `budget_id`，不得重新领取总额度。预算停止须阻断新出口请求并取消/结算已启动工作。

模型调用与 AWS 资源分别绑定预算，不能从一类已授权额度推导另一类额度。模型账包含所有 Agent、subagent、提取、摘要、修复、embedding、judge、失败和重试；AWS 账包含节点、卷、网络、日志与保留费用。预算金额、供应商价目和实际环境绑定保存在操作者私有执行配置中，不在通用公开草案中记录个人云账户或凭据。

trial 租约只保护执行调度，不是 RecallLedger 内部维护 lease 的替代品。恢复后分别验证两者。基础设施失败的重跑是否排除、是否对配对双方重跑，执行 [SCORING](SCORING.md) 的冻结规则，不根据结果好坏选择重试。

## 6. Sandbox 与节点选择

第一阶段候选采用专用 EC2 Worker node pool，与 Controller/评分器分离；选定可提供所需隔离的 runtime 并验证 `RuntimeClass`、socket、SQLite、子进程、卷挂载和观测能力。**普通 Pod、namespace 或专用节点均不能单独证明是强 Sandbox。** 未通过隔离 qualification 时只运行受信任的 fixture，不接开放式 Agent 代码执行。

2026-10-15 已在一台新建 AL2023 / `m7i.large` 节点上执行 gVisor `systrap` 的 offline 和受控 online 探针，见 [Sandbox 验收记录](SANDBOX-QUALIFICATION.md)及[可复现实现](../../infra/eks/sandbox/README.md)。online 只验证合成服务出口，未接付费模型或开放式 Agent。Kata、Nitro Sandbox、其他 AMI 和 runtime 尚未验证。当前 EKS 官方文档明确 EBS 不能挂载到 Fargate Pod，因此本文 EBS 方案不采用 Fargate。

Worker 使用最小 RBAC/云身份，默认不挂 ServiceAccount token，不提供 hostPath、宿主 runtime socket、privileged 或 host network/PID；限制 metadata service、跨 trial 网络和任意外网出口。NetworkPolicy 需核验 CNI 实际执行；模型出口及当前输入入口还须做独立身份校验。

qualification 包含 gold/future/cross-trial sentinel、卷/网络/凭据隔离和越权失败记录。这些是被测访问路径的证据，不是证明不存在容器或内核漏洞。

## 7. 性能池、成本与首轮候选资源

主要延迟/成本对照使用固定 On-Demand node pool、固定并发与单一实例型号/架构。一 trial 一实例，配对锁定精确 AMI、内核、container runtime、Sandbox、DaemonSet 镜像和资源配置。固定场景、工具、数据前缀与任务顺序；Native 与 RL 各自保持完整独立状态。记录 CPU throttling、RSS、磁盘等待、网络和队列延迟。

节点专用 taint、Worker required node affinity 与 required Pod anti-affinity 共同约束调度；anti-affinity 按 `kubernetes.io/hostname` 排斥**所有试验 Worker**，覆盖相关 Worker namespaces，不能只匹配同一个 trial ID。Controller、judge、网关和业务 Pod 不容忍试验节点 taint。系统 DaemonSets 的开销仍存在，需逐节点核对并保留相同版本和配置；“专用”不等于没有系统负载。

Karpenter 固定 instance type、architecture、On-Demand、AZ 和精确 AMI；测量期禁止 Drift/Underutilized 等自愿 disruption。`WhenEmpty` 仅限制 consolidation，不阻止 expiration 或所有故障；不得把它、PDB 或 `do-not-disrupt` 当作无中断保证。运行时锁定 expiration 策略，强制中断按协议记入可靠性与失败事件。Karpenter NodePool limits 也不是原子美元预算或严格最大节点数；Controller 另行限制并发和记录实际实例启动。

Spot 中断、Karpenter 弹性与不同 Sandbox runtime 分别进入可靠性/吞吐试验；不混入主要 Memory 延迟结果后归因给算法。模型服务若不在同区域，外部延迟与模型版本漂移单列。

首轮配对 qualification 候选为最多2个并发 trial，各独占一台 `m7i.large`（2 vCPU / 8 GiB）同规格 On-Demand 实例；每 trial 的 Agent、daemon 和观测进程总计请求/限制 1 CPU / 4 GiB，另留系统与 Sandbox overhead；每实例32 GiB gp3系统盘，每 trial 独占32 GiB gp3数据盘，无 GPU。**配对容量仍待验证，不是 Agent 性能结论。** 2026-10-15 的单节点合成 Sandbox 验证使用32 GiB系统盘、8 GiB数据盘，Pod含runtime overhead为1100m/4224Mi，已能运行；这不证明真实Agent或本地模型能容纳。若容量不足，统一提高整个对照分层的规格并重新冻结配置；不能只给 RL 或某款 Agent 单独加资源后报告算法优势。正式执行后发生的 OOM/timeout 保留在任务分母中，不作为静默加配重试的理由。本地模型需求另设硬件分层。

成本账包括模型/embedding/judge、后台修复、EC2、EKS 控制面、闲置预留、EBS/snapshot、对象存储请求与容量、网络/NAT、日志、失败重试和清理滞留。按现行 [费用合同](INTERFACES.md#6-cost-event-contract)分账，不遗漏 infrastructure，也不把被测系统必要 ingestion/修复移出 system_runtime；unknown 不填0。

## 8. 实施里程碑与验收

| 阶段 | 交付 | 进入下一阶段的证据 |
| --- | --- | --- |
| M0 离线合同 | Rust Controller/Worker 接口、队列/预算状态机、fixture verifier | 重复启动、过期 lease、预算停止、成本去重及评分正负控通过 |
| M1 EKS qualification | 锁定版本的镜像/配置、身份与网络策略、EBS、sidecar 收尾 | 隔离检查、持久状态、Job 退出、故障证据与资源清理通过 |
| M2 Agent 小型 pilot | 每个已支持 Agent 的 Native/RL 配对、长期与迁移路径 | 实际捕获、自动准入/修订/撤回、注入/刷新、任务产物与完整费用可审计；pending-only 不通过自主 RL 格 |
| M3 正式冻结 | 封存数据、版本、主对照、样本量与预算 | 独立审阅及 pilot 后新验证集；按 [SPEC](SPEC.md) 执行 |
| M4 扩展 | 更多 Agent、Spot/弹性、运行时与跨平台迁移 | 分层报告，覆盖不足保留 unsupported/not_run |

macOS 原生 App 以 external Worker 接同一 Controller、预算与证据合同；实际运行在合格 Mac 环境，分别固定账号状态与 App 版本。Linux CLI/SDK 通过不能替代桌面 App 验证，Mac 与 EKS 的不同硬件结果不得直接用作 Memory 算法性能配对。

执行前仍需在私有执行配置中锁定 AWS account/region、集群与权限、节点/AMI/runtime 版本、Agent/模型版本、供应商访问及两类预算。公开草案保留未绑定状态。已执行的只读发现和本地工具测试不构成云部署、模型调用或 Benchmark 成绩。

## 一手来源与读取边界

2026-10-14 实际核对以下章节及本文链接的 RecallLedger 源码；基础设施约束为 **E2 方法/官方说明已审阅，支持相应限制**。方案有效性与性能仍是 H0，未作 E3/E4 主张。

- [Kubernetes sidecar containers](https://kubernetes.io/docs/concepts/workloads/pods/sidecar-containers/)：native sidecar 的启动、终止和 Job 完成语义；部署时须锁定对应版本文档。
- [Kubernetes Jobs](https://kubernetes.io/docs/concepts/workloads/controllers/job/)：单并发仍可能重复启动、重试与 deadline；Job 成功不等于业务结果幂等。
- [EKS EBS CSI](https://docs.aws.amazon.com/eks/latest/userguide/ebs-csi.html)：EC2/Fargate 限制、CSI 身份和 snapshot controller 前提。
- [Kubernetes StorageClass](https://kubernetes.io/docs/concepts/storage/storage-classes/)：`WaitForFirstConsumer`、拓扑和 `nodeName` 限制。
