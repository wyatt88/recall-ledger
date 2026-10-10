import json
import os
import socket
import tempfile
import threading
import unittest

from recall_ledger import Client, MemoryError, TransportError
from recall_ledger.client import MAX_FRAME_BYTES

BINDING = {
    "namespace": "fixture",
    "history_id": "12345678-1234-5678-9234-567812345678",
}


@unittest.skipUnless(hasattr(socket, "AF_UNIX"), "Unix sockets require macOS or Linux")
class ClientTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="rl-", dir="/tmp")
        self.socket_path = os.path.join(self.directory.name, "s")
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(self.socket_path)
        self.listener.listen(1)
        self.listener.settimeout(3)
        self.client = Client(self.socket_path, timeout=2)
        self.failures = []
        self.worker = None

    def tearDown(self):
        if self.worker:
            self.worker.join(timeout=4)
            self.assertFalse(self.worker.is_alive(), "fake daemon worker did not stop")
        self.listener.close()
        self.directory.cleanup()
        if self.failures:
            raise self.failures[0]

    def serve(self, handler):
        def work():
            try:
                with self.listener.accept()[0] as stream:
                    stream.settimeout(2)
                    with stream.makefile("rb") as reader:
                        request = json.loads(reader.readline(MAX_FRAME_BYTES + 1))
                    handler(stream, request)
            except Exception as exc:
                self.failures.append(exc)

        self.worker = threading.Thread(target=work)
        self.worker.start()

    def test_unicode_roundtrip_and_stable_write_key(self):
        def handler(stream, request):
            self.assertEqual(request["method"], "remember")
            self.assertEqual(request["params"]["body"], "决定使用 Rust")
            self.assertEqual(request["params"]["idempotency_key"], "turn-123")
            stream.sendall(json.dumps({"id": request["id"], "result": {"revision": 1}}).encode() + b"\n")

        self.serve(handler)
        result = self.client.remember("决定使用 Rust", idempotency_key="turn-123", kind="commitment")
        self.assertEqual(result, {"revision": 1})

    def test_structured_error_preserved(self):
        def handler(stream, request):
            stream.sendall(json.dumps({"id": request["id"], "error": {"code": "stale_dependency", "message": "changed source", "retryable": False}}).encode() + b"\n")

        self.serve(handler)
        with self.assertRaises(MemoryError) as caught:
            self.client.call("publish", {})
        self.assertEqual(caught.exception.code, "stale_dependency")
        self.assertFalse(caught.exception.retryable)

    def test_lost_receipt_is_unknown_not_automatically_retried(self):
        self.serve(lambda stream, request: None)
        with self.assertRaises(TransportError) as caught:
            self.client.remember("body", idempotency_key="original-key")
        self.assertTrue(caught.exception.outcome_unknown)

    def test_namespace_override_rejected_before_connect(self):
        with self.assertRaises(ValueError):
            self.client.call("recall", {"namespace": "other"})

    def test_binding_metadata_uses_no_parameters_or_implicit_pin(self):
        def handler(stream, request):
            self.assertEqual(request["method"], "binding")
            self.assertEqual(request["params"], {})
            self.assertNotIn("expected_binding", request)
            stream.sendall(json.dumps({"id": request["id"], "result": BINDING}).encode() + b"\n")

        self.serve(handler)
        self.assertEqual(self.client.binding(), BINDING)
        self.assertIsNone(self.client.expected_binding)

    def test_expected_binding_is_pinned_in_envelope_and_cannot_be_mutated_by_caller(self):
        selected = dict(BINDING)
        self.client = Client(self.socket_path, timeout=2, expected_binding=selected)
        selected["namespace"] = "other"
        returned = self.client.expected_binding
        returned["history_id"] = "87654321-4321-8765-9234-567812345678"

        def handler(stream, request):
            self.assertEqual(request["expected_binding"], BINDING)
            self.assertNotIn("expected_binding", request["params"])
            self.assertEqual(request["method"], "operation")
            stream.sendall(json.dumps({"id": request["id"], "result": {"found": True}}).encode() + b"\n")

        self.serve(handler)
        self.assertEqual(self.client.operation("remember", "original-key"), {"found": True})
        self.assertEqual(self.client.expected_binding, BINDING)

    def test_invalid_bindings_rejected_before_connect(self):
        malformed = [
            {}, [], {"namespace": "fixture"}, dict(BINDING, extra=True),
            dict(BINDING, namespace=""), dict(BINDING, namespace="记" * 86),
            dict(BINDING, namespace="line\nbreak"), dict(BINDING, namespace="\u0085"),
            dict(BINDING, namespace=1), dict(BINDING, history_id=None),
            dict(BINDING, history_id="not-a-uuid"),
            dict(BINDING, history_id="x" * 36),
            dict(BINDING, history_id="12345678123456789234567812345678"),
            dict(BINDING, history_id="12345678123456789234567812345678----"),
        ]
        for binding in malformed:
            with self.subTest(binding=binding), self.assertRaises(ValueError):
                Client(self.socket_path, expected_binding=binding)
        self.assertEqual(
            Client(self.socket_path, expected_binding=dict(BINDING, namespace="记" * 85 + "a")).expected_binding["namespace"],
            "记" * 85 + "a",
        )

    def test_invalid_binding_response_is_not_accepted(self):
        def handler(stream, request):
            stream.sendall(json.dumps({"id": request["id"], "result": {"namespace": "fixture"}}).encode() + b"\n")

        self.serve(handler)
        with self.assertRaisesRegex(TransportError, "invalid binding response"):
            self.client.binding()

    def test_binding_mismatch_preserves_nonretryable_error(self):
        self.client = Client(self.socket_path, timeout=2, expected_binding=BINDING)

        def handler(stream, request):
            self.assertEqual(request["expected_binding"], BINDING)
            stream.sendall(json.dumps({"id": request["id"], "error": {
                "code": "binding_mismatch", "message": "daemon identity differs", "retryable": False,
            }}).encode() + b"\n")

        self.serve(handler)
        with self.assertRaises(MemoryError) as caught:
            self.client.remember("Do not redirect this write", idempotency_key="original-key")
        self.assertEqual(caught.exception.code, "binding_mismatch")
        self.assertFalse(caught.exception.retryable)

    def test_oversized_request_rejected_before_connect(self):
        with self.assertRaises(ValueError):
            self.client.remember("x" * MAX_FRAME_BYTES, idempotency_key="oversize")

    def test_oversized_response_is_bounded(self):
        def handler(stream, request):
            try:
                stream.sendall(b"x" * (MAX_FRAME_BYTES + 1) + b"\n")
            except BrokenPipeError:
                pass

        self.serve(handler)
        with self.assertRaises(TransportError) as caught:
            self.client.call("recall", {})
        self.assertTrue(caught.exception.outcome_unknown)

    def test_wrong_response_id_rejected(self):
        self.serve(lambda stream, request: stream.sendall(b'{"id":"wrong","result":{}}\n'))
        with self.assertRaises(TransportError):
            self.client.call("status")

    def test_summary_returns_queue_receipt_with_original_write_key(self):
        def handler(stream, request):
            self.assertEqual(request["method"], "summarize")
            self.assertEqual(request["params"]["inputs"], ["source-one", "source-two"])
            self.assertEqual(request["params"]["max_chars"], 800)
            self.assertEqual(request["params"]["idempotency_key"], "summary-request-1")
            receipt = {"task_id": "task-1", "output_id": "output-1", "state": "pending"}
            stream.sendall(json.dumps({"id": request["id"], "result": receipt}).encode() + b"\n")

        self.serve(handler)
        result = self.client.summarize(
            ["source-one", "source-two"], max_chars=800, idempotency_key="summary-request-1"
        )
        self.assertEqual(result["state"], "pending")
        self.assertEqual(result["output_id"], "output-1")

    def test_maintenance_status_preserves_failed_task_metadata(self):
        def handler(stream, request):
            self.assertEqual(request["method"], "maintenance_status")
            self.assertEqual(request["params"], {"limit": 20})
            result = {"tasks": [{"task_id": "task-1", "state": "failed", "error_code": "model_timeout"}]}
            stream.sendall(json.dumps({"id": request["id"], "result": result}).encode() + b"\n")

        self.serve(handler)
        result = self.client.maintenance_status(limit=20)
        self.assertEqual(result["tasks"][0]["error_code"], "model_timeout")

    def test_recall_mode_and_coverage_are_preserved(self):
        def handler(stream, request):
            self.assertEqual(request["params"]["mode"], "hybrid")
            result = {"items": [], "coverage": {"degraded": True, "mode": "lexical"}}
            stream.sendall(json.dumps({"id": request["id"], "result": result}).encode() + b"\n")

        self.serve(handler)
        result = self.client.recall("remembered topic", mode="hybrid")
        self.assertTrue(result["coverage"]["degraded"])

    def test_model_compatible_timeout_defaults_and_invalid_values(self):
        self.assertEqual(Client(self.socket_path).timeout, 330.0)
        for timeout in (0, -1, float("inf"), float("nan")):
            with self.assertRaises(ValueError):
                Client(self.socket_path, timeout=timeout)
        with self.assertRaises(ValueError):
            self.client.summarize("single-string", idempotency_key="invalid")

    def test_candidate_submission_preserves_origin_event_key_and_fact_identity(self):
        fact_key = {"entity": "project", "attribute": "language", "scope": "repo", "conditions": "default"}

        def handler(stream, request):
            self.assertEqual(request["method"], "candidate_submit")
            self.assertEqual(request["params"], {
                "event_id": "host-event-1", "body": "Rust", "origin": "user_message",
                "source": "fixture", "idempotency_key": "stable-key", "fact_key": fact_key,
            })
            result = {"candidate_id": "candidate-1", "state": "pending", "record_id": None}
            stream.sendall(json.dumps({"id": request["id"], "result": result}).encode() + b"\n")

        self.serve(handler)
        result = self.client.candidate_submit(
            "Rust", event_id="host-event-1", origin="user_message", source="fixture",
            idempotency_key="stable-key", fact_key=fact_key,
        )
        self.assertIsNone(result["record_id"])

    def test_candidate_resolution_sends_explicit_revision_and_original_key(self):
        def handler(stream, request):
            self.assertEqual(request["method"], "candidate_resolve")
            self.assertEqual(request["params"], {
                "candidate_id": "candidate-1", "decision": "accept",
                "expected_revision": 7, "idempotency_key": "review-7",
            })
            stream.sendall(json.dumps({"id": request["id"], "error": {
                "code": "revision_conflict", "message": "record changed", "retryable": False,
            }}).encode() + b"\n")

        self.serve(handler)
        with self.assertRaises(MemoryError) as caught:
            self.client.candidate_resolve("candidate-1", "accept", expected_revision=7, idempotency_key="review-7")
        self.assertEqual(caught.exception.code, "revision_conflict")

    def test_context_validation_passes_exact_manifest(self):
        manifest = {
            "schema_version": "recall-ledger.context/1", "namespace": "fixture",
            "commit_token": "uuid:4", "records": [{"id": "record-1", "revision": 3}],
        }

        def handler(stream, request):
            self.assertEqual(request["method"], "validate_context")
            self.assertEqual(request["params"], {"manifest": manifest})
            result = {"valid": False, "invalid": [{"id": "record-1", "reason": "revision_changed"}], "commit_token": "uuid:5"}
            stream.sendall(json.dumps({"id": request["id"], "result": result}).encode() + b"\n")

        self.serve(handler)
        self.assertFalse(self.client.validate_context(manifest)["valid"])


if __name__ == "__main__":
    unittest.main()
