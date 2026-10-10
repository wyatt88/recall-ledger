import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch

from recall_ledger import Client, MemoryError, TransportError
from recall_ledger import host_runtime as host


class FakeClient:
    def __init__(self):
        self.expected = {"namespace": "fixture", "history_id": "history"}
        self.actual = dict(self.expected)
        self.timeout = 2
        self.calls = []
        self.revision = 1
        self.body = "Rust canonical memory"

    def binding(self):
        self.calls.append(("binding", {}))
        if self.actual != self.expected:
            raise MemoryError("binding_mismatch", "wrong store", False)
        return dict(self.actual)

    def call(self, method, params=None):
        if self.actual != self.expected:
            raise MemoryError("binding_mismatch", "wrong store", False)
        self.calls.append((method, copy.deepcopy(params)))
        if method == "candidate_submit":
            return {"candidate_id": params["event_id"], "state": "pending"}
        if method == "recall":
            items = [{"id": "record-1", "revision": self.revision, "body": self.body, "trust_boundary": "retrieved_data"}]
            return {
                "items": items, "coverage": {"mode": "lexical"},
                "manifest": {
                    "schema_version": "recall-ledger.context/1", "namespace": "fixture",
                    "commit_token": "history:2", "records": [{"id": "record-1", "revision": self.revision}],
                },
            }
        if method == "export":
            return {"records": [], "commit_token": "history:2"}
        raise AssertionError(method)

    def candidate_list(self, **kwargs):
        return {"candidates": [], "total": 0, "truncated": False}

    def validate_context(self, manifest):
        self.calls.append(("validate_context", manifest))
        return {"valid": manifest["records"][0]["revision"] == self.revision}


def fixture_config(root, *, host_name="codex"):
    return {
        "schema": host.SCHEMA, "host": host_name, "project": str(root),
        "state": str(root / "state"), "socket": str(root / "s"),
        "installation_id": "fixture-install",
        "expected_binding": {"namespace": "fixture", "history_id": "history"},
        "memory_mode": "persistent", "timeout": 2, "max_output_bytes": 4096,
        "max_chars": 1800, "capture_chars": 4000,
    }


class RuntimeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rl-runtime-")
        self.root = Path(self.temp.name).resolve()
        self.config = fixture_config(self.root)
        host.install_files(self.config["state"], self.config, {})
        self.fake = FakeClient()
        self.runtime = host.Runtime(self.config, self.fake)

    def tearDown(self):
        self.temp.cleanup()

    def capture(self, event="native-1", body="Candidate body", mode="persistent"):
        return self.runtime.capture(event_id=event, session_id="session", agent_id="root", body=body, origin="user_message", memory_mode=mode)

    def test_injected_real_client_requires_matching_per_request_binding(self):
        expected = {"namespace": "fixture", "history_id": "00000000-0000-0000-0000-000000000001"}
        config = dict(self.config, expected_binding=expected)
        with self.assertRaisesRegex(host.HostError, "client_binding_mismatch"):
            host.Runtime(config, Client(config["socket"]))
        with self.assertRaisesRegex(host.HostError, "client_binding_mismatch"):
            host.Runtime(config, Client(config["socket"], expected_binding=dict(expected, namespace="other")))
        pinned = Client(config["socket"], expected_binding=expected)
        self.assertIs(host.Runtime(config, pinned).client, pinned)

    def test_privacy_precedes_every_queue_and_daemon_action(self):
        before = {str(p): p.read_bytes() for p in Path(self.config["state"]).rglob("*") if p.is_file()}
        self.assertFalse(self.capture(mode="incognito")["captured"])
        self.assertFalse(self.capture(mode="temporary")["captured"])
        self.runtime.flush(memory_mode="incognito")
        self.runtime.read(query="private", session_id="s", agent_id="a", memory_mode="temporary")
        self.runtime.refresh(query="private", session_id="s", agent_id="a", memory_mode="temporary", manifest={"untrusted": True})
        after = {str(p): p.read_bytes() for p in Path(self.config["state"]).rglob("*") if p.is_file()}
        self.assertEqual(before, after)
        self.assertEqual(self.fake.calls, [])
        self.runtime.read(query="read-only", session_id="s", agent_id="a", memory_mode="incognito")
        self.assertFalse((Path(self.config["state"]) / "sessions").exists())
        self.assertEqual(self.fake.calls[-1][0], "recall")

    def test_stable_event_conflicts_and_same_text_different_events(self):
        first = self.capture()
        self.assertEqual(first["event_id"], self.capture()["event_id"])
        reinstalled = host.Runtime(dict(self.config, installation_id="another-install"), self.fake)
        self.assertEqual(first["event_id"], reinstalled.event_key("native-1", "session", "root"))
        self.assertNotEqual(first["event_id"], self.capture("native-2")["event_id"])
        with self.assertRaisesRegex(host.HostError, "event_conflict"):
            self.capture(body="Changed same event")
        self.runtime.flush(memory_mode="persistent")
        submitted = [params for method, params in self.fake.calls if method == "candidate_submit"]
        self.assertEqual(len(submitted), 2)
        self.assertTrue(all(item["origin"] == "user_message" for item in submitted))
        self.assertFalse(any(method == "candidate_resolve" for method, _ in self.fake.calls))

    def test_concurrent_install_identity_check_includes_gateway_member_and_store(self):
        state = self.root / "gateway-state"
        original = dict(self.config, state=str(state), gateway_binding={"host_instance_id": "host", "member_id": "alice", "store_id": "store"})
        host.install_files(state, original, {})
        different = dict(original, gateway_binding={"host_instance_id": "host", "member_id": "bob", "store_id": "store"})
        with self.assertRaisesRegex(host.HostError, "installation_binding_changed"):
            host.install_files(state, different, {})
        self.assertEqual(host.read_json(state / "config.json")["gateway_binding"]["member_id"], "alice")

    def test_permanent_bad_candidate_is_retained_without_blocking_later_candidates(self):
        self.capture("first")
        self.capture("second")
        original = self.fake.call
        rejected = []

        def call(method, params=None):
            if method == "candidate_submit" and not rejected:
                rejected.append(True)
                raise MemoryError("invalid_request", "synthetic rejected candidate", False)
            return original(method, params)

        self.fake.call = call
        result = self.runtime.flush(memory_mode="persistent")
        self.assertEqual((result["delivered"], result["quarantined"], result["queued"]), (1, 1, 0))
        retained = list((self.runtime.state / "dead-letter").glob("*.json"))
        self.assertEqual(host.read_json(retained[0])["params"]["body"], "Candidate body")
        self.assertEqual(host.read_json(retained[0])["error"]["code"], "invalid_request")

    def test_unknown_method_and_retryable_errors_preserve_queue(self):
        self.capture()
        for code, retryable in (("unknown_method", False), ("busy", True)):
            with patch.object(self.fake, "call", side_effect=MemoryError(code, "synthetic", retryable)):
                result = self.runtime.flush(memory_mode="persistent")
            self.assertEqual(result["queued"], 1)
            self.assertEqual(result["quarantined"], 0)

    def test_wrong_binding_prevents_new_capture_and_old_queue_delivery(self):
        self.capture()
        before = {str(p): p.read_bytes() for p in Path(self.config["state"]).rglob("*") if p.is_file()}
        self.fake.actual["namespace"] = "other"
        with self.assertRaises(MemoryError):
            self.capture("new")
        with self.assertRaises(MemoryError):
            self.runtime.flush(memory_mode="persistent")
        after = {str(p): p.read_bytes() for p in Path(self.config["state"]).rglob("*") if p.is_file()}
        self.assertEqual(before, after)
        self.assertFalse(any(method == "candidate_submit" for method, _ in self.fake.calls))

    def test_refresh_reads_even_when_previous_context_is_valid_and_never_caches_bodies(self):
        first = self.runtime.read(query="Rust", session_id="s", agent_id="a", memory_mode="persistent")
        second = self.runtime.refresh(query="Rust", session_id="s", agent_id="a", memory_mode="persistent", manifest=first["manifest"])
        self.assertTrue(second["previous_context_valid"])
        self.assertEqual(sum(m == "recall" for m, _ in self.fake.calls), 2)
        saved = host.read_bytes(self.runtime.session_path("s", "a"))
        self.assertNotIn(self.fake.body.encode(), saved)
        self.fake.revision = 2
        self.assertFalse(self.runtime.refresh(query="", session_id="s", agent_id="a", memory_mode="persistent", manifest=first["manifest"])["previous_context_valid"])

    def test_output_budget_removes_whole_records_and_keeps_manifest_consistent(self):
        self.fake.body = "记忆🧠" * 10000
        packet = self.runtime.read(query="", session_id="s", agent_id="a", memory_mode="persistent")
        self.assertEqual(packet["items"], [])
        self.assertEqual(packet["manifest"]["records"], [])
        self.assertEqual(packet["adapter_omitted"], 1)
        self.assertLess(len(host.encode(packet)), 4096)

    def test_multifile_install_and_uninstall_resume_after_interruption(self):
        state = self.root / "journal-state"
        config = dict(self.config, state=str(state))
        first, second = self.root / "a.json", self.root / "b.toml"
        first.write_bytes(b"original")
        original_write = host.atomic_write
        interrupted = []

        def fail_second(path, data):
            if Path(path) == second and not interrupted:
                interrupted.append(True)
                raise OSError("fixture interruption")
            return original_write(path, data)

        with patch.object(host, "atomic_write", side_effect=fail_second):
            with self.assertRaises(OSError):
                host.install_files(state, config, {first: b"new", second: b"new"}, expected_before={first: b"original", second: None})
        self.assertEqual(host.read_json(state / "installation.json")["phase"], "installing")
        self.assertTrue(host.install_files(state, config, {})["resumed"])
        interrupted.clear()

        def fail_finalize(path, data):
            if Path(path) == state / "installation.json" and json.loads(data).get("phase") == "uninstalled" and not interrupted:
                interrupted.append(True)
                raise OSError("fixture interruption")
            return original_write(path, data)

        with patch.object(host.Runtime, "load", return_value=self.runtime):
            with patch.object(host, "atomic_write", side_effect=fail_finalize):
                with self.assertRaises(OSError):
                    host.uninstall_files(state)
            result = host.uninstall_files(state)
        self.assertTrue(result["configuration_removed"])
        self.assertEqual(first.read_bytes(), b"original")
        self.assertFalse(second.exists())

    def test_new_patch_requires_original_snapshots(self):
        state = self.root / "no-snapshot-state"
        config = dict(self.config, state=str(state))
        target = self.root / "must-not-write"
        with self.assertRaisesRegex(host.HostError, "installation_snapshot_required"):
            host.install_files(state, config, {target: b"generated"})
        self.assertFalse(state.exists())
        self.assertFalse(target.exists())


class DaemonFixture:
    """Only used by opt-in synthetic subprocess tests, never a user's daemon."""

    def __init__(self, root, namespace="fixture", database="memory.db"):
        self.root, self.socket = root, root / "s"
        self.log = (root / (database + ".log")).open("wb")
        self.process = subprocess.Popen(
            [os.environ["RECALL_LEDGER_BIN"], "--db", str(root / database), "--namespace", namespace,
             "serve", "--socket", str(self.socket), "--no-worker"],
            stdout=self.log, stderr=self.log,
        )
        self.client = Client(str(self.socket), timeout=2)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise AssertionError("fixture daemon exited")
            try:
                self.client.binding()
                break
            except TransportError:
                time.sleep(0.02)
        else:
            raise AssertionError("fixture daemon startup timed out")

    def close(self):
        self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
        self.log.close()


@unittest.skipUnless(os.environ.get("RECALL_LEDGER_BIN"), "set RECALL_LEDGER_BIN for a built daemon")
class RuntimeDaemonTests(unittest.TestCase):
    def test_rebinding_socket_cannot_deliver_old_or_new_candidates(self):
        with tempfile.TemporaryDirectory(prefix="rl-hr-", dir="/tmp") as temp:
            root = Path(temp).resolve()
            daemon = DaemonFixture(root, "alpha")
            try:
                config = host.new_config("fixture", root, root / "state", daemon.socket)
                host.install_files(config["state"], config, {})
                runtime = host.Runtime(config)
                runtime.capture(event_id="event-1", session_id="session", agent_id="root", body="Alpha private proposal", origin="user_message", memory_mode="persistent")
            finally:
                daemon.close()
            daemon = DaemonFixture(root, "beta", "beta.db")
            try:
                with self.assertRaises(MemoryError) as failed:
                    runtime.flush(memory_mode="persistent")
                self.assertEqual(failed.exception.code, "binding_mismatch")
                with self.assertRaises(MemoryError):
                    runtime.capture(event_id="event-2", session_id="session", agent_id="root", body="Must not cross", origin="user_message", memory_mode="persistent")
                with self.assertRaises(MemoryError):
                    runtime.read(query="", session_id="session", agent_id="root", memory_mode="persistent")
                self.assertEqual(daemon.client.candidate_list()["total"], 0)
                self.assertEqual(len(list((root / "state" / "outbox").glob("*.json"))), 1)
            finally:
                daemon.close()


if __name__ == "__main__":
    unittest.main()
