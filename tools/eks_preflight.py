#!/usr/bin/env python3
"""Read-only EKS inventory for RecallLedger; never authorizes benchmark execution.

Python 3.9+ on Linux/macOS, standard library only. AWS credentials must already work. The tool
does not log in, alter RBAC, read Secret resources, or create cloud resources.
The output is deliberately a lossy local inventory, not an isolation test.
"""

import argparse
import base64
from collections import Counter
from datetime import datetime, timezone
from decimal import Decimal, DecimalException
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlparse


LIMITS = [
    "Local cluster inventory can be sensitive; review before publishing.",
    "Inventory success is not Sandbox qualification or benchmark acceptance.",
    "RuntimeClass objects do not prove the configured runtime runs on nodes.",
    "CNI flags and NetworkPolicy objects do not prove traffic is blocked.",
    "Resource requests are configured requests, not measured usage or free capacity.",
    "Image tags are reduced to numerical versions; these are not immutable image pins.",
    "No model calls, admission pipeline, workload execution or cost enforcement is tested.",
]
VERSION = re.compile(r"v?(\d+\.\d+(?:\.\d+)?)(?:[-+][A-Za-z][A-Za-z0-9.-]{0,50})?$")
INSTANCE = re.compile(r"[a-z][a-z0-9]{1,8}\.(?:nano|micro|small|medium|large|xlarge|\d+xlarge|metal(?:-\d+xl)?)$")
QUANTITY = re.compile(r"([+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?)([numkKMGTEP]|[KMGTPE]i)?$")
UNITS = {"n": Decimal("1e-9"), "u": Decimal("1e-6"), "m": Decimal("1e-3")}
UNITS.update({s: Decimal(1000) ** (i + 1) for i, s in enumerate("kMGTPE")})
UNITS["K"] = UNITS["k"]
UNITS.update({s + "i": Decimal(1024) ** (i + 1) for i, s in enumerate("KMGTPE")})
KNOWN_COMPONENTS = {
    "aws-node", "aws-ebs-csi-driver", "ebs-csi-node", "ebs-csi-controller",
    "coredns", "kube-proxy", "karpenter", "metrics-server", "snapshot-controller",
}
KNOWN_DRIVERS = {"ebs.csi.aws.com", "efs.csi.aws.com", "kubernetes.io/aws-ebs"}
KNOWN_RUNTIMES = {"runc", "runsc", "gvisor", "kata", "kata-qemu", "kata-fc", "kata-clh"}


def now():
    return datetime.now(timezone.utc).isoformat()


def choice(value, allowed):
    return value if value in allowed else "other_or_unknown"


def version(value):
    """Retain a numerical version, never arbitrary image/registry text."""
    if isinstance(value, str):
        match = VERSION.fullmatch(value)
        if match:
            return match.group(1)
    return "unknown"


def count(values):
    return dict(sorted(Counter(values).items()))


def quantity(value):
    if len(str(value)) > 64:
        raise ValueError("invalid Kubernetes resource quantity")
    match = QUANTITY.fullmatch(str(value))
    if not match:
        raise ValueError("invalid Kubernetes resource quantity")
    try:
        number = Decimal(match.group(1)) * UNITS.get(match.group(2), Decimal(1))
    except DecimalException as error:
        raise ValueError("invalid Kubernetes resource quantity") from error
    if not number.is_finite() or number < 0 or number > Decimal("1e30"):
        raise ValueError("invalid Kubernetes resource quantity")
    return number


def resources(values):
    values = values or {}
    return {key: quantity(values.get(key, "0")) for key in ("cpu", "memory", "ephemeral-storage")}


def serial_resources(values):
    return {
        "cpu_cores": str(values["cpu"]),
        "memory_bytes": str(values["memory"]),
        "ephemeral_storage_bytes": str(values["ephemeral-storage"]),
    }


def total_resources(values):
    result = resources({})
    for value in values:
        for key, amount in resources(value).items():
            result[key] += amount
    return serial_resources(result)


def items(document):
    value = document.get("items")
    if not isinstance(value, list) or any(not isinstance(item, dict) for item in value):
        raise ValueError("missing resource list")
    return value


def ready_condition(item):
    condition = next((c for c in item.get("status", {}).get("conditions", [])
                      if c.get("type") == "Ready"), {})
    return choice(condition.get("status"), {"True", "False", "Unknown"})


def node_inventory(document):
    nodes = items(document)
    for node in nodes:
        for field in ("capacity", "allocatable"):
            if not all(key in node.get("status", {}).get(field, {}) for key in ("cpu", "memory")):
                raise ValueError("node resource fields unavailable")
    info = [node.get("status", {}).get("nodeInfo", {}) for node in nodes]
    labels = [node.get("metadata", {}).get("labels", {}) for node in nodes]
    runtimes = []
    for row in info:
        runtime, _, runtime_version = row.get("containerRuntimeVersion", "").partition("://")
        runtimes.append(choice(runtime, {"containerd", "docker", "cri-o"}) + ":" + version(runtime_version))
    return {
        "count": len(nodes),
        "ready": count(ready_condition(node) for node in nodes),
        "unschedulable_count": sum(node.get("spec", {}).get("unschedulable") is True for node in nodes),
        "architectures": count(choice(row.get("architecture"), {"arm64", "amd64", "ppc64le", "s390x"}) for row in info),
        "operating_systems": count(choice(row.get("operatingSystem"), {"linux", "windows"}) for row in info),
        "os_families": count(next((family for family in ("Bottlerocket", "Amazon Linux", "Ubuntu", "Windows")
                                   if str(row.get("osImage", "")).startswith(family)), "other_or_unknown") for row in info),
        "kubelet_versions": count(version(row.get("kubeletVersion")) for row in info),
        "container_runtimes": count(runtimes),
        "instance_types": count(value if INSTANCE.fullmatch(str(value)) else "other_or_unknown"
                                for value in (row.get("node.kubernetes.io/instance-type") for row in labels)),
        "capacity_types": count(choice(row.get("karpenter.sh/capacity-type", row.get("eks.amazonaws.com/capacityType")),
                                       {"spot", "on-demand", "reserved", "SPOT", "ON_DEMAND", "CAPACITY_BLOCK"}) for row in labels),
        "availability_zone_count": len({row["topology.kubernetes.io/zone"] for row in labels
                                       if row.get("topology.kubernetes.io/zone")}),
        "taint_effects": count(choice(taint.get("effect"), {"NoSchedule", "PreferNoSchedule", "NoExecute"})
                               for node in nodes for taint in node.get("spec", {}).get("taints", [])),
        "capacity": total_resources(node.get("status", {}).get("capacity", {}) for node in nodes),
        "allocatable": total_resources(node.get("status", {}).get("allocatable", {}) for node in nodes),
    }


def pod_request(pod):
    """Scheduler-style requests including restartable init containers/overhead.

    Pod-level CPU/memory requests override container-level totals when present.
    In-place resize status is intentionally not modeled; callers flag such Pods.
    """
    spec = pod.get("spec", {})
    regular = resources({})
    for container in spec.get("containers", []):
        for key, value in resources(container.get("resources", {}).get("requests")).items():
            regular[key] += value
    running_init, init_peak = resources({}), resources({})
    for container in spec.get("initContainers", []):
        request = resources(container.get("resources", {}).get("requests"))
        if container.get("restartPolicy") == "Always":
            for key in running_init:
                running_init[key] += request[key]
            request = running_init.copy()
        else:
            request = {key: value + running_init[key] for key, value in request.items()}
        init_peak = {key: max(init_peak[key], request[key]) for key in init_peak}
    result = {key: max(regular[key] + running_init[key], init_peak[key]) for key in regular}
    for key, value in spec.get("resources", {}).get("requests", {}).items():
        if key in ("cpu", "memory"):
            result[key] = quantity(value)
    for key, value in resources(spec.get("overhead")).items():
        result[key] += value
    return result


def pod_inventory(document):
    pods = items(document)
    active = [pod for pod in pods if pod.get("status", {}).get("phase") not in {"Succeeded", "Failed"}]
    assigned = [pod for pod in active if pod.get("spec", {}).get("nodeName")]
    totals = resources({})
    resize_count = 0
    for pod in assigned:
        for key, value in pod_request(pod).items():
            totals[key] += value
        status = pod.get("status", {})
        resize_count += bool(status.get("resize") or any(
            row.get("allocatedResources") or row.get("resources")
            for row in status.get("containerStatuses", []) + status.get("initContainerStatuses", [])))
    return {
        "count": len(pods),
        "phases": count(choice(pod.get("status", {}).get("phase"),
                               {"Pending", "Running", "Succeeded", "Failed", "Unknown"}) for pod in pods),
        "active_assigned_count": len(assigned),
        "active_unassigned_count": len(active) - len(assigned),
        "active_assigned_spec_requests": serial_resources(totals),
        "pods_with_allocation_or_resize_status": resize_count,
        "requests_limit": "Spec-only aggregate; excludes terminal/unassigned Pods; not free capacity or scheduler admission.",
    }


def runtime_inventory(document):
    rows = items(document)
    return {
        "count": len(rows),
        "handlers": count(choice(row.get("handler"), KNOWN_RUNTIMES) for row in rows),
        "with_scheduling_constraints": sum(bool(row.get("scheduling")) for row in rows),
        "with_overhead": sum(bool(row.get("overhead")) for row in rows),
        "actual_runtime_execution_verified": False,
    }


def storage_inventory(document):
    return {"classes": [{
        "index": index,
        "provisioner": choice(row.get("provisioner"), KNOWN_DRIVERS),
        "volume_type": choice(row.get("parameters", {}).get("type"), {"gp2", "gp3", "io1", "io2", "st1", "sc1"}),
        "encrypted": {"true": True, "false": False}.get(str(row.get("parameters", {}).get("encrypted", "")).lower()),
        "binding_mode": choice(row.get("volumeBindingMode"), {"Immediate", "WaitForFirstConsumer"}),
        "reclaim_policy": choice(row.get("reclaimPolicy"), {"Delete", "Retain"}),
        "allow_volume_expansion": row.get("allowVolumeExpansion") is True,
        "default": row.get("metadata", {}).get("annotations", {}).get("storageclass.kubernetes.io/is-default-class") == "true",
        "topology_constraint_count": len(row.get("allowedTopologies", [])),
    } for index, row in enumerate(items(document))]}


def image_version(image):
    # Never return registry/repository strings (private ECR names contain account IDs).
    if "@sha256:" in image:
        digest = image.rsplit("@sha256:", 1)[1]
        return {"sha256": digest} if re.fullmatch(r"[0-9a-f]{64}", digest) else {"version": "unknown"}
    return {"version": version(image.rsplit(":", 1)[-1])}


def component_inventory(document):
    rows = items(document)
    result = []
    for row in rows:
        name = row.get("metadata", {}).get("name")
        if name not in KNOWN_COMPONENTS:
            continue
        spec = row.get("spec", {}).get("template", {}).get("spec", {})
        containers = spec.get("containers", [])
        data = {
            "component": name,
            "images": [image_version(str(container.get("image", ""))) for container in containers],
            "available": integer(row.get("status", {}).get("numberAvailable", row.get("status", {}).get("availableReplicas"))),
            "desired": integer(row.get("status", {}).get("desiredNumberScheduled", row.get("spec", {}).get("replicas"))),
            "ready": integer(row.get("status", {}).get("numberReady", row.get("status", {}).get("readyReplicas"))),
        }
        if name == "aws-node":
            network_flags = [env for container in containers for env in container.get("env", [])
                             if env.get("name") == "ENABLE_NETWORK_POLICY"]
            data["network_policy_flag"] = [
                {"true": True, "false": False}.get(str(env.get("value", "")).lower()) for env in network_flags
            ]
            data["node_agent_network_policy_args"] = [
                {"true": True, "false": False}.get(argument.partition("=")[2].lower())
                for container in containers if container.get("name") == "aws-eks-nodeagent"
                for argument in container.get("args", [])
                if isinstance(argument, str) and argument.partition("=")[0] == "--enable-network-policy"
            ]
            data["network_policy_enforcing_modes"] = [
                choice(env.get("value"), {"standard", "strict"})
                for container in containers for env in container.get("env", [])
                if env.get("name") == "NETWORK_POLICY_ENFORCING_MODE"
            ]
            data["policy_agent_container_present"] = any(container.get("name") == "aws-eks-nodeagent" for container in containers)
            data["network_policy_enforcement_verified"] = False
        result.append(data)
    return {"resource_count": len(rows), "recognized_components": result,
            "unrecognized_resource_count": len(rows) - len(result)}


def csi_inventory(document):
    rows = items(document)
    return {"count": len(rows), "drivers": count(choice(row.get("metadata", {}).get("name"), KNOWN_DRIVERS) for row in rows)}


def namespace_inventory(document):
    rows = items(document)
    return {"count": len(rows), "phases": count(choice(row.get("status", {}).get("phase"), {"Active", "Terminating"}) for row in rows)}


def network_inventory(document):
    rows = items(document)
    return {"count": len(rows),
            "namespaces_with_policies": len({row.get("metadata", {}).get("namespace") for row in rows}),
            "policy_types": count(choice(value, {"Ingress", "Egress"}) for row in rows
                                  for value in row.get("spec", {}).get("policyTypes", [])),
            "enforcement_verified": False}


def integer(value):
    return value if type(value) is int and value >= 0 else None


def karpenter_inventory(document):
    rows = items(document)
    return {"count": len(rows), "ready": count(ready_condition(row) for row in rows),
            "disruption_budget_count": sum(len(row.get("spec", {}).get("disruption", {}).get("budgets", [])) for row in rows),
            "consolidation_policies": count(choice(row.get("spec", {}).get("disruption", {}).get("consolidationPolicy"),
                                                   {"WhenEmpty", "WhenEmptyOrUnderutilized"}) for row in rows),
            "pool_limits": [
                {key: str(quantity(value)) for key, value in row.get("spec", {}).get("limits", {}).items()
                 if key in {"cpu", "memory"}}
                for row in rows
            ]}


def nodeclaim_inventory(document):
    rows = items(document)
    return {"count": len(rows), "ready": count(ready_condition(row) for row in rows),
            "capacity_types": count(choice(row.get("metadata", {}).get("labels", {}).get("karpenter.sh/capacity-type"),
                                           {"spot", "on-demand", "reserved"}) for row in rows)}


def nodeclass_inventory(document):
    rows = items(document)
    return {"count": len(rows), "ready": count(ready_condition(row) for row in rows),
            "ami_families": count(choice(row.get("spec", {}).get("amiFamily"),
                                         {"AL2", "AL2023", "Bottlerocket", "Windows2019", "Windows2022", "Custom"}) for row in rows),
            "imds_token_settings": count(choice(row.get("spec", {}).get("metadataOptions", {}).get("httpTokens"),
                                                {"required", "optional"}) for row in rows),
            "imds_hop_limits": [integer(row.get("spec", {}).get("metadataOptions", {}).get("httpPutResponseHopLimit")) for row in rows],
            "imds_reachability_verified": False}


def run_command(command, env, timeout):
    """Use a process group so timed-out kubectl credential plugins stop too."""
    with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          stdin=subprocess.DEVNULL, env=env, text=True,
                          start_new_session=True) as process:
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.communicate()
            raise
        return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


def error_kind(stderr):
    text = stderr.lower()
    if "sso" in text and ("expired" in text or "refresh" in text or "token" in text):
        return "sso_session_unavailable"
    if "forbidden" in text or "accessdenied" in text or "access denied" in text:
        return "forbidden"
    if "unauthorized" in text or "must be logged in" in text or "unable to locate credentials" in text:
        return "credentials_unavailable"
    if "doesn't have a resource type" in text or "could not find the requested resource" in text:
        return "resource_api_unavailable"
    if "timed out" in text or "timeout" in text or "deadline exceeded" in text:
        return "timeout"
    if "could not connect" in text or "unable to connect" in text or "no such host" in text:
        return "connection_failed"
    return "command_failed"


class Collector:
    def __init__(self, profile, region, cluster, aws_bin="aws", kubectl_bin="kubectl", timeout=20, runner=run_command):
        if not 1 <= timeout <= 120:
            raise ValueError("timeout must be between 1 and 120 seconds")
        self.profile, self.region, self.cluster = profile, region, cluster
        # Resolve the credential plugin binary before kubectl runs it.
        self.aws_bin = shutil.which(aws_bin) or str(Path(aws_bin).absolute())
        self.kubectl_bin = shutil.which(kubectl_bin) or str(Path(kubectl_bin).absolute())
        self.timeout, self.runner = timeout, runner
        self.env = dict(os.environ, AWS_PAGER="", AWS_CLI_AUTO_PROMPT="off",
                        AWS_PROFILE=profile, AWS_REGION=region, AWS_DEFAULT_REGION=region)
        self.report = {
            "schema_version": 1, "started_at": now(),
            "target": {"profile": profile, "region": region, "cluster": cluster},
            "execution_enabled": False, "qualification_status": "not_assessed",
            "inventory_status": "unavailable", "limits": LIMITS.copy(), "checks": [],
        }

    def check(self, name, command, project=lambda document: document, json_output=True, allow_denied=False):
        started = time.monotonic()
        row = {"name": name, "status": "unavailable"}
        self.report["checks"].append(row)
        try:
            response = self.runner(command, self.env, self.timeout)
            denied = allow_denied and response.returncode == 1 and response.stdout.strip() == "no"
            if response.returncode and not denied:
                row["error"] = {"kind": error_kind(response.stderr or ""), "exit_code": response.returncode}
                return None
            raw = json.loads(response.stdout) if json_output else response.stdout.strip()
            if json_output and not isinstance(raw, dict):
                raise ValueError("expected object")
            row["data"] = project(raw)
            row["status"] = "observed"
            return raw
        except FileNotFoundError:
            row["error"] = {"kind": "binary_not_found"}
        except subprocess.TimeoutExpired:
            row["error"] = {"kind": "timeout"}
        except (ValueError, TypeError, KeyError, AttributeError):
            row["error"] = {"kind": "invalid_response"}
        except OSError:
            row["error"] = {"kind": "local_command_error"}
        finally:
            row["elapsed_seconds"] = round(time.monotonic() - started, 3)
        return None

    def aws(self, *args):
        return [self.aws_bin, "--profile", self.profile, "--region", self.region,
                "--no-cli-pager", "--cli-connect-timeout", str(min(self.timeout, 10)),
                "--cli-read-timeout", str(self.timeout), *args, "--output", "json"]

    def finish(self):
        unavailable = [row["name"] for row in self.report["checks"] if row["status"] != "observed"]
        self.report["missing_checks"] = unavailable
        self.report["inventory_status"] = (
            "complete" if not unavailable else
            "partial" if any(row["status"] == "observed" for row in self.report["checks"]) else "unavailable"
        )
        self.report["finished_at"] = now()
        return self.report

    def collect(self):
        identity = self.check("aws_identity", self.aws("sts", "get-caller-identity"),
                              lambda value: {"identity_received": bool(re.fullmatch(r"\d{12}", str(value.get("Account", ""))))})
        cluster_response = self.check("eks_cluster", self.aws("eks", "describe-cluster", "--name", self.cluster),
                                      self.cluster_projection)
        if identity is None or cluster_response is None:
            self.report["checks"].append({"name": "kubernetes_inventory", "status": "not_attempted",
                                           "reason": "aws_identity_and_cluster_required"})
            return self.finish()
        cluster = cluster_response.get("cluster", {})
        arn = str(cluster.get("arn", "")).split(":", 5)
        match = (len(arn) == 6 and arn[0] == "arn" and arn[1] in {"aws", "aws-us-gov", "aws-cn"}
                 and arn[2] == "eks" and arn[3] == self.region
                 and re.fullmatch(r"\d{12}", str(identity.get("Account", ""))) is not None
                 and arn[4] == identity["Account"] and arn[5] == "cluster/" + self.cluster)
        self.report["checks"].append({"name": "target_identity_match",
                                       "status": "observed" if match else "unavailable",
                                       "data": {"account_region_cluster_match": match}})
        if not match:
            self.report["checks"].append({"name": "kubernetes_inventory", "status": "not_attempted",
                                           "reason": "target_identity_mismatch"})
            return self.finish()
        try:
            endpoint = cluster["endpoint"]
            parsed = urlparse(endpoint)
            ca_data = cluster["certificateAuthority"]["data"]
            if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password:
                raise ValueError("invalid cluster endpoint")
            if not base64.b64decode(ca_data, validate=True):
                raise ValueError("invalid certificate authority")
        except (KeyError, TypeError, ValueError):
            self.report["checks"].append({"name": "kubernetes_inventory", "status": "not_attempted",
                                           "reason": "invalid_cluster_connection_metadata"})
            return self.finish()
        with tempfile.TemporaryDirectory(prefix="recall-ledger-preflight-") as temp:
            os.chmod(temp, 0o700)
            kubeconfig = Path(temp) / "kubeconfig.json"
            config = {
                "apiVersion": "v1", "kind": "Config", "current-context": "preflight",
                "clusters": [{"name": "preflight", "cluster": {"server": endpoint, "certificate-authority-data": ca_data}}],
                "contexts": [{"name": "preflight", "context": {"cluster": "preflight", "user": "preflight"}}],
                "users": [{"name": "preflight", "user": {"exec": {
                    # AWS CLI get-token emits v1beta1 ExecCredential responses.
                    "apiVersion": "client.authentication.k8s.io/v1beta1", "interactiveMode": "Never",
                    "command": self.aws_bin, "args": self.aws("eks", "get-token", "--cluster-name", self.cluster)[1:],
                }}}],
            }
            with os.fdopen(os.open(kubeconfig, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as handle:
                json.dump(config, handle)
            self.env["KUBECONFIG"] = str(kubeconfig)
            base = [self.kubectl_bin, "--kubeconfig", str(kubeconfig), "--context", "preflight",
                    "--cache-dir", str(Path(temp) / "cache"), "--request-timeout", str(self.timeout) + "s"]
            self.check("kubernetes_version", base + ["version", "-o", "json"],
                       lambda value: {key: version(value.get(key, {}).get("gitVersion"))
                                      for key in ("clientVersion", "serverVersion")})
            checks = [
                ("nodes", "nodes", [], node_inventory),
                ("pods", "pods", ["--all-namespaces"], pod_inventory),
                ("runtime_classes", "runtimeclasses.node.k8s.io", [], runtime_inventory),
                ("storage_classes", "storageclasses.storage.k8s.io", [], storage_inventory),
                ("csi_drivers", "csidrivers.storage.k8s.io", [], csi_inventory),
                ("system_daemonsets", "daemonsets.apps", ["--namespace", "kube-system"], component_inventory),
                ("system_deployments", "deployments.apps", ["--namespace", "kube-system"], component_inventory),
                ("karpenter_deployments", "deployments.apps", ["--all-namespaces", "--selector", "app.kubernetes.io/name=karpenter"], component_inventory),
                ("karpenter_nodepools", "nodepools.karpenter.sh", [], karpenter_inventory),
                ("karpenter_nodeclaims", "nodeclaims.karpenter.sh", [], nodeclaim_inventory),
                ("karpenter_nodeclasses", "ec2nodeclasses.karpenter.k8s.aws", [], nodeclass_inventory),
                ("network_policies", "networkpolicies.networking.k8s.io", ["--all-namespaces"], network_inventory),
                ("namespaces", "namespaces", [], namespace_inventory),
            ]
            for name, resource, options, project in checks:
                self.check(name, base + ["get", resource, *options, "-o", "json"], project)
            self.check("caller_can_list_nodes", base + ["auth", "can-i", "list", "nodes"],
                       lambda value: {"allowed": {"yes": True, "no": False}[value]},
                       json_output=False, allow_denied=True)
        return self.finish()

    @staticmethod
    def cluster_projection(response):
        cluster = response["cluster"]
        vpc = cluster.get("resourcesVpcConfig", {})
        return {
            "status": choice(cluster.get("status"), {"CREATING", "ACTIVE", "DELETING", "FAILED", "UPDATING", "PENDING"}),
            "version": version(cluster.get("version")),
            "authentication_mode": choice(cluster.get("accessConfig", {}).get("authenticationMode"),
                                          {"API", "CONFIG_MAP", "API_AND_CONFIG_MAP"}),
            "public_endpoint": vpc.get("endpointPublicAccess") is True,
            "private_endpoint": vpc.get("endpointPrivateAccess") is True,
            "subnet_count": len(vpc.get("subnetIds", [])),
            "public_cidr_count": len(vpc.get("publicAccessCidrs", [])),
            "health_issue_count": len(cluster.get("health", {}).get("issues", [])),
        }


def write_report(path, report):
    output = Path(path).expanduser().absolute()
    protected = [Path.home() / ".kube" / "config"]
    protected.extend(Path(value).expanduser() for value in os.environ.get("KUBECONFIG", "").split(os.pathsep) if value)
    if any(output.resolve() == value.resolve() for value in protected):
        raise ValueError("refusing to overwrite an existing kubeconfig location")
    output.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=".preflight-", dir=output.parent)
    try:
        with os.fdopen(descriptor, "w") as handle:
            os.chmod(temporary, 0o600)
            json.dump(report, handle, indent=2, ensure_ascii=False)
            handle.write("\n")
        os.replace(temporary, output)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    for field in ("profile", "region", "cluster", "output"):
        parser.add_argument("--" + field, required=True)
    parser.add_argument("--aws-bin", default="aws")
    parser.add_argument("--kubectl-bin", default="kubectl")
    parser.add_argument("--timeout", type=int, choices=range(1, 121), default=20, metavar="1..120")
    args = parser.parse_args(argv)
    report = Collector(args.profile, args.region, args.cluster, args.aws_bin, args.kubectl_bin, args.timeout).collect()
    try:
        write_report(args.output, report)
    except (OSError, ValueError):
        print("Could not write the local report safely; choose a writable non-kubeconfig output path.", file=sys.stderr)
        return 2
    print("Inventory %s; qualification not assessed; benchmark execution remains disabled." % report["inventory_status"])
    return 0 if report["inventory_status"] == "complete" else 1


if __name__ == "__main__":
    sys.exit(main())
