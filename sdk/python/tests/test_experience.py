import json
import unittest

from recall_ledger.client import MemoryError
from recall_ledger.experience import ExperienceStore
from experience_fixtures import BINARY, DaemonCase, NOW, LATER


@unittest.skipUnless(BINARY, "Set RECALL_LEDGER_BIN for real daemon validation")
class ExperienceTests(DaemonCase):
    def store(self, client=None, verifier=True):
        selected = client or self.client
        return ExperienceStore(selected, expected_binding=selected.expected_binding,
                               receipt_verifier=self.verify if verifier else None)

    def create_card(self, store=None, key="card"):
        store = store or self.store()
        receipt = store.record_receipt(self.receipt(), idempotency_key="receipt")
        card = store.derive_card([receipt["id"]], title="Known test invocation",
                                 steps=["Use the verified fixture command."],
                                 environment=self.snapshot(), idempotency_key=key)
        return receipt, card

    def test_verified_card_dependency_restart_and_identical_retry(self):
        store = self.store()
        receipt, card = self.create_card(store)
        repeated = store.derive_card([receipt["id"]], title="Known test invocation",
                                      steps=["Use the verified fixture command."],
                                      environment=self.snapshot(), idempotency_key="card")
        self.assertEqual(card["id"], repeated["id"])
        current = self.store(self.restart()).inspect(card["id"], snapshot=self.snapshot(), now=NOW)
        self.assertTrue(current["can_suggest"])
        self.assertFalse(current["verification"]["semantic_generalization_verified"])
        self.assertFalse(current["permissions_granted"])
        self.assertEqual(current["dependencies"], [{"id": receipt["id"], "revision": 1}])

    def test_self_report_tampering_and_inconsistent_success_are_rejected(self):
        store = self.store()
        source = self.receipt()
        source["output_sha256"] = "c" * 64
        with self.assertRaisesRegex(ValueError, "unverified"):
            store.record_receipt(source, idempotency_key="spoof")
        with self.assertRaisesRegex(ValueError, "unverified"):
            self.store(verifier=False).record_receipt(self.receipt(), idempotency_key="no-host")
        source = self.receipt()
        source["checks"][0]["passed"] = False
        with self.assertRaisesRegex(ValueError, "inconsistent"):
            store.record_receipt(source, idempotency_key="bad-success")
        self.assertEqual(self.client.call("status")["records"], 0)

    def test_failed_receipt_is_recordable_but_cannot_derive_success_card(self):
        store = self.store()
        failed = store.record_receipt(self.receipt("failed", "failed"), idempotency_key="failed")
        with self.assertRaisesRegex(ValueError, "requires_successful"):
            store.derive_card([failed["id"]], title="Not a success",
                              steps=["Do not generalize this."], environment=self.snapshot(),
                              idempotency_key="bad-card")

    def test_source_correction_invalidates_card(self):
        receipt, card = self.create_card()
        raw = self.client.inspect(receipt["id"])
        self.client.revise(receipt["id"], expected_revision=1, body=raw["body"] + " ",
                           idempotency_key="receipt-correction")
        with self.assertRaises(MemoryError) as caught:
            self.store().inspect(card["id"], snapshot=self.snapshot(), now=NOW)
        self.assertEqual(caught.exception.code, "stale_dependency")
        self.assertEqual(self.store().list()["items"], [])

    def test_source_retraction_and_card_revocation_propagate(self):
        receipt, card = self.create_card()
        self.client.retract(receipt["id"], expected_revision=1, idempotency_key="source-withdraw")
        with self.assertRaises(MemoryError):
            self.store().inspect(card["id"], snapshot=self.snapshot(), now=NOW)
        store = self.store()
        fresh = store.record_receipt(self.receipt("second"), idempotency_key="second")
        new_card = store.derive_card([fresh["id"]], title="Second", steps=["Check"],
                                     environment=self.snapshot(), idempotency_key="second-card")
        result = store.revoke(new_card["id"], expected_revision=1, idempotency_key="card-withdraw")
        self.assertEqual(result["state"], "retracted")
        self.assertEqual(store.list()["items"], [])

    def test_environment_change_and_unknown_trust_downgrade_use(self):
        _, card = self.create_card()
        (self.root / "requirements.lock").write_text("dependency==2\n")
        result = self.store().inspect(card["id"], snapshot=self.snapshot(), now=NOW)
        self.assertEqual(result["applicability"]["status"], "mismatch")
        self.assertFalse(result["can_suggest"])
        (self.root / "requirements.lock").write_text("dependency==1\n")
        result = self.store(verifier=False).inspect(card["id"], snapshot=self.snapshot(), now=NOW)
        self.assertEqual(result["applicability"]["status"], "unknown")
        self.assertFalse(result["can_suggest"])
        self.assertFalse(self.store().list()["items"][0]["can_suggest"])

    def test_feedback_is_receipt_backed_and_retracts_with_source(self):
        store = self.store()
        _, card = self.create_card(store)
        failed = store.record_receipt(self.receipt("failed", "failed"), idempotency_key="failed")
        feedback = store.feedback(card["id"], outcome="failed", receipt_id=failed["id"],
                                  recorded_at=LATER, idempotency_key="feedback")
        self.assertEqual(json.loads(self.client.inspect(feedback["id"])["body"])["outcome"], "failed")
        with self.assertRaisesRegex(ValueError, "mismatch"):
            store.feedback(card["id"], outcome="succeeded", receipt_id=failed["id"],
                           recorded_at=LATER, idempotency_key="lie")
        store.revoke(card["id"], expected_revision=1, idempotency_key="revoke")
        with self.assertRaises(MemoryError):
            self.client.inspect(feedback["id"])

    def test_namespace_binding_and_cross_namespace_source_are_rejected(self):
        receipt, card = self.create_card()
        other = self.start("user-facts")
        with self.assertRaisesRegex(ValueError, "binding"):
            ExperienceStore(other, expected_binding=self.client.expected_binding,
                            receipt_verifier=self.verify)
        with self.assertRaises(MemoryError):
            self.store(other).derive_card([receipt["id"]], title="Cross namespace",
                                          steps=["No"], environment=self.snapshot(),
                                          idempotency_key="cross")
        self.assertEqual(other.call("status")["records"], 0)

    def test_receipt_revision_changed_between_read_and_prepare_is_rejected(self):
        store = self.store()
        source = store.record_receipt(self.receipt(), idempotency_key="receipt")
        client = self.client

        class RacingClient:
            expected_binding = client.expected_binding
            changed = False

            def call(self, method, params):
                if method == "prepare" and not self.changed:
                    self.changed = True
                    raw = client.inspect(source["id"])
                    client.revise(source["id"], expected_revision=1, body=raw["body"] + " ",
                                  idempotency_key="race")
                return client.call(method, params)

        racing = RacingClient()
        raced = ExperienceStore(racing, expected_binding=racing.expected_binding,
                                 receipt_verifier=self.verify)
        with self.assertRaises(MemoryError) as caught:
            raced.derive_card([source["id"]], title="Race", steps=["Check"],
                              environment=self.snapshot(), idempotency_key="raced")
        self.assertEqual(caught.exception.code, "stale_dependency")
        self.assertEqual(store.list()["items"], [])

    def test_transitive_tool_input_revision_dependency_is_enforced(self):
        store = self.store()
        input_record = self.client.remember("Fixture invocation input", idempotency_key="input")
        raw_receipt = self.receipt()
        raw_receipt["source_refs"] = [{"id": input_record["id"], "revision": 1}]
        self.trusted["run-1"] = raw_receipt.copy()
        receipt = store.record_receipt(raw_receipt, idempotency_key="receipt-with-source")
        card = store.derive_card([receipt["id"]], title="Input dependent",
                                 steps=["Check"], environment=self.snapshot(), idempotency_key="source-card")
        self.assertTrue(store.inspect(card["id"], snapshot=self.snapshot(), now=NOW)["can_suggest"])
        self.client.revise(input_record["id"], expected_revision=1, body="Changed invocation input",
                           idempotency_key="input-change")
        with self.assertRaises(MemoryError):
            store.inspect(card["id"], snapshot=self.snapshot(), now=NOW)
        with self.assertRaises(MemoryError):
            store.record_receipt(raw_receipt, idempotency_key="stale-receipt")

    def test_forged_card_without_core_dependencies_is_not_suggested(self):
        store = self.store()
        _, card = self.create_card(store)
        copied_body = self.client.inspect(card["id"])["body"]
        forged = self.client.remember(copied_body, kind="inference", idempotency_key="untracked-copy")
        with self.assertRaisesRegex(ValueError, "dependency_mismatch"):
            store.inspect(forged["id"], snapshot=self.snapshot(), now=NOW)
