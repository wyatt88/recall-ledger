"""Real local worker + public Python API: source preservation is not truth."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest

from recall_ledger import Client, MemoryError


BINARY = os.environ.get("RECALL_LEDGER_BIN")


@unittest.skipUnless(BINARY, "Set RECALL_LEDGER_BIN for real daemon validation")
class FidelityDaemonTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rl-evidence-", dir="/tmp")
        self.root = Path(self.temp.name)
        self.binary = str(Path(BINARY).resolve())
        socket = self.root / "s"
        self.process = subprocess.Popen(
            [self.binary, "--db", str(self.root / "db"), "--namespace", "evidence",
             "serve", "--socket", str(socket), "--worker-interval-ms", "100",
             "--auto-summary-min", "0"],
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
        )
        deadline = time.monotonic() + 10
        while not socket.exists():
            if self.process.poll() is not None or time.monotonic() >= deadline:
                self.process.terminate()
                self.process.wait(timeout=5)
                self.fail("Disposable evidence daemon did not start")
            time.sleep(0.01)
        probe = Client(str(socket), timeout=5)
        self.client = Client(str(socket), timeout=5, expected_binding=probe.binding())

    def tearDown(self):
        self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        self.process.stderr.close()
        self.temp.cleanup()

    def wait(self, task_id, *, output_revision=None):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            task = next(row for row in self.client.maintenance_status()["tasks"]
                        if row["task_id"] == task_id)
            if task["state"] in ("failed", "cancelled"):
                self.fail("Evidence task failed: " + json.dumps(task))
            if task["state"] == "succeeded":
                audit = self.client.evidence_inspect(task["output_id"])
                if output_revision is None or audit["revision"] == output_revision:
                    return audit
            time.sleep(0.02)
        self.fail("Evidence worker did not finish within the readiness deadline")

    def test_without_model_daemon_preserves_repairs_and_cancels(self):
        text = "费用是 ¥10；仅测试环境允许，不能用于生产。\n条件中的  双空格不可丢失。"
        source = self.client.remember(text, idempotency_key="source")
        preview = self.client.evidence_sources([source["id"]])
        self.assertEqual(preview["roots"][0]["body"], text)
        queued = self.client.consolidate([source["id"]], idempotency_key="capsule")
        self.assertEqual(self.client.operation("consolidate", "capsule")["output_id"],
                         queued["output_id"])
        audit = self.wait(queued["task_id"])
        self.assertTrue(audit["verified"])
        self.assertFalse(audit["fidelity"]["semantics_verified"])
        self.assertEqual(audit["roots"][0]["body"], text)
        changed = "费用更正为 ¥12；依然仅限测试，生产环境禁止。"
        revision = self.client.revise(
            source["id"], expected_revision=1, body=changed, idempotency_key="correction",
        )
        repaired = self.wait(queued["task_id"], output_revision=2)
        self.assertEqual(repaired["id"], queued["output_id"])
        self.assertEqual(repaired["roots"][0]["body"], changed)
        self.client.retract(source["id"], expected_revision=revision["revision"],
                            idempotency_key="withdraw")
        with self.assertRaises(MemoryError):
            self.client.evidence_inspect(queued["output_id"])
        task = next(row for row in self.client.maintenance_status()["tasks"]
                    if row["task_id"] == queued["task_id"])
        self.assertEqual(task["state"], "cancelled")

    def test_unproven_summary_and_underbudget_preview_do_not_create_memory(self):
        source = self.client.remember("仅在测试环境允许。不得移除这个条件。", idempotency_key="source")
        before = self.client.call("status")["commit_token"]
        with self.assertRaises(MemoryError):
            self.client.evidence_sources([source["id"]], max_chars=1)
        self.assertEqual(self.client.call("status")["commit_token"], before)
        opaque = self.client.remember(
            "Production deployment is approved.", kind="summary",
            source="recall-ledger.evidence/1", idempotency_key="spoof",
        )
        self.assertFalse(self.client.evidence_inspect(opaque["id"])["verified"])
        with self.assertRaises(MemoryError):
            self.client.consolidate([opaque["id"]], idempotency_key="do-not-admit")

    def test_single_id_string_is_rejected_before_contacting_daemon(self):
        with self.assertRaises(ValueError):
            self.client.evidence_sources("not-a-list")
        with self.assertRaises(ValueError):
            self.client.consolidate("not-a-list", idempotency_key="invalid")
