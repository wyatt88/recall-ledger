import copy
import os
from pathlib import Path
import tempfile
import unittest

from recall_ledger.client import Client, MemoryError
from recall_ledger import semantic

BINDING = {"namespace": "fixture", "history_id": "12345678-1234-5678-9234-567812345678"}
KEY = {"entity": "person:me", "attribute": "drink", "scope": "personal", "conditions": "default"}


class MemoryFixture:
    """Small protocol fixture; live daemon tests below validate core behavior."""
    def __init__(self):
        self.records, self.candidates, self.operations, self.calls = {}, {}, {}, []
        self.sequence, self.valid = 0, True
        self.actual = copy.deepcopy(BINDING)
        self.query_ids = None
        self.index = {"indexed": 0, "eligible": 0, "pending": 0, "dimensions": None}

    @property
    def token(self):
        return self.actual["history_id"] + ":" + str(self.sequence)

    def record(self, rid, body, key=None, dependencies=None):
        self.records[rid] = {"id": rid, "revision": 1, "body": body, "kind": "assertion",
                             "source": "fixture", "fact_key": copy.deepcopy(key or KEY),
                             "applicable_now": True, "dependencies": dependencies or []}
        return self.records[rid]

    def call(self, method, params=None):
        params = params or {}
        self.calls.append((method, copy.deepcopy(params)))
        if method == "binding":
            return copy.deepcopy(self.actual)
        if method == "status":
            return {"commit_token": self.token}
        if method == "embedding_status":
            return dict(self.index, commit_token=self.token)
        if method == "embedding_candidates":
            return {"items": [copy.deepcopy(row) for row in self.records.values()][:params["limit"]]}
        if method == "embedding_set":
            if self.records[params["id"]]["revision"] != params["expected_revision"]:
                raise MemoryError("revision_conflict", "stale")
            self.sequence += 1
            self.index.update(indexed=self.index["indexed"] + 1, dimensions=len(params["vector"]))
            return {"id": params["id"], "revision": params["expected_revision"],
                    "index_state": "semantic_ready", "commit_token": self.token}
        if method == "operation":
            key = (params["method"], params["idempotency_key"])
            if key not in self.operations:
                raise MemoryError("not_found", "missing")
            return copy.deepcopy(self.operations[key][1])
        if method == "candidate_submit":
            key = (method, params["idempotency_key"])
            if key in self.operations:
                if self.operations[key][0] != params:
                    raise MemoryError("idempotency_conflict", "changed")
                return copy.deepcopy(self.operations[key][1])
            cid = "candidate-" + str(len(self.candidates))
            self.candidates[cid] = dict(params, candidate_id=cid, state="pending", metadata_fields=["kind"])
            self.sequence += 1
            result = {"candidate_id": cid, "state": "pending", "commit_token": self.token}
            self.operations[key] = (copy.deepcopy(params), copy.deepcopy(result))
            return result
        if method == "candidate_inspect":
            return copy.deepcopy(self.candidates[params["candidate_id"]])
        if method == "fact_get":
            for row in self.records.values():
                if row["fact_key"] == params["fact_key"]:
                    return copy.deepcopy(row)
            raise MemoryError("not_found", "missing")
        if method == "candidate_resolve":
            key = (method, params["idempotency_key"])
            if key in self.operations:
                if params != self.operations[key][0]:
                    raise MemoryError("idempotency_conflict", "changed")
                return copy.deepcopy(self.operations[key][1])
            candidate = self.candidates[params["candidate_id"]]
            if candidate["state"] != "pending":
                raise MemoryError("candidate_resolved", "terminal")
            current = next((r for r in self.records.values() if r["fact_key"] == candidate.get("fact_key")), None)
            if params["decision"] == "accept":
                if (current["revision"] if current else None) != params.get("expected_revision"):
                    raise MemoryError("revision_conflict", "stale")
                if current:
                    current.update(revision=current["revision"] + 1, body=candidate["body"])
                else:
                    current = self.record("record-" + str(len(self.records)), candidate["body"], candidate.get("fact_key"))
                candidate.update(state="accepted", record_id=current["id"], accepted_revision=current["revision"])
            else:
                candidate.update(state="rejected")
            self.sequence += 1
            result = {k: candidate.get(k) for k in ("candidate_id", "state", "record_id", "accepted_revision")}
            result["commit_token"] = self.token
            self.operations[key] = (copy.deepcopy(params), copy.deepcopy(result))
            return result
        if method == "recall":
            ids = self.query_ids.get(params["query"], []) if self.query_ids is not None else list(self.records)
            rows = [copy.deepcopy(self.records[rid]) for rid in ids]
            return {"items": rows, "manifest": {
                "schema_version": "recall-ledger.context/1", "namespace": self.actual["namespace"],
                "commit_token": self.token, "records": [{"id": row["id"], "revision": row["revision"]} for row in rows]},
                "coverage": {"mode": params["mode"], "semantic_index_complete": self.index["pending"] == 0}}
        if method == "inspect":
            if params["id"] not in self.records:
                raise MemoryError("not_found", "missing")
            return dict(copy.deepcopy(self.records[params["id"]]), commit_token=self.token)
        if method == "validate_context":
            valid = self.valid and all(ref["id"] in self.records and self.records[ref["id"]]["revision"] == ref["revision"]
                                      for ref in params["manifest"]["records"])
            return {"valid": valid, "invalid": [] if valid else [{"reason": "revision_changed"}], "commit_token": self.token}
        raise AssertionError(method)


class SemanticTests(unittest.TestCase):
    def setUp(self):
        self.client = MemoryFixture()

    def capture(self, body, key=None, **extra):
        return semantic.capture_candidate(self.client, body, scope="personal", fact_key=key or KEY,
                                          event_id="event", source="user-turn", idempotency_key="capture", **extra)

    def test_alias_match_suggests_without_merging_and_scopes_stay_separate(self):
        self.client.record("a", "我不喝咖啡")
        self.client.record("b", "我不喝咖啡", dict(KEY, scope="work"))
        matcher = semantic.RuleMatcher({"不饮用": "不喝"})
        result = semantic.preview_candidate(self.client, "我不饮用咖啡", scope="personal", matcher=matcher)
        self.assertEqual([m["id"] for m in result["matches"]], ["a"])
        self.assertEqual(result["matches"][0]["similarity"], 1)
        self.assertFalse(result["matches"][0]["score_is_probability"])
        self.assertEqual(len(self.client.candidates), 0)

    def test_different_conditions_are_not_same_identity(self):
        self.client.record("a", "我喝茶", dict(KEY, conditions="晚上"))
        result = semantic.preview_candidate(self.client, "我喝茶", scope="personal", fact_key=KEY)
        self.assertEqual(result["matches"][0]["relation"], "different_conditions")

    def test_wrong_scope_fails_before_capture(self):
        with self.assertRaisesRegex(semantic.SemanticError, "scope_mismatch"):
            self.capture("我喝茶", dict(KEY, scope="work"))
        self.assertEqual(self.client.calls, [])

    def test_uncertainty_and_quote_are_not_automatically_admitted(self):
        result = self.capture("也许以后喜欢茶")
        self.assertEqual(result["kind"], "inference")
        self.assertEqual(result["candidate"]["state"], "pending")
        review = semantic.review_candidate(self.client, result["candidate"]["candidate_id"], scope="personal")
        with self.assertRaisesRegex(semantic.SemanticError, "acknowledgement_required"):
            semantic.resolve_candidate(self.client, review, decision="accept", idempotency_key="accept")
        self.assertEqual(self.client.records, {})

    def test_chinese_unsettled_intents_and_local_exception_require_review(self):
        for body in ("还在考虑去日本", "正在考虑换工作", "打算下周出发但未定"):
            with self.subTest(body=body):
                self.assertIn("uncertain", semantic.qualification_flags(body))
        flags = semantic.qualification_flags("通常用中文，这份报告用英文")
        self.assertIn("temporary_exception", flags)
        self.assertIn("exception", flags)
        result = self.capture("通常用中文，这份报告用英文")
        review = semantic.review_candidate(self.client, result["candidate"]["candidate_id"], scope="personal")
        self.assertIn("explicit_conditions_required", review["blockers"])

    def test_unqualified_condition_cannot_be_admitted_as_default(self):
        result = self.capture("只有晚上喝茶，咖啡除外")
        review = semantic.review_candidate(self.client, result["candidate"]["candidate_id"], scope="personal")
        self.assertIn("explicit_conditions_required", review["blockers"])
        with self.assertRaisesRegex(semantic.SemanticError, "admission_blocked"):
            semantic.resolve_candidate(self.client, review, decision="accept", idempotency_key="accept",
                                       acknowledged_flags=review["required_acknowledgements"])
        rejected = semantic.resolve_candidate(self.client, review, decision="reject", idempotency_key="reject")
        self.assertEqual(rejected["state"], "rejected")

    def test_explicit_correction_uses_exact_reviewed_revision(self):
        self.client.record("old", "我喜欢茶")
        cid = self.capture("我现在喜欢水")["candidate"]["candidate_id"]
        review = semantic.review_candidate(self.client, cid, scope="personal")
        result = semantic.resolve_candidate(self.client, review, decision="accept", idempotency_key="resolve",
                                            acknowledged_flags=["existing_fact_change"])
        self.assertEqual(result["accepted_revision"], 2)
        self.assertEqual(self.client.records["old"]["body"], "我现在喜欢水")
        self.assertEqual(semantic.resolve_candidate(self.client, review, decision="accept", idempotency_key="resolve",
                                                    acknowledged_flags=["existing_fact_change"]), result)

    def test_changed_fact_blocks_stale_review_and_preserves_pending(self):
        record = self.client.record("old", "我喜欢茶")
        cid = self.capture("我现在喜欢水")["candidate"]["candidate_id"]
        review = semantic.review_candidate(self.client, cid, scope="personal")
        record.update(revision=2, body="别人同时更正了记录")
        with self.assertRaisesRegex(semantic.SemanticError, "review_stale"):
            semantic.resolve_candidate(self.client, review, decision="accept", idempotency_key="resolve",
                                       acknowledged_flags=["existing_fact_change"])
        self.assertEqual(self.client.candidates[cid]["state"], "pending")

    def test_tampered_review_or_nonfinite_matcher_is_rejected(self):
        cid = self.capture("我喜欢茶")["candidate"]["candidate_id"]
        review = semantic.review_candidate(self.client, cid, scope="personal")
        review["proposal"]["body"] = "改写"
        with self.assertRaisesRegex(semantic.SemanticError, "review_digest_mismatch"):
            semantic.resolve_candidate(self.client, review, decision="reject", idempotency_key="x")
        self.client.record("a", "茶")
        with self.assertRaisesRegex(semantic.SemanticError, "invalid_matcher_score"):
            semantic.preview_candidate(self.client, "茶", scope="personal", matcher=lambda *_: float("nan"))


@unittest.skipUnless(os.environ.get("RECALL_LEDGER_BIN"), "requires the current built daemon")
class SemanticDaemonTests(unittest.TestCase):
    def test_real_candidate_update_and_condition_gate(self):
        from test_host_runtime import DaemonFixture
        with tempfile.TemporaryDirectory(prefix="rl-sem-", dir="/tmp") as temp:
            daemon = DaemonFixture(Path(temp))
            try:
                client = Client(str(daemon.socket), expected_binding=daemon.client.binding())
                for n, body in enumerate(("我喜欢茶", "我喜欢白水")):
                    cid = semantic.capture_candidate(client, body, scope="personal", fact_key=KEY,
                        event_id=f"event-{n}", source="fixture", idempotency_key=f"cap-{n}")["candidate"]["candidate_id"]
                    review = semantic.review_candidate(client, cid, scope="personal")
                    result = semantic.resolve_candidate(client, review, decision="accept", idempotency_key=f"ok-{n}",
                                                         acknowledged_flags=review["required_acknowledgements"])
                    self.assertEqual(result["accepted_revision"], n + 1)
                self.assertEqual(client.call("fact_get", {"fact_key": KEY})["body"], "我喜欢白水")
                bad = semantic.capture_candidate(client, "只有晚上喝茶", scope="personal", fact_key=KEY,
                    event_id="conditional", source="fixture", idempotency_key="conditional")["candidate"]["candidate_id"]
                self.assertIn("explicit_conditions_required", semantic.review_candidate(client, bad, scope="personal")["blockers"])
            finally:
                daemon.close()


if __name__ == "__main__":
    unittest.main()
