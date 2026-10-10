"""Bounded synchronous requests to a local Recall Ledger daemon.

No operation is automatically retried. A transport error after sending may mean
that a write committed but its receipt was lost. Recover with ``operation`` using
the original method and idempotency key, or retry identical arguments and key.
"""

import json
import math
import socket
import unicodedata
import uuid
from typing import Any, Dict, Optional, Sequence

MAX_FRAME_BYTES = 1024 * 1024


def validate_binding(value: Any) -> Dict[str, str]:
    """Return a detached, strictly shaped daemon identity without its data."""
    if not isinstance(value, dict) or set(value) != {"namespace", "history_id"}:
        raise ValueError("expected_binding must contain only namespace and history_id")
    namespace, history_id = value["namespace"], value["history_id"]
    if (
        not isinstance(namespace, str)
        or not 1 <= len(namespace.encode("utf-8")) <= 256
        or any(unicodedata.category(char) == "Cc" for char in namespace)
    ):
        raise ValueError("binding namespace must be 1..256 UTF-8 bytes without controls")
    if (
        not isinstance(history_id, str)
        or len(history_id) != 36
        or any(history_id[index] != "-" for index in (8, 13, 18, 23))
    ):
        raise ValueError("binding history_id must be a UUID")
    try:
        uuid.UUID(history_id)
    except ValueError as exc:
        raise ValueError("binding history_id must be a UUID") from exc
    return {"namespace": namespace, "history_id": history_id}


class MemoryError(Exception):
    """Structured error returned by the daemon."""

    def __init__(self, code: str, message: str, retryable: bool = False):
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message
        self.retryable = retryable


class TransportError(Exception):
    """IPC failure; ``outcome_unknown`` prevents unsafe automatic write retries."""

    def __init__(self, message: str, outcome_unknown: bool):
        super().__init__(message)
        self.outcome_unknown = outcome_unknown


class Client:
    """Thread-safe client: every call owns a separate bounded socket connection.

    ``socket_path`` chooses an already configured daemon. ``expected_binding``
    pins its namespace and history identity on every request, including reads
    and receipt recovery. A socket pathname alone does not establish identity.
    Host application hooks must decide what to record and when to retrieve it.
    """

    def __init__(
        self, socket_path: str, timeout: float = 330.0,
        expected_binding: Optional[Dict[str, str]] = None,
    ):
        if not math.isfinite(timeout) or timeout <= 0:
            raise ValueError("timeout must be positive and finite")
        self.socket_path = str(socket_path)
        self.timeout = timeout
        self._expected_binding = (
            validate_binding(expected_binding) if expected_binding is not None else None
        )

    @property
    def expected_binding(self) -> Optional[Dict[str, str]]:
        """A detached copy; mutating a caller's dictionary cannot rebind a client."""
        return dict(self._expected_binding) if self._expected_binding is not None else None

    def call(self, method: str, params: Optional[Dict[str, Any]] = None) -> Any:
        if not isinstance(method, str) or not method:
            raise ValueError("method must be a nonempty string")
        if params is None:
            params = {}
        if not isinstance(params, dict) or "namespace" in params:
            raise ValueError("params must be a dict without namespace")
        request_id = str(uuid.uuid4())
        request = {"id": request_id, "method": method, "params": params}
        if self._expected_binding is not None:
            request["expected_binding"] = self.expected_binding
        payload = (
            json.dumps(
                request,
                ensure_ascii=False,
                separators=(",", ":"),
                allow_nan=False,
            ).encode("utf-8")
            + b"\n"
        )
        if len(payload) > MAX_FRAME_BYTES:
            raise ValueError("request exceeds 1 MiB")
        sent = False
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
                stream.settimeout(self.timeout)
                stream.connect(self.socket_path)
                # sendall can fail after a partial or full write.
                sent = True
                stream.sendall(payload)
                with stream.makefile("rb") as reader:
                    frame = reader.readline(MAX_FRAME_BYTES + 1)
        except OSError as exc:
            raise TransportError(str(exc), outcome_unknown=sent) from exc
        if not frame:
            raise TransportError("daemon closed without a response", outcome_unknown=True)
        if len(frame) > MAX_FRAME_BYTES or not frame.endswith(b"\n"):
            raise TransportError("oversized or unterminated response", outcome_unknown=True)
        try:
            response = json.loads(frame)
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise TransportError("invalid JSON response", outcome_unknown=True) from exc
        if not isinstance(response, dict) or response.get("id") != request_id:
            raise TransportError("response ID mismatch", outcome_unknown=True)
        if "result" in response and "error" not in response:
            return response["result"]
        if "error" in response and "result" not in response:
            error = response["error"]
            if (
                isinstance(error, dict)
                and isinstance(error.get("code"), str)
                and isinstance(error.get("message"), str)
                and isinstance(error.get("retryable"), bool)
            ):
                raise MemoryError(error["code"], error["message"], error["retryable"])
        raise TransportError("invalid response envelope", outcome_unknown=True)

    def binding(self) -> Dict[str, str]:
        """Read only namespace/history metadata, for explicit initial binding."""
        try:
            return validate_binding(self.call("binding"))
        except ValueError as exc:
            raise TransportError("invalid binding response", outcome_unknown=True) from exc

    def remember(self, body: str, *, idempotency_key: str, **metadata: Any) -> Any:
        return self.call(
            "remember", dict(metadata, body=body, idempotency_key=idempotency_key)
        )

    def recall(self, query: str = "", *, limit: int = 10, max_chars: int = 4000, **options: Any) -> Any:
        return self.call(
            "recall", dict(options, query=query, limit=limit, max_chars=max_chars)
        )

    def inspect(self, record_id: str, *, history: bool = False) -> Any:
        return self.call("inspect", {"id": record_id, "history": history})

    def revise(self, record_id: str, *, expected_revision: int, body: str, idempotency_key: str) -> Any:
        return self.call(
            "revise",
            {
                "id": record_id,
                "expected_revision": expected_revision,
                "body": body,
                "idempotency_key": idempotency_key,
            },
        )

    def retract(self, record_id: str, *, expected_revision: int, idempotency_key: str) -> Any:
        return self.call(
            "retract",
            {
                "id": record_id,
                "expected_revision": expected_revision,
                "idempotency_key": idempotency_key,
            },
        )

    def operation(self, method: str, idempotency_key: str) -> Any:
        """Recover an original receipt; not_found does not mean a retry is unsafe."""
        return self.call("operation", {"method": method, "idempotency_key": idempotency_key})

    def summarize(
        self,
        inputs: Sequence[str],
        *,
        idempotency_key: str,
        max_chars: int = 2000,
    ) -> Any:
        """Enqueue a managed summary; the receipt is not the generated result.

        The daemon must have a summary provider configured. Its worker generates
        the summary and repairs it when a recorded source revision changes.
        """
        if isinstance(inputs, (str, bytes)):
            raise ValueError("inputs must be a sequence of record IDs")
        return self.call(
            "summarize",
            {
                "inputs": list(inputs),
                "max_chars": max_chars,
                "idempotency_key": idempotency_key,
            },
        )

    def maintenance_status(self, *, limit: int = 100) -> Any:
        """Read bounded task metadata and aggregate state counts."""
        return self.call("maintenance_status", {"limit": limit})

    def candidate_submit(
        self,
        body: str,
        *,
        event_id: str,
        origin: str,
        source: str,
        idempotency_key: str,
        **metadata: Any,
    ) -> Any:
        """Submit an untrusted proposal. Submission never admits a fact."""
        return self.call(
            "candidate_submit",
            dict(
                metadata, body=body, event_id=event_id, origin=origin,
                source=source, idempotency_key=idempotency_key,
            ),
        )

    def candidate_list(self, *, state: Optional[str] = None, limit: int = 50) -> Any:
        """Read proposal metadata; use candidate_inspect to review its body."""
        params = {"limit": limit}
        if state is not None:
            params["state"] = state
        return self.call("candidate_list", params)

    def candidate_inspect(self, candidate_id: str) -> Any:
        return self.call("candidate_inspect", {"candidate_id": candidate_id})

    def candidate_resolve(
        self,
        candidate_id: str,
        decision: str,
        *,
        idempotency_key: str,
        expected_revision: Optional[int] = None,
    ) -> Any:
        """Explicit admission/rejection; a revision is required for fact changes."""
        params = {
            "candidate_id": candidate_id,
            "decision": decision,
            "idempotency_key": idempotency_key,
        }
        if expected_revision is not None:
            params["expected_revision"] = expected_revision
        return self.call("candidate_resolve", params)

    def fact_get(self, fact_key: Dict[str, str]) -> Any:
        return self.call("fact_get", {"fact_key": fact_key})

    def changes(self, *, after: Optional[str] = None, limit: int = 100) -> Any:
        """Read invalidation metadata, not changed record bodies."""
        params = {"limit": limit}
        if after is not None:
            params["after"] = after
        return self.call("changes", params)

    def validate_context(self, manifest: Dict[str, Any]) -> Any:
        """Validate cited revisions; cannot erase an existing host prompt."""
        return self.call("validate_context", {"manifest": manifest})
