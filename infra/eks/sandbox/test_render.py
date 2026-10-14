"""Offline manifest contracts and two-process IPC, not EKS acceptance tests."""

import base64
import gzip
import importlib.util
import json
from pathlib import Path
import re
import socket
import stat
import subprocess
import sys
import tempfile
import time
import unittest


HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("sandbox_manifest_renderer", HERE / "render.py")
renderer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(renderer)
VERSIONS = json.loads((HERE / "versions.json").read_text())
IMAGE = "public.ecr.aws/docker/library/python@sha256:" + "a" * 64


def bindings():
    return {
        "ami_id": "ami-0123456789abcdef0",
        "node_role": "SyntheticQualificationNodeRole",
        "subnet_id": "subnet-0123456789abcdef0",
        "security_group_ids": ["sg-0123456789abcdef0"],
        "zone": "us-east-1a",
        "image": IMAGE,
        "runsc_release": VERSIONS["gvisor"]["release"],
        "runsc_sha512": VERSIONS["gvisor"]["archive_sha512"],
    }


def by_kind(resources, kind):
    return [resource for resource in resources if resource["kind"] == kind]


def only(resources, kind):
    matches = by_kind(resources, kind)
    if len(matches) != 1:
        raise AssertionError(f"Expected one {kind}, found {len(matches)}")
    return matches[0]


class ManifestContractTests(unittest.TestCase):
    def setUp(self):
        self.foundation, self.initial, self.resume = renderer.render(bindings())

    def test_all_images_and_bootstrap_runtime_have_explicit_immutable_bindings(self):
        for job in [self.initial, self.resume]:
            pod = job["spec"]["template"]["spec"]
            for container in pod["containers"] + pod["initContainers"]:
                self.assertEqual(container["image"], IMAGE)
                self.assertRegex(container["image"], r"@sha256:[a-f0-9]{64}$")
        node_class = only(self.foundation, "EC2NodeClass")["spec"]
        self.assertEqual(node_class["amiSelectorTerms"], [{"id": bindings()["ami_id"]}])
        user_data = node_class["userData"]
        self.assertIn(f"RUNSC_RELEASE='{bindings()['runsc_release']}'", user_data)
        self.assertIn(f"RUNSC_SHA512='{bindings()['runsc_sha512']}'", user_data)
        archive = re.search(r"echo '([A-Za-z0-9+/=]+)' \| base64 -d \| gzip -d", user_data)
        self.assertIsNotNone(archive)
        compressed = base64.b64decode(archive.group(1))
        self.assertEqual(compressed[4:8], b"\0\0\0\0", "gzip must not embed render time")
        self.assertEqual(gzip.decompress(compressed), (HERE / "bootstrap.sh").read_bytes())
        self.assertLess(len(user_data.encode()), 14000)
        runtime = only(self.foundation, "RuntimeClass")
        self.assertEqual(runtime["handler"], VERSIONS["runtime"]["handler"])
        self.assertEqual(runtime["scheduling"]["nodeSelector"], {renderer.NODE_LABEL: renderer.NAME})

    def test_missing_mutable_or_invalid_bindings_are_rejected(self):
        cases = [
            ("image", "python:3.12"),
            ("image", "python@sha256:" + "f" * 63),
            ("runsc_release", "latest"),
            ("runsc_release", "../20261005.0"),
            ("runsc_sha512", "a" * 127),
            ("ami_id", "auto"),
            ("subnet_id", "subnet-all"),
            ("security_group_ids", ["default"]),
            ("zone", "us-east-1"),
        ]
        for key, value in cases:
            with self.subTest(key=key, value=value):
                values = bindings()
                values[key] = value
                with self.assertRaises(ValueError):
                    renderer.render(values)
        for key in bindings():
            values = bindings()
            del values[key]
            with self.subTest(missing=key), self.assertRaises(ValueError):
                renderer.render(values)

    def test_capacity_is_one_pinned_on_demand_instance_with_no_disruptive_consolidation(self):
        pool = only(self.foundation, "NodePool")["spec"]
        self.assertEqual(pool["limits"], {"cpu": "2", "memory": "8Gi"})
        template = pool["template"]
        requirements = {item["key"]: item for item in template["spec"]["requirements"]}
        for key, values in [
            ("node.kubernetes.io/instance-type", ["m7i.large"]),
            ("karpenter.sh/capacity-type", ["on-demand"]),
            ("kubernetes.io/arch", ["amd64"]),
            ("topology.kubernetes.io/zone", [bindings()["zone"]]),
        ]:
            self.assertEqual(requirements[key]["operator"], "In")
            self.assertEqual(requirements[key]["values"], values)
        self.assertEqual(template["metadata"]["labels"][renderer.NODE_LABEL], renderer.NAME)
        self.assertEqual(pool["disruption"]["budgets"], [{"nodes": "0"}])
        self.assertEqual(template["spec"]["expireAfter"], "2h")
        # This checks declarative capacity limits, not an absolute concurrency
        # guarantee for Karpenter's eventually consistent provisioning.

    def test_startup_taint_requires_external_qualification_before_worker_scheduling(self):
        pool = only(self.foundation, "NodePool")["spec"]["template"]["spec"]
        startup = pool["startupTaints"]
        self.assertEqual(startup, [{"key": "recall-ledger.io/unqualified", "value": "true", "effect": "NoSchedule"}])
        runtime = only(self.foundation, "RuntimeClass")
        tolerations = list(runtime["scheduling"]["tolerations"])
        for job in [self.initial, self.resume]:
            pod = job["spec"]["template"]["spec"]
            self.assertNotIn("nodeName", pod)
            tolerations.extend(pod.get("tolerations", []))
        for tolerance in tolerations:
            self.assertNotEqual(tolerance.get("key"), startup[0]["key"])
            self.assertFalse(tolerance.get("operator") == "Exists" and not tolerance.get("key"))
        # Worker resources contain no bootstrap auditor, RBAC grant, or service
        # account token with which the worker could remove this taint itself.
        self.assertFalse(by_kind(self.foundation, "ClusterRoleBinding"))
        self.assertFalse(by_kind(self.foundation, "RoleBinding"))

    def test_one_trial_per_node_and_resume_share_the_same_state_volume(self):
        for phase, job in [("initial", self.initial), ("resume", self.resume)]:
            self.assertEqual(job["metadata"]["name"], "probe-" + phase)
            pod = job["spec"]["template"]["spec"]
            labels = job["spec"]["template"]["metadata"]["labels"]
            self.assertIn(renderer.TRIAL_LABEL, labels)
            anti = pod["affinity"]["podAntiAffinity"]["requiredDuringSchedulingIgnoredDuringExecution"]
            self.assertTrue(any(
                item["topologyKey"] == "kubernetes.io/hostname"
                and item.get("namespaceSelector") == {}
                and {"key": renderer.TRIAL_LABEL, "operator": "Exists"}
                in item["labelSelector"]["matchExpressions"]
                for item in anti
            ))
            data = next(volume for volume in pod["volumes"] if volume["name"] == "data")
            self.assertEqual(data["persistentVolumeClaim"]["claimName"], "trial-data")
            command = " ".join(pod["containers"][0]["command"])
            self.assertIn(f"--phase {phase}", command)
            self.assertIn("peer.py check", command)
        pvc = only(self.foundation, "PersistentVolumeClaim")["spec"]
        self.assertEqual(pvc["accessModes"], ["ReadWriteOnce"])
        self.assertEqual(pvc["storageClassName"], "gp3")
        self.assertEqual(pvc["resources"]["requests"]["storage"], "8Gi")
        quota = only(self.foundation, "ResourceQuota")["spec"]["hard"]
        self.assertEqual(quota["persistentvolumeclaims"], "1")
        self.assertEqual(quota["count/jobs.batch"], "2")
        self.assertEqual(quota["pods"], "2")

    def test_restricted_workers_have_explicit_runtime_and_no_api_credentials(self):
        namespaces = {item["metadata"]["name"]: item for item in by_kind(self.foundation, "Namespace")}
        labels = namespaces[renderer.NAME]["metadata"]["labels"]
        self.assertEqual(labels["pod-security.kubernetes.io/enforce"], "restricted")
        self.assertEqual(labels["pod-security.kubernetes.io/enforce-version"], "v" + VERSIONS["target"]["kubernetes_minor"])
        self.assertEqual(labels["recall-ledger.io/qualification"], "true")
        self.assertFalse(only(self.foundation, "ServiceAccount")["automountServiceAccountToken"])
        for job in [self.initial, self.resume]:
            pod = job["spec"]["template"]["spec"]
            self.assertEqual(job["metadata"]["namespace"], renderer.NAME)
            self.assertEqual(pod["runtimeClassName"], renderer.NAME)
            self.assertFalse(pod["automountServiceAccountToken"])
            self.assertTrue(pod["securityContext"]["runAsNonRoot"])
            self.assertNotEqual(pod["securityContext"]["runAsUser"], 0)
            self.assertEqual(pod["securityContext"]["seccompProfile"]["type"], "RuntimeDefault")
            for field in ["hostNetwork", "hostPID", "hostIPC"]:
                self.assertFalse(pod.get(field, False))
            for volume in pod["volumes"]:
                self.assertNotIn("hostPath", volume)
                self.assertNotIn("secret", volume)
                self.assertNotIn("projected", volume)
            for container in pod["containers"] + pod["initContainers"]:
                context = container["securityContext"]
                self.assertFalse(context.get("privileged", False))
                self.assertFalse(context["allowPrivilegeEscalation"])
                self.assertTrue(context["readOnlyRootFilesystem"])
                self.assertEqual(context["capabilities"]["drop"], ["ALL"])
                self.assertNotIn("add", context["capabilities"])

    def test_admission_policy_fails_closed_and_rejects_unsandboxed_or_token_bearing_pods(self):
        policy = only(self.foundation, "ValidatingAdmissionPolicy")["spec"]
        self.assertEqual(policy["failurePolicy"], "Fail")
        constraints = policy["matchConstraints"]
        self.assertEqual(constraints["namespaceSelector"]["matchLabels"],
                         {"recall-ledger.io/qualification": "true"})
        self.assertEqual(constraints["resourceRules"], [
            {"apiGroups": [""], "apiVersions": ["v1"], "operations": ["CREATE", "UPDATE"], "resources": ["pods"]}
        ])
        expressions = [item["expression"] for item in policy["validations"]]
        self.assertIn(
            f"has(object.spec.runtimeClassName) && object.spec.runtimeClassName == '{renderer.NAME}'",
            expressions,
        )
        self.assertIn("has(object.spec.automountServiceAccountToken) && !object.spec.automountServiceAccountToken", expressions)
        self.assertIn(
            "!has(object.spec.volumes) || object.spec.volumes.all(v, !has(v.projected) || !has(v.projected.sources) || v.projected.sources.all(s, !has(s.serviceAccountToken)))",
            expressions,
        )
        binding = only(self.foundation, "ValidatingAdmissionPolicyBinding")["spec"]
        self.assertEqual(binding["policyName"], renderer.NAME)
        self.assertEqual(binding["validationActions"], ["Deny"])
        # These are required CEL expression contracts. Compilation and actual
        # negative admission probes still require the selected API server.

    def test_jobs_never_blindly_retry_and_have_native_sidecar_and_deadline(self):
        for job in [self.initial, self.resume]:
            spec = job["spec"]
            self.assertEqual(spec["backoffLimit"], 0)
            # Qualification includes EC2/bootstrap/PVC and external audit wait.
            # This deadline must not become the Agent task performance budget.
            self.assertEqual(spec["activeDeadlineSeconds"], 1800)
            pod = spec["template"]["spec"]
            self.assertEqual(pod["restartPolicy"], "Never")
            self.assertEqual(len(pod["initContainers"]), 1)
            sidecar = pod["initContainers"][0]
            self.assertEqual(sidecar["restartPolicy"], "Always")
            self.assertIn("startupProbe", sidecar)
            tmp = next(volume for volume in pod["volumes"] if volume["name"] == "tmp")
            self.assertEqual(tmp["emptyDir"], {"medium": "Memory", "sizeLimit": "128Mi"})
            for container in [sidecar] + pod["containers"]:
                self.assertEqual(container["resources"]["requests"], container["resources"]["limits"])
                self.assertEqual(next(m for m in container["volumeMounts"] if m["name"] == "tmp")["mountPath"], "/tmp")

    def test_cli_writes_private_files_and_refuses_to_overwrite(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            source = base / "bindings.json"
            source.write_text(json.dumps(bindings()))
            output = base / "private"
            args = [sys.executable, str(HERE / "render.py"), "--bindings", str(source), "--output", str(output)]
            process = subprocess.run(args, capture_output=True, text=True, timeout=5, check=False)
            self.assertEqual(process.returncode, 0, process.stderr)
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o700)
            before = {}
            for name in ["foundation.json", "initial.json", "resume.json"]:
                path = output / name
                self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
                before[name] = path.read_bytes()
                json.loads(before[name])
            second = subprocess.run(args, capture_output=True, text=True, timeout=5, check=False)
            self.assertNotEqual(second.returncode, 0)
            self.assertEqual({name: (output / name).read_bytes() for name in before}, before)


class PeerProcessTests(unittest.TestCase):
    def test_real_server_and_client_processes_exchange_tcp_and_private_unix_messages(self):
        unix = Path("/tmp/recall-peer.sock")
        if unix.exists() or unix.is_symlink():
            self.skipTest("existing peer socket belongs to another run; never unlink it")
        with socket.socket() as port_check:
            try:
                port_check.bind(("127.0.0.1", 17991))
            except OSError:
                self.skipTest("peer TCP port is in use; never stop its owner")
        server = subprocess.Popen([sys.executable, "-B", str(HERE / "peer.py"), "serve"],
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        socket_identity = None
        try:
            deadline = time.monotonic() + 5
            last_failure = ""
            while time.monotonic() < deadline:
                if server.poll() is not None:
                    self.fail("peer server exited: " + server.stderr.read().decode("utf-8", errors="replace"))
                if unix.exists():
                    socket_identity = unix.stat().st_ino
                client = subprocess.run([sys.executable, "-B", str(HERE / "peer.py"), "check"],
                                        capture_output=True, text=True, timeout=7, check=False)
                if client.returncode == 0:
                    observation = json.loads(client.stdout)
                    self.assertEqual(observation["cross_container_tcp"], "pass")
                    self.assertEqual(observation["cross_container_unix_0600"], "pass")
                    self.assertTrue(stat.S_ISSOCK(unix.stat().st_mode))
                    self.assertEqual(stat.S_IMODE(unix.stat().st_mode), 0o600)
                    socket_identity = unix.stat().st_ino
                    payload = b"recall-sandbox-peer-v1"
                    for family, address in [
                        (socket.AF_INET, ("127.0.0.1", 17991)),
                        (socket.AF_UNIX, str(unix)),
                    ]:
                        with self.subTest(transport=family):
                            with socket.socket(family, socket.SOCK_STREAM) as fragmented:
                                fragmented.settimeout(3)
                                if family == socket.AF_INET:
                                    fragmented.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                                fragmented.connect(address)
                                # Separate arrivals make a single recv() fail
                                # this wire-protocol test on both transports.
                                for offset in range(0, len(payload), 4):
                                    fragmented.sendall(payload[offset:offset + 4])
                                    time.sleep(0.02)
                                echo = b""
                                while len(echo) < len(payload):
                                    part = fragmented.recv(len(payload) - len(echo))
                                    if not part:
                                        break
                                    echo += part
                                self.assertEqual(echo, payload)
                            with socket.socket(family, socket.SOCK_STREAM) as incomplete:
                                incomplete.settimeout(3)
                                incomplete.connect(address)
                                incomplete.sendall(payload[:3])
                                incomplete.shutdown(socket.SHUT_WR)
                                self.assertEqual(incomplete.recv(128), b"")
                    after_short_message = subprocess.run(
                        [sys.executable, "-B", str(HERE / "peer.py"), "check"],
                        capture_output=True, text=True, timeout=7, check=False,
                    )
                    self.assertEqual(after_short_message.returncode, 0, after_short_message.stderr)
                    break
                last_failure = client.stderr
                time.sleep(0.05)
            else:
                self.fail("peer did not become ready: " + last_failure)
        finally:
            if server.poll() is None:
                server.terminate()
                try:
                    server.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=2)
            server.communicate()
            if unix.exists() and socket_identity is not None and unix.stat().st_ino == socket_identity:
                unix.unlink()
        # This proves actual two-process IPC locally. The peer's JSON labels do
        # not turn this test into proof of cross-container networking in EKS.


if __name__ == "__main__":
    unittest.main()
