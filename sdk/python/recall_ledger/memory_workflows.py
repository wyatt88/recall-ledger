"""Bounded, binding-pinned memory workflows for a local daemon.

Run with ``python -m recall_ledger.memory_workflows --help``. This command never
installs a host, calls a model, delivers a reminder, or executes remembered task
text. Mutation commands require caller-provided operation keys. Reviewed plans
are applied exactly as supplied; a stale plan is never refreshed automatically.
"""

import argparse
import json
import os
from pathlib import Path
import stat
import sys
import tempfile

from .client import Client, MemoryError, TransportError, validate_binding
from . import migration_wizard as wizard
from . import retrieval, semantic
from .commitments import CommitmentStore
from .experience import ExperienceStore
from .handoff import HandoffError

MAX_INPUT_BYTES = 512 * 1024
COMMANDS = (
    "semantic-preview", "semantic-capture", "semantic-review", "semantic-resolve",
    "evidence-query", "evidence-index",
    "native-preview", "native-apply", "portable-export", "portable-preview", "portable-apply",
    "selected-export", "handoff-prepare", "handoff-receive",
    "experience-inspect", "commitment-poll",
)
MUTATIONS = {"semantic-capture", "semantic-resolve", "evidence-index", "native-apply", "portable-apply"}


class WorkflowError(ValueError):
    def __init__(self, code):
        super().__init__(code)
        self.code = code


def _pairs(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise WorkflowError("duplicate_json_key")
        value[key] = item
    return value


def _constant(_):
    raise WorkflowError("nonfinite_json")


def _path(value):
    path = Path(value).expanduser().absolute()
    if any(part.is_symlink() for part in (path, *path.parents)):
        raise WorkflowError("symlink_path_refused")
    return path


def _read(path):
    path = _path(path)
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_size > MAX_INPUT_BYTES:
            raise WorkflowError("unsupported_or_large_input")
        with os.fdopen(fd, "rb", closefd=False) as stream:
            data = stream.read(MAX_INPUT_BYTES + 1)
        if len(data) > MAX_INPUT_BYTES:
            raise WorkflowError("input_too_large")
        return json.loads(data.decode("utf-8"), object_pairs_hook=_pairs, parse_constant=_constant)
    finally:
        os.close(fd)


def _write_new(path, value):
    target = _path(path)
    encoded = json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2, allow_nan=False).encode() + b"\n"
    fd, temporary = tempfile.mkstemp(prefix=".memory-workflow-", dir=target.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            os.fchmod(stream.fileno(), 0o600)
            stream.write(encoded)
            stream.flush()
            os.fsync(stream.fileno())
        try:
            os.link(temporary, target)
        except FileExistsError as exc:
            raise WorkflowError("output_exists") from exc
        parent = os.open(target.parent, os.O_RDONLY)
        try:
            os.fsync(parent)
        finally:
            os.close(parent)
    finally:
        os.unlink(temporary)


def _fields(value, required, optional=()):
    if not isinstance(value, dict):
        raise WorkflowError("request_must_be_object")
    if set(value) - set(required) - set(optional):
        raise WorkflowError("unknown_request_field")
    if set(required) - set(value):
        raise WorkflowError("missing_request_field")
    return dict(value)


def _encoder(value):
    if value is None:
        return None
    config = _fields(value, ("type",), ("aliases", "dimensions"))
    if config.pop("type") != "rules":
        raise WorkflowError("cli_encoder_must_be_explicit_rules")
    return retrieval.RuleEncoder(**config)


def dispatch(client, command, request, *, expected_binding):
    """Use known workflows only; a request cannot select arbitrary core methods."""
    expected = validate_binding(expected_binding)
    if client.call("binding", {}) != expected:
        raise WorkflowError("binding_mismatch")
    if command in ("semantic-preview", "semantic-capture"):
        allowed = ("fact_key",)
        if command == "semantic-preview":
            args = _fields(request, ("body", "scope"), allowed + ("aliases", "limit"))
            aliases = args.pop("aliases", None)
            if aliases is not None:
                args["matcher"] = semantic.RuleMatcher(aliases)
            return semantic.preview_candidate(client, **args)
        args = _fields(request, ("body", "scope", "event_id", "source", "idempotency_key"),
                       allowed + ("origin", "kind"))
        return semantic.capture_candidate(client, **args)
    if command == "semantic-review":
        return semantic.review_candidate(client, **_fields(request, ("candidate_id", "scope")))
    if command == "semantic-resolve":
        return semantic.resolve_candidate(client, **_fields(
            request, ("review", "decision", "idempotency_key"), ("acknowledged_flags",)))
    if command in ("evidence-query", "evidence-index"):
        if command == "evidence-query":
            args = _fields(request, ("queries", "scope"), (
                "max_chars", "limit", "mode", "encoder", "model_key", "evidence_groups",
                "allow_fallback", "allow_unscoped", "condition_context"))
        else:
            args = _fields(request, ("scope", "encoder", "idempotency_key"), (
                "model_key", "limit", "allow_unscoped", "record_ids"))
        encoder = _encoder(args.pop("encoder", None))
        if encoder is not None:
            if args.get("model_key", encoder.model_key) != encoder.model_key:
                raise WorkflowError("rule_encoder_model_key_mismatch")
            args.update(encoder=encoder, model_key=encoder.model_key)
        elif command == "evidence-index":
            raise WorkflowError("index_encoder_required")
        function = retrieval.recall_evidence if command == "evidence-query" else retrieval.index_evidence
        return function(client, **args)
    if command == "native-preview":
        return wizard.preview_native(client, **_fields(
            request, ("content", "format", "scope", "source_name")), expected_target=expected)
    if command == "native-apply":
        return wizard.apply_native(client, **_fields(
            request, ("plan", "reviewed_digest", "idempotency_key")))
    if command == "portable-export":
        _fields(request, ())
        return client.call("bundle_export", {})
    if command == "portable-preview":
        return wizard.preview_portable(client, **_fields(
            request, ("bundle", "expected_source", "scope"), ("allow_unscoped",)),
            expected_target=expected)
    if command == "portable-apply":
        return wizard.apply_portable(client, **_fields(
            request, ("bundle", "plan", "reviewed_digest", "idempotency_key")))
    if command == "selected-export":
        return wizard.export_selection(client, **_fields(
            request, ("record_ids", "scope"), ("allow_unscoped",)))
    if command == "handoff-prepare":
        return wizard.prepare_handoff_packet(client, **_fields(
            request, ("task", "manifest")), expected_source=expected)
    if command == "handoff-receive":
        return wizard.transfer_handoff(client, **_fields(
            request, ("packet",), ("bundle",)), expected_target=expected)
    if command == "experience-inspect":
        # There is no generic CLI receipt authenticator. A stored "verified"
        # boolean or an input file must never become a substitute for one.
        return ExperienceStore(client, expected_binding=expected).inspect(**_fields(
            request, ("card_id", "snapshot", "now"), ("max_age_seconds",)))
    if command == "commitment-poll":
        return CommitmentStore(client, expected_binding=expected).poll(**_fields(
            request, ("now", "signals"), ("limit",)))
    raise WorkflowError("unknown_workflow")


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--socket", required=True, help="Existing local daemon socket.")
    result.add_argument("--binding-file", required=True, help="Previously verified namespace/history JSON.")
    result.add_argument("--timeout", type=float, default=5.0)
    result.add_argument("--request-file", help="Bounded JSON request; defaults to an empty object.")
    result.add_argument("--output", help="New private result file; existing files are refused.")
    result.add_argument("workflow", choices=COMMANDS)
    return result


def main(argv=None):
    args = parser().parse_args(argv)
    dispatched = False
    result_ready = False
    try:
        if not 0 < args.timeout <= 330:
            raise WorkflowError("invalid_timeout")
        expected = validate_binding(_read(args.binding_file))
        request = _read(args.request_file) if args.request_file else {}
        # Fail before mutations if a result would overwrite existing material.
        if args.output and _path(args.output).exists():
            raise WorkflowError("output_exists")
        client = Client(args.socket, timeout=args.timeout, expected_binding=expected)
        dispatched = True
        value = dispatch(client, args.workflow, request, expected_binding=expected)
        result_ready = True
        if args.output:
            _write_new(args.output, value)
            print(json.dumps({"ok": True, "path": str(_path(args.output)),
                              "workflow": args.workflow}, ensure_ascii=False))
        else:
            print(json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2, allow_nan=False))
        incomplete = value.get("state") in ("partial", "outcome_unknown")
        if args.workflow == "evidence-index":
            incomplete = incomplete or any(
                item.get("state") in ("failed", "outcome_unknown") for item in value.get("results", []))
        return 2 if incomplete else 0
    except TransportError as exc:
        print(json.dumps({"ok": False, "error": "transport_error",
                          "outcome_unknown": exc.outcome_unknown,
                          "retry_requires_same_request_and_key": bool(exc.outcome_unknown)}), file=sys.stderr)
        return 2
    except (MemoryError, WorkflowError, HandoffError, wizard.WizardError, semantic.SemanticError,
            retrieval.RetrievalError) as exc:
        print(json.dumps({"ok": False, "error": exc.code,
                          "automatic_plan_refresh": False,
                          "operation_may_have_committed": result_ready and args.workflow in MUTATIONS,
                          "retry_requires_same_request_and_key": result_ready and args.workflow in MUTATIONS}),
              file=sys.stderr)
        return 2
    except (OSError, ValueError, TypeError, KeyError, UnicodeError, RecursionError) as exc:
        # Never echo the request or a body's repr into error diagnostics.
        print(json.dumps({"ok": False, "error": "invalid_request_or_local_file",
                          "error_type": type(exc).__name__,
                          "operation_may_have_committed": dispatched and args.workflow in MUTATIONS,
                          "retry_requires_same_request_and_key": dispatched and args.workflow in MUTATIONS}),
              file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
