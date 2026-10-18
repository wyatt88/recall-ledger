import importlib.util
from contextlib import closing
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools" / "host_memory_probe.py"
SDK = ROOT / "sdk" / "python"
spec = importlib.util.spec_from_file_location("host_memory_probe", SCRIPT)
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class ProbeUnitTests(unittest.TestCase):
    def test_failed_check_is_retained(self):
        report = {"checks": []}
        with self.assertRaisesRegex(probe.ProbeError, "pending_became_memory"):
            probe.check(report, "pending_became_memory", False)
        self.assertEqual(report["checks"], [{"name": "pending_became_memory", "status": "fail"}])

    def test_unavailable_hook_is_not_a_successful_empty_recall(self):
        with self.assertRaisesRegex(probe.ProbeError, "did_not_return_memory_packet"):
            probe.packet({"hookSpecificOutput": {"additionalContext": "Memory unavailable"}})

    def test_packet_reads_only_structured_retrieval_items(self):
        output = {"hookSpecificOutput": {"additionalContext": 'untrusted data\n{"items":[{"body":"canary"}]}'}}
        self.assertEqual(probe.bodies(output), ["canary"])

    def test_sdk_fingerprint_changes_with_python_source(self):
        with tempfile.TemporaryDirectory() as temp:
            package = Path(temp) / "recall_ledger"
            package.mkdir()
            (package / "__init__.py").write_text("first")
            before = probe.sdk_fingerprint(temp)
            (package / "__init__.py").write_text("second")
            changed = probe.sdk_fingerprint(temp)
            self.assertNotEqual(changed, before)
            (package / "__pycache__").mkdir()
            (package / "__pycache__" / "ignored.pyc").write_bytes(b"bytecode")
            self.assertEqual(probe.sdk_fingerprint(temp), changed)

    def test_initial_refuses_existing_directory_without_modification(self):
        with tempfile.TemporaryDirectory(prefix="rl-hpu-", dir="/tmp") as temp:
            root = Path(temp)
            sentinel = root / "keep"
            sentinel.write_text("untouched")
            with self.assertRaises(FileExistsError):
                probe.run_probe(binary=SCRIPT, sdk_root=SDK, work_dir=root,
                                phase="initial", adapter="all")
            self.assertEqual(sentinel.read_text(), "untouched")
            self.assertFalse((root / "probe.lock").exists())

    def test_resume_refuses_changed_fingerprint_before_daemon_start(self):
        with tempfile.TemporaryDirectory(prefix="rl-hpu-", dir="/tmp") as temp:
            root = Path(temp)
            (root / "fixture-state.json").write_text(json.dumps({
                "schema_version": probe.SCHEMA, "fingerprints": {}, "adapters": [],
            }))
            with self.assertRaisesRegex(probe.ProbeError, "fingerprint_mismatch"):
                probe.run_probe(binary=SCRIPT, sdk_root=SDK, work_dir=root,
                                phase="resume", adapter="all")
            self.assertFalse((root / "a0").exists())

    def test_missing_resume_state_fails_without_a_database(self):
        with tempfile.TemporaryDirectory(prefix="rl-hpu-", dir="/tmp") as temp:
            root = Path(temp)
            with self.assertRaisesRegex(probe.ProbeError, "manifest_missing"):
                probe.run_probe(binary=SCRIPT, sdk_root=SDK, work_dir=root,
                                phase="resume", adapter="all")
            self.assertFalse(list(root.rglob("memory.db")))


@unittest.skipUnless(os.environ.get("RECALL_LEDGER_BIN") and sys.version_info >= (3, 11),
                     "requires RECALL_LEDGER_BIN and Python 3.11+; do not report skipped as qualified")
class RealDaemonProbeTests(unittest.TestCase):
    def invoke(self, root, phase, adapter="all"):
        env = dict(os.environ, PYTHONDONTWRITEBYTECODE="1")
        command = [
            sys.executable, "-B", str(SCRIPT), "--binary", os.environ["RECALL_LEDGER_BIN"],
            "--sdk-root", str(SDK), "--work-dir", str(root), "--phase", phase, "--adapter", adapter,
        ]
        result = subprocess.run(command, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                text=True, timeout=90, check=False)
        self.assertEqual(result.stderr, "", result.stderr)
        return result.returncode, json.loads(result.stdout)

    def test_all_adapters_initial_and_persisted_resume(self):
        with tempfile.TemporaryDirectory(prefix="rl-hpi-", dir="/tmp") as temp:
            root = Path(temp) / "run"
            code, initial = self.invoke(root, "initial")
            self.assertEqual(code, 0, initial)
            self.assertEqual(initial["status"], "pass", initial)
            self.assertEqual(len(initial["adapters"]), 3)
            self.assertTrue(initial["durable_manifest_written"])
            state = json.loads((root / "fixture-state.json").read_text())
            self.assertEqual(len({r["binding"]["history_id"] for r in state["adapters"]}), 3)
            self.assertEqual(len({r["stable_body"] for r in state["adapters"]}), 3)
            self.assertEqual(len({r["namespace"] for r in state["adapters"]}), 3)
            code, resumed = self.invoke(root, "resume")
            self.assertEqual(code, 0, resumed)
            self.assertEqual(initial["run_id"], resumed["run_id"])
            for output in (initial, resumed):
                self.assertEqual(output["test_kind"], "adapter_fixture")
                self.assertFalse(output["live_host_executed"])
                self.assertFalse(output["native_memory_replacement_verified"])
                self.assertEqual(output["replacement_status"], "blocked_autonomous_admission")
                self.assertEqual(output["model_provider_requests_by_probe"], 0)
                self.assertTrue(output["cleanup"]["all_daemons_stopped"])
                for adapter in output["adapters"]:
                    self.assertTrue(adapter["checks"])
                    self.assertTrue(all(c["status"] == "pass" for c in adapter["checks"]), adapter)
            self.assertEqual(json.loads((root / "fixture-state.json").read_text()), state)

    def test_resume_never_silently_recreates_missing_database(self):
        with tempfile.TemporaryDirectory(prefix="rl-hpi-", dir="/tmp") as temp:
            root = Path(temp) / "run"
            code, initial = self.invoke(root, "initial", "claude-code")
            self.assertEqual(code, 0, initial)
            database = root / "a0" / "memory.db"
            database.unlink()
            code, resumed = self.invoke(root, "resume", "claude-code")
            self.assertEqual(code, 1, resumed)
            self.assertEqual(resumed["adapters"][0]["error"]["code"], "resume_database_missing_or_symlink")
            self.assertFalse(database.exists())
            self.assertTrue(resumed["cleanup"]["all_daemons_stopped"])

    def test_damaged_or_foreign_resume_database_is_unchanged(self):
        cases = {
            "zero": "resume_database_empty_or_invalid_header",
            "truncated": "resume_database_empty_or_invalid_header",
            "schema": "resume_database_schema_mismatch",
            "history": "resume_database_history_mismatch",
        }
        for case, expected in cases.items():
            with self.subTest(case=case), tempfile.TemporaryDirectory(prefix="rl-hpi-", dir="/tmp") as temp:
                root = Path(temp) / "run"
                code, initial = self.invoke(root, "initial", "claude-code")
                self.assertEqual(code, 0, initial)
                database = root / "a0" / "memory.db"
                if case == "zero":
                    database.write_bytes(b"")
                elif case == "truncated":
                    database.write_bytes(b"SQLite format 3\0" + b"\0" * 40)
                else:
                    with closing(sqlite3.connect(database)) as db, db:
                        if case == "schema":
                            db.execute("PRAGMA user_version=0")
                        else:
                            db.execute("UPDATE meta SET value='foreign-history' WHERE key='store_id'")
                before = database.read_bytes()
                log = (root / "a0" / "daemon.log").read_bytes()
                code, resumed = self.invoke(root, "resume", "claude-code")
                self.assertEqual(code, 1, resumed)
                self.assertEqual(resumed["adapters"][0]["error"]["code"], expected)
                self.assertEqual(database.read_bytes(), before)
                self.assertEqual((root / "a0" / "daemon.log").read_bytes(), log)

    def test_original_pending_must_survive_before_read_hooks_can_create_more(self):
        for adapter in ("claude-code", "chatgpt"):
            with self.subTest(adapter=adapter), tempfile.TemporaryDirectory(prefix="rl-hpi-", dir="/tmp") as temp:
                root = Path(temp) / "run"
                code, initial = self.invoke(root, "initial", adapter)
                self.assertEqual(code, 0, initial)
                database = root / "a0" / "memory.db"
                with closing(sqlite3.connect(database)) as db, db:
                    count = db.execute("SELECT count(*) FROM ingestion_candidates WHERE state='pending'").fetchone()[0]
                    self.assertGreater(count, 0)
                    db.execute("UPDATE ingestion_candidates SET state='rejected',body=NULL WHERE state='pending'")
                before = database.read_bytes()
                code, resumed = self.invoke(root, "resume", adapter)
                self.assertEqual(code, 1, resumed)
                self.assertEqual(resumed["adapters"][0]["error"]["code"],
                                 "resume_original_pending_candidate_changed")
                self.assertEqual(database.read_bytes(), before)
                with closing(sqlite3.connect(database)) as db, db:
                    self.assertEqual(db.execute(
                        "SELECT count(*) FROM ingestion_candidates WHERE state='pending'").fetchone()[0], 0)


if __name__ == "__main__":
    unittest.main()
