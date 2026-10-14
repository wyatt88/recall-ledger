#!/usr/bin/env python3
"""Render a private, single-node qualification bundle; never contacts AWS."""
import argparse
import base64
import gzip
import json
import os
from pathlib import Path
import re

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
NAME = "recall-sandbox-qual"
LABEL = {"app.kubernetes.io/part-of": NAME}
NODE_LABEL = "recall-ledger.io/sandbox"
TRIAL_LABEL = "recall-ledger.io/trial"


def resource(kind, name, spec=None, namespace=None, api="v1", **extra):
    meta = {"name": name, "labels": dict(LABEL)}
    if namespace:
        meta["namespace"] = namespace
    value = {"apiVersion": api, "kind": kind, "metadata": meta, **extra}
    if spec is not None:
        value["spec"] = spec
    return value


def render(bindings):
    required = ["ami_id", "node_role", "subnet_id", "security_group_ids",
                "zone", "image", "runsc_release", "runsc_sha512"]
    if any(not bindings.get(k) for k in required):
        raise ValueError("all deployment bindings must be explicit")
    for key, pattern in [("ami_id", r"ami-[a-f0-9]+"), ("subnet_id", r"subnet-[a-f0-9]+"),
                         ("runsc_release", r"20[0-9]{6}(?:\.[0-9]+)?"),
                         ("runsc_sha512", r"[a-f0-9]{128}"),
                         ("zone", r"[a-z]{2}-[a-z]+-\d[a-z]")]:
        if not re.fullmatch(pattern, bindings[key]):
            raise ValueError(f"invalid {key}")
    if not re.fullmatch(r"[\w./:-]+@sha256:[a-f0-9]{64}", bindings["image"]):
        raise ValueError("image must be digest-pinned")
    if not all(re.fullmatch(r"sg-[a-f0-9]+", x) for x in bindings["security_group_ids"]):
        raise ValueError("invalid security group binding")
    compressed = base64.b64encode(gzip.compress((HERE / "bootstrap.sh").read_bytes(), mtime=0)).decode()
    user_data = f"""#!/bin/bash
set -euo pipefail
umask 077
mkdir -p /var/lib/recall-ledger
echo '{compressed}' | base64 -d | gzip -d > /var/lib/recall-ledger/bootstrap.sh
export ALLOW_NEW_NODE_BOOTSTRAP=1 RUNSC_ARCH=x86_64
export RUNSC_RELEASE='{bindings["runsc_release"]}'
export RUNSC_SHA512='{bindings["runsc_sha512"]}'
bash /var/lib/recall-ledger/bootstrap.sh --install
"""
    if len(user_data.encode()) > 14000:
        raise ValueError("user data leaves insufficient room for Karpenter's NodeConfig")
    ns = resource("Namespace", NAME)
    ns["metadata"]["labels"].update({
        "pod-security.kubernetes.io/enforce": "restricted",
        "pod-security.kubernetes.io/enforce-version": "v1.35",
        "recall-ledger.io/qualification": "true",
    })
    ops = resource("Namespace", NAME + "-ops")
    ops["metadata"]["labels"]["pod-security.kubernetes.io/enforce"] = "privileged"
    foundation = [
        ns, ops,
        resource("ServiceAccount", "worker", namespace=NAME, automountServiceAccountToken=False),
        resource("RuntimeClass", NAME, api="node.k8s.io/v1",
                 handler="runsc-offline",
                 overhead={"podFixed": {"cpu": "100m", "memory": "128Mi"}},
                 scheduling={"nodeSelector": {NODE_LABEL: NAME},
                             "tolerations": [{"key": "dedicated", "value": NAME, "effect": "NoSchedule"}]}),
        resource("EC2NodeClass", NAME, {
            "amiFamily": "AL2023", "amiSelectorTerms": [{"id": bindings["ami_id"]}],
            "role": bindings["node_role"],
            "subnetSelectorTerms": [{"id": bindings["subnet_id"]}],
            "securityGroupSelectorTerms": [{"id": x} for x in bindings["security_group_ids"]],
            "metadataOptions": {"httpEndpoint": "enabled", "httpTokens": "required",
                                "httpPutResponseHopLimit": 1, "httpProtocolIPv6": "disabled"},
            "blockDeviceMappings": [{"deviceName": "/dev/xvda", "ebs": {
                "volumeSize": "32Gi", "volumeType": "gp3", "encrypted": True, "deleteOnTermination": True}}],
            "tags": {"Project": NAME, "Purpose": "synthetic-no-model-qualification"},
            "userData": user_data,
        }, api="karpenter.k8s.aws/v1"),
        resource("NodePool", NAME, {
            "template": {"metadata": {"labels": {NODE_LABEL: NAME}}, "spec": {
                "nodeClassRef": {"group": "karpenter.k8s.aws", "kind": "EC2NodeClass", "name": NAME},
                "taints": [{"key": "dedicated", "value": NAME, "effect": "NoSchedule"}],
                "startupTaints": [{"key": "recall-ledger.io/unqualified", "value": "true", "effect": "NoSchedule"}],
                "requirements": [
                    {"key": "node.kubernetes.io/instance-type", "operator": "In", "values": ["m7i.large"]},
                    {"key": "karpenter.sh/capacity-type", "operator": "In", "values": ["on-demand"]},
                    {"key": "kubernetes.io/arch", "operator": "In", "values": ["amd64"]},
                    {"key": "topology.kubernetes.io/zone", "operator": "In", "values": [bindings["zone"]]}],
                "expireAfter": "2h", "terminationGracePeriod": "5m"}},
            "limits": {"cpu": "2", "memory": "8Gi"},
            "disruption": {"consolidationPolicy": "WhenEmpty", "consolidateAfter": "5m",
                           "budgets": [{"nodes": "0"}]},
        }, api="karpenter.sh/v1"),
        resource("ResourceQuota", "qualification-limit", {"hard": {
            "count/jobs.batch": "2", "pods": "2", "requests.cpu": "2200m",
            "requests.memory": "8448Mi", "requests.storage": "8Gi",
            "persistentvolumeclaims": "1"}}, namespace=NAME),
        resource("PersistentVolumeClaim", "trial-data", {
            "accessModes": ["ReadWriteOnce"], "storageClassName": "gp3",
            "resources": {"requests": {"storage": "8Gi"}}}, namespace=NAME),
        resource("ConfigMap", "probe", namespace=NAME,
                 data={"sandbox_probe.py": (ROOT / "tools/sandbox_probe.py").read_text(),
                       "peer.py": (HERE / "peer.py").read_text()}),
    ]
    # Restricted PSA alone allows an unsandboxed runc Pod. Fail closed for this namespace.
    policy = resource("ValidatingAdmissionPolicy", NAME, {
        "failurePolicy": "Fail",
        "matchConstraints": {"resourceRules": [{"apiGroups": [""], "apiVersions": ["v1"],
                                               "operations": ["CREATE", "UPDATE"], "resources": ["pods"]}],
                             "namespaceSelector": {"matchLabels": {"recall-ledger.io/qualification": "true"}}},
        "validations": [
            {"expression": f"has(object.spec.runtimeClassName) && object.spec.runtimeClassName == '{NAME}'",
             "message": "qualification Pods must use the pinned offline gVisor runtime"},
            {"expression": "has(object.spec.automountServiceAccountToken) && !object.spec.automountServiceAccountToken",
             "message": "qualification Pods cannot mount API credentials"},
            {"expression": "!has(object.spec.volumes) || object.spec.volumes.all(v, !has(v.projected) || !has(v.projected.sources) || v.projected.sources.all(s, !has(s.serviceAccountToken)))",
             "message": "explicit service account token projections are forbidden"},
        ]}, api="admissionregistration.k8s.io/v1")
    foundation.extend([policy, resource("ValidatingAdmissionPolicyBinding", NAME, {
        "policyName": NAME, "validationActions": ["Deny"]}, api="admissionregistration.k8s.io/v1")])

    def job(phase):
        security = {"allowPrivilegeEscalation": False, "readOnlyRootFilesystem": True,
                    "capabilities": {"drop": ["ALL"]}}
        mounts = [{"name": "data", "mountPath": "/work"}, {"name": "tmp", "mountPath": "/tmp"},
                  {"name": "probe", "mountPath": "/opt/probe", "readOnly": True}]
        pod = {
            "runtimeClassName": NAME, "serviceAccountName": "worker",
            "automountServiceAccountToken": False, "restartPolicy": "Never",
            "enableServiceLinks": True,
            "securityContext": {"runAsNonRoot": True, "runAsUser": 1000, "runAsGroup": 1000,
                                "fsGroup": 1000, "seccompProfile": {"type": "RuntimeDefault"}},
            "affinity": {"podAntiAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": [{
                "labelSelector": {"matchExpressions": [{"key": TRIAL_LABEL, "operator": "Exists"}]},
                "namespaceSelector": {}, "topologyKey": "kubernetes.io/hostname"}]}},
            "volumes": [{"name": "data", "persistentVolumeClaim": {"claimName": "trial-data"}},
                        {"name": "tmp", "emptyDir": {"medium": "Memory", "sizeLimit": "128Mi"}},
                        {"name": "probe", "configMap": {"name": "probe"}}],
            "initContainers": [{
                "name": "peer", "restartPolicy": "Always", "image": bindings["image"],
                "command": ["python3", "-B", "/opt/probe/peer.py", "serve"],
                "startupProbe": {"exec": {"command": ["python3", "-B", "/opt/probe/peer.py", "check"]},
                                 "periodSeconds": 2, "failureThreshold": 30},
                "securityContext": security, "volumeMounts": mounts,
                "resources": {"requests": {"cpu": "100m", "memory": "256Mi"},
                              "limits": {"cpu": "100m", "memory": "256Mi"}}}],
            "containers": [{
                "name": "probe", "image": bindings["image"],
                "command": ["sh", "-c", "python3 -B /opt/probe/peer.py check && exec python3 -B /opt/probe/sandbox_probe.py --phase " + phase + " --work-dir /work"],
                "securityContext": security, "volumeMounts": mounts,
                "resources": {"requests": {"cpu": "900m", "memory": "3840Mi"},
                              "limits": {"cpu": "900m", "memory": "3840Mi"}}}],
        }
        return resource("Job", "probe-" + phase, {
            # Includes EC2 provisioning, startup audit and first image/PVC setup.
            # This qualification deadline is not an Agent task latency budget.
            "backoffLimit": 0, "activeDeadlineSeconds": 1800,
            "template": {"metadata": {"labels": {**LABEL, TRIAL_LABEL: "qualification"},
                                     "annotations": {
                                         "dev.gvisor.spec.mount.tmp.share": "pod",
                                         "dev.gvisor.spec.mount.tmp.type": "tmpfs",
                                         "dev.gvisor.spec.mount.tmp.options": "rw,rprivate",
                                     }},
                         "spec": pod}}, namespace=NAME, api="batch/v1")
    return foundation, job("initial"), job("resume")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bindings", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    foundation, initial, resume = render(json.loads(args.bindings.read_text()))
    args.output.mkdir(mode=0o700, parents=True, exist_ok=True)
    if args.output.stat().st_mode & 0o077:
        raise SystemExit("output directory must be owner-only (0700)")
    for name, data in [("foundation", {"apiVersion": "v1", "kind": "List", "items": foundation}),
                       ("initial", initial), ("resume", resume)]:
        path = args.output / (name + ".json")
        with path.open("x") as output:
            os.chmod(path, 0o600)
            json.dump(data, output, indent=2)
    print("Rendered private manifests only; no cloud resources created.")


if __name__ == "__main__":
    main()
