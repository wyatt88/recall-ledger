# gVisor bootstrap for a fresh EKS node

This script prepares **a new dedicated AL2023 EKS 1.35 / containerd 2.x node**.
It must run in that node's cloud-init shell part, before `nodeadm-run` starts
kubelet. It refuses active or previously joined nodes. Creating the node,
IAM, labels, taints, RuntimeClass and workload admission are separate deployment
steps. Do not execute it against the current shared nodes.

Set `RUNSC_RELEASE` to a dated immutable release and `RUNSC_SHA512` to the
independently retrieved checksum of that release's architecture-specific
`gvisor.tar.bz2`. There is no `latest` default or checksum auto-trust. Set
`ALLOW_NEW_NODE_BOOTSTRAP=1` and call `bash bootstrap.sh --install` in user data.
No cluster/account credentials belong in these variables.

The initial x86_64 release and checksum are recorded in [versions.json](versions.json):
`20261005.0`. The 166,822,039-byte official archive was downloaded locally and
passed the script's SHA512 and complete-package checks on 2026-10-14. No Linux
runtime was executed during that local check. Architecture or release changes
require a separately verified checksum; do not reuse this one for arm64.

The complete package goes into
`/opt/recall-ledger/gvisor/<release>-<architecture>/`, keeping `runsc`,
`containerd-shim-runsc-v1` and `gvisor-bin/` together. SHA512 is checked before
extraction or execution. The extractor accepts only regular runtime files and
directories, rejects links/traversal, and never extracts directly over system
binaries. Installation does not invoke a package manager. It requires
`python3` with the standard-library `bz2`, `tarfile` and `hashlib` modules;
it does not require separate `tar`, `bzip2` or `zstd` executables. Missing
Python/compression support fails explicitly; include it when preparing the AMI.

`/etc/eks/nodeadm.d/90-recall-sandbox.yaml` adds this **V3** containerd runtime:

```toml
[plugins."io.containerd.cri.v1.runtime".containerd.runtimes.runsc-offline]
runtime_type = "io.containerd.runsc.v1"
runtime_path = "/opt/recall-ledger/gvisor/<release>-<architecture>/containerd-shim-runsc-v1"
base_runtime_spec = "/etc/containerd/base-runtime-spec.json"
pod_annotations = [
  "dev.gvisor.spec.mount.tmp.share",
  "dev.gvisor.spec.mount.tmp.type",
  "dev.gvisor.spec.mount.tmp.options"
]
[plugins."io.containerd.cri.v1.runtime".containerd.runtimes.runsc-offline.options]
TypeUrl = "io.containerd.runsc.v1.options"
ConfigPath = "/etc/containerd/recall-runsc-offline.toml"
```

The shim configuration selects its paired binary and `systrap`, `network=none`,
systemd cgroups, and disables flag overrides. No default runtime, CNI, image
registry, pause image or containerd root setting is replaced. AWS nodeadm
performs the merge using the rest of the node's existing NodeConfig. User data
must still supply the cluster fields. The script requires an AMI whose
`nodeadm-run.service` reads the `nodeadm.d` configuration source.

Only the three named mount hints above can pass from Pod annotations to runsc.
There is no `dev.gvisor.*` wildcard and flag overrides remain disabled. The
trusted workload controller must supply these values, using the exact Pod
volume names:

| Volume | `share` | `type` | `options` |
| --- | --- | --- | --- |
| `tmp` | `pod` | `tmpfs` | `rw,rprivate` |

These hints give containers in the sandbox a shared view of `tmp`, including
Unix sockets. The bootstrap allowlist restricts annotation
keys; workload admission or the trusted controller must restrict their values.
On 2026-10-15 the deployment owner observed cross-container TCP working while
the Unix socket was absent from the other container without these mount hints.
The first repair also applied hints to the CSI-backed `data` PVC, but the shim
failed to resolve its logical volume name (`get volume path for "data":
unexpected matched volume list []`). CSI uses a generated mount directory rather
than that logical name. The final configuration therefore passes hints only for
the `tmp` emptyDir. The `data` PVC keeps its default bind mount; only the main
container accesses the persistent database, so it does not need a shared mount
hint. The local test verifies the exact allowlist; successful socket sharing and
data persistence require a separate rerun on the target node.

The installation adds a systemd condition requiring its success marker before
`nodeadm-run` can start this fresh node. A failed installation leaves that
condition unsatisfied. Investigate the bootstrap log and replace the node;
this script deliberately refuses to overwrite a partial installation.

`network=none` leaves the sandbox's internal loopback available and removes its
external network connectivity. It is suitable for an **offline** runtime,
SQLite/filesystem, IPC and denied-network smoke test. It cannot run an Agent
that needs a remote model. It does not restrict host services, image pulls, or
other runtimes. Do not equate installation with escape resistance, CNI policy
enforcement, resource-limit acceptance or completed Agent benchmarks.

Offline checks:

```sh
bash -n infra/eks/sandbox/bootstrap.sh
python3 -m unittest discover -s infra/eks/sandbox -p 'test_bootstrap.py' -v
RUNSC_RELEASE=20261005.0 RUNSC_ARCH=x86_64 \
  bash infra/eks/sandbox/bootstrap.sh --print-nodeconfig
```

The render-only command does not execute an installation. The install command always needs the matching
explicit SHA512. The success marker says `runtime_qualified=false`; the
deployment owner must separately verify the actual handler, guest identity,
blocked IMDS/API/public/private destinations, restart behavior, isolation and
limits before admitting workloads.

Sources reviewed 2026-10-14:

- [Official installation and multi-file package](https://gvisor.dev/docs/user_guide/install/).
- [Shim ConfigPath and runsc flags](https://gvisor.dev/docs/user_guide/containerd/configuration/).
- [Shared-volume mount hints](https://gvisor.dev/docs/user_guide/containerd/configuration/#enabling-inotify-for-shared-volumes)
  (annotation keys and meanings checked again on 2026-10-15).
- [Offline networking](https://gvisor.dev/docs/user_guide/networking/).
- [Systemd cgroup requirements](https://gvisor.dev/docs/user_guide/systemd/).
- [AWS NodeConfig drop-ins](https://awslabs.github.io/amazon-eks-ami/nodeadm/doc/examples/).
- [AWS merge implementation](https://github.com/awslabs/amazon-eks-ami/blob/main/nodeadm/internal/containerd/config.go)
  (read blob `d93a1fd56dcaa2cb774eb487175f726dd08e5246`).
- [AWS containerd V3 template](https://github.com/awslabs/amazon-eks-ami/blob/main/nodeadm/internal/containerd/config2.template.toml)
  (read blob `d138db38003bba7b6821bb70859e7c6583387643`).
- [containerd absolute shim runtime_path](https://github.com/containerd/containerd/blob/main/internal/cri/config/config.go)
  (read blob `30de76c1eb7220b085fe45eb571571c47df8d5a1`).
