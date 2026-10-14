#!/usr/bin/env bash
# New, dedicated AL2023 EKS 1.35 nodes only. Never run this on an existing node.
set -euo pipefail
umask 022

die() { printf 'recall-sandbox bootstrap: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Usage: bootstrap.sh --install
       bootstrap.sh --print-nodeconfig | --print-runsc-config
       bootstrap.sh --check-archive /path/to/gvisor.tar.bz2

Required: RUNSC_RELEASE=YYYYMMDD[.N] (immutable release, never "latest")
Install/check: RUNSC_SHA512=<128 hexadecimal characters for gvisor.tar.bz2>
Install only: ALLOW_NEW_NODE_BOOTSTRAP=1
Optional: RUNSC_ARCH=x86_64|aarch64 (must match the node when installing)

Install is for a new dedicated AL2023 EKS 1.35 node's cloud-init user script,
before nodeadm-run/kubelet. It downloads only the pinned official tarball.
It writes a NodeConfig overlay; it does not replace EKS containerd configuration,
restart running services, create Kubernetes resources, or call model providers.
EOF
}

mode=${1:---help}
if [[ "$mode" == "--help" || "$mode" == "-h" ]]; then usage; exit 0; fi
case "$mode" in
  --install|--print-nodeconfig|--print-runsc-config|--check-archive) ;;
  *) die "unsupported mode; use --help" ;;
esac
[[ "${RUNSC_RELEASE:-}" =~ ^20[0-9]{6}(\.[0-9]+)?$ ]] ||
  die "RUNSC_RELEASE must name an immutable dated release"
host_arch=$(uname -m)
[[ "$host_arch" != "arm64" ]] || host_arch=aarch64
arch=${RUNSC_ARCH:-$host_arch}
[[ "$arch" == "x86_64" || "$arch" == "aarch64" ]] || die "unsupported architecture"
bundle_dir="/opt/recall-ledger/gvisor/${RUNSC_RELEASE}-${arch}"
shim_config=/etc/containerd/recall-runsc-offline.toml
overlay=/etc/eks/nodeadm.d/90-recall-sandbox.yaml
ready=/var/lib/recall-ledger/sandbox-bootstrap.ready

print_runsc_config() {
  cat <<EOF
binary_name = "${bundle_dir}/runsc"
[runsc_config]
  platform = "systrap"
  network = "none"
  systemd-cgroup = "true"
  allow-flag-override = "false"
EOF
}

print_nodeconfig() {
  cat <<EOF
apiVersion: node.eks.aws/v1alpha1
kind: NodeConfig
spec:
  containerd:
    config: |
      version = 3
      [plugins."io.containerd.cri.v1.runtime".containerd.runtimes.runsc-offline]
        runtime_type = "io.containerd.runsc.v1"
        runtime_path = "${bundle_dir}/containerd-shim-runsc-v1"
        base_runtime_spec = "/etc/containerd/base-runtime-spec.json"
        pod_annotations = [
          "dev.gvisor.spec.mount.tmp.share",
          "dev.gvisor.spec.mount.tmp.type",
          "dev.gvisor.spec.mount.tmp.options"
        ]
      [plugins."io.containerd.cri.v1.runtime".containerd.runtimes.runsc-offline.options]
        TypeUrl = "io.containerd.runsc.v1.options"
        ConfigPath = "${shim_config}"
EOF
}

case "$mode" in
  --print-nodeconfig) print_nodeconfig; exit 0 ;;
  --print-runsc-config) print_runsc_config; exit 0 ;;
esac
[[ "${RUNSC_SHA512:-}" =~ ^[a-fA-F0-9]{128}$ ]] || die "RUNSC_SHA512 must pin the bz2 archive"
command -v python3 >/dev/null || die "python3 is required"
python3 -c 'import bz2, hashlib, tarfile' ||
  die "Python standard-library bz2 support is required; bake it into the new AMI"

# Verify before interpreting the archive, then copy only regular files/directories.
# Do not use tar's unrestricted extraction for a root installation.
extract_bundle() {
  python3 - "$1" "$2" "$RUNSC_SHA512" <<'PY'
import hashlib
import os
from pathlib import Path, PurePosixPath
import shutil
import sys
import tarfile

archive, destination, expected = sys.argv[1:]
digest = hashlib.sha512()
with open(archive, "rb") as handle:
    for chunk in iter(lambda: handle.read(1024 * 1024), b""):
        digest.update(chunk)
if digest.hexdigest() != expected.lower():
    raise SystemExit("archive SHA512 mismatch")
root = Path(destination)
with tarfile.open(archive, "r:bz2") as source:
    members = source.getmembers()
    seen, runtime_files, selected, total = set(), set(), [], 0
    for member in members:
        name = PurePosixPath(member.name)
        if not name.parts and member.isdir():
            continue  # A harmless "." archive root entry.
        if name.is_absolute() or ".." in name.parts or not name.parts:
            raise SystemExit("unsafe archive path")
        if name.parts[0] not in {"runsc", "containerd-shim-runsc-v1", "gvisor-bin"}:
            continue
        if str(name) in seen or not (member.isfile() or member.isdir()):
            raise SystemExit("duplicate, linked or special archive member")
        if name.parts[0] in {"runsc", "containerd-shim-runsc-v1"} and len(name.parts) != 1:
            raise SystemExit("invalid binary path")
        if name.parts[0] in {"runsc", "containerd-shim-runsc-v1"}:
            if not member.isfile() or not member.mode & 0o111:
                raise SystemExit("runtime entry must be an executable regular file")
            runtime_files.add(str(name))
        seen.add(str(name))
        total += member.size
        if member.size > 512 * 1024 * 1024 or total > 2 * 1024 * 1024 * 1024:
            raise SystemExit("bundle exceeds installation size limit")
        selected.append((member, name))
    if not {"runsc", "containerd-shim-runsc-v1"} <= runtime_files or not any(
        member.isfile() and member.mode & 0o111 and len(name.parts) > 1 and name.parts[0] == "gvisor-bin"
        for member, name in selected
    ):
        raise SystemExit("bundle must include runsc, shim and gvisor-bin sidecars")
    for member, name in selected:
        target = root.joinpath(*name.parts)
        if member.isdir():
            target.mkdir(parents=True, exist_ok=True)
            target.chmod(0o755)
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            with source.extractfile(member) as src, target.open("xb") as dst:
                shutil.copyfileobj(src, dst)
            target.chmod(0o755 if member.mode & 0o111 else 0o644)
    for directory, _, _ in os.walk(root):
        Path(directory).chmod(0o755)
PY
}

temporary=""
staging=""
cleanup() {
  [[ -z "$temporary" ]] || rm -rf -- "$temporary"
  [[ -z "$staging" ]] || rm -rf -- "$staging"
}
trap cleanup EXIT
trap 'printf "recall-sandbox bootstrap failed at line %s; node must remain unused\n" "$LINENO" >&2' ERR

if [[ "$mode" == "--check-archive" ]]; then
  [[ $# == 2 && -f "$2" ]] || die "--check-archive requires one existing archive"
  temporary=$(mktemp -d)
  chmod 0700 "$temporary"
  mkdir "$temporary/extracted"
  extract_bundle "$2" "$temporary/extracted"
  printf 'Archive checksum and multi-file layout verified; no runtime executed.\n'
  exit 0
fi

[[ "${ALLOW_NEW_NODE_BOOTSTRAP:-}" == "1" ]] || die "first-boot guard is required"
[[ $(id -u) == 0 && $(uname -s) == Linux ]] || die "install requires root on the new Linux node"
[[ "$arch" == "$host_arch" ]] || die "archive architecture differs from this node"
grep -Eq '^ID="?amzn"?$' /etc/os-release || die "Amazon Linux is required"
grep -Eq '^VERSION_ID="?2023"?$' /etc/os-release || die "Amazon Linux 2023 is required"
for binary in curl containerd kubelet systemctl install; do
  command -v "$binary" >/dev/null || die "missing required binary: $binary"
done
containerd --version | grep -Eq '(^| )v?2\.[0-9]+\.' || die "containerd 2.x is required"
kubelet --version | grep -Eq '^Kubernetes v1\.35\.' || die "this bootstrap targets EKS 1.35"
[[ -f /sys/fs/cgroup/cgroup.controllers ]] || die "unified cgroup v2 is required"
if systemctl is-active --quiet kubelet ||
   [[ -e /var/lib/kubelet/pki/kubelet-client-current.pem ]] ||
   [[ -e /var/lib/kubelet/pki/kubelet-client.crt ]]; then
  die "refusing an active or previously joined node; create a fresh dedicated node"
fi
systemctl cat nodeadm-run.service | grep -Fq -- 'file:///etc/eks/nodeadm.d/' ||
  die "this AMI must support nodeadm-run NodeConfig drop-ins"
for target in "$bundle_dir" "$shim_config" "$overlay" "$ready" \
  /etc/systemd/system/nodeadm-run.service.d/90-recall-sandbox.conf; do
  [[ ! -e "$target" && ! -L "$target" ]] || die "refusing to overwrite existing sandbox installation"
done

# Fail closed if checksum, extraction, or configuration installation fails.
# The existing nodeadm unit is unchanged; its additional condition only applies
# on this fresh node and is satisfied after successful local installation.
install -d -m 0755 /etc/systemd/system/nodeadm-run.service.d
cat > /etc/systemd/system/nodeadm-run.service.d/90-recall-sandbox.conf <<EOF
[Unit]
ConditionPathExists=${ready}
EOF
systemctl daemon-reload
temporary=$(mktemp -d /var/tmp/recall-sandbox.XXXXXX)
chmod 0700 "$temporary"
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
  --connect-timeout 20 --max-time 240 --retry 2 \
  "https://storage.googleapis.com/gvisor/releases/release/${RUNSC_RELEASE}/${arch}/gvisor.tar.bz2" \
  --output "$temporary/gvisor.tar.bz2"
install -d -m 0755 /opt/recall-ledger/gvisor
staging=$(mktemp -d /opt/recall-ledger/gvisor/.staging.XXXXXX)
extract_bundle "$temporary/gvisor.tar.bz2" "$staging"
mv "$staging" "$bundle_dir"
staging=""
"$bundle_dir/runsc" --version
print_runsc_config > "$temporary/runsc.toml"
print_nodeconfig > "$temporary/nodeconfig.yaml"
install -d -m 0755 /etc/containerd /etc/eks/nodeadm.d /var/lib/recall-ledger
install -m 0644 "$temporary/runsc.toml" "$shim_config"
install -m 0644 "$temporary/nodeconfig.yaml" "$overlay"
cat > "$temporary/ready.json" <<EOF
{"release":"${RUNSC_RELEASE}","arch":"${arch}","archive_sha512":"${RUNSC_SHA512}","handler":"runsc-offline","platform":"systrap","network":"none","runtime_qualified":false}
EOF
install -m 0644 "$temporary/ready.json" "$ready"
printf 'Pinned runtime installed. nodeadm-run will merge the overlay at first boot; qualification remains pending.\n'
