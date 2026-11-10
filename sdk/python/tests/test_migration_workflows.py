import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from recall_ledger import memory_workflows as workflows
from recall_ledger.client import Client


class WorkflowFileTests(unittest.TestCase):
    def test_rejects_duplicate_keys_nonfinite_and_symlink_files(self):
        with tempfile.TemporaryDirectory(prefix="rl-workflow-files-", dir="/tmp") as temporary:
            root = Path(temporary).resolve()
            target = root / "input.json"
            for text, code in [('{"k":1,"k":2}', "duplicate_json_key"), ('{"k":NaN}', "nonfinite_json")]:
                target.write_text(text)
                with self.assertRaisesRegex(workflows.WorkflowError, code):
                    workflows._read(target)
            link = root / "link.json"
            link.symlink_to(target)
            with self.assertRaisesRegex(workflows.WorkflowError, "symlink_path_refused"):
                workflows._read(link)

    def test_output_failure_after_mutation_reports_possible_commit(self):
        binding = {"namespace": "fixture", "history_id": "00000000-0000-0000-0000-000000000001"}
        with patch.object(workflows, "_read", return_value=binding):
            with patch.object(workflows, "dispatch", return_value={"state": "applied"}):
                with patch.object(workflows, "_write_new", side_effect=OSError("fixture unavailable")):
                    with patch("sys.stderr", new_callable=io.StringIO) as stderr:
                        code = workflows.main(["--socket", "/unused", "--binding-file", "/unused",
                                               "--output", "/does-not-exist/result.json", "native-apply"])
        self.assertEqual(code, 2)
        error = json.loads(stderr.getvalue())
        self.assertTrue(error["operation_may_have_committed"])
        self.assertTrue(error["retry_requires_same_request_and_key"])
        self.assertNotIn("fixture unavailable", stderr.getvalue())


@unittest.skipUnless(os.environ.get("RECALL_LEDGER_BIN"), "requires the current daemon")
class WorkflowDaemonTests(unittest.TestCase):
    def test_bound_cli_native_review_recall_portable_and_read_only_stores(self):
        from test_host_runtime import DaemonFixture
        from recall_ledger.commitments import CommitmentStore
        from recall_ledger.environment import capture_environment
        from recall_ledger.experience import ExperienceStore, RECEIPT_SCHEMA

        with tempfile.TemporaryDirectory(prefix="rl-workflow-cli-", dir="/tmp") as temporary:
            root = Path(temporary).resolve()
            for name in ("a", "b"):
                (root / name).mkdir()
            da, db = DaemonFixture(root / "a", namespace="a"), DaemonFixture(root / "b", namespace="b")
            try:
                a = Client(str(da.socket), expected_binding=da.client.binding())
                b = Client(str(db.socket), expected_binding=db.client.binding())
                bindings = {}
                for name, client in (("a", a), ("b", b)):
                    bindings[name] = root / (name + "-binding.json")
                    bindings[name].write_text(json.dumps(client.expected_binding))
                sequence = [0]

                def cli(command, request=None, target="a", output=None, expected_code=0, binding=None):
                    sequence[0] += 1
                    argv = [sys.executable, "-m", "recall_ledger.memory_workflows",
                            "--socket", str(da.socket if target == "a" else db.socket),
                            "--binding-file", str(binding or bindings[target]), command]
                    if request is not None:
                        path = root / ("request-" + str(sequence[0]) + ".json")
                        path.write_text(json.dumps(request, ensure_ascii=False))
                        argv += ["--request-file", str(path)]
                    if output is not None:
                        argv += ["--output", str(output)]
                    result = subprocess.run(
                        argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15,
                        env=dict(os.environ, PYTHONPATH=str(Path(__file__).resolve().parents[1])))
                    self.assertEqual(result.returncode, expected_code, result.stderr.decode())
                    return json.loads(result.stdout if expected_code == 0 else result.stderr)

                native = {"content": [{"id": "tea", "text": "我喜欢茶", "entity": "user",
                                       "attribute": "drink", "conditions": "default"}],
                          "format": "json", "scope": "personal", "source_name": "synthetic"}
                path = root / "reviewed-plan.json"
                cli("native-preview", native, output=path)
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                plan = json.loads(path.read_text())
                imported = cli("native-apply", {"plan": plan, "reviewed_digest": plan["digest"],
                                               "idempotency_key": "native"})
                candidate = imported["receipts"][0]["candidate_id"]
                self.assertEqual(a.candidate_inspect(candidate)["state"], "pending")
                review = cli("semantic-review", {"candidate_id": candidate, "scope": "personal"})
                receipt = cli("semantic-resolve", {"review": review, "decision": "accept",
                                                  "idempotency_key": "review",
                                                  "acknowledged_flags": review["required_acknowledgements"]})
                rid = receipt["record_id"]
                encoding = {"type": "rules", "aliases": {"tea": "茶"}}
                indexed = cli("evidence-index", {"scope": "personal", "encoder": encoding,
                                                 "record_ids": [rid], "idempotency_key": "index"})
                self.assertEqual(indexed["results"][0]["state"], "indexed")
                recalled = cli("evidence-query", {"scope": "personal", "queries": ["tea"],
                                                  "encoder": encoding, "max_chars": 1024})
                self.assertTrue(recalled["usable"])
                self.assertIn("我喜欢茶", recalled["context"])
                self.assertFalse(recalled["semantics_verified"])
                self.assertTrue(a.validate_context(recalled["manifest"])["valid"])

                bundle = cli("portable-export")
                portable = cli("portable-preview", {"bundle": bundle, "expected_source": a.expected_binding,
                                                     "scope": "personal"}, target="b")
                cli("portable-apply", {"bundle": bundle, "plan": portable,
                                      "reviewed_digest": portable["digest"], "idempotency_key": "portable"}, target="b")
                self.assertEqual(b.recall("")["items"][0]["body"], "我喜欢茶")

                before = a.call("status", {})["commit_token"]
                bad_binding = cli("portable-export", binding=bindings["b"], expected_code=2)
                self.assertEqual(bad_binding["error"], "binding_mismatch")
                forbidden = cli("portable-export", {"method": "retract", "id": rid}, expected_code=2)
                self.assertEqual(forbidden["error"], "unknown_request_field")
                overwrite = cli("native-apply", {"plan": plan, "reviewed_digest": plan["digest"],
                                                "idempotency_key": "must-not-write"},
                                output=path, expected_code=2)
                self.assertEqual(overwrite["error"], "output_exists")
                self.assertEqual(a.call("status", {})["commit_token"], before)

                timestamp = "2026-11-10T00:00:00Z"
                commitments = CommitmentStore(a, expected_binding=a.expected_binding)
                reminder = commitments.create(title="只读待办", due_at=timestamp, conditions=[],
                                              created_at=timestamp, idempotency_key="reminder")
                before = a.call("status", {})["commit_token"]
                due = cli("commitment-poll", {"now": timestamp, "signals": {}})
                self.assertEqual(due["items"][0]["id"], reminder["id"])
                self.assertEqual(a.call("status", {})["commit_token"], before)

                fixture_file = root / "observed.txt"
                fixture_file.write_text("synthetic environment")
                snapshot = capture_environment(
                    roots={"fixture": root}, files=[{"root": "fixture", "path": "observed.txt"}],
                    tool_versions={"synthetic": "1"}, observed_at=timestamp)
                tool_receipt = {
                    "schema": RECEIPT_SCHEMA, "receipt_id": "fixture-receipt",
                    "host_id": "fixture", "run_id": "fixture", "trace_id": "fixture",
                    "tool": "synthetic", "tool_version": "1", "status": "succeeded",
                    "exit_code": 0, "started_at": timestamp, "completed_at": timestamp,
                    "input_sha256": "1" * 64, "output_sha256": "2" * 64,
                    "checks": [{"name": "synthetic-test", "passed": True}],
                }
                store = ExperienceStore(a, expected_binding=a.expected_binding,
                                        receipt_verifier=lambda receipt: receipt == tool_receipt)
                saved = store.record_receipt(tool_receipt, idempotency_key="fixture-receipt")
                card = store.derive_card([saved["id"]], title="合成经验",
                                         steps=["只用于检验接口"], environment=snapshot, idempotency_key="card")
                before = a.call("status", {})["commit_token"]
                inspected = cli("experience-inspect", {"card_id": card["id"], "snapshot": snapshot,
                                                       "now": timestamp})
                self.assertEqual(inspected["receipt_trust"], "unknown")
                self.assertFalse(inspected["can_suggest"])
                self.assertFalse(inspected["permissions_granted"])
                self.assertEqual(a.call("status", {})["commit_token"], before)
            finally:
                da.close()
                db.close()


if __name__ == "__main__":
    unittest.main()
