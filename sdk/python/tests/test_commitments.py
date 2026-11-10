import unittest

from recall_ledger.client import MemoryError, TransportError
from recall_ledger.commitments import CommitmentStore
from experience_fixtures import BINARY, DaemonCase, NOW, LATER


@unittest.skipUnless(BINARY, "Set RECALL_LEDGER_BIN for real daemon validation")
class CommitmentTests(DaemonCase):
    def setUp(self):
        super().setUp()
        self.client = self.start("commitments")

    def store(self, client=None):
        selected = client or self.client
        return CommitmentStore(selected, expected_binding=selected.expected_binding)

    def create(self, key="commitment", due_at=NOW):
        return self.store().create(title="Review the draft", due_at=due_at,
                                   conditions=[{"signal": "draft_ready", "equals": True}],
                                   created_at="2026-11-09T09:00:00Z", idempotency_key=key)

    def claim(self, record, store=None, **overrides):
        arguments = dict(expected_revision=record["revision"], delivery_id="delivery-1", now=NOW,
                         signals={"draft_ready": True}, idempotency_key="claim")
        arguments.update(overrides)
        return (store or self.store()).claim(record["id"], **arguments)

    def test_due_conditions_are_typed_and_creation_retry_is_idempotent(self):
        record = self.create()
        self.assertEqual(self.create()["id"], record["id"])
        for signals in ({}, {"draft_ready": False}, {"draft_ready": 1}):
            self.assertEqual(self.store().poll(now=NOW, signals=signals)["items"], [])
        self.assertEqual(len(self.store().poll(now=NOW, signals={"draft_ready": True})["items"]), 1)
        self.assertEqual(self.store().poll(now="2026-11-09T09:59:00Z", signals={"draft_ready": True})["items"], [])

    def test_claim_survives_restart_and_cannot_be_automatically_repeated(self):
        record = self.create()
        first = self.claim(record)
        self.assertTrue(first["deliver"])
        self.assertFalse(first["permissions_granted"])
        store = self.store(self.restart("commitments"))
        self.assertEqual(store.poll(now=LATER, signals={"draft_ready": True})["items"], [])
        self.assertFalse(self.claim(record, store=store)["deliver"])
        self.assertTrue(store.delivery(record["id"], delivery_id="delivery-1", now=LATER,
                                       signals={"draft_ready": True})["deliver"])
        ack = store.ack(record["id"], delivery_id="delivery-1", now=LATER, idempotency_key="ack")
        self.assertTrue(ack["acknowledged"])
        self.assertTrue(store.ack(record["id"], delivery_id="delivery-1", now=LATER,
                                  idempotency_key="ack")["already_acknowledged"])
        self.assertFalse(store.delivery(record["id"], delivery_id="delivery-1", now=LATER,
                                        signals={"draft_ready": True})["deliver"])

    def test_cancelled_before_claim_is_never_due_after_restart(self):
        record = self.create()
        self.store().cancel(record["id"], expected_revision=1, now=NOW, idempotency_key="cancel")
        store = self.store(self.restart("commitments"))
        self.assertEqual(store.poll(now=LATER, signals={"draft_ready": True})["items"], [])
        self.assertFalse(self.claim(record, store=store)["deliver"])
        self.assertEqual(store.inspect(record["id"])["state"], "cancelled")

    def test_cancel_after_claim_invalidates_delivery_preflight(self):
        record = self.create()
        current = self.claim(record)["item"]
        self.store().cancel(record["id"], expected_revision=current["revision"], now=LATER,
                             idempotency_key="cancel")
        self.assertFalse(self.store().delivery(record["id"], delivery_id="delivery-1", now=LATER,
                                              signals={"draft_ready": True})["deliver"])
        with self.assertRaisesRegex(ValueError, "not_active"):
            self.store().ack(record["id"], delivery_id="delivery-1", now=LATER, idempotency_key="late-ack")

    def test_cas_prevents_stale_cancel_from_overwriting_claim(self):
        record = self.create()
        self.claim(record)
        with self.assertRaises(MemoryError) as caught:
            self.store().cancel(record["id"], expected_revision=1, now=LATER,
                                 idempotency_key="stale-cancel")
        self.assertEqual(caught.exception.code, "revision_conflict")
        self.assertEqual(self.store().inspect(record["id"])["delivery"]["state"], "claimed")

    def test_complete_and_revoke_stop_polling(self):
        record = self.create()
        self.store().complete(record["id"], expected_revision=1, now=NOW, idempotency_key="complete")
        self.assertEqual(self.store().poll(now=LATER, signals={"draft_ready": True})["items"], [])
        other = self.create(key="other")
        self.client.retract(other["id"], expected_revision=1, idempotency_key="withdraw")
        self.assertEqual(self.store().poll(now=LATER, signals={"draft_ready": True})["items"], [])

    def test_condition_becoming_false_after_claim_stops_delivery(self):
        record = self.create()
        self.claim(record)
        self.assertFalse(self.store().delivery(record["id"], delivery_id="delivery-1", now=LATER,
                                              signals={"draft_ready": False})["deliver"])
        self.assertFalse(self.store().delivery(record["id"], delivery_id="different", now=LATER,
                                              signals={"draft_ready": True})["deliver"])

    def test_unbound_clients_cross_namespace_and_naive_times_are_rejected(self):
        record = self.create()
        other = self.start("user-facts")
        with self.assertRaisesRegex(ValueError, "binding"):
            CommitmentStore(other, expected_binding=self.client.expected_binding)
        with self.assertRaises(MemoryError):
            self.store(other).inspect(record["id"])
        with self.assertRaises(ValueError):
            self.create(key="naive", due_at="2026-11-09T10:00:00")
        with self.assertRaises(ValueError):
            self.store().create(title="Expression", due_at=NOW,
                                 conditions=[{"signal": "__import__('os')", "operator": "eval"}],
                                 created_at=NOW, idempotency_key="eval")

    def test_lost_claim_receipt_is_not_reclaimed_after_restart(self):
        record = self.create()
        client = self.client

        class LostReceiptClient:
            expected_binding = client.expected_binding

            def call(self, method, params):
                result = client.call(method, params)
                if method == "revise":
                    raise TransportError("response lost after durable write", outcome_unknown=True)
                return result

        losing = LostReceiptClient()
        store = CommitmentStore(losing, expected_binding=losing.expected_binding)
        with self.assertRaises(TransportError) as caught:
            self.claim(record, store=store)
        self.assertTrue(caught.exception.outcome_unknown)
        recovered = self.store(self.restart("commitments"))
        self.assertFalse(self.claim(record, store=recovered)["deliver"])
        self.assertEqual(recovered.poll(now=LATER, signals={"draft_ready": True})["items"], [])
        self.assertEqual(recovered.inspect(record["id"])["delivery"]["state"], "claimed")

    def test_edit_due_and_conditions_before_claim_and_reject_after_claim(self):
        record = self.create()
        store = self.store()
        updated = store.update(record["id"], expected_revision=1, now=NOW,
                               due_at=LATER, conditions=[{"signal": "approved", "equals": "yes"}],
                               idempotency_key="reschedule")
        self.assertEqual(store.poll(now=NOW, signals={"approved": "yes"})["items"], [])
        self.assertEqual(store.poll(now=LATER, signals={"draft_ready": True})["items"], [])
        self.assertEqual(len(store.poll(now=LATER, signals={"approved": "yes"})["items"]), 1)
        current = self.claim(updated, now=LATER, signals={"approved": "yes"})["item"]
        with self.assertRaisesRegex(ValueError, "not_editable"):
            store.update(record["id"], expected_revision=current["revision"], now=LATER,
                         title="Do not erase unknown delivery", idempotency_key="bad-update")
