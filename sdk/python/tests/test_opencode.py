"""OpenCode protocol fixtures and an installed JS/Python/real-daemon chain."""
import copy
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import time
import unittest
import venv
from unittest.mock import patch

from recall_ledger import Client, MemoryError, TransportError
from recall_ledger import opencode as adapter


class FakeClient:
    def __init__(self):
        self.identity = {"namespace": "fixture", "history_id": "history"}
        self.calls = []
        self.fail = False
        self.items = [{"id": "fact", "revision": 1, "body": "accepted canonical text"}]
        self.token = "history:1"

    def binding(self):
        return copy.deepcopy(self.identity)

    def call(self, method, params):
        self.calls.append((method, copy.deepcopy(params)))
        if self.fail:
            raise TransportError("response lost", outcome_unknown=True)
        if method == "candidate_submit":
            return {"candidate_id": params["event_id"], "state": "pending"}
        if method == "recall":
            return {"items": self.items, "manifest": {"records": [
                {"id": item["id"], "revision": item["revision"]} for item in self.items
            ]}, "commit_token": self.token}
        raise AssertionError(method)


class OpenCodeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rl-oc-")
        self.root = Path(self.temp.name).resolve()
        self.project = self.root / "project with spaces"
        self.project.mkdir()
        self.fake = FakeClient()
        self.client_patch = patch.object(adapter, "Client", return_value=self.fake)
        self.client_patch.start()
        self.addCleanup(self.client_patch.stop)
        self.addCleanup(self.temp.cleanup)
        self.install()

    def install(self):
        result = adapter.install(str(self.project), str(self.root / "socket"))
        self.config_path = result["config"]
        self.managed = Path(self.config_path).parent
        self.entry = Path(result["plugin"])

    def request(self, action="capture", **values):
        params = {"action": action, "session_id": "s1", "message_id": "u1",
                  "role": "user", "body": "proposed text", **values}
        return adapter.bridge(self.config_path, params, self.fake)

    def test_install_and_uninstall_preserve_native_files_and_other_plugins(self):
        native = self.project / "AGENTS.md"
        native.write_text("Human controlled")
        config = self.project / "opencode.jsonc"
        config.write_text('// existing comment\n{"plugin":["other"]}')
        other = self.entry.parent / "other.js"
        other.write_text("export const Other = async () => ({})")
        adapter.uninstall(str(self.project))
        self.assertFalse(self.entry.exists())
        self.assertFalse(adapter.load_config(self.config_path)["enabled"])
        self.assertEqual(native.read_text(), "Human controlled")
        self.assertEqual(config.read_text(), '// existing comment\n{"plugin":["other"]}')
        self.assertTrue(other.exists())
        self.assertEqual(self.managed.stat().st_mode & 0o777, 0o700)
        self.install()
        self.assertTrue(self.entry.exists())

    def test_interrupted_install_is_recoverable_but_user_edit_is_not_overwritten(self):
        self.entry.unlink()
        value = adapter.load_config(self.config_path)
        value["phase"] = "prepared"
        adapter.atomic_json(Path(self.config_path), value)
        self.install()
        self.assertEqual(adapter.load_config(self.config_path)["phase"], "installed")
        self.entry.write_text("user edit")
        with self.assertRaises(ValueError):
            self.install()
        with self.assertRaises(ValueError):
            adapter.uninstall(str(self.project))
        self.assertEqual(self.entry.read_text(), "user edit")

    def test_all_capture_is_pending_with_stable_message_identity(self):
        self.request()
        self.request()
        self.request(message_id="a1", role="assistant", body="model proposal")
        submits = [p for m, p in self.fake.calls if m == "candidate_submit"]
        self.assertEqual(len(submits), 2)
        self.assertEqual({p["origin"] for p in submits}, {"user_message", "model_inference"})
        self.assertFalse(any(m in ("remember", "candidate_resolve") for m, _ in self.fake.calls))
        receipts = "".join(p.read_text() for p in (self.managed / "receipts").glob("*.json"))
        self.assertNotIn("proposed text", receipts)
        self.assertNotIn("model proposal", receipts)
        with self.assertRaises(ValueError):
            self.request(body="same ID different message")

    def test_lost_receipt_retains_exact_body_key_until_success(self):
        self.fake.fail = True
        first = self.request()
        self.assertEqual(first["delivery"]["queued"], 1)
        queued = next((self.managed / "outbox").glob("*.json"))
        original = queued.read_text()
        self.request()
        self.assertEqual(queued.read_text(), original)
        self.fake.fail = False
        self.request("flush")
        self.assertFalse(queued.exists())
        submits = [p for m, p in self.fake.calls if m == "candidate_submit"]
        self.assertEqual(submits[0], submits[-1])

    def test_initial_offline_capture_is_queued_under_original_binding(self):
        with patch.object(self.fake, "binding", side_effect=TransportError("offline", outcome_unknown=False)):
            first = self.request(body="saved during outage")
            self.assertEqual(first["delivery"]["error"], "transport_error")
            self.assertEqual(first["delivery"]["queued"], 1)
            queued = next((self.managed / "outbox").glob("*.json"))
            original = queued.read_bytes()
            self.request(body="saved during outage")
            self.assertEqual(queued.read_bytes(), original)
            self.assertEqual(json.loads(original)["binding"], self.fake.identity)
            self.assertEqual(self.fake.calls, [])
        self.request("flush")
        self.assertFalse(queued.exists())
        submits = [p for m, p in self.fake.calls if m == "candidate_submit"]
        self.assertEqual(len(submits), 1)
        self.assertEqual(submits[0]["body"], "saved during outage")

    def test_concurrent_first_install_cannot_replace_winning_binding(self):
        project = self.root / "concurrent"
        project.mkdir()
        first_entered, release_first, second_started = (threading.Event() for _ in range(3))
        second_handshake = threading.Event()
        identities = {name: {"namespace": name, "history_id": "history"} for name in ("first", "second")}

        class InstallerClient:
            def __init__(self, socket_path, *, expected_binding=None, **kwargs):
                self.name = Path(socket_path).name
                self.expected = expected_binding

            def binding(self):
                if self.name == "first":
                    first_entered.set()
                    if not release_first.wait(timeout=5):
                        raise AssertionError("first handshake never released")
                else:
                    second_handshake.set()
                identity = identities[self.name]
                if self.expected and self.expected != identity:
                    raise MemoryError("binding_mismatch", "different concurrent target")
                return identity

        def second_install():
            second_started.set()
            return adapter.install(str(project), str(self.root / "second"))

        with patch.object(adapter, "Client", InstallerClient), ThreadPoolExecutor(max_workers=2) as pool:
            first = pool.submit(adapter.install, str(project), str(self.root / "first"))
            self.assertTrue(first_entered.wait(timeout=5))
            second = pool.submit(second_install)
            self.assertTrue(second_started.wait(timeout=5))
            try:
                # Keep the first handshake blocked while the other install runs.
                self.assertFalse(second_handshake.wait(timeout=0.1))
            finally:
                release_first.set()
            winning = first.result(timeout=5)
            with self.assertRaises(MemoryError) as raised:
                second.result(timeout=5)
            self.assertEqual(raised.exception.code, "binding_mismatch")
        saved = adapter.load_config(winning["config"])
        self.assertEqual(saved["binding"], identities["first"])
        self.assertEqual(saved["phase"], "installed")

    def test_binding_change_prevents_recall_or_queue_delivery_and_reinstall(self):
        self.fake.fail = True
        self.request()
        self.fake.fail = False
        self.fake.identity["namespace"] = "wrong"
        count = len(self.fake.calls)
        with self.assertRaises(MemoryError) as raised:
            self.request("recall")
        self.assertEqual(raised.exception.code, "binding_mismatch")
        self.assertEqual(len(self.fake.calls), count)
        self.assertEqual(len(list((self.managed / "outbox").glob("*.json"))), 1)
        with self.assertRaises(MemoryError):
            self.request(message_id="new", body="must not be added to old queue")
        self.assertEqual(len(list((self.managed / "outbox").glob("*.json"))), 1)
        with self.assertRaises(ValueError):
            self.install()

    def test_manifest_is_per_session_and_never_caches_bodies(self):
        self.request("recall", query="canonical")
        self.fake.token = "history:3"
        self.request("recall", session_id="s2", query="other")
        self.request("recall", query="canonical")
        reads = [p for m, p in self.fake.calls if m == "recall"]
        self.assertNotIn("minimum_commit", reads[1])
        self.assertEqual(reads[2]["minimum_commit"], "history:1")
        states = list((self.managed / "sessions").glob("*.json"))
        self.assertEqual(len(states), 2)
        self.assertTrue(all("accepted canonical text" not in p.read_text() for p in states))
        self.fake.items = []
        self.assertEqual(json.loads(self.request("recall")["context"].split("\n", 1)[1])["items"], [])

    def test_disabled_is_checked_before_capture(self):
        adapter.uninstall(str(self.project))
        self.assertTrue(self.request(body="never queue")["disabled"])
        self.assertFalse(list((self.managed / "outbox").glob("*.json")))

    def test_validation_and_size_boundaries(self):
        with self.assertRaises(ValueError):
            self.request(role="tool")
        with self.assertRaises(ValueError):
            self.request(session_id="")
        self.assertEqual(self.request(body="x" * 12001)["skipped"], "text_exceeds_capture_limit")
        self.assertEqual(self.request(body="   ")["skipped"], "empty_text")
        with self.assertRaises(ValueError):
            self.request("accept")
        with self.assertRaises(ValueError):
            adapter.install(str(self.project), str(self.root / "s"), timeout=float("nan"))


@unittest.skipUnless(os.environ.get("RECALL_LEDGER_BIN"), "set RECALL_LEDGER_BIN for real Rust daemon tests")
class OpenCodeRealDaemonTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rl-oc-real-", dir="/tmp")
        self.root = Path(self.temp.name).resolve()
        self.project = self.root / "project"
        self.project.mkdir()
        self.socket = self.root / "s"
        self.binary = os.environ["RECALL_LEDGER_BIN"]
        self.log = (self.root / "log").open("wb")
        self.start("expected")
        self.client = Client(str(self.socket), timeout=2)
        installed = adapter.install(str(self.project), str(self.socket))
        self.config = installed["config"]

    def start(self, namespace):
        self.process = subprocess.Popen([
            self.binary, "--db", str(self.root / "data.db"), "--namespace", namespace,
            "serve", "--socket", str(self.socket), "--no-worker",
        ], stdout=self.log, stderr=self.log)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                Client(str(self.socket), timeout=0.2).binding()
                return
            except (TransportError, MemoryError):
                if self.process.poll() is not None:
                    self.fail((self.root / "log").read_text())
                time.sleep(0.025)
        self.fail("daemon did not start")

    def stop(self):
        self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)

    def tearDown(self):
        self.stop()
        self.log.close()
        self.temp.cleanup()

    def run_plugin(self, action, body="canonical", session="session-1"):
        node = shutil.which("node")
        if not node:
            self.skipTest("Node.js required for actual installed JavaScript smoke test")
        smoke = Path(__file__).resolve().parents[3] / "integrations" / "opencode" / "tests" / "host-smoke.mjs"
        result = subprocess.run([node, str(smoke)], input=json.dumps({
            "project": str(self.project), "session": session, "body": body, "action": action,
        }), text=True, capture_output=True, timeout=40)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_installed_js_bridge_capture_review_revision_and_retract(self):
        self.run_plugin("capture", "canonical user proposal")
        self.run_plugin("capture", "canonical user proposal")
        pending = self.client.candidate_list(state="pending")
        self.assertEqual(pending["total"], 2)
        self.assertEqual({c["origin"] for c in pending["candidates"]}, {"user_message", "model_inference"})
        self.assertEqual(self.client.recall("canonical", mode="lexical")["items"], [])
        user_candidate = next(c["candidate_id"] for c in pending["candidates"] if c["origin"] == "user_message")
        accepted = self.client.candidate_resolve(user_candidate, "accept", idempotency_key="approve")
        one = self.run_plugin("recall")
        self.assertIn("canonical user proposal", str(one))
        record = accepted["record_id"]
        self.client.revise(record, expected_revision=1, body="canonical updated preference", idempotency_key="revise")
        two = self.run_plugin("recall")
        self.assertNotIn("canonical user proposal", str(two))
        self.assertIn("canonical updated preference", str(two))
        self.client.retract(record, expected_revision=2, idempotency_key="retract")
        three = self.run_plugin("recall")
        self.assertNotIn("canonical updated preference", str(three))
        self.run_plugin("capture", "canonical user proposal")
        self.assertEqual(self.client.candidate_list(state="pending")["total"], 1)
        states = Path(self.config).parent / "sessions"
        self.assertTrue(all("canonical" not in p.read_text() for p in states.glob("*.json")))

    def test_same_socket_rebind_cannot_inject_or_write_new_namespace(self):
        # Queue under the old binding, then reconnect the same endpoint elsewhere.
        config = adapter.load_config(self.config)
        managed = Path(self.config).parent
        adapter.capture(config, managed, {"session_id": "queued", "message_id": "q1",
                        "role": "user", "body": "old namespace queued text"})
        self.stop()
        self.start("different")
        current = Client(str(self.socket), timeout=2)
        current.remember("canonical wrong namespace secret", idempotency_key="wrong")
        result = self.run_plugin("recall")
        self.assertIn("unavailable", str(result))
        self.assertNotIn("wrong namespace secret", str(result))
        self.assertEqual(current.candidate_list()["total"], 0)
        self.assertEqual(len(list((managed / "outbox").glob("*.json"))), 1)
        with self.assertRaises(MemoryError):
            adapter.install(str(self.project), str(self.socket))

    def test_installed_js_queues_while_daemon_is_down_then_delivers_same_events(self):
        self.stop()
        self.run_plugin("capture", "canonical offline proposal")
        managed = Path(self.config).parent
        queued = list((managed / "outbox").glob("*.json"))
        self.assertEqual(len(queued), 2)
        original = {p.name: json.loads(p.read_text())["params"] for p in queued}
        self.run_plugin("capture", "canonical offline proposal")
        self.assertEqual({p.name: json.loads(p.read_text())["params"]
                          for p in (managed / "outbox").glob("*.json")}, original)
        self.start("expected")
        delivered = adapter.bridge(self.config, {"action": "flush"})
        self.assertEqual(delivered["delivery"]["delivered"], 2)
        self.assertEqual(self.client.candidate_list(state="pending")["total"], 2)
        self.assertEqual(self.client.recall("canonical", mode="lexical")["items"], [])

    def test_injected_sdk_client_must_assert_the_installed_binding(self):
        expected = self.client.binding()
        unpinned = Client(str(self.socket), timeout=2)
        wrong = Client(str(self.socket), timeout=2,
                       expected_binding={**expected, "namespace": "not-the-installed-namespace"})
        for supplied in (unpinned, wrong):
            with patch.object(supplied, "binding", side_effect=AssertionError("must reject before any IPC")):
                for action in ("capture", "recall"):
                    with self.assertRaises(MemoryError) as raised:
                        adapter.bridge(self.config, {
                            "action": action, "session_id": "s", "message_id": "m",
                            "role": "user", "body": "must not be queued", "query": "canonical",
                        }, client=supplied)
                    self.assertEqual(raised.exception.code, "client_binding_mismatch")
        self.assertFalse(list((Path(self.config).parent / "outbox").glob("*.json")))
        pinned = Client(str(self.socket), timeout=2, expected_binding=expected)
        result = adapter.bridge(self.config, {
            "action": "capture", "session_id": "s", "message_id": "good",
            "role": "user", "body": "properly pinned candidate",
        }, client=pinned)
        self.assertEqual(result["delivery"]["delivered"], 1)
        self.assertEqual(self.client.candidate_list(state="pending")["total"], 1)

    @unittest.skipUnless(os.environ.get("RECALL_LEDGER_WHEEL"), "set RECALL_LEDGER_WHEEL for isolated wheel install")
    def test_wheel_installs_plugin_outside_checkout_and_runs_actual_bridge(self):
        wheel = Path(os.environ["RECALL_LEDGER_WHEEL"]).resolve(strict=True)
        environment = self.root / "venv"
        venv.EnvBuilder(with_pip=True).create(environment)
        python = environment / "bin" / "python"
        clean_env = {key: value for key, value in os.environ.items()
                     if key not in ("PYTHONPATH", "PYTHONHOME")}

        def run(arguments):
            completed = subprocess.run(arguments, cwd=self.root, env=clean_env,
                                       capture_output=True, text=True, timeout=60)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            return completed.stdout

        run([str(python), "-m", "pip", "install", "--no-index", "--no-deps", str(wheel)])
        asset = json.loads(run([str(python), "-c", (
            "import json; from recall_ledger import opencode; "
            "print(json.dumps({'module': opencode.__file__, "
            "'asset': str(opencode.source_path()), 'exists': opencode.source_path().is_file()}))"
        )]))
        self.assertTrue(Path(asset["module"]).is_relative_to(environment))
        self.assertTrue(Path(asset["asset"]).is_relative_to(environment))
        self.assertTrue(asset["exists"])

        self.project = self.root / "wheel-only-project"
        self.project.mkdir()
        (self.project / "AGENTS.md").write_text("Human-controlled native file")
        command = environment / "bin" / "recall-ledger-opencode"
        installed = json.loads(run([str(command), "install", "--project", str(self.project),
                                    "--socket", str(self.socket)]))
        self.assertTrue(installed["ok"])
        self.config = installed["result"]["config"]
        saved = adapter.load_config(self.config)
        self.assertTrue(Path(saved["sdk_path"]).is_relative_to(environment))
        self.assertEqual(saved["python"], str(python))
        self.run_plugin("capture", "canonical packaged runtime proposal")
        self.assertEqual(self.client.candidate_list(state="pending")["total"], 2)
        user_candidate = next(row["candidate_id"] for row in self.client.candidate_list()["candidates"]
                              if row["origin"] == "user_message")
        self.client.candidate_resolve(user_candidate, "accept", idempotency_key="wheel-admit")
        self.assertIn("canonical packaged runtime proposal", str(self.run_plugin("recall")))
        removed = json.loads(run([str(command), "uninstall", "--project", str(self.project)]))
        self.assertTrue(removed["ok"])
        self.assertFalse((self.project / ".opencode" / "plugins" / "recall-ledger.js").exists())
        self.assertEqual((self.project / "AGENTS.md").read_text(), "Human-controlled native file")


if __name__ == "__main__":
    unittest.main()
