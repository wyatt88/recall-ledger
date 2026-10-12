"""Bounded task handoffs containing data and versioned evidence references.

The digest detects accidental modification, not a forged sender. Verification
checks the referenced ledger revisions; it does not verify task prose,
artifacts, environment conditions, or permission to execute the next step.

SDK handoff between two hosts sharing one ledger::

    a = Client(socket_path, expected_binding=chosen_binding)
    b = Client(socket_path, expected_binding=chosen_binding)
    packet_a = a.prepare_handoff(task, a.recall(mode="lexical")["manifest"])
    check_b = b.verify_handoff(packet_a)
    # B separately reviews environment, artifacts and execution authorization.
    # After an authorized revision, A's old packet fails evidence verification.
    packet_b = b.prepare_handoff(updated_task, b.recall(mode="lexical")["manifest"])
    check_a = a.verify_handoff(packet_b)

No function here runs task commands, opens artifact URIs, or authorizes tools.
For a different ledger, explicitly migrate a reviewed portable bundle first,
then call rebase_handoff on the pinned target client. The daemon verifies
imported source mappings and returns a target manifest; a second validation
checks it again before a new package is created. The original is unchanged.
"""

import argparse
import copy
from datetime import datetime, timezone
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import sys
import tempfile

from .client import Client, MemoryError, TransportError, validate_binding

SCHEMA = "recall-ledger.handoff/1"
MAX_HANDOFF_BYTES = 64 * 1024
MAX_BUNDLE_BYTES = 512 * 1024
MAX_REFERENCES = 100
POLICY = {
    "trust_boundary": "untrusted_data",
    "tool_authorization": False,
    "task_replay": False,
}


class HandoffError(Exception):
    """Body-free validation error; the code is safe to include in CLI output."""

    def __init__(self, code):
        super().__init__(code)
        self.code = code


def _canonical(value):
    try:
        return json.dumps(
            value, ensure_ascii=False, sort_keys=True, separators=(",", ":"),
            allow_nan=False,
        ).encode("utf-8")
    except (ValueError, TypeError, UnicodeError, RecursionError) as exc:
        raise HandoffError("invalid_handoff_json") from exc


def _object(value, required, optional=()):
    if not isinstance(value, dict) or set(value) - set(required) - set(optional) or set(required) - set(value):
        raise HandoffError("invalid_handoff_fields")


def _text(value, *, limit=8192):
    if not isinstance(value, str) or len(value) > limit or not value.strip():
        raise HandoffError("invalid_handoff_text")
    try:
        encoded = value.encode("utf-8")
    except UnicodeError as exc:
        raise HandoffError("invalid_handoff_text") from exc
    if len(encoded) > limit or "\0" in value:
        raise HandoffError("invalid_handoff_text")


def _array(value, *, maximum=100):
    if not isinstance(value, list) or len(value) > maximum:
        raise HandoffError("invalid_handoff_array")


def _task(value):
    _object(value, ("task_id", "goal", "progress", "open_questions", "artifacts", "environment_checks"))
    _text(value["task_id"], limit=256)
    _text(value["goal"])
    for key in ("progress", "open_questions"):
        _array(value[key])
        for text in value[key]:
            _text(text, limit=4000)
    _array(value["artifacts"])
    for artifact in value["artifacts"]:
        _object(artifact, ("uri",), ("description", "sha256"))
        _text(artifact["uri"], limit=4096)
        if "description" in artifact:
            _text(artifact["description"], limit=4000)
        if "sha256" in artifact and (
            not isinstance(artifact["sha256"], str)
            or re.fullmatch(r"[0-9a-f]{64}", artifact["sha256"]) is None
        ):
            raise HandoffError("invalid_artifact_digest")
    _array(value["environment_checks"])
    for condition in value["environment_checks"]:
        _object(condition, ("condition",), ("reason",))
        _text(condition["condition"], limit=4000)
        if "reason" in condition:
            _text(condition["reason"], limit=4000)


def _manifest(value, expected):
    _object(value, ("schema_version", "namespace", "commit_token", "records"))
    if value["schema_version"] != "recall-ledger.context/1":
        raise HandoffError("unsupported_context_schema")
    if value["namespace"] != expected["namespace"]:
        raise HandoffError("handoff_binding_mismatch")
    token = value["commit_token"]
    if not isinstance(token, str) or re.fullmatch(
        re.escape(expected["history_id"]) + r":[0-9]+", token
    ) is None:
        raise HandoffError("handoff_binding_mismatch")
    _array(value["records"], maximum=MAX_REFERENCES)
    seen = set()
    for reference in value["records"]:
        _object(reference, ("id", "revision"))
        _text(reference["id"], limit=256)
        if reference["id"] in seen:
            raise HandoffError("duplicate_evidence_reference")
        seen.add(reference["id"])
        revision = reference["revision"]
        if type(revision) is not int or not 1 <= revision <= (1 << 63) - 1:
            raise HandoffError("invalid_evidence_revision")


def _payload(packet):
    return {key: value for key, value in packet.items() if key != "digest"}


def _digest(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def _rebase_origin(value):
    _object(value, ("handoff_digest", "bundle_digest", "source_binding", "source_manifest"))
    if not _digest(value["handoff_digest"]) or not _digest(value["bundle_digest"]):
        raise HandoffError("invalid_rebase_digest")
    try:
        expected = validate_binding(value["source_binding"])
    except ValueError as exc:
        raise HandoffError("invalid_rebase_binding") from exc
    _manifest(value["source_manifest"], expected)


def _validate(packet):
    _object(packet, ("schema_version", "task", "evidence", "prepared_at", "policy", "digest"), ("rebased_from",))
    if packet["schema_version"] != SCHEMA:
        raise HandoffError("unsupported_handoff_schema")
    _task(packet["task"])
    if (
        packet["policy"] != POLICY
        or packet["policy"].get("tool_authorization") is not False
        or packet["policy"].get("task_replay") is not False
    ):
        raise HandoffError("handoff_policy_mismatch")
    _text(packet["prepared_at"], limit=40)
    try:
        prepared_at = datetime.fromisoformat(packet["prepared_at"].replace("Z", "+00:00"))
    except ValueError as exc:
        raise HandoffError("invalid_prepared_at") from exc
    if prepared_at.tzinfo is None:
        raise HandoffError("invalid_prepared_at")
    evidence = packet["evidence"]
    _object(evidence, ("binding", "manifest"))
    try:
        expected = validate_binding(evidence["binding"])
    except ValueError as exc:
        raise HandoffError("invalid_handoff_binding") from exc
    _manifest(evidence["manifest"], expected)
    if "rebased_from" in packet:
        _rebase_origin(packet["rebased_from"])
    digest = packet["digest"]
    if not _digest(digest):
        raise HandoffError("invalid_handoff_digest")
    if len(_canonical(packet)) + 1 > MAX_HANDOFF_BYTES:
        raise HandoffError("handoff_too_large")
    actual = hashlib.sha256(_canonical(_payload(packet))).hexdigest()
    if not hmac.compare_digest(digest, actual):
        raise HandoffError("handoff_digest_mismatch")
    return expected


def _bound_client(client, expected=None):
    if not isinstance(client, Client) or client.expected_binding is None:
        raise HandoffError("pinned_client_required")
    chosen = client.expected_binding
    if expected is not None and chosen != expected:
        raise HandoffError("handoff_binding_mismatch")
    return chosen


def prepare_handoff(client, task, manifest):
    """Prepare data only after a read-only evidence check on a pinned client."""
    expected = _bound_client(client)
    _task(task)
    _manifest(manifest, expected)
    packet = {
        "schema_version": SCHEMA,
        "task": copy.deepcopy(task),
        "evidence": {"binding": expected, "manifest": copy.deepcopy(manifest)},
        "prepared_at": datetime.now(timezone.utc).isoformat(),
        "policy": dict(POLICY),
    }
    packet["digest"] = hashlib.sha256(_canonical(packet)).hexdigest()
    _validate(packet)
    checked = client.validate_context(packet["evidence"]["manifest"])
    if checked.get("valid") is not True:
        raise HandoffError("stale_handoff_evidence")
    return packet


def inspect_handoff(packet):
    """Check shape/digest without I/O; never mark referenced evidence verified."""
    packet = copy.deepcopy(packet)
    _validate(packet)
    result = {
        "schema_version": SCHEMA,
        "integrity_valid": True,
        "sender_authenticated": False,
        "digest": packet["digest"],
        "prepared_at": packet["prepared_at"],
        "task": copy.deepcopy(packet["task"]),
        "evidence": copy.deepcopy(packet["evidence"]),
        "evidence_verified": False,
        "execution_authorized": False,
        "trust_boundary": "untrusted_data",
    }
    if "rebased_from" in packet:
        result["rebased_from"] = copy.deepcopy(packet["rebased_from"])
    return result


def verify_handoff(client, packet):
    """Recheck IDs/revisions now; callers still own authorization and environment."""
    packet = copy.deepcopy(packet)
    expected = _validate(packet)
    _bound_client(client, expected)
    validation = client.validate_context(packet["evidence"]["manifest"])
    if not isinstance(validation.get("valid"), bool):
        raise HandoffError("invalid_verification_response")
    return {
        "schema_version": "recall-ledger.handoff-verification/1",
        "task_id": packet["task"]["task_id"],
        "digest": packet["digest"],
        "binding": copy.deepcopy(expected),
        "evidence_valid": validation["valid"],
        "has_evidence": bool(packet["evidence"]["manifest"]["records"]),
        "invalid": copy.deepcopy(validation.get("invalid", [])),
        "commit_token": validation["commit_token"],
        "checked_at": validation.get("checked_at"),
        "task_semantics_verified": False,
        "sender_authenticated": False,
        "artifacts_verified": False,
        "environment_verified": False,
        "requires_artifact_review": bool(packet["task"]["artifacts"]),
        "requires_environment_review": bool(packet["task"]["environment_checks"]),
        "execution_authorized": False,
        "task_replayed": False,
        "delivered_context_erased": False,
        "trust_boundary": "untrusted_data",
    }


def rebase_handoff(target_client, packet, bundle):
    """Explicitly create a target-bound package after verified portable import.

    This read-only operation does not import the bundle or mutate saved host
    bindings. Only direct-predecessor provenance is retained, under the digest.
    """
    packet = copy.deepcopy(packet)
    bundle = copy.deepcopy(bundle)
    source = _validate(packet)
    _bound_client(target_client)
    if len(_canonical(bundle)) > MAX_BUNDLE_BYTES:
        raise HandoffError("bundle_too_large")
    _object(bundle, ("format", "payload", "digest"))
    if bundle["format"] != "recall-ledger.portable/1" or not _digest(bundle["digest"]):
        raise HandoffError("invalid_rebase_bundle")
    mapped = target_client.bundle_context(
        bundle, expected_source=source, manifest=packet["evidence"]["manifest"],
    )
    if (
        not isinstance(mapped, dict)
        or not isinstance(mapped.get("manifest"), dict)
        or mapped.get("evidence_valid") is not True
        or mapped.get("source_binding") != source
        or mapped.get("source_digest") != bundle["digest"]
        or mapped.get("commit_token") != mapped["manifest"].get("commit_token")
    ):
        raise HandoffError("invalid_rebase_response")
    result = prepare_handoff(target_client, packet["task"], mapped["manifest"])
    result["rebased_from"] = {
        "handoff_digest": packet["digest"],
        "bundle_digest": bundle["digest"],
        "source_binding": source,
        "source_manifest": packet["evidence"]["manifest"],
    }
    result["digest"] = hashlib.sha256(_canonical(_payload(result))).hexdigest()
    _validate(result)
    return result


def serialize_handoff(packet):
    packet = copy.deepcopy(packet)
    _validate(packet)
    return _canonical(packet) + b"\n"


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise HandoffError("duplicate_json_key")
        result[key] = value
    return result


def _decode(raw, *, limit=MAX_HANDOFF_BYTES):
    if not isinstance(raw, bytes) or len(raw) > limit:
        raise HandoffError("handoff_too_large")
    try:
        return json.loads(
            raw.decode("utf-8"), object_pairs_hook=_unique_object,
            parse_constant=_reject_constant,
        )
    except (UnicodeError, ValueError, RecursionError) as exc:
        raise HandoffError("invalid_handoff_json") from exc


def _reject_constant(_):
    raise HandoffError("invalid_handoff_json")


def deserialize_handoff(raw):
    value = _decode(raw)
    _validate(value)
    return value


def _path(path):
    path = Path(path).expanduser().absolute()
    if any(part.is_symlink() for part in (path, *path.parents)):
        raise HandoffError("symlink_path_refused")
    return path


def _read_json(path, *, limit=MAX_HANDOFF_BYTES):
    with _path(path).open("rb") as stream:
        return _decode(stream.read(limit + 1), limit=limit)


def load_handoff(path):
    packet = _read_json(path)
    _validate(packet)
    return packet


def save_handoff(packet, path):
    """Publish a private file atomically; never overwrite an existing artifact."""
    data = serialize_handoff(packet)
    target = _path(path)
    fd, temporary = tempfile.mkstemp(prefix=".handoff-", dir=str(target.parent))
    try:
        with os.fdopen(fd, "wb") as stream:
            os.fchmod(stream.fileno(), 0o600)
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        try:
            os.link(temporary, target)
        except FileExistsError as exc:
            raise HandoffError("handoff_output_exists") from exc
        parent_fd = os.open(target.parent, os.O_RDONLY)
        try:
            os.fsync(parent_fd)
        finally:
            os.close(parent_fd)
    finally:
        os.unlink(temporary)
    return {"path": str(target), "digest": packet["digest"], "bytes": len(data)}


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    commands = result.add_subparsers(dest="command", required=True)
    for name in ("prepare", "inspect", "verify", "rebase"):
        command = commands.add_parser(name)
        if name != "inspect":
            command.add_argument("--socket", required=True)
            command.add_argument("--binding-file", required=True)
            command.add_argument("--timeout", type=float, default=5)
        if name == "prepare":
            command.add_argument("--task-file", required=True)
            command.add_argument("--manifest-file", required=True)
            command.add_argument("--output", required=True)
        else:
            command.add_argument("--file", required=True)
            if name == "rebase":
                command.add_argument("--bundle-file", required=True)
                command.add_argument("--output", required=True)
    return result


def main(argv=None):
    args = parser().parse_args(argv)
    try:
        if args.command != "inspect":
            client = Client(
                args.socket, timeout=args.timeout,
                expected_binding=validate_binding(_read_json(args.binding_file)),
            )
        if args.command == "prepare":
            packet = prepare_handoff(client, _read_json(args.task_file), _read_json(args.manifest_file))
            result = save_handoff(packet, args.output)
            result.update(prepared=True, receiver_must_reverify=True, execution_authorized=False)
        elif args.command == "inspect":
            result = inspect_handoff(load_handoff(args.file))
        elif args.command == "rebase":
            packet = rebase_handoff(
                client, load_handoff(args.file),
                _read_json(args.bundle_file, limit=MAX_BUNDLE_BYTES),
            )
            result = save_handoff(packet, args.output)
            result.update(rebased=True, receiver_must_reverify=True, execution_authorized=False)
        else:
            result = verify_handoff(client, load_handoff(args.file))
        sys.stdout.buffer.write(_canonical(result) + b"\n")
        return 2 if args.command == "verify" and not result["evidence_valid"] else 0
    except (HandoffError, MemoryError, TransportError, OSError, ValueError, TypeError, KeyError) as exc:
        code = exc.code if isinstance(exc, (HandoffError, MemoryError)) else type(exc).__name__
        sys.stderr.write("RecallLedger handoff error: " + code + "\n")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
