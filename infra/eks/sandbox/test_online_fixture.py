"""Local synthetic contracts only; no AWS, model, or cluster calls."""

import contextlib
import copy
import http.client
import importlib.util
import io
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import unittest
from unittest import mock


HERE = Path(__file__).resolve().parent


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


fixture = load("online_fixture_under_test", HERE / "online_fixture.py")
renderer = load("online_base_renderer_under_test", HERE / "render.py")
POD_UID = "b3743718-7205-4590-b924-cc9e48ba6c15"
NONCE = "synthetic-controller-nonce-123456"
GATEWAY = "10.55.0.20"


def base_manifest():
    versions = json.loads((HERE / "versions.json").read_text())
    return renderer.render({
        "ami_id": "ami-0123456789abcdef0",
        "node_role": "SyntheticQualificationNodeRole",
        "subnet_id": "subnet-0123456789abcdef0",
        "security_group_ids": ["sg-0123456789abcdef0"],
        "zone": "us-east-1a",
        "image": "public.ecr.aws/docker/library/python@sha256:" + "a" * 64,
        "runsc_release": versions["gvisor"]["release"],
        "runsc_sha512": versions["gvisor"]["archive_sha512"],
    })


@contextlib.contextmanager
def gate_file(path):
    real_open = os.open

    def mapped_open(name, flags, *args, **kwargs):
        if name != "/gate/ready":
            raise AssertionError(f"unexpected gate filesystem access: {name}")
        if flags & (os.O_WRONLY | os.O_RDWR | os.O_CREAT):
            raise AssertionError("gate must never write or create data")
        if not flags & os.O_NOFOLLOW or not flags & os.O_NONBLOCK:
            raise AssertionError("gate open must reject links and blocking special files")
        return real_open(path, flags, *args, **kwargs)

    with mock.patch.dict(os.environ, {"POD_UID": POD_UID}), \
            mock.patch.object(fixture.os, "open", side_effect=mapped_open), \
            mock.patch.object(fixture, "emit") as emitted:
        yield emitted


class RenderOnlineTests(unittest.TestCase):
    def test_preserves_restricted_resources_pvc_and_peer_and_adds_isolated_uid_nonce_gate(self):
        foundation, initial, _ = base_manifest()
        before = copy.deepcopy(initial)
        online = fixture.render_online(initial, GATEWAY, NONCE)
        self.assertEqual(initial, before, "render must not mutate the original")
        namespace = next(item for item in foundation if item["kind"] == "Namespace"
                         and item["metadata"]["name"] == initial["metadata"]["namespace"])
        self.assertEqual(namespace["metadata"]["labels"]["pod-security.kubernetes.io/enforce"], "restricted")
        self.assertEqual(online["metadata"]["namespace"], initial["metadata"]["namespace"])
        source = initial["spec"]["template"]["spec"]
        pod = online["spec"]["template"]["spec"]
        self.assertEqual(pod["securityContext"], source["securityContext"])
        self.assertFalse(pod["automountServiceAccountToken"])
        self.assertEqual(pod["runtimeClassName"], fixture.ONLINE_RUNTIME)
        for field in ["backoffLimit", "activeDeadlineSeconds"]:
            self.assertEqual(online["spec"][field], initial["spec"][field])
        self.assertEqual(pod["affinity"], source["affinity"])
        self.assertEqual(pod["volumes"][:-1], source["volumes"])
        self.assertEqual(pod["volumes"][-1], {"name": "gate", "emptyDir": {"sizeLimit": "1Mi"}})
        self.assertEqual(pod["initContainers"][1:], source["initContainers"])
        main = pod["containers"][0]
        for field in ["image", "resources", "securityContext", "volumeMounts"]:
            self.assertEqual(main[field], source["containers"][0][field])
        self.assertIn(GATEWAY, main["command"])
        gate = pod["initContainers"][0]
        self.assertEqual(gate["name"], "network-gate")
        self.assertNotIn("restartPolicy", gate)
        self.assertEqual(gate["image"], main["image"])
        self.assertIn(NONCE, gate["command"])
        self.assertEqual(gate["env"], [{
            "name": "POD_UID",
            "valueFrom": {"fieldRef": {"apiVersion": "v1", "fieldPath": "metadata.uid"}},
        }])
        self.assertEqual(gate["volumeMounts"], [
            {"name": "gate", "mountPath": "/gate", "readOnly": True},
            {"name": "probe", "mountPath": "/opt/probe", "readOnly": True},
        ])
        self.assertEqual(gate["securityContext"]["runAsUser"], 1000)
        self.assertTrue(gate["securityContext"]["readOnlyRootFilesystem"])
        self.assertFalse(gate["securityContext"]["allowPrivilegeEscalation"])
        self.assertEqual(gate["securityContext"]["capabilities"]["drop"], ["ALL"])
        for container in pod["containers"] + pod["initContainers"][1:]:
            self.assertFalse(any(mount["name"] == "gate" for mount in container["volumeMounts"]))
        annotations = online["spec"]["template"]["metadata"]["annotations"]
        for name, value in fixture.GATE_ANNOTATIONS.items():
            self.assertEqual(annotations[name], value)

    def test_rejects_unbound_gateway_nonce_live_object_and_duplicate_gate(self):
        _, initial, _ = base_manifest()
        for address in ["0.0.0.0", "169.254.169.254", "127.0.0.1", "224.0.0.1", "::1"]:
            with self.subTest(address=address), self.assertRaises(ValueError):
                fixture.render_online(initial, address, NONCE)
        for nonce in ["short", "x" * 129, "with spaces not permitted"]:
            with self.subTest(nonce=nonce), self.assertRaises(ValueError):
                fixture.render_online(initial, GATEWAY, nonce)
        live = copy.deepcopy(initial)
        live["metadata"]["uid"] = POD_UID
        with self.assertRaises(ValueError):
            fixture.render_online(live, GATEWAY, NONCE)
        online = fixture.render_online(initial, GATEWAY, NONCE)
        with self.assertRaises(ValueError):
            fixture.render_online(online, GATEWAY, NONCE)


class GateTests(unittest.TestCase):
    def test_exact_uid_and_nonce_are_required(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "ready"
            path.write_text(json.dumps({"pod_uid": POD_UID, "nonce": NONCE}))
            with gate_file(path) as emitted:
                self.assertEqual(fixture.wait_for_gate(NONCE), 0)
                self.assertIn("network_gate_released", [call.args[0] for call in emitted.call_args_list])
            cases = [
                {"pod_uid": "123b60a3-babe-4cf7-af4f-4414c5ce746d", "nonce": NONCE},
                {"pod_uid": POD_UID, "nonce": NONCE + "-stale"},
                {"pod_uid": POD_UID, "nonce": 123},
                {"pod_uid": POD_UID, "nonce": "非 ASCII"},
                [],
            ]
            for value in cases:
                path.write_text(json.dumps(value))
                with self.subTest(value=value), gate_file(path), self.assertRaises(ValueError):
                    fixture.wait_for_gate(NONCE)

    def test_missing_gate_expires_without_creating_gate_or_data(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "missing"
            clock = [0.0]

            def sleep(seconds):
                clock[0] += seconds

            with gate_file(path) as emitted, \
                    mock.patch.object(fixture, "GATE_TIMEOUT", 0.5), \
                    mock.patch.object(fixture.time, "monotonic", side_effect=lambda: clock[0]), \
                    mock.patch.object(fixture.time, "sleep", side_effect=sleep), \
                    mock.patch.object(fixture.os, "mkdir", side_effect=AssertionError("gate must not create data")):
                self.assertEqual(fixture.wait_for_gate(NONCE), 1)
            self.assertGreaterEqual(clock[0], 0.5)
            self.assertEqual(list(Path(directory).iterdir()), [])
            self.assertIn("network_gate_timeout", [call.args[0] for call in emitted.call_args_list])
            self.assertNotIn("network_gate_released", [call.args[0] for call in emitted.call_args_list])

    def test_rejects_symlink_and_oversized_gate(self):
        with tempfile.TemporaryDirectory() as directory:
            original = Path(directory) / "original"
            original.write_text(json.dumps({"pod_uid": POD_UID, "nonce": NONCE}))
            path = Path(directory) / "ready"
            path.symlink_to(original)
            with gate_file(path), self.assertRaises(OSError):
                fixture.wait_for_gate(NONCE)
            path.unlink()
            path.write_bytes(b"x" * 4097)
            with gate_file(path), self.assertRaises(ValueError):
                fixture.wait_for_gate(NONCE)


class CanaryTests(unittest.TestCase):
    def test_health_exact_body_and_mutations_connect_and_proxy_urls_never_forward(self):
        with mock.patch.object(fixture, "emit"), fixture.listeners("127.0.0.1", (0,)) as servers, socket.socket() as target:
            server_port = servers[0].server_port
            target.bind(("127.0.0.1", 0))
            target.listen(8)
            target.settimeout(0.1)
            target_address = f"127.0.0.1:{target.getsockname()[1]}"

            def request(method, path, body=None):
                connection = http.client.HTTPConnection("127.0.0.1", server_port, timeout=2)
                try:
                    connection.request(method, path, body=body)
                    response = connection.getresponse()
                    return response.status, dict(response.getheaders()), response.read()
                finally:
                    connection.close()

            status, headers, body = request("GET", "/healthz")
            self.assertEqual(status, 200)
            self.assertEqual(body, b'{"fixture":"recall-ledger-synthetic-canary-v1","synthetic":true}\n')
            self.assertEqual(int(headers["Content-Length"]), len(body))
            self.assertEqual(headers["Cache-Control"], "no-store")
            self.assertEqual(headers["Connection"], "close")
            fixture.check_health("127.0.0.1", server_port)
            for path in ["/healthz?query=1", "/", f"http://{target_address}/healthz"]:
                self.assertEqual(request("GET", path)[0], 404)
            for method, path in [("POST", f"http://{target_address}/healthz"),
                                 ("CONNECT", target_address), ("PUT", "/healthz")]:
                status, headers, body = request(method, path, b"synthetic")
                self.assertEqual(status, 405)
                self.assertEqual(headers["Allow"], "GET")
                self.assertEqual(body, b'{"error":"method_not_allowed"}\n')
            with self.assertRaises(socket.timeout):
                target.accept()

    def test_accepted_socket_has_real_timeout_and_stalled_client_cannot_block_forever(self):
        server = fixture.SyntheticServer(("127.0.0.1", 0), fixture.SyntheticHandler)
        try:
            with socket.create_connection(("127.0.0.1", server.server_port), timeout=1):
                accepted, _ = server.get_request()
                with accepted:
                    self.assertEqual(accepted.gettimeout(), 3.0)
                    with self.assertRaises(socket.timeout):
                        accepted.recv(1)
        finally:
            server.server_close()

    def test_health_validation_rejects_wrong_status_body_or_trailing_bytes(self):
        for code, payload in [
            (200, fixture.HEALTH_PAYLOAD + b" "),
            (200, fixture.HEALTH_PAYLOAD[:-1]),
            (200, b'{"synthetic":true}'),
            (503, fixture.HEALTH_PAYLOAD),
        ]:
            connection = mock.Mock()
            response = connection.getresponse.return_value
            response.status = code
            response.read.return_value = payload
            with self.subTest(status=code, payload=payload), \
                    mock.patch.object(fixture.http.client, "HTTPConnection", return_value=connection), \
                    self.assertRaises(ValueError):
                fixture.check_health("127.0.0.1", 18080)
            connection.close.assert_called_once()


class ProbeExitTests(unittest.TestCase):
    def test_run_check_rejects_missing_malformed_or_empty_evidence(self):
        for stdout in ["", "   ", "not JSON", '{"partial":', "[]", "[1]", "null", "true", "7", '""', "{}"]:
            completed = subprocess.CompletedProcess(["synthetic"], 0, stdout=stdout, stderr="")
            with self.subTest(stdout=stdout), \
                    mock.patch.object(fixture.subprocess, "run", return_value=completed), \
                    mock.patch.object(fixture, "emit") as emitted, \
                    contextlib.redirect_stdout(io.StringIO()) as printed:
                self.assertEqual(fixture.run_check("test", ["synthetic.py"], 2), 1)
            self.assertEqual(printed.getvalue(), "")
            emitted.assert_called_once_with("invalid_probe_output", check="test", status="failed")

    def test_run_check_keeps_original_failure_code_even_when_evidence_is_invalid(self):
        for stdout in ["", "not JSON", "[]", "{}"]:
            completed = subprocess.CompletedProcess(["synthetic"], 17, stdout=stdout, stderr="synthetic failure")
            with self.subTest(stdout=stdout), \
                    mock.patch.object(fixture.subprocess, "run", return_value=completed), \
                    mock.patch.object(fixture, "emit"):
                self.assertEqual(fixture.run_check("test", ["synthetic.py"], 2), 17)

    def test_run_check_emits_nonempty_object_and_preserves_success_or_failure(self):
        for code in [0, 23]:
            completed = subprocess.CompletedProcess(
                ["synthetic"], code, stdout='{"synthetic":true,"fixture":"test"}', stderr="x" * 2200,
            )
            with self.subTest(code=code), \
                    mock.patch.object(fixture.subprocess, "run", return_value=completed), \
                    mock.patch.object(fixture, "emit") as emitted, \
                    contextlib.redirect_stdout(io.StringIO()) as printed:
                self.assertEqual(fixture.run_check("test", ["synthetic.py"], 2), code)
            self.assertEqual(json.loads(printed.getvalue()), {"synthetic": True, "fixture": "test"})
            emitted.assert_called_once_with(
                "probe_command_completed", check="test", return_code=code, stderr="x" * 2000,
            )

    def test_sandbox_failure_remains_nonzero_after_audit_window_without_real_wait(self):
        with mock.patch.object(fixture, "run_check", side_effect=[0, 17]) as check, \
                mock.patch.object(fixture, "check_health") as health, \
                mock.patch.object(fixture, "listeners", return_value=contextlib.nullcontext([])), \
                mock.patch.object(fixture.time, "sleep") as sleep, \
                mock.patch.object(fixture, "emit") as emit:
            self.assertEqual(fixture.probe_main(GATEWAY), 17)
        self.assertEqual(check.call_count, 2)
        sandbox_args = check.call_args_list[1].args[1]
        self.assertIn(f"{GATEWAY}:18080", sandbox_args)
        self.assertIn(f"{GATEWAY}:18081", sandbox_args)
        self.assertIn("resume", sandbox_args)
        self.assertEqual(health.call_count, 2)
        sleep.assert_called_once_with(fixture.AUDIT_SECONDS)
        completed = next(call for call in emit.call_args_list if call.args[0] == "audit_window_completed")
        self.assertEqual(completed.kwargs["status"], "failed")
        self.assertEqual(completed.kwargs["sandbox_probe_return_code"], 17)

    def test_peer_failure_short_circuits_canary_and_audit(self):
        with mock.patch.object(fixture, "run_check", return_value=23) as check, \
                mock.patch.object(fixture, "check_health") as health, \
                mock.patch.object(fixture, "listeners") as listeners, \
                mock.patch.object(fixture.time, "sleep") as sleep:
            self.assertEqual(fixture.probe_main(GATEWAY), 23)
        check.assert_called_once()
        health.assert_not_called()
        listeners.assert_not_called()
        sleep.assert_not_called()


if __name__ == "__main__":
    unittest.main()
