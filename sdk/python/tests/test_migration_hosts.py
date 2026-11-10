"""Actual installed command hooks against temporary local ledger processes.

This invokes the SDK hook subprocesses recorded in temporary host configs. It
does not start either vendor's host application, run a model, or install into
any real user project.
"""

import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest

from recall_ledger import claude_code, codex
from recall_ledger.client import Client
from recall_ledger import migration_wizard as wizard
from recall_ledger.semantic import capture_candidate, review_candidate, resolve_candidate


@unittest.skipUnless(os.environ.get("RECALL_LEDGER_BIN") and sys.version_info >= (3, 11),
                     "requires the current daemon and Python 3.11+ for Codex TOML")
class MigrationHostTests(unittest.TestCase):
    def test_claude_to_codex_to_claude_correction_withdrawal_and_old_bundle(self):
        from test_host_runtime import DaemonFixture

        with tempfile.TemporaryDirectory(prefix="rl-host-migration-", dir="/tmp") as temporary:
            root = Path(temporary).resolve()
            for name in ("a", "b", "claude-project", "codex-project"):
                (root / name).mkdir()
            da = DaemonFixture(root / "a", namespace="host-a")
            db = DaemonFixture(root / "b", namespace="host-b")
            try:
                a = Client(str(da.socket), expected_binding=da.client.binding())
                b = Client(str(db.socket), expected_binding=db.client.binding())
                project_a, project_b = root / "claude-project", root / "codex-project"
                claude_code.install(str(project_a), str(da.socket),
                                    capture_user=False, capture_assistant=False)
                codex_install = codex.install(str(project_b), str(db.socket))
                self.assertFalse(codex_install["host_trusted"])
                self.assertFalse(codex_install["effective_hooks_verified"])

                def hook(project, host, event_name):
                    relative = (".claude/settings.local.json" if host == "claude-code"
                                else ".codex/hooks.json")
                    config = json.loads((project / relative).read_text())
                    group = config["hooks"][event_name]
                    commands = [entry["command"] for rule in group for entry in rule["hooks"]
                                if entry.get("type") == "command" and "recall_ledger" in entry.get("command", "")]
                    self.assertEqual(len(commands), 1)
                    event = {"hook_event_name": event_name, "session_id": "migration-session",
                             "cwd": str(project)}
                    if event_name == "SessionStart":
                        event["source"] = "compact"
                    else:
                        event.update(prompt="记忆迁移测试", turn_id="read-turn")
                    result = subprocess.run(
                        shlex.split(commands[0]), input=json.dumps(event).encode(),
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15, check=True,
                        cwd=project,
                        env=dict(os.environ, PYTHONPATH=str(Path(__file__).resolve().parents[1])),
                    )
                    self.assertEqual(result.stderr, b"")
                    value = json.loads(result.stdout)
                    self.assertEqual(value["hookSpecificOutput"]["hookEventName"], event_name)
                    context = value["hookSpecificOutput"]["additionalContext"]
                    packet = json.loads(context[context.index("{"):])
                    self.assertIn("manifest", packet)
                    self.assertIn("items", packet)
                    expected = a.expected_binding if host == "claude-code" else b.expected_binding
                    self.assertEqual(packet["manifest"]["namespace"], expected["namespace"])
                    # Both adapters may JSON-escape non-ASCII within the context.
                    # Assert the decoded actual hook payload, not presentation.
                    return json.dumps(packet, ensure_ascii=False), packet

                def move(source, target, bundle, key):
                    plan = wizard.preview_portable(
                        target, bundle, expected_source=source.expected_binding,
                        expected_target=target.expected_binding, scope="personal",
                        allow_unscoped=True)  # The derived summary has no fact key.
                    self.assertTrue(plan["can_apply"], plan)
                    return wizard.apply_portable(
                        target, bundle, plan, reviewed_digest=plan["digest"], idempotency_key=key)

                fact = {"entity": "user", "attribute": "report-language",
                        "scope": "personal", "conditions": "default"}
                original = "记忆迁移测试：报告用中文。"
                corrected = "记忆迁移测试：报告改用英文。"
                summary = "记忆迁移测试摘要：报告用中文。"
                pending = wizard.preview_native(
                    a, [{"id": "language", "text": original, "kind": "preference", **fact}],
                    format="json", scope="personal", source_name="synthetic-native-note")
                admission = wizard.apply_native(a, pending, reviewed_digest=pending["digest"],
                                                idempotency_key="native-import")
                candidate = admission["receipts"][0]["candidate_id"]
                self.assertEqual(a.candidate_inspect(candidate)["state"], "pending")
                before, _ = hook(project_a, "claude-code", "UserPromptSubmit")
                self.assertNotIn(original, before)
                review = review_candidate(a, candidate, scope="personal")
                receipt = resolve_candidate(a, review, decision="accept", idempotency_key="review-native",
                                            acknowledged_flags=review["required_acknowledgements"])
                record_id = receipt["record_id"]
                prepared = a.call("prepare", {"inputs": [record_id], "processor": "synthetic-summary",
                                              "idempotency_key": "summary-prepare"})
                a.call("publish", {"job_id": prepared["job_id"], "body": summary,
                                   "kind": "inference", "idempotency_key": "summary-publish"})

                context_a, packet_a = hook(project_a, "claude-code", "UserPromptSubmit")
                self.assertIn(original, context_a)
                self.assertIn(summary, context_a)
                task = {"task_id": "migration-task", "goal": "继续核对报告用语",
                        "progress": ["读取已审阅偏好"], "open_questions": [],
                        "artifacts": [], "environment_checks": []}
                handoff_a = wizard.prepare_handoff_packet(
                    a, task, packet_a["manifest"], expected_source=a.expected_binding)
                first_bundle = a.bundle_export()
                move(a, b, first_bundle, "a-to-b")
                receive_b = wizard.transfer_handoff(
                    b, handoff_a, expected_target=b.expected_binding, bundle=first_bundle)
                self.assertTrue(receive_b["verification"]["evidence_valid"])
                self.assertFalse(receive_b["execution_authorized"])
                self.assertFalse(receive_b["host_binding_changed"])
                context_b, _ = hook(project_b, "codex", "SessionStart")
                self.assertIn(original, context_b)
                self.assertIn(summary, context_b)

                correction = capture_candidate(
                    b, corrected, scope="personal", fact_key=fact, kind="preference",
                    event_id="reviewed-correction", source="synthetic-user-correction",
                    idempotency_key="correction-capture")
                change_review = review_candidate(b, correction["candidate"]["candidate_id"], scope="personal")
                self.assertIn("existing_fact_change", change_review["required_acknowledgements"])
                resolve_candidate(b, change_review, decision="accept", idempotency_key="correction-accept",
                                  acknowledged_flags=change_review["required_acknowledgements"])
                changed_b, packet_b = hook(project_b, "codex", "SessionStart")
                self.assertIn(corrected, changed_b)
                self.assertNotIn(original, changed_b)
                self.assertNotIn(summary, changed_b)
                self.assertEqual(wizard.transfer_handoff(
                    b, receive_b["packet"], expected_target=b.expected_binding)["state"], "stale")

                handoff_b = wizard.prepare_handoff_packet(
                    b, task, packet_b["manifest"], expected_source=b.expected_binding)
                return_bundle = b.bundle_export()
                returned = move(b, a, return_bundle, "b-to-a")
                self.assertEqual(returned["stats"]["records"]["create"], 0)
                self.assertEqual(a.fact_get(fact)["id"], record_id)
                self.assertEqual(a.fact_get(fact)["revision"], 2)
                receive_a = wizard.transfer_handoff(
                    a, handoff_b, expected_target=a.expected_binding, bundle=return_bundle)
                self.assertTrue(receive_a["verification"]["evidence_valid"])
                changed_a, _ = hook(project_a, "claude-code", "UserPromptSubmit")
                self.assertIn(corrected, changed_a)
                self.assertNotIn(original, changed_a)
                self.assertNotIn(summary, changed_a)
                self.assertFalse(a.validate_context(packet_a["manifest"])["valid"])

                a.retract(record_id, expected_revision=2, idempotency_key="withdraw-at-a")
                withdrawn_a, _ = hook(project_a, "claude-code", "UserPromptSubmit")
                self.assertNotIn(original, withdrawn_a)
                self.assertNotIn(corrected, withdrawn_a)
                self.assertNotIn(summary, withdrawn_a)
                move(a, b, a.bundle_export(), "withdraw-a-to-b")
                withdrawn_b, _ = hook(project_b, "codex", "SessionStart")
                self.assertNotIn(original, withdrawn_b)
                self.assertNotIn(corrected, withdrawn_b)
                self.assertNotIn(summary, withdrawn_b)
                move(b, a, b.bundle_export(), "withdraw-b-to-a")
                still_withdrawn_a, _ = hook(project_a, "claude-code", "UserPromptSubmit")
                self.assertNotIn(corrected, still_withdrawn_a)

                replay = wizard.preview_portable(
                    b, first_bundle, expected_source=a.expected_binding, scope="personal", allow_unscoped=True)
                self.assertFalse(replay["can_apply"])
                token = b.call("status", {})["commit_token"]
                with self.assertRaisesRegex(wizard.WizardError, "portable_plan_blocked"):
                    wizard.apply_portable(b, first_bundle, replay, reviewed_digest=replay["digest"],
                                          idempotency_key="do-not-resurrect")
                self.assertEqual(token, b.call("status", {})["commit_token"])
                after_replay, _ = hook(project_b, "codex", "SessionStart")
                self.assertNotIn(original, after_replay)
                self.assertNotIn(corrected, after_replay)
                self.assertEqual(a.expected_binding, claude_code.load_config(str(project_a))["expected_binding"])
                self.assertEqual(b.expected_binding, json.loads(
                    (project_b / ".codex/recall-ledger/binding.json").read_text()))
            finally:
                da.close()
                db.close()


if __name__ == "__main__":
    unittest.main()
