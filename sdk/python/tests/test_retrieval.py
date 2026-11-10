import json
import os
from pathlib import Path
import tempfile
import unittest

from recall_ledger.client import Client
from recall_ledger.retrieval import recall_evidence, RetrievalError, RuleEncoder, index_evidence
from test_semantic import MemoryFixture, KEY


class RetrievalTests(unittest.TestCase):
    def setUp(self):
        self.client = MemoryFixture()

    def test_missing_encoder_degrades_explicitly_and_budget_covers_exact_context(self):
        self.client.record("a", "偏好短句")
        result = recall_evidence(self.client, "写作", scope="personal")
        self.assertTrue(result["degraded"])
        self.assertEqual(result["queries"][0]["reason"], "query_encoder_not_configured")
        self.assertEqual(result["budget"]["used_chars"], len(result["context"]))
        self.assertEqual(json.loads(result["context"]), result["items"])
        self.assertIn("conditions", result["context"])

    def test_offline_encoder_aliases_are_explicit_and_configuration_pinned(self):
        encoder = RuleEncoder({"不饮用": "不喝"})
        self.assertEqual(encoder("我不饮用咖啡"), encoder("我不喝咖啡"))
        self.assertNotEqual(encoder.model_key, RuleEncoder().model_key)
        self.assertEqual(encoder.model_key, RuleEncoder({"不饮用": "不喝"}).model_key)
        self.assertIn("not_semantic", encoder.name)

    def test_indexer_scopes_input_and_reports_revision_conflict(self):
        self.client.record("a", "我不喝咖啡")
        self.client.record("b", "foreign", dict(KEY, scope="other"))
        encoder = RuleEncoder({"不饮用": "不喝"})
        result = index_evidence(self.client, scope="personal", encoder=encoder, model_key=encoder.model_key, idempotency_key="build")
        self.assertEqual([r["state"] for r in result["results"]], ["indexed", "excluded"])
        self.assertTrue(result["may_require_explicit_record_ids"])
        def changed(text):
            self.client.records["a"]["revision"] += 1
            return encoder(text)
        result = index_evidence(self.client, scope="personal", encoder=changed, model_key=encoder.model_key,
                                idempotency_key="race", record_ids=["a"])
        self.assertEqual(result["results"][0]["error_code"], "revision_conflict")

    def test_encoder_dimension_and_partial_index_are_visible(self):
        self.client.record("a", "tea")
        self.client.index = {"indexed": 1, "eligible": 2, "pending": 1, "dimensions": 2}
        result = recall_evidence(self.client, "drink", scope="personal", model_key="fixture",
                                 encoder=lambda _: [1.0, 0.0])
        self.assertTrue(result["degraded"])
        self.assertEqual(result["queries"][0]["reason"], "index_partial")
        self.assertEqual(result["queries"][0]["effective_mode"], "hybrid")
        with self.assertRaisesRegex(RetrievalError, "query_encoding_failed"):
            recall_evidence(self.client, "drink", scope="personal", model_key="fixture",
                            encoder=lambda _: [1], allow_fallback=False)

    def test_multiquery_selects_all_evidence_and_dependencies(self):
        self.client.record("parent", "原始依据")
        self.client.record("a", "饮料", dependencies=[{"id": "parent", "revision": 1}])
        self.client.record("b", "时间")
        self.client.query_ids = {"喝什么": ["a"], "什么时候": ["b"]}
        result = recall_evidence(self.client, ["喝什么", "什么时候"], scope="personal", mode="lexical",
                                 evidence_groups={"完整答案": ["a", "b"]})
        self.assertEqual({r["id"] for r in result["items"]}, {"a", "b", "parent"})
        self.assertTrue(result["groups"]["完整答案"]["complete"])
        self.assertEqual(len(result["manifest"]["records"]), 3)
        self.assertTrue(result["usable"])

    def test_group_budget_never_leaks_one_half_through_overlapping_group(self):
        self.client.record("a", "x")
        self.client.record("b", "y" * 1000)
        result = recall_evidence(self.client, "", scope="personal", mode="lexical", max_chars=500,
                                 evidence_groups={"both": ["a", "b"], "small": ["a"]})
        self.assertEqual(result["items"], [])
        self.assertFalse(result["groups"]["both"]["complete"])
        self.assertFalse(result["groups"]["small"]["complete"])

    def test_cross_scope_dependency_excludes_root_without_leaking_body(self):
        self.client.record("private", "不得泄露的原文", dict(KEY, scope="other"))
        self.client.record("summary", "summary", dependencies=[{"id": "private", "revision": 1}])
        self.client.query_ids = {"summary": ["summary"]}
        result = recall_evidence(self.client, "summary", scope="personal", mode="lexical")
        self.assertEqual(result["items"], [])
        self.assertEqual(result["excluded"][0]["reason"], "scope_mismatch")
        self.assertNotIn("不得泄露的原文", json.dumps(result, ensure_ascii=False))

    def test_exact_condition_filter_and_unscoped_default(self):
        self.client.record("a", "tea", dict(KEY, conditions="evening"))
        self.client.records["b"] = dict(self.client.records["a"], id="b", fact_key=None)
        result = recall_evidence(self.client, "", scope="personal", mode="lexical", condition_context=["morning"])
        self.assertEqual(result["items"], [])
        self.assertTrue(result["condition_filter_applied"])
        self.assertFalse(result["conditions_verified"])

    def test_final_freshness_failure_clears_all_model_text(self):
        self.client.record("a", "过期消息")
        self.client.valid = False
        result = recall_evidence(self.client, "", scope="personal", mode="lexical")
        self.assertEqual(result["context"], "[]")
        self.assertFalse(result["usable"])
        self.assertEqual(result["selected"], [])

    def test_change_between_recall_and_inspect_fails_closed(self):
        self.client.record("a", "old")
        original = self.client.call
        def changed(method, params=None):
            if method == "inspect":
                self.client.records["a"]["revision"] = 2
            return original(method, params)
        self.client.call = changed
        with self.assertRaisesRegex(RetrievalError, "changed_during_retrieval"):
            recall_evidence(self.client, "", scope="personal", mode="lexical")


@unittest.skipUnless(os.environ.get("RECALL_LEDGER_BIN"), "requires the current built daemon")
class RetrievalDaemonTests(unittest.TestCase):
    def test_real_hybrid_vectors_and_multievidence_manifest(self):
        from test_host_runtime import DaemonFixture
        from recall_ledger.semantic import capture_candidate, review_candidate, resolve_candidate
        with tempfile.TemporaryDirectory(prefix="rl-retr-", dir="/tmp") as temp:
            daemon = DaemonFixture(Path(temp))
            try:
                client = Client(str(daemon.socket), expected_binding=daemon.client.binding())
                ids = []
                for index, (body, attribute) in enumerate((("茶", "drink"), ("晚上", "time"))):
                    cid = capture_candidate(client, body, scope="personal", fact_key=dict(KEY, attribute=attribute),
                        event_id=f"e{index}", source="fixture", idempotency_key=f"c{index}")["candidate"]["candidate_id"]
                    review = review_candidate(client, cid, scope="personal")
                    ids.append(resolve_candidate(client, review, decision="accept", idempotency_key=f"a{index}")["record_id"])
                for index, rid in enumerate(ids):
                    client.call("embedding_set", {"id": rid, "expected_revision": 1, "model_key": "cpu-fixture",
                        "vector": [1.0, float(index)], "idempotency_key": f"v{index}"})
                result = recall_evidence(client, ["饮品", "时段"], scope="personal", model_key="cpu-fixture",
                    encoder=lambda _: [1.0, 1.0], evidence_groups={"answer": ids})
                self.assertEqual({r["id"] for r in result["items"]}, set(ids))
                self.assertTrue(result["usable"])
                self.assertFalse(result["degraded"])
                encoder = RuleEncoder({"饮品": "茶", "时段": "晚上"})
                indexed = index_evidence(client, scope="personal", encoder=encoder, model_key=encoder.model_key,
                                         idempotency_key="offline", record_ids=ids)
                self.assertTrue(all(row["state"] == "indexed" for row in indexed["results"]))
                offline = recall_evidence(client, ["饮品", "时段"], scope="personal", encoder=encoder,
                                           model_key=encoder.model_key, evidence_groups={"answer": ids})
                self.assertTrue(offline["usable"])
                self.assertEqual({row["id"] for row in offline["items"]}, set(ids))
            finally:
                daemon.close()


if __name__ == "__main__":
    unittest.main()
