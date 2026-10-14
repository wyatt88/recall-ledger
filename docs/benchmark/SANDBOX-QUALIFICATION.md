# EKS Sandbox：单节点合成验收

日期：2026-10-15，Asia/Shanghai（部署从10月14日晚开始）。这是**原创工程验证已执行**，不是论文复现、E4 全场景验收或 Memory 胜负报告。实现与复现步骤在 [infra/eks/sandbox](../../infra/eks/sandbox/README.md)。

## 结果与范围

在新建的一台独占 EC2 节点上，三个串行阶段最终成功：offline initial、换 Pod 后 offline resume、受控 online。原有业务节点、全局 VPC CNI 配置、现有 Agent 和用户 Memory 没有被修改。模型调用为0。

| 项目 | 本次实际证据 |
| --- | --- |
| 节点 | 一台 `m7i.large`，x86_64，On-Demand，单 AZ；独立 taint、节点选择和跨 namespace 的 required Pod anti-affinity |
| OS / Kubernetes | AL2023 EKS optimized AMI `1.35.8-20261003`，kernel `6.12.110-135.202.amzn2023.x86_64`，kubelet `v1.35.8-eks-3b4a6ca`，containerd `2.2.7+unknown`；精确地域 AMI ID 保留在私有绑定 |
| runtime | `release-20261005.0`，完整 runsc/shim/gvisor-bin；官方 SHA512 两处核对并实际下载校验 |
| 资源 | 主容器900m/3840Mi + sidecar100m/256Mi；runtime overhead100m/128Mi；节点还运行系统 DaemonSets 和临时管理员审计进程 |
| 磁盘 | 32 GiB加密gp3系统盘；独占8 GiB加密gp3 PVC；外部读回 `ebs.csi.aws.com`、PV、卷和节点绑定 |
| 运行方式 | offline `network=none`；online `network=sandbox` + 针对单个实际 host veth 的规则；均为 `systrap`、systemd cgroups、禁止 flag override |
| 程序证据 | Pod内 gVisor kernel signature；节点上实际 runsc 进程及生效参数；RuntimeClass/配置文件读回 |

这一资源规格容纳了合成探针，尚未证明能容纳所有真实 Agent，也没有测量 Sandbox 的性能净开销。Karpenter limits 不是瞬时扩容硬上限，本轮还实际核对了只有一个 NodeClaim/EC2；两个并行 trial 的公平性仍待测。

## 通过的检查

1. **跨容器通信与权限。** 主容器与独立 native sidecar 经 localhost TCP 和0600 Unix socket交换合成内容；非root、CapEff=0、NoNewPrivs=1、只读rootfs、内存tmp均通过。已知宿主根目录、runtime socket和默认ServiceAccount token路径均不可见。这不是任意隐藏挂载或内核逃逸的穷尽测试。
2. **数据库与生命周期。** SQLite WAL/FULL中先提交数据，再让持有未提交事务的子进程以73异常退出；重新打开后未提交行数为0，`integrity_check=ok`。删除旧Job/Pod后，新Pod挂同一PVC，SQLite identity不变，sequence由1→2→3递增。各阶段最终Job成功，native sidecar被正常收尾。没有模拟EC2断电、卷损坏或并发writer fencing。
3. **offline网络。** IMDS IPv4、Kubernetes API、公网TCP连接均返回不可达；Sandbox内部loopback可用。此模式不能调用远程模型。
4. **online出口。** 外部可信canary同时监听18080与18081，并在host侧验证两者可连接；trial只允许18080并验证固定HTTP响应。18081、IMDS、Kubernetes API、公网均超时拒绝。host主动连接已在Pod内自检成功的18082监听器也被拒绝；对应DROP和许可规则计数均有增长。
5. **启动门控。** 主程序启动前，可信init等待只属于当前Pod的emptyDir；host规则安装和verify成功后，管理员原子写入匹配Pod UID与nonce的ready文件，主程序才启动。一次规则读取失败时，gate未释放并按期超时，未运行主程序。
6. **API拒绝。** API server实际dry-run拒绝无Sandbox runtime、挂ServiceAccount token和hostPath的三个负向样例。不是只检查清单文本。

online的IPv4/IPv6 PREROUTING、FORWARD、OUTPUT精确规则均安装并readback验证。**真实流量只覆盖上述IPv4出站与host入站；跨节点FORWARD和IPv6流量没有实测。** 不使用只创建NetworkPolicy对象来冒充执行，因为目标共享集群的CNI NetworkPolicy未开启。

## 发现并修复的失败

本轮是探索性工程qualification，以下失败没有被隐藏或计为Agent任务结果：

| 原因 | 处理与回验 |
| --- | --- |
| 初始420秒Job deadline包含了新节点准备与管理员审计，容器尚未运行就超时 | qualification deadline改为1800秒；节点仍限制2小时寿命。它不是Agent任务延迟预算 |
| 默认gVisor挂载视图下，两个容器的Unix socket不可见 | 只向runtime传递`tmp`的三个精确mount hints，跨容器TCP/Unix实际重跑通过 |
| 给CSI数据卷套逻辑卷名`data`的mount hints，shim无法解析目录 | 移除CSI卷hints，保留默认bind；SQLite持久性及恢复通过 |
| AL2023 iptables-nft对不存在链的`-S CHAIN`返回误导性的`incompatible` | 改读完整表并识别精确chain声明，任何读取失败仍关闭门控；现场安装、verify、流量与删除均通过 |

本地最终 **74项Python测试通过**：48项bootstrap/清单/网络规则/online fixture测试，26项preflight及probe测试。包括真实本地进程崩溃、TCP/Unix与HTTP行为；iptables单元测试采用mock，不能代替上述EKS流量证据。CI已加入这些测试。本轮没有修改Rust核心，上一轮Rust结果不能作为本轮新增Python的测试结果。

## 当前可用能力与尚未完成部分

已交付可重复部署的Sandbox基础实现及可信管理员操作流程。online固定服务只是合成canary，不转发任意URL、CONNECT或模型请求。后续模型服务需接入可信出口和共享费用账本，才可启动带费用的Agent测试。

在线配置放在`/run`，containerd和kubelet追加了依赖当前boot授权文件的启动条件；重启后不自动恢复online授权。规则绑定boot、netns、veth index/peer/MAC，身份变化拒绝复用。**本轮核对了配置和单元测试，未执行真实节点重启恢复试验。** 当前guard是单次安装/验证工具，不是连续策略控制器；规则被其他管理员改动、额外NIC、host networking、XDP或任意第三方Pod提交不在验收范围。下一轮必须重新审计和绑定新Pod，而不是复用旧gate。

真实 Agent driver、raw-conversation自动准入、付费模型出口、完整lease/fencing、正式封存数据和评分器仍待完成。此结果不能说明RecallLedger优于原生Memory，不能用Linux CLI结果代替桌面App。

资源回收读回于2026-10-14T16:49:36Z完成：测试EC2已terminated，两块所属EBS卷均已删除，专用NodeClaim和两个namespace均不存在；原有6节点全部Ready。精确AWS账单未核算，模型调用及费用均为0。

本轮资源回收状态和裁剪后的证据指纹见 [SANDBOX-CHECK.json](SANDBOX-CHECK.json)。详细运行对象、私网地址、账户绑定及原始日志只保留在操作者的私有记录中。现场调试后的最终源码还收紧了缺失、空或无效JSON输出的拒绝逻辑；这部分通过本地负向测试，未为它重新创建EKS资源。现场验证属于本轮探索性实现，不能称整个最终提交已在云端逐字节重跑。

## 一手依据

- [gVisor安装及多文件分发](https://gvisor.dev/docs/user_guide/install/)
- [固定release的网络实现](https://github.com/google/gvisor/blob/release-20261005.0/runsc/sandbox/network.go)
- [固定release的共享卷处理](https://github.com/google/gvisor/blob/release-20261005.0/pkg/shim/v1/utils/volumes.go)
- [gVisor containerd配置](https://gvisor.dev/docs/user_guide/containerd/configuration/)
- [AL2023 nodeadm增量配置](https://awslabs.github.io/amazon-eks-ami/nodeadm/doc/examples/)

工程由Agent系统、推理服务、评估与独立审阅岗位协作，主代理核对现场证据。岗位名称不意味着独立统计样本或真实人员聘任。
