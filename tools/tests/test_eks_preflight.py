import base64
import importlib.util
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("eks_preflight", Path(__file__).parents[1] / "eks_preflight.py")
preflight = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(preflight)


def fixture_cluster(account="123456789012"):
    return {"cluster": {
        "arn": "arn:aws:eks:us-east-1:" + account + ":cluster/test-cluster",
        "status": "ACTIVE", "version": "1.35",
        "endpoint": "https://private-cluster.example.test",
        "certificateAuthority": {"data": base64.b64encode(b"test-only-ca").decode()},
        "resourcesVpcConfig": {"subnetIds": ["subnet-sensitive"], "endpointPrivateAccess": True},
    }}


class FakeRunner:
    def __init__(self, failures=None, overrides=None):
        self.calls = []
        self.failures = failures or {}
        self.overrides = overrides or {}
        self.temporary_paths = []

    def __call__(self, command, env, timeout):
        self.calls.append(command)
        if "get-caller-identity" in command:
            key, result = "sts", {"Account": "123456789012", "Arn": "arn:sensitive", "UserId": "sensitive"}
        elif "describe-cluster" in command:
            key, result = "cluster", fixture_cluster()
        else:
            path = Path(command[command.index("--kubeconfig") + 1])
            self.temporary_paths.append(path)
            assert env["KUBECONFIG"] == str(path)
            assert stat.S_IMODE(path.stat().st_mode) == 0o600
            assert stat.S_IMODE(path.parent.stat().st_mode) == 0o700
            config = json.loads(path.read_text())
            assert config["users"][0]["user"]["exec"]["interactiveMode"] == "Never"
            assert config["users"][0]["user"]["exec"]["apiVersion"] == "client.authentication.k8s.io/v1beta1"
            assert "get-token" in config["users"][0]["user"]["exec"]["args"]
            assert "--profile" in config["users"][0]["user"]["exec"]["args"]
            assert "--region" in config["users"][0]["user"]["exec"]["args"]
            if "get" in command:
                key = command[command.index("get") + 1]
                result = {"items": []}
            elif "version" in command:
                key, result = "version", {"clientVersion": {"gitVersion": "v1.35.0"}, "serverVersion": {"gitVersion": "v1.35.8-eks-build"}}
            else:
                key, result = "can-i", "yes"
        if key in self.failures:
            failure = self.failures[key]
            if isinstance(failure, Exception):
                raise failure
            return subprocess.CompletedProcess(command, 1, "", failure)
        result = self.overrides.get(key, result)
        stdout = result if isinstance(result, str) else json.dumps(result)
        return subprocess.CompletedProcess(command, 0, stdout, "")


def collect(runner):
    return preflight.Collector("test-profile", "us-east-1", "test-cluster", runner=runner).collect()


def check(report, name):
    return next(row for row in report["checks"] if row["name"] == name)


class PreflightTests(unittest.TestCase):
    def test_commands_are_read_only_and_private_configuration_is_cleaned(self):
        runner = FakeRunner()
        with tempfile.TemporaryDirectory() as temp:
            home = Path(temp)
            config = home / ".kube" / "config"
            config.parent.mkdir()
            config.write_text("existing-user-context")
            alternate = home / "alternate-config"
            alternate.write_text("alternate-user-context")
            with patch.dict(os.environ, {"HOME": str(home), "KUBECONFIG": str(alternate)}):
                report = collect(runner)
            self.assertEqual(config.read_text(), "existing-user-context")
            self.assertEqual(alternate.read_text(), "alternate-user-context")
        for command in runner.calls:
            if "get-caller-identity" in command or "describe-cluster" in command:
                continue
            # Options have fixed positions; only these three kubectl operations occur.
            operation = command[command.index("--request-timeout") + 2]
            self.assertIn(operation, {"get", "version", "auth"})
            self.assertNotIn("secrets", command)
            self.assertNotIn("apply", command)
            self.assertNotIn("create", command)
            self.assertNotIn("exec", command)
            self.assertNotIn("logs", command)
        self.assertTrue(runner.temporary_paths)
        self.assertTrue(all(not path.exists() for path in runner.temporary_paths))
        self.assertEqual(report["inventory_status"], "complete")
        self.assertFalse(report["execution_enabled"])
        self.assertEqual(report["qualification_status"], "not_assessed")
        self.assertFalse(check(report, "runtime_classes")["data"]["actual_runtime_execution_verified"])

    def test_account_region_or_cluster_mismatch_prevents_all_kubectl_calls(self):
        for arn in ("arn:aws:eks:us-east-1:000000000000:cluster/test-cluster",
                    "arn:aws:eks:us-west-2:123456789012:cluster/test-cluster",
                    "arn:aws:eks:us-east-1:123456789012:cluster/wrong"):
            with self.subTest(arn=arn):
                cluster = fixture_cluster()
                cluster["cluster"]["arn"] = arn
                runner = FakeRunner(overrides={"cluster": cluster})
                report = collect(runner)
                self.assertEqual(len(runner.calls), 2)
                self.assertEqual(check(report, "kubernetes_inventory")["reason"], "target_identity_mismatch")
                self.assertFalse(report["execution_enabled"])

    def test_sso_expired_is_reported_without_stderr_or_kubernetes_queries(self):
        runner = FakeRunner(failures={"sts": "SSO token has expired, refresh failed arn:private-secret"})
        report = collect(runner)
        self.assertEqual(check(report, "aws_identity")["error"]["kind"], "sso_session_unavailable")
        self.assertEqual(len(runner.calls), 2)
        self.assertNotIn("arn:private-secret", json.dumps(report))
        self.assertEqual(check(report, "kubernetes_inventory")["status"], "not_attempted")

    def test_forbidden_missing_crd_timeout_and_bad_json_are_not_empty_inventory(self):
        runner = FakeRunner(failures={
            "nodes": "Forbidden: user arn:private cannot list nodes 10.0.2.3",
            "nodepools.karpenter.sh": "the server doesn't have a resource type nodepools",
            "pods": subprocess.TimeoutExpired("sensitive command", 20),
        }, overrides={"namespaces": "{bad-json", "csidrivers.storage.k8s.io": {}})
        report = collect(runner)
        for name, kind in [("nodes", "forbidden"), ("karpenter_nodepools", "resource_api_unavailable"),
                           ("pods", "timeout"), ("namespaces", "invalid_response"), ("csi_drivers", "invalid_response")]:
            row = check(report, name)
            self.assertEqual(row["status"], "unavailable")
            self.assertEqual(row["error"]["kind"], kind)
            self.assertNotIn("data", row)
            self.assertIn(name, report["missing_checks"])
        self.assertEqual(check(report, "runtime_classes")["data"]["count"], 0)
        self.assertNotIn("10.0.2.3", json.dumps(report))
        self.assertEqual(report["inventory_status"], "partial")

    def test_allowlist_redaction_removes_objects_addresses_annotations_and_env(self):
        sensitive = "do-not-publish-this"
        node = {
            "metadata": {"name": "ip-10-0-2-3.ec2.internal", "annotations": {"anything": sensitive},
                         "labels": {"untrusted": sensitive, "node.kubernetes.io/instance-type": "m7i.xlarge"}},
            "status": {"addresses": [{"address": "10.0.2.3"}],
                       "nodeInfo": {"kubeletVersion": "v1.35.8", "containerRuntimeVersion": "containerd://2.2.8",
                                    "architecture": "amd64", "osImage": "Bottlerocket OS 1.60", "operatingSystem": "linux"},
                       "capacity": {"cpu": "4", "memory": "8Gi"},
                       "allocatable": {"cpu": "3920m", "memory": "7Gi"}},
        }
        daemonset = {
            "metadata": {"name": "aws-node", "annotations": {"private": sensitive}},
            "spec": {"template": {"spec": {"containers": [{
                "name": "aws-node", "image": "123456789012.dkr.ecr.us-east-1.amazonaws.com/private:v1.20.0-eksbuild.1",
                "env": [{"name": "TOKEN", "value": sensitive}, {"name": "ENABLE_NETWORK_POLICY", "value": "true"}],
            }, {"name": "aws-eks-nodeagent", "image": "private-registry/private:" + sensitive}]}}},
        }
        runner = FakeRunner(overrides={"nodes": {"items": [node]}, "daemonsets.apps": {"items": [daemonset]}})
        report = collect(runner)
        rendered = json.dumps(report)
        for value in [sensitive, "123456789012", "arn:", "private-cluster.example.test", "subnet-sensitive",
                      "private-registry", "dkr.ecr", "ip-10-0-2-3", "10.0.2.3", "annotations", "TOKEN"]:
            self.assertNotIn(value, rendered)
        self.assertEqual(check(report, "nodes")["data"]["allocatable"]["cpu_cores"], "3.920")
        cni = check(report, "system_daemonsets")["data"]["recognized_components"][0]
        self.assertEqual(cni["network_policy_flag"], [True])
        self.assertTrue(cni["policy_agent_container_present"])
        self.assertFalse(cni["network_policy_enforcement_verified"])

    def test_scheduler_style_pod_requests_include_init_sidecars_and_overhead(self):
        pod = {"spec": {
            "nodeName": "private-node",
            "containers": [{"resources": {"requests": {"cpu": "500m", "memory": "1Gi"}}}],
            "initContainers": [
                {"restartPolicy": "Always", "resources": {"requests": {"cpu": "200m", "memory": "100Mi"}}},
                {"resources": {"requests": {"cpu": "2", "memory": "2Gi"}}},
            ],
            "overhead": {"cpu": "100m", "memory": "20Mi"},
        }, "status": {"phase": "Running"}}
        request = preflight.pod_request(pod)
        self.assertEqual(str(request["cpu"]), "2.300")
        self.assertEqual(request["memory"], preflight.quantity("2Gi") + preflight.quantity("120Mi"))
        finished = {"spec": pod["spec"], "status": {"phase": "Succeeded"}}
        inventory = preflight.pod_inventory({"items": [pod, finished]})
        self.assertEqual(inventory["active_assigned_count"], 1)
        self.assertEqual(inventory["active_assigned_spec_requests"]["cpu_cores"], "2.300")

    def test_cni_disable_arg_without_enable_env_is_preserved_and_other_args_removed(self):
        daemonset = {
            "metadata": {"name": "aws-node"},
            "spec": {"template": {"spec": {"containers": [
                {"name": "aws-node", "env": [{"name": "NETWORK_POLICY_ENFORCING_MODE", "value": "standard"},
                                           {"name": "PRIVATE_FLAG", "value": "do-not-publish"}]},
                {"name": "aws-eks-nodeagent", "args": ["--enable-network-policy=false",
                                                       "--private-endpoint=10.0.2.3"],
                 "env": [{"name": "PRIVATE_TOKEN", "value": "do-not-publish"}]},
            ]}}},
        }
        result = preflight.component_inventory({"items": [daemonset]})
        cni = result["recognized_components"][0]
        self.assertEqual(cni["network_policy_flag"], [])
        self.assertEqual(cni["node_agent_network_policy_args"], [False])
        self.assertEqual(cni["network_policy_enforcing_modes"], ["standard"])
        self.assertFalse(cni["network_policy_enforcement_verified"])
        self.assertNotIn("10.0.2.3", json.dumps(result))
        self.assertNotIn("do-not-publish", json.dumps(result))

    def test_invalid_quantity_does_not_create_fabricated_resource_totals(self):
        for status in [{}, {"capacity": {"cpu": "4", "memory": "8Gi"},
                           "allocatable": {"cpu": "invalid", "memory": "8Gi"}}]:
            runner = FakeRunner(overrides={"nodes": {"items": [{"status": status}]}})
            report = collect(runner)
            self.assertEqual(check(report, "nodes")["error"]["kind"], "invalid_response")
            self.assertNotIn("data", check(report, "nodes"))

    def test_auth_can_i_no_is_an_observed_denial_not_empty_or_failed_inventory(self):
        runner = FakeRunner()
        def denied(command, env, timeout):
            response = runner(command, env, timeout)
            if "can-i" in command:
                return subprocess.CompletedProcess(command, 1, "no\n", "")
            return response
        report = collect(denied)
        row = check(report, "caller_can_list_nodes")
        self.assertEqual(row["status"], "observed")
        self.assertEqual(row["data"], {"allowed": False})

    def test_auth_transport_error_is_unavailable_not_a_permission_denial(self):
        runner = FakeRunner(failures={"can-i": "Unable to connect: sensitive endpoint"})
        report = collect(runner)
        row = check(report, "caller_can_list_nodes")
        self.assertEqual(row["status"], "unavailable")
        self.assertEqual(row["error"]["kind"], "connection_failed")
        self.assertNotIn("data", row)

    def test_quantity_si_units_and_pod_level_overrides(self):
        self.assertEqual(preflight.quantity("1P"), 10 ** 15)
        self.assertEqual(preflight.quantity("1E"), 10 ** 18)
        self.assertEqual(preflight.quantity("1Pi"), 1024 ** 5)
        request = preflight.pod_request({"spec": {
            "containers": [{"resources": {"requests": {"cpu": "1"}}}],
            "resources": {"requests": {"cpu": "2"}},
            "overhead": {"cpu": "100m"},
        }})
        self.assertEqual(request["cpu"], preflight.quantity("2100m"))

    def test_output_is_atomic_private_and_cannot_overwrite_kubeconfig(self):
        with tempfile.TemporaryDirectory() as temp:
            home = Path(temp)
            with patch.dict(os.environ, {"HOME": str(home), "KUBECONFIG": str(home / "alternate")}):
                for path in [home / ".kube" / "config", home / "alternate"]:
                    with self.assertRaises(ValueError):
                        preflight.write_report(path, {"test": 1})
                output = home / "reports" / "inventory.json"
                preflight.write_report(output, {"test": 1})
                self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o600)
                self.assertEqual(json.loads(output.read_text()), {"test": 1})
                self.assertEqual([path.name for path in output.parent.iterdir()], ["inventory.json"])

    def test_invalid_endpoint_never_reaches_kubectl(self):
        cluster = fixture_cluster()
        cluster["cluster"]["endpoint"] = "http://private-cluster.example.test"
        runner = FakeRunner(overrides={"cluster": cluster})
        report = collect(runner)
        self.assertEqual(len(runner.calls), 2)
        self.assertEqual(check(report, "kubernetes_inventory")["reason"], "invalid_cluster_connection_metadata")


if __name__ == "__main__":
    unittest.main()
