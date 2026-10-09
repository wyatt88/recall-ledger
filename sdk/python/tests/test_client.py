import json
import os
import socket
import tempfile
import threading
import unittest

from recall_ledger import Client, MemoryError, TransportError
from recall_ledger.client import MAX_FRAME_BYTES


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


if __name__ == "__main__":
    unittest.main()
