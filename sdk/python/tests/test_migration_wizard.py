import copy
import os
from pathlib import Path
import tempfile
import unittest

from recall_ledger.client import Client, MemoryError, TransportError
from recall_ledger import migration_wizard as wizard
from test_semantic import MemoryFixture, KEY


class WizardTests(unittest.TestCase):
    def setUp(self):
        self.client = MemoryFixture()

    def preview(self, content=None, **kwargs):
        return wizard.preview_native(self.client, content or "# 偏好\n用短句。\n# 任务\n尚未完成。",
                                     format=kwargs.pop("format", "markdown"), scope="personal", source_name="notes", **kwargs)

    def test_markdown_preserves_fences_and_conditions_as_pending(self):
        plan = self.preview("# 原文\n如果晚上才喝茶。\n```\n# this is data\n```\n# 下一节\n尚未确认")
        self.assertEqual(len(plan["candidates"]), 2)
        self.assertIn("# this is data", plan["candidates"][0]["body"])
        self.assertTrue(all(c["origin"] == "import" for c in plan["candidates"]))
        result = wizard.apply_native(self.client, plan, reviewed_digest=plan["digest"], idempotency_key="import")
        self.assertEqual(result["completed"], 2)
        self.assertTrue(all(c["state"] == "pending" for c in self.client.candidates.values()))
        self.assertFalse(result["automatic_admission"])

    def test_unknown_authority_fields_and_scope_mismatch_block(self):
        plan = self.preview({"schema": wizard.NATIVE_SCHEMA, "memories": [
            {"text": "一个事实", "scope": "another", "permissions": ["execute"]}]}, format="json")
        self.assertFalse(plan["can_apply"])
        self.assertEqual({b["reason"] for b in plan["blockers"]}, {"unknown_field", "scope_mismatch"})
        with self.assertRaisesRegex(wizard.WizardError, "native_plan_blocked"):
            wizard.apply_native(self.client, plan, reviewed_digest=plan["digest"], idempotency_key="bad")
        self.assertEqual(self.client.candidates, {})

    def test_duplicate_json_keys_and_unknown_schema_rejected(self):
        with self.assertRaisesRegex(wizard.WizardError, "duplicate_json_key"):
            self.preview('{"schema":"x","schema":"y","memories":[]}', format="json")
        with self.assertRaisesRegex(wizard.WizardError, "nonfinite_json"):
            self.preview('[{"text":NaN}]', format="json")
        plan = self.preview({"schema": "another/1", "memories": [{"text": "body"}]}, format="json")
        self.assertFalse(plan["can_apply"])

    def test_target_change_never_silently_refreshes_plan(self):
        plan = self.preview()
        self.client.sequence += 1
        with self.assertRaisesRegex(wizard.WizardError, "target_changed"):
            wizard.apply_native(self.client, plan, reviewed_digest=plan["digest"], idempotency_key="import")
        self.assertEqual(self.client.candidates, {})

    def test_lost_receipt_resume_recovers_original_keys_without_duplicate(self):
        plan = self.preview()
        original = self.client.call
        once = [True]
        def lost(method, params=None):
            result = original(method, params)
            if method == "candidate_submit" and once[0]:
                once[0] = False
                raise TransportError("lost after commit", outcome_unknown=True)
            return result
        self.client.call = lost
        result = wizard.apply_native(self.client, plan, reviewed_digest=plan["digest"], idempotency_key="import")
        self.assertTrue(result["outcome_unknown"])
        self.assertEqual(len(self.client.candidates), 1)
        recovered = wizard.apply_native(self.client, plan, reviewed_digest=plan["digest"], idempotency_key="import")
        self.assertEqual(recovered["state"], "applied")
        self.assertEqual(len(self.client.candidates), 2)

    def test_tampered_native_review_rejected_before_writing(self):
        plan = self.preview()
        original = plan["digest"]
        plan["candidates"][0]["body"] = "not reviewed"
        with self.assertRaisesRegex(wizard.WizardError, "plan_digest_mismatch"):
            wizard.apply_native(self.client, plan, reviewed_digest=original, idempotency_key="import")

    def test_selected_export_closes_dependencies_but_does_not_forge_portable(self):
        self.client.record("parent", "原文")
        self.client.record("derived", "派生", dict(KEY, attribute="summary"), [{"id": "parent", "revision": 1}])
        result = wizard.export_selection(self.client, ["derived"], scope="personal")
        self.assertEqual(result["ancestor_ids"], ["parent"])
        self.assertFalse(result["portable_apply_supported"])
        self.assertTrue(result["closure_complete"])
        native = self.preview(result["native_json"], format="json")
        self.assertTrue(native["can_apply"])
        self.assertEqual(len(native["candidates"]), 2)

    def test_export_cross_scope_ancestor_and_stale_versions_refused(self):
        self.client.record("p", "foreign", dict(KEY, scope="other"))
        self.client.record("d", "derived", dependencies=[{"id": "p", "revision": 1}])
        with self.assertRaisesRegex(wizard.WizardError, "scope_mismatch"):
            wizard.export_selection(self.client, ["d"], scope="personal")
        self.client.records["p"]["fact_key"] = KEY
        self.client.records["p"]["revision"] = 2
        with self.assertRaisesRegex(wizard.WizardError, "changed_dependency"):
            wizard.export_selection(self.client, ["d"], scope="personal")


@unittest.skipUnless(os.environ.get("RECALL_LEDGER_BIN"), "requires the current built daemon")
class WizardDaemonTests(unittest.TestCase):
    def test_real_native_portable_stale_plan_and_rebased_handoff(self):
        from test_host_runtime import DaemonFixture
        from recall_ledger.semantic import review_candidate, resolve_candidate
        from recall_ledger.handoff import prepare_handoff
        with tempfile.TemporaryDirectory(prefix="rl-wiz-", dir="/tmp") as temp:
            root = Path(temp)
            (root / "a").mkdir()
            (root / "b").mkdir()
            da, db = DaemonFixture(root / "a", namespace="a"), DaemonFixture(root / "b", namespace="b")
            try:
                a = Client(str(da.socket), expected_binding=da.client.binding())
                b = Client(str(db.socket), expected_binding=db.client.binding())
                native = wizard.preview_native(a, [{"id": "one", "text": "我喜欢茶", "conditions": "default"}],
                                               format="json", scope="personal", source_name="fixture")
                imported = wizard.apply_native(a, native, reviewed_digest=native["digest"], idempotency_key="native")
                self.assertEqual(imported["state"], "applied")
                cid = imported["receipts"][0]["candidate_id"]
                review = review_candidate(a, cid, scope="personal")
                resolve_candidate(a, review, decision="accept", idempotency_key="accept",
                                  acknowledged_flags=review["required_acknowledgements"])
                task = {"task_id": "t", "goal": "接续查阅资料", "progress": [], "open_questions": [],
                        "artifacts": [], "environment_checks": []}
                packet = prepare_handoff(a, task, a.recall("")["manifest"])
                bundle = a.bundle_export()
                plan = wizard.preview_portable(b, bundle, expected_source=a.expected_binding, scope="personal")
                b.remember("无关写入", idempotency_key="intervening")
                with self.assertRaises(MemoryError) as stale:
                    wizard.apply_portable(b, bundle, plan, reviewed_digest=plan["digest"], idempotency_key="migration")
                self.assertEqual(stale.exception.code, "target_changed")
                fresh = wizard.preview_portable(b, bundle, expected_source=a.expected_binding, scope="personal")
                wizard.apply_portable(b, bundle, fresh, reviewed_digest=fresh["digest"], idempotency_key="fresh-migration")
                received = wizard.transfer_handoff(b, packet, expected_target=b.expected_binding, bundle=bundle)
                self.assertTrue(received["verification"]["evidence_valid"])
                self.assertFalse(received["execution_authorized"])
                mapped = received["packet"]["evidence"]["manifest"]["records"][0]
                b.revise(mapped["id"], expected_revision=mapped["revision"], body="改成白水", idempotency_key="rev")
                stale_packet = wizard.transfer_handoff(b, received["packet"], expected_target=b.expected_binding)
                self.assertEqual(stale_packet["state"], "stale")
            finally:
                da.close()
                db.close()


if __name__ == "__main__":
    unittest.main()
