# RecallLedger Memory Benchmark v0.1 设计包

日期：2026-10-13，Asia/Shanghai。项目内名称，不宣称已形成公共标准。

**2026-10-14 GitHub 发布说明：**本目录发布设计规格与公开开发样例。根据用户后续选择，[测试环境方案](ENVIRONMENT.md)已调整为 EKS 主平台、多 Agent Worker；本机用于开发，真实桌面 App 经外部 Worker 接入。没有发布 Benchmark 成绩或可运行的 Benchmark runner。背景审阅记录保留其原始日期和证据边界；本目录规格与统计合同是后续实现入口。

**交付状态：设计规格、基础工具及已实测的单节点 Sandbox；Memory 比较实验未运行，正式协议未冻结。** 2026-10-15 完成 gVisor、跨容器通信、SQLite 恢复与受控网络探针，未运行真实 Agent 或付费模型。目标仍是公平比较 Agent Native Memory、强原文检索/生命周期基线与 RecallLedger 在实际任务、长期变化、迁移和成本上的差异。规格沿用[已审阅比较原则](background/PROTOCOL.md)，此处进一步定义实现接口、输入样例、配置与判定。

| 文件 | 用途 |
| --- | --- |
| [SPEC.md](SPEC.md) | Suite、对照、数据、负载、执行阶段和实现顺序 |
| [ENVIRONMENT.md](ENVIRONMENT.md) | EKS 主平台、本机开发与外部真实 App Worker 的构建边界 |
| [EKS.md](EKS.md) / [eks-plan.draft.json](eks-plan.draft.json) | EKS 拓扑、运行生命周期、资源与预算待绑定的部署计划 |
| [PREFLIGHT.md](PREFLIGHT.md) | 只读 EKS 发现和本地 Rust 模型费用账本；一 trial 一实例的运行前边界 |
| [SANDBOX-QUALIFICATION.md](SANDBOX-QUALIFICATION.md) | EKS 单节点合成 Sandbox 实测、失败修复、证据哈希及未验证边界 |
| [AGENT-MATRIX.md](AGENT-MATRIX.md) | 多 Agent 的 Native / RecallLedger / 共存资格与迁移矩阵 |
| [protocol.draft.json](protocol.draft.json) | 机器可读候选配置、预算上限及未满足的运行门槛 |
| [INTERFACES.md](INTERFACES.md) | replay、backend/Host driver、trace、cost、verifier 与报告的接口合同 |
| [TASKS.md](TASKS.md) | 八类任务的具体刺激、正确行为与反例 |
| [examples/README.md](examples/README.md) | 三个开发 history cluster、六条分支；公开事件和示意 gold |
| [SCORING.md](SCORING.md) / [analysis-plan.json](analysis-plan.json) | 指标、分母、统计、候选 margin 及通过/失败/不确定规则 |
| [REVIEW.md](REVIEW.md) | 反方审查及处理记录 |
| [原始结构检查](DESIGN-CHECK-2026-10-13.json) | 2026-10-13 原始工作区检查记录；其中哈希不用于校验调整后的发布版 |
| [PUBLICATION-CHECK.json](PUBLICATION-CHECK.json) | 当前发布文件的结构检查与哈希；Sandbox 证据另列，不包含 Agent 效果 |

## 建议的首个决策

先实现受控比较 runner 与强 baseline，同时补齐 RecallLedger 的 raw conversation → extraction/admission 路径。真实 Host 必须通过资格检查，才能进入 Native 对照。一个自写 MD backend 的结果不能标成某品牌 Native。

正式主要对照分别是：

- Controlled：`C_RL_EVIDENCE` 对 `C_LIFECYCLE`。
- Real Host：`H_RL_INTEGRATED` 对同 Host 的 `H_NATIVE_TUNED`。
- Migration：同一 A→B，对比两套实际迁移流程及各自的 B continuous。

第一个模型 pilot 采用双方相同的 `admitted_events`，为候选 8 个开发 cluster × 2 个 counterfactual 分支 × 4 配置 × 2 生命周期 × 4 checkpoints，即 **512 task slots**。它不含全部 background/model calls，也不是 512 个独立样本。正式 sample size 留空，按开发方差和 power 冻结；pilot 数据尚未构造，执行预算尚未落实。

新增 EKS 多 Agent 计划是独立的 Host qualification 与后续配对路线。候选 3 个 Linux Agent × 3 种 Memory 模式并不代表 9 个配置已经可运行，也不替换上述受控 pilot；真实 raw-conversation 对照仍须完成自动准入和 Native 资格。

## 2026-10-13 设计阶段边界

初版设计阶段仅进行了文稿与小型本地结构检查，模型/API/GPU 调用为 0。2026-10-14 实现环境发现和费用账本；2026-10-15 在新建的专用 EKS 节点运行 Sandbox 合成服务与恢复探针，并回收本轮云资源。上述进展没有运行被测 Agent、修改用户 Memory 或产生 Memory 比较成绩。实际执行预算按操作者的模型和云资源授权分别绑定。

示例 gold 与公开事件目前放在同一资料包中，**没有物理隔离**，只供实现和人工审阅。正式验证必须使用新封存历史，并通过真实访问控制隔离评分器。

负责人：沈知远；任务与 Host 设计：程知行；统计：方衡之；反方审阅：魏慎言，均为 AI 岗位。方案效用为 H0；历史实现边界沿用 E2 代码审阅，不冒称 E3 论文复现或 E4 场景验收。
