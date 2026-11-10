import importlib.util
from contextlib import closing
import copy
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools" / "host_memory_probe.py"
SDK = ROOT / "sdk" / "python"
spec = importlib.util.spec_from_file_location("host_memory_probe", SCRIPT)
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class ProbeUnitTests(unittest.TestCase):
    def test_schema_normalization_preserves_quoted_contents(self):
        self.assertEqual(
            probe.normalized_schema_sql("  CREATE TABLE x(\n a TEXT  DEFAULT 'two  words')  "),
            "CREATE TABLE x( a TEXT DEFAULT 'two  words')",
        )
        for first, second in (
                ("SELECT 'two  words'", "SELECT 'two words'"),
                ('SELECT "two  words"', 'SELECT "two words"'),
                ("SELECT 'it''s  quoted'", "SELECT 'it''s quoted'")):
            self.assertNotEqual(probe.normalized_schema_sql(first),
                                probe.normalized_schema_sql(second))

    def test_schema_inventory_excludes_only_literal_sqlite_prefix(self):
        with closing(sqlite3.connect(":memory:")) as db:
            db.execute("CREATE TABLE normal(id INTEGER PRIMARY KEY AUTOINCREMENT)")
            db.execute("CREATE TABLE sqliteXforeign(value TEXT)")
            inventory = probe.schema_inventory(db)
            self.assertIn("sqliteXforeign", inventory)
            self.assertNotIn("sqlite_sequence", inventory)

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
    @classmethod
    def setUpClass(cls):
        cls.expected_schema = probe.reference_schema(os.environ["RECALL_LEDGER_BIN"])

    def database_state(self, database):
        state = {}
        for suffix in ("", "-wal", "-shm", "-journal"):
            path = Path(str(database) + suffix)
            if path.is_symlink():
                state[suffix] = ("symlink", os.readlink(path))
            elif path.exists():
                state[suffix] = ("file", path.read_bytes())
        return state

    def assert_resume_refused_without_mutation(self, root, expected, adapter="claude-code"):
        database = root / "a0" / "memory.db"
        before = self.database_state(database)
        log = (root / "a0" / "daemon.log").read_bytes()
        saved = json.loads((root / "fixture-state.json").read_text())["adapters"][0]
        daemon = probe.Daemon(
            os.environ["RECALL_LEDGER_BIN"], database.parent, saved["namespace"], {},
            expected_binding=saved["binding"], expected_pending=saved["pending_candidates"],
        )
        daemon.expected_schema = self.expected_schema
        with mock.patch.object(probe.subprocess, "Popen") as start:
            with self.assertRaisesRegex(probe.ProbeError, "^" + expected + "$"):
                daemon.start(existing=True)
            start.assert_not_called()
        self.assertEqual(self.database_state(database), before)
        code, resumed = self.invoke(root, "resume", adapter)
        self.assertEqual(code, 1, resumed)
        self.assertEqual(resumed["adapters"][0]["error"]["code"], expected)
        self.assertTrue(resumed["cleanup"]["all_daemons_stopped"])
        self.assertEqual(self.database_state(database), before)
        self.assertEqual((root / "a0" / "daemon.log").read_bytes(), log)

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
            self.assert_resume_refused_without_mutation(root, "resume_database_missing_or_symlink")
            self.assertFalse(database.exists())

    def test_damaged_or_foreign_resume_database_is_unchanged(self):
        cases = {
            "zero": "resume_database_empty_or_invalid_header",
            "truncated": "resume_database_empty_or_invalid_header",
            "schema": "resume_database_schema_mismatch",
            "schema4": "resume_database_schema_mismatch",
            "schema6": "resume_database_schema_mismatch",
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
                        if case.startswith("schema"):
                            version = {"schema": 0, "schema4": 4, "schema6": 6}[case]
                            db.execute("PRAGMA user_version=" + str(version))
                        else:
                            db.execute("UPDATE meta SET value='foreign-history' WHERE key='store_id'")
                self.assert_resume_refused_without_mutation(root, expected)

    def test_schema5_malformed_structure_is_refused_unchanged(self):
        cases = {
            "history_table_missing": (
                "DROP TABLE history_events", "resume_database_required_tables_missing"),
            "history_column_changed": (
                "ALTER TABLE history_events ADD COLUMN unexpected TEXT",
                "resume_database_structure_mismatch"),
            "history_index_missing": (
                "DROP INDEX history_events_scope", "resume_database_structure_mismatch"),
            "history_trigger_replaced": (
                "DROP TRIGGER history_revision_insert;"
                "CREATE TRIGGER history_revision_insert AFTER INSERT ON revisions BEGIN SELECT 1; END",
                "resume_database_structure_mismatch"),
            "extra_table_literal_prefix": (
                "CREATE TABLE sqliteXforeign(value TEXT)", "resume_database_structure_mismatch"),
            "extra_trigger": (
                "CREATE TRIGGER foreign_trigger AFTER INSERT ON revisions BEGIN SELECT 1; END",
                "resume_database_structure_mismatch"),
        }
        for case, (statement, expected) in cases.items():
            with self.subTest(case=case), tempfile.TemporaryDirectory(prefix="rl-hpi-", dir="/tmp") as temp:
                root = Path(temp) / "run"
                code, initial = self.invoke(root, "initial", "claude-code")
                self.assertEqual(code, 0, initial)
                with closing(sqlite3.connect(root / "a0" / "memory.db")) as db, db:
                    self.assertEqual(db.execute("PRAGMA user_version").fetchone()[0], 5)
                    db.executescript(statement)
                self.assert_resume_refused_without_mutation(root, expected)

    def test_schema5_invalid_history_is_refused_unchanged(self):
        invalid_meta = "resume_database_history_metadata_invalid"
        invalid_events = "resume_database_history_events_invalid"
        cases = {
            "sequence_missing": ("DELETE FROM meta WHERE key='sequence'", invalid_meta),
            "sequence_not_integer": ("UPDATE meta SET value='no' WHERE key='sequence'", invalid_meta),
            "sequence_out_of_range": (
                "UPDATE meta SET value='9223372036854775808' WHERE key='sequence'", invalid_meta),
            "event_foreign_history": (
                "UPDATE history_events SET history_id='00000000-0000-0000-0000-000000000000'",
                invalid_events),
            "event_missing_history": ("UPDATE history_events SET history_id=NULL", invalid_events),
            "event_unfinalized": ("UPDATE history_events SET finalized=0", invalid_events),
            "event_sequence_ahead": (
                "UPDATE history_events SET commit_sequence="
                "CAST((SELECT value FROM meta WHERE key='sequence') AS INTEGER)+1", invalid_events),
            "event_sequence_wrong_type": (
                "UPDATE history_events SET commit_sequence='not-a-sequence'", invalid_events),
            "event_wrong_namespace": (
                "UPDATE history_events SET namespace='wrong-namespace'", invalid_events),
            "event_orphan": (
                "UPDATE history_events SET record_id='missing-record'", "resume_database_foreign_key_failed"),
            "events_missing": ("DELETE FROM history_events", "resume_database_history_events_missing"),
        }
        for case, (statement, expected) in cases.items():
            with self.subTest(case=case), tempfile.TemporaryDirectory(prefix="rl-hpi-", dir="/tmp") as temp:
                root = Path(temp) / "run"
                code, initial = self.invoke(root, "initial", "claude-code")
                self.assertEqual(code, 0, initial)
                with closing(sqlite3.connect(root / "a0" / "memory.db")) as db, db:
                    self.assertGreater(db.execute("SELECT COUNT(*) FROM history_events").fetchone()[0], 0)
                    db.execute(statement)
                self.assert_resume_refused_without_mutation(root, expected)

    def test_committed_wal_is_read_without_changing_original_files(self):
        with tempfile.TemporaryDirectory(prefix="rl-hpi-", dir="/tmp") as temp:
            root = Path(temp) / "run"
            code, initial = self.invoke(root, "initial", "claude-code")
            self.assertEqual(code, 0, initial)
            database = root / "a0" / "memory.db"
            saved = json.loads((root / "fixture-state.json").read_text())["adapters"][0]
            pending = copy.deepcopy(saved["pending_candidates"])
            candidate = pending[0]
            new_body = "committed-WAL-only-candidate"
            candidate["body_sha256"] = hashlib.sha256(new_body.encode()).hexdigest()
            # Keep this connection open without further writes: closing it would
            # checkpoint the WAL before verification and defeat this test.
            with closing(sqlite3.connect(database)) as writer:
                writer.execute("PRAGMA journal_mode=WAL")
                writer.execute("PRAGMA wal_autocheckpoint=0")
                writer.execute("UPDATE ingestion_candidates SET body=? WHERE id=?",
                               (new_body, candidate["candidate_id"]))
                writer.commit()
                self.assertGreater(Path(str(database) + "-wal").stat().st_size, 0)
                # immutable deliberately ignores WAL, proving the main file
                # alone does not contain the new committed candidate body.
                with closing(sqlite3.connect(database.as_uri() + "?immutable=1", uri=True)) as main:
                    body = main.execute("SELECT body FROM ingestion_candidates WHERE id=?",
                                        (candidate["candidate_id"],)).fetchone()[0]
                    self.assertNotEqual(body, new_body)
                before = self.database_state(database)
                probe.verify_existing_database(database, saved["binding"], pending,
                                               expected_schema=self.expected_schema)
                self.assertEqual(self.database_state(database), before)
                with self.assertRaisesRegex(probe.ProbeError, "resume_original_pending_candidate_changed"):
                    probe.verify_existing_database(
                        database, saved["binding"], saved["pending_candidates"],
                        expected_schema=self.expected_schema)
                self.assertEqual(self.database_state(database), before)

    def test_sidecar_symlink_is_refused_without_touching_target(self):
        with tempfile.TemporaryDirectory(prefix="rl-hpi-", dir="/tmp") as temp:
            root = Path(temp) / "run"
            code, initial = self.invoke(root, "initial", "claude-code")
            self.assertEqual(code, 0, initial)
            database = root / "a0" / "memory.db"
            target = Path(temp) / "unrelated"
            target.write_bytes(b"do not read as a WAL")
            sidecar = Path(str(database) + "-wal")
            sidecar.unlink(missing_ok=True)
            sidecar.symlink_to(target)
            self.assert_resume_refused_without_mutation(root, "resume_database_sidecar_or_path_invalid")
            self.assertEqual(target.read_bytes(), b"do not read as a WAL")

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
                self.assert_resume_refused_without_mutation(
                    root, "resume_original_pending_candidate_changed", adapter)
                with closing(sqlite3.connect(database)) as db, db:
                    self.assertEqual(db.execute(
                        "SELECT count(*) FROM ingestion_candidates WHERE state='pending'").fetchone()[0], 0)


if __name__ == "__main__":
    unittest.main()
