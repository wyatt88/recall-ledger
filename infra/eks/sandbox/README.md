# 专用 EKS Sandbox 的复现与清理

本目录提供受控、无模型调用的 qualification fixtures。目标是**新建的一台专用 AL2023 / EKS 1.35 / containerd 2.x 节点**，使用固定版本 gVisor `systrap`。不把现有业务节点改造成试验节点。

本轮 offline 探针已在目标环境通过。实际配置、失败记录、重试与 online 验收结论由 [SANDBOX-QUALIFICATION.md](../../../docs/benchmark/SANDBOX-QUALIFICATION.md) 记录；本 README 描述操作方法，不替代该记录。不能由安装成功、单次探针或这些文档推出绝对隔离、E4 验收、付费模型预算网关完成，或真实 Agent benchmark 完成。

## 文件与接口

| 文件 | 用途 |
| --- | --- |
| `versions.json` | 固定 gVisor release、架构与独立核对的 SHA512；不包含部署账户的 AMI ID。 |
| `bootstrap.sh` | 新节点首次启动安装完整 `runsc`、shim、`gvisor-bin`，合并 nodeadm runtime 配置；见 [BOOTSTRAP.md](BOOTSTRAP.md)。 |
| `render.py --bindings FILE --output DIR` | 仅生成 `foundation.json`、`initial.json`、`resume.json`，不调用 AWS。 |
| `peer.py serve\|check` | 独立 native sidecar 与主容器之间的 localhost TCP、0600 Unix socket 检查。 |
| `online_fixture.py` | `render_online(base_initial_job, gateway_ip, nonce)`，以及 `network-gate`、`probe-main`、`canary` CLI。 |
| `egress_guard.py install\|verify\|remove --pod-ip IP --gateway-ip IP --veth NAME` | 在专用节点 host network namespace 中，由 root 为一个 Pod 的精确 host veth 管理规则。 |
| `../../../tools/sandbox_probe.py` | 进程安全、文件系统、SQLite WAL/崩溃恢复/跨 Pod 持久性和网络探针；由 ConfigMap 分发。 |

本目录没有完整 controller、自动申请权限、任意 Agent 执行器或付费 provider proxy。可信管理员负责节点绑定、host audit、临时 admission、gate 释放及资源回收。

## 1. 私有部署输入

以下命令从仓库根目录运行。AWS profile、cluster、固定 AMI、node role、subnet、security groups、AZ、镜像 digest 均由操作者从当前账户核对后提供。不要使用 `latest` AMI alias 或可变镜像 tag；固定 AMI 还必须匹配 EKS minor、AL2023、架构与 containerd 版本。

先在当前终端设置这些环境变量，值不要提交到仓库：

```text
AWS_PROFILE, AWS_REGION, RL_CLUSTER
RL_AMI_ID, RL_NODE_ROLE, RL_SUBNET_ID, RL_SECURITY_GROUP_IDS
RL_ZONE, RL_IMAGE
```

`RL_SECURITY_GROUP_IDS` 为逗号分隔的精确 ID；`RL_IMAGE` 为带 `@sha256:…` 的 Python 镜像。不能把角色名相似、同 Region 或同 cluster 名当作账户一致性证明。

```sh
umask 077
export RL_RUN_DIR="$(mktemp -d "${TMPDIR:-/tmp}/recall-sandbox.XXXXXX")"
chmod 700 "$RL_RUN_DIR"
export RL_NS=recall-sandbox-qual
export RL_OPS_NS=recall-sandbox-qual-ops

python3 -B tools/eks_preflight.py \
  --profile "$AWS_PROFILE" --region "$AWS_REGION" --cluster "$RL_CLUSTER" \
  --output "$RL_RUN_DIR/preflight.json"

aws eks update-kubeconfig \
  --profile "$AWS_PROFILE" --region "$AWS_REGION" --name "$RL_CLUSTER" \
  --kubeconfig "$RL_RUN_DIR/kubeconfig"
chmod 600 "$RL_RUN_DIR/kubeconfig"
export KUBECONFIG="$RL_RUN_DIR/kubeconfig"
```

检查 preflight 中 STS 与 cluster 的绑定一致、所需 CRD/API/RBAC 可用，再继续。未知、Forbidden、超时不是空结果；preflight 的 inventory 也不是 Sandbox qualification。

```sh
python3 -B - <<'PY'
import json, os
from pathlib import Path
root = Path(os.environ["RL_RUN_DIR"])
pin = json.loads(Path("infra/eks/sandbox/versions.json").read_text())["gvisor"]
binding = {
    "ami_id": os.environ["RL_AMI_ID"],
    "node_role": os.environ["RL_NODE_ROLE"],
    "subnet_id": os.environ["RL_SUBNET_ID"],
    "security_group_ids": os.environ["RL_SECURITY_GROUP_IDS"].split(","),
    "zone": os.environ["RL_ZONE"],
    "image": os.environ["RL_IMAGE"],
    "runsc_release": pin["release"],
    "runsc_sha512": pin["archive_sha512"],
}
with (root / "bindings.json").open("x") as output:
    json.dump(binding, output, indent=2)
os.chmod(root / "bindings.json", 0o600)
PY

python3 -B infra/eks/sandbox/render.py \
  --bindings "$RL_RUN_DIR/bindings.json" --output "$RL_RUN_DIR/manifests"
```

目录应为 **0700**，bindings、kubeconfig、渲染产物与原始证据为 **0600**。它们可能包含账户、ARN、私网地址、节点名、实例/卷 ID；公开报告只保留脱敏投影、版本、哈希与检查结论。

## 2. Offline：provision → host audit → initial → resume

确认固定名称的 qualification 资源属于本次操作，且没有其他试验。新环境执行：

```sh
kubectl apply -f "$RL_RUN_DIR/manifests/foundation.json"
kubectl apply -f "$RL_RUN_DIR/manifests/initial.json"
kubectl get nodes -l recall-ledger.io/sandbox=recall-sandbox-qual
kubectl get pods -n "$RL_NS"
```

`probe-initial` 的需求触发 Karpenter provisioning。新节点保留 `recall-ledger.io/unqualified=true:NoSchedule` startup taint，因此探针先等待。不要等探针成功才去移除该 taint。

从该 label 的结果中核实**恰好一台**新节点，保存 Node/NodeClaim/EC2/两块 EBS 的对应关系到私有证据，设置 `RL_NODE`。可信 host audit 容器由管理员在 `recall-sandbox-qual-ops` 创建，用 `nodeName` 精确绑定这台节点，不依赖试验 Job 调度。这个管理员环境不是 Agent 可访问的执行入口，不得挂到现有业务节点。

在新节点 host namespace 核对至少以下内容，并保留原始输出：

```sh
cat /var/lib/recall-ledger/sandbox-bootstrap.ready
cat /proc/sys/kernel/random/boot_id
systemctl is-active containerd kubelet
containerd config dump
cat /etc/containerd/recall-runsc-offline.toml
```

核对有效 runtime 是 `runsc-offline`，shim/runsc 指向完整的固定 release 目录，`platform=systrap`、`network=none`、禁止 flag override；确认 NodeConfig 合并没有替换 EKS 的 CNI、registry 或默认 runtime。核对节点/镜像/容量/卷绑定及本次安装日志。marker 只表示 bootstrap 完成，不表示运行时已通过探针。

所有 host audit 检查满足本次协议后，管理员才执行：

```sh
kubectl taint node "$RL_NODE" recall-ledger.io/unqualified:NoSchedule-
kubectl wait -n "$RL_NS" --for=condition=complete job/probe-initial --timeout=15m
kubectl logs -n "$RL_NS" job/probe-initial -c probe > "$RL_RUN_DIR/initial.log"
kubectl get pods -n "$RL_NS" -l job-name=probe-initial -o json > "$RL_RUN_DIR/initial-pod.json"
```

若 Job 失败，先保留日志、Pod 状态、runtime 错误和事件，不把删除重试解释为首次通过。`network=none` 适合 offline 检查，不能调用远程模型。`tmp` emptyDir 使用精确的共享 mount hints；CSI `data` PVC 不传通用的 `data.*` hints，保持默认 bind mount，避免 shim 按逻辑卷名查找 CSI 目录失败。

跨 Pod 持久性必须换 Pod 并保留原 PVC：

```sh
kubectl delete job -n "$RL_NS" probe-initial --cascade=foreground --wait=true --timeout=180s
kubectl get pods -n "$RL_NS" -l job-name=probe-initial
kubectl apply -f "$RL_RUN_DIR/manifests/resume.json"
kubectl wait -n "$RL_NS" --for=condition=complete job/probe-resume --timeout=15m
kubectl logs -n "$RL_NS" job/probe-resume -c probe > "$RL_RUN_DIR/resume.log"
kubectl get pods -n "$RL_NS" -l job-name=probe-resume -o json > "$RL_RUN_DIR/resume-pod.json"
```

确认旧 Pod 已消失，新旧 Pod UID 不同，PVC/PV/EBS 与 SQLite identity 相同，sequence 增量、WAL 崩溃回滚与 integrity check 符合探针预期。这不等于断电恢复、并发 fencing 或数据永不损坏。进入 online 前删除 `probe-resume` Job 并确认 Pod 消失，保留同一个 PVC。

## 3. Online 仅在当前 boot 生效

Online RuntimeClass 名为 `recall-sandbox-online-qual`，handler 为 `runsc-gateway`。它使用相同固定 runsc/shim、`systrap`、systemd cgroups 和禁止 flag override；唯一网络配置变化为 `network="sandbox"`。这个模式提供 gVisor 网络栈，外联限制依赖下文精确 veth 规则。

管理员只在本次专用节点合并新的 containerd V3 runtime 段，保留其余 EKS 配置。**其 `ConfigPath` 必须在 `/run` 下**，例如 `/run/recall-ledger/runsc-gateway.toml`，不能指向重启后仍存在的 online 配置。临时文件 root 所有、0600。配置内容：

```toml
binary_name = "/opt/recall-ledger/gvisor/<fixed-release>-x86_64/runsc"
[runsc_config]
platform = "systrap"
network = "sandbox"
systemd-cgroup = "true"
allow-flag-override = "false"
```

`runsc-gateway` 的 `pod_annotations` 只允许这七个精确 key，不用 wildcard：

```text
dev.gvisor.spec.mount.tmp.share
dev.gvisor.spec.mount.tmp.type
dev.gvisor.spec.mount.tmp.options
dev.gvisor.empty-dir.gate.force-shared
dev.gvisor.spec.mount.gate.share
dev.gvisor.spec.mount.gate.type
dev.gvisor.spec.mount.gate.options
```

`gate` 值由 fixture 固定为 `force-shared=true`、`share=shared`、`type=bind`、`options=ro,rprivate`。其支持已核对固定 release 的 [shim 实现](https://github.com/google/gvisor/blob/release-20261005.0/pkg/shim/v1/utils/volumes.go)。不能给 CSI `data` PVC 加这些 mount hints。

在专用节点为 **containerd 和 kubelet 两个 service** 都创建持久的 systemd drop-in，例如各自的 `/etc/systemd/system/<service>.service.d/90-recall-online-boot-guard.conf`：

```ini
[Unit]
ConditionPathExists=/run/recall-online-boot-authorized
```

管理员检查本次 offline/host audit 后，只为**当前 boot**创建该 marker，再做受控 reload/restart：

```sh
systemctl daemon-reload
install -m 0600 /dev/null /run/recall-online-boot-authorized
systemctl show containerd kubelet -p Conditions
```

这些命令不证明 active service 已使用新配置；必须检查 effective containerd runtime 与启动日志。不要给 marker 配置 cloud-init、tmpfiles、ExecStartPre 或任何 reboot 自动重建。重启后 `/run` 配置与 marker 消失，两 service 应拒绝启动；该节点退出本轮试验，删除并重新 provision/qualification，不能补回 marker 继续计分。这是受控部署要求，`online_fixture.py` 不会替管理员安装或验证这些 host guard。

## 4. 生成 online Job，先安装规则，再释放 gate

`RL_GATEWAY_IP` 必须是该专用节点精确的本地 IPv4，保留在私有环境中。以下仅渲染文件，并把新脚本加入已有 probe ConfigMap：

```sh
python3 -B - <<'PY'
import json, os, secrets, sys
from pathlib import Path
sys.path.insert(0, "infra/eks/sandbox")
from online_fixture import render_online, ONLINE_RUNTIME
root = Path(os.environ["RL_RUN_DIR"])
base = json.loads((root / "manifests/initial.json").read_text())
foundation = json.loads((root / "manifests/foundation.json").read_text())["items"]
nonce = secrets.token_hex(32)
job = render_online(base, os.environ["RL_GATEWAY_IP"], nonce)
config_map = next(item for item in foundation if item["kind"] == "ConfigMap")
config_map["data"]["online_fixture.py"] = Path("infra/eks/sandbox/online_fixture.py").read_text()
runtime = next(item for item in foundation if item["kind"] == "RuntimeClass")
runtime["metadata"]["name"] = ONLINE_RUNTIME
runtime["handler"] = "runsc-gateway"
for name, value in (("online.json", job), ("online-configmap.json", config_map),
                    ("online-runtime.json", runtime), ("online-nonce.json", {"nonce": nonce})):
    with (root / name).open("x") as output:
        json.dump(value, output, indent=2)
    os.chmod(root / name, 0o600)
PY

kubectl apply -f "$RL_RUN_DIR/online-configmap.json"
kubectl apply -f "$RL_RUN_DIR/online-runtime.json"
```

现有 ValidatingAdmissionPolicy 只允许 offline runtime。可信 controller 必须先准备、审阅并记录仅允许本次 fixture 的临时 admission 变更；绑定 runtime、namespace、service account、固定镜像、gate、security context、资源和禁止 API token 等约束。不能删除 policy 或把整个 namespace 开放给 runc。这里没有自动生成此临时 policy 的工具；未完成该步骤不得应用 online Job。

```sh
kubectl apply -f "$RL_RUN_DIR/online.json"
kubectl get pods -n "$RL_NS" -l job-name=probe-online -o json > "$RL_RUN_DIR/online-pod.json"
```

`network-gate` 是第一个普通 initContainer，不带 `restartPolicy: Always`；它以 UID 1000 只读挂载 `/gate`，等待最多 180 秒。`/gate` 为新建的 **disk emptyDir**，不是 SQLite PVC。它从 Downward API 读取自身 `metadata.uid`；ready JSON 中 UID 和本轮 nonce 均正确才退出成功，随后 native peer sidecar 和主容器才能开始。main/peer 不挂载 gate。

Host controller 核对 Pod 的 namespace、UID、nodeName、Pod IP、nonce 与本次 manifest 相符，在节点 host namespace 用 `ip -j route get "$RL_POD_IP"` 找到唯一直接路由的 host veth，设置 `RL_VETH`。不能根据命名模式猜接口或取第一个接口。

在专用节点 host network namespace 中，先运行无凭据 canary，保留其 PID 和日志：

```sh
python3 -u /path/to/online_fixture.py canary --bind "$RL_GATEWAY_IP"
```

管理员应通过受控进程管理使它在审计期间保持运行。`canary_ready` 表示 18080、18081 两个端口都已绑定，并分别通过 host 本机的 `/healthz` 正对照。服务只返回固定 synthetic JSON；POST、CONNECT 等请求被拒绝，不提供任意 URL 转发、模型 API、凭据或预算拦截。18081 已监听，是 deny 的对照，不能用一个本来没服务的端口代替。

随后在同一 host network namespace，以 root 执行，并确保状态目录是 host 的 `/run/recall-ledger-egress`（使用 host mount namespace 或显式挂载 host `/run`）。不能使用临时管理员容器自己的 `/run`，否则容器更换后无法核对规则所有权：

```sh
python3 -B /path/to/egress_guard.py install \
  --pod-ip "$RL_POD_IP" --gateway-ip "$RL_GATEWAY_IP" --veth "$RL_VETH"
python3 -B /path/to/egress_guard.py verify \
  --pod-ip "$RL_POD_IP" --gateway-ip "$RL_GATEWAY_IP" --veth "$RL_VETH"
```

只有两次成功、返回 `status=verified`，且在线配置、boot identity、canary、Pod identity 均未变化，controller 才能写 gate。失败保留 gate 阻塞、规则与证据，停止并清理该 Pod；不要反复写 ready 绕过失败。

规则覆盖如下：

| 路径 | 实现与限制 |
| --- | --- |
| Pod → host 或外部 | host veth 的 `mangle/PREROUTING` 第一条跳转，只有原始 source Pod IP → gateway IP:18080 TCP 可继续，其他丢弃；不放行 DNS。进入 host INPUT 前已经过此检查，没有额外覆盖全节点 INPUT 的规则。 |
| host → Pod | `filter/OUTPUT -o veth`，仅允许 gateway:18080 的 conntrack `ESTABLISHED/REPLY`，拒绝新的 host 入站连接。 |
| 经 host 转发 → Pod | `filter/FORWARD -o veth` 全部拒绝。规则检查不等于已经做过跨节点实测。 |
| IPv6 | 对应三条路径全部拒绝。 |

保持其他系统规则；`RETURN` 仍会继续执行既有规则，不保证可达。Egress 检查发生在 DNAT 前，因此管理员还必须核对 gateway:18080 没有后续 DNAT/redirect。此方案依赖可信 host/control plane，不抵抗能修改 host NAT、防火墙或 runtime 的管理员。guard 保存 boot ID、network namespace inode、veth/peer ifindex 和 MAC；`verify` 发现身份或第一跳规则变化后，由 controller 停止试验、保留证据、重新 qualification。脚本没有持续监控或自动调和服务。

核对 kubelet 的实际 root 目录后，在 host 上原子写入该 Pod 的 gate。下例为 AL2023 默认路径；`RL_POD_UID` 与 `RL_NONCE` 来自私有 controller 状态，不能使用另一 Pod 的值：

```sh
python3 -B - <<'PY'
import json, os, tempfile, uuid
from pathlib import Path
uid = os.environ["RL_POD_UID"]
assert str(uuid.UUID(uid)) == uid
gate = Path("/var/lib/kubelet/pods") / uid / "volumes/kubernetes.io~empty-dir/gate"
assert gate.is_dir() and not gate.is_symlink()
fd, temporary = tempfile.mkstemp(prefix=".ready-", dir=gate)
try:
    with os.fdopen(fd, "w") as output:
        os.fchmod(output.fileno(), 0o644)
        json.dump({"pod_uid": uid, "nonce": os.environ["RL_NONCE"]}, output)
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, gate / "ready")
finally:
    if os.path.exists(temporary):
        os.unlink(temporary)
PY
```

Host 写入不等于容器已读到；检查 `network_gate_released` 日志和主容器实际启动。主容器检查 peer，再以 `resume` 访问原 PVC，测试允许 18080、拒绝 18081，并校验 18080 的固定 HTTP payload。

`inbound_fixture_ready` 且 `inbound_fixture_listening=true` 表示 Pod 已监听 `0.0.0.0:18082`，本容器 `127.0.0.1:18082/healthz` 自测成功。随后保留 300 秒 host audit 窗口。管理员从 host 连接 Pod IP:18082 应被 OUTPUT 规则丢弃；记录真实错误类型，`connection refused`、DNS 错误或缺 listener 不能当作通过。首轮不为 FORWARD 测试侵入现有业务节点；若没有独立跨节点探针，其覆盖状态保持未验证。

收集 `probe-online` 的所有 JSON 日志、gate/guard 前后状态与控制器观测。等待审计窗口结束后检查 Job 的最终退出结果；原始 probe 失败不会因 300 秒等待或 listener 自测成功而改成成功。整个 online 状态以本轮专用节点、同一 boot、同一 Pod/veth 为范围。

## 5. 按顺序清理并核对账单资源

删除前在私有文件中固定 NodeClaim、EC2 instance、系统 EBS、PVC/PV/data EBS 的精确映射。预期是两块 EBS；不能按名称模糊匹配后删除，或在资源消失后再猜 ID。确认 data PV 的 reclaim policy，不能把保留卷报告为已删除。

1. 保存所有证据，删除 initial/resume/online Jobs，等待它们的 Pod 全部消失。
2. 在 host 确认**对应 veth 已消失**，再执行 guard remove；接口仍存在时脚本会拒绝删除规则。
3. 停止本次记录的 canary 进程；删除 PVC 和两个本次 namespace。
4. 删除本次 NodePool，等待 NodeClaims/节点退场，再删除 EC2NodeClass、两个 RuntimeClasses、ValidatingAdmissionPolicyBinding 和 policy。
5. 用 AWS API 独立确认 EC2 已 terminated，系统 EBS 与 data EBS 分别已 deleted。Kubernetes 对象消失不是 AWS 回收完成。

```sh
kubectl delete jobs -n "$RL_NS" \
  -l app.kubernetes.io/part-of=recall-sandbox-qual \
  --cascade=foreground --wait=true --timeout=180s
kubectl get pods -n "$RL_NS"
```

确认 Pod 和对应 host veth 消失后，在节点 host namespace 执行：

```sh
python3 -B /path/to/egress_guard.py remove \
  --pod-ip "$RL_POD_IP" --gateway-ip "$RL_GATEWAY_IP" --veth "$RL_VETH"
```

若尚未安装 guard，则不需要创造状态。partial install 保留了状态时也走此清理接口；遇到未知规则/读取错误先调查，不用全表 flush。停止 canary、完成 host 清理后：

```sh
kubectl delete pvc -n "$RL_NS" trial-data --wait=true --timeout=180s
kubectl delete namespace "$RL_NS" "$RL_OPS_NS" --wait=true --timeout=180s
kubectl delete nodepool recall-sandbox-qual --wait=true --timeout=300s
kubectl get nodeclaims -l karpenter.sh/nodepool=recall-sandbox-qual
kubectl delete ec2nodeclass recall-sandbox-qual --wait=true --timeout=180s
kubectl delete runtimeclass recall-sandbox-qual recall-sandbox-online-qual --ignore-not-found
kubectl delete validatingadmissionpolicybinding recall-sandbox-qual --ignore-not-found
kubectl delete validatingadmissionpolicy recall-sandbox-qual --ignore-not-found
```

只操作本次已核对的资源。若另有临时 admission 对象，按 controller 记录逐一回收。任何 timeout、finalizer、AWS 权限错误或资源残留都保持“清理未完成”，继续调查，不能直接计为成功。

AWS 核对使用此前保存的精确变量，不把 ID 写入公共报告：

```sh
aws ec2 wait instance-terminated --profile "$AWS_PROFILE" --region "$AWS_REGION" \
  --instance-ids "$RL_INSTANCE_ID"
aws ec2 wait volume-deleted --profile "$AWS_PROFILE" --region "$AWS_REGION" \
  --volume-ids "$RL_ROOT_VOLUME_ID"
aws ec2 wait volume-deleted --profile "$AWS_PROFILE" --region "$AWS_REGION" \
  --volume-ids "$RL_DATA_VOLUME_ID"
```

分别等待每块卷，保留终态证据。若某卷仍存在，核对本次 ownership/attachment/reclaim 后再处理；不要自动删除未识别资源。保留私有证据目录直到审阅结束，再按本地数据保留规则处置。

## 本地验证的范围

```sh
python3 -B -m unittest discover -s infra/eks/sandbox -p 'test_*.py' -v
```

这些测试覆盖脚本、manifest、mock 命令与本地 IPC 等合同，不会替代 Linux/EKS 的 runtime、iptables 实际 readback、网络探针和 AWS 清理检查。真实 Agent、模型 provider、$500 模型预算执行与 memory 效果比较属于后续独立协议，参见 [Benchmark 设计](../../../docs/benchmark/README.md)。
