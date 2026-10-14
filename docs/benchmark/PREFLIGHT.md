# 环境发现与模型费用账本

日期：2026-10-14。这里提供两个可本地运行的基础工具；完整 Benchmark Controller、Agent driver、自动准入、模型出口和 verifier 尚未实现。工具通过不等于 EKS Sandbox、真实 Agent 或 Memory 效果通过。

后续进展（2026-10-15）：[Sandbox 代码与操作流程](../../infra/eks/sandbox/README.md)已完成独立节点实测，[验收记录](SANDBOX-QUALIFICATION.md)区分合成探针、已修复的失败和未验证边界。它不改变本页 preflight 本身只读、只发现环境的定位。

## 只读 EKS 发现

依赖 Python 3.9+、AWS CLI 和 kubectl，使用已有有效登录；工具不会自动登录、修改 RBAC、读取 Secret 资源或创建云资源。

在仓库根目录运行，将实际目标替换为操作者自己的配置：

```sh
python3 tools/eks_preflight.py \
  --profile YOUR_PROFILE \
  --region YOUR_REGION \
  --cluster YOUR_CLUSTER \
  --output .local-state/eks/preflight.json
```

可用 `--aws-bin`、`--kubectl-bin` 指定程序；`--timeout` 限制每条命令的等待。使用独立私有 kubeconfig 和 discovery cache，退出后清理临时目录；不更新默认 kubeconfig。报告以0600权限原子写入。

工具先核对 STS 与 EKS 的账户、区域和集群身份，再采集 Kubernetes 版本、节点与 Pod 请求汇总、RuntimeClass、StorageClass、CSI、系统组件、Karpenter、NetworkPolicy 和 namespace 概况。只输出允许字段，移除原始账户、ARN、地址、任意 annotation/env、未裁剪错误文本及私有镜像仓库名。`profile/region/cluster` 是用户传入的目标，会保留；**报告仍属于本地云库存，应审阅后才决定是否分享**。

退出码0表示所列库存读取完成，1表示缺项/部分不可读，2表示参数或报告写入失败。SSO过期、Forbidden、超时、缺少资源API和真实空列表分开记录。

任何退出码下，`execution_enabled=false`、`qualification_status=not_assessed`。RuntimeClass的存在、CNI的开启参数和CSI的就绪数，均不证明实际隔离、存储恢复或运行兼容。Pod requests只是配置汇总，不能当作可调度余量；精简版本信息不能替代原始镜像digest与系统配置锁定。

## Rust 模型费用账本

构建独立 CLI：

```sh
cargo build --locked --bin recall-bench-budget
target/debug/recall-bench-budget --help
```

初始化示例额度并读取快照；该金额只是示例，实际金额必须来自操作者授权：

```sh
target/debug/recall-bench-budget --db .local-state/model-budget/budget.sqlite \
  init --budget-id first-pilot --limit-usd 500
target/debug/recall-bench-budget --db .local-state/model-budget/budget.sqlite snapshot
```

使用专门的0700目录、0600普通文件。工具可以创建不存在的私有目录；已有宽松权限目录会被拒绝，不会替用户悄悄修改。不要把 RecallLedger 产品数据库作为费用数据库，也不要把该数据库挂进 Agent Worker。

金额输入采用十进制USD，最多六位小数，拒绝浮点指数、负值、舍入和溢出。JSON金额用**整数 micro-USD 的十进制字符串**表达，例如 `"500000000"` 表示US$500。数据库固定预算身份与上限；重复init不能扩额或清账。

调用方必须遵循：

| 操作 | 行为 |
| --- | --- |
| `reserve` | 在同一事务中检查余额并预留请求最大可能账单。固定request ID、请求SHA、configuration ID、run、provider计费范围、价格快照、stage和费用分类。同ID同参数重试幂等，异参拒绝 |
| `mark-dispatched` | 只有首次成功领取返回 `dispatch_permit_granted=true`。重试返回false，不能根据命令退出码0重复调用供应商 |
| `settle` | 记录最终实际费用、唯一cost event及provider request ID；相同结算重放不重复记费，冲突拒绝 |
| `cancel` | 只能释放尚未dispatch的预留；未知、超时或丢失回执的已dispatch请求继续占额 |
| `close` / `reopen` | 暂停/恢复新预留和新dispatch许可；不重置费用、请求ID或未结算额度，reopen不解除超预留halt |
| `snapshot` | 一致读回预算、已结算、未发送预留、已发送未知费用；可按request ID读取明细 |

每项子命令的完整参数通过 `--help` 查看。只在首次dispatch许可成功返回后调用provider；若领取响应丢失，保留未知并回查，不能再发一次。若provider支持其自身幂等键，还必须按该provider合同接入；本工具本身不会发网络请求。

真实费用超过预留时，**先保留实际账单，再永久halt该账本的新预留和dispatch**，而不是拒绝入账、截断金额或把差额抹掉。错误估价可能已导致总额超限，因此这不是供应商账单硬封顶保证。调用前必须固定可核验价目、输入与最大输出及所有可计费项目上界；拿不到上界或无法结算时，模型出口需拒绝或停发。

实现使用SQLite WAL、`synchronous=FULL`和`BEGIN IMMEDIATE`，支持可信控制主机上的多进程原子预留。它不是多节点数据库、EBS多writer方案或高可用服务；不把live SQLite放NFS/EFS上。跨节点Controller需要独立事务服务，并让Worker通过可信出口访问。

模型调用预算覆盖各Agent、subagent、提取、embedding、摘要、修复、judge、开发、失败与重试。`ledger`是模型调用的归因字段，不允许以它把EC2等云费用混入本模型额度。AWS费用另行管理。工具尚不校验trial lease/generation、每trial配额、真实供应商token、网关访问或AWS资源账；这些仍是Controller/出口的待实现工作。

## 一 trial 一同规格小实例

按 [EKS方案](EKS.md#7-性能池成本与首轮候选资源) 独占小实例；Controller、评分器和模型出口在测试节点之外。固定两侧instance type、AMI/kernel/runtime、系统DaemonSets、资源限额、磁盘、AZ、场景、数据和工具。严格Pod anti-affinity覆盖全部Worker，不能只排斥相同trial ID。

容量资格测试先扣实际系统与Sandbox开销。如果候选机型不足，两侧共同换规格并重新冻结；正式测试后不能挑选性升级某一组并抹去OOM/timeout。系统负载、CPU steal/throttling、网络和磁盘波动仍需观测，独占实例不证明不存在所有外部干扰。

新运行时必须实际安装和验收。当前gVisor官方安装包含runsc、shim和相邻的gvisor-bin文件；containerd 2.x需按其配置格式合并handler。不要把新增RuntimeClass当作完成安装。共享集群CNI修改需单独评估；仅写NetworkPolicy对象不证明已执行，standard模式还需测试Pod启动时的访问窗口。

参考：[gVisor安装](https://gvisor.dev/docs/user_guide/install/)、[containerd集成](https://gvisor.dev/docs/user_guide/containerd/quick_start/)、[EKS NetworkPolicy配置](https://docs.aws.amazon.com/eks/latest/userguide/cni-network-policy-configure.html)。读取范围为上述安装/配置章节，属于官方方法审阅，未在目标节点运行验证。

## 本地检查

```sh
python3 -m unittest discover -s tools/tests -v
cargo test --locked --test benchmark_budget
```

Python测试模拟外部程序，检查只读命令、独立kubeconfig、脱敏、CNI开关和缺项。Rust测试启动真实本地CLI进程，检查并发争额、dispatch去重、重启后的未知费用、初始化冲突、真实超额入账、精度与路径限制。它们不运行真实模型，不证明provider费用上界、EKS隔离或Memory效果。
