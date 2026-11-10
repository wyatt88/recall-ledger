"""Explicit preview/apply migration for native notes and portable ledgers.

Native notes become pending candidates. Portable imports preserve reviewed
admission states through the core's transactional import. Neither path moves
execution authority, credentials, model state, or installed host bindings.
"""

import copy
import hashlib
import json
import re

from .client import Client, validate_binding
from .semantic import qualification_flags

MAX_BYTES = 512 * 1024
NATIVE_SCHEMA = "recall-ledger.native-memory/1"


class WizardError(ValueError):
    def __init__(self, code, details=None):
        super().__init__(code)
        self.code, self.details = code, details


def _json(value):
    try:
        raw = json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode()
    except (ValueError, TypeError, UnicodeError, RecursionError) as exc:
        raise WizardError("invalid_json") from exc
    if len(raw) > MAX_BYTES:
        raise WizardError("input_too_large")
    return raw


def _hash(value):
    return hashlib.sha256(_json(value)).hexdigest()


def _seal(value):
    value["digest"] = _hash(value)
    return value


def _verified(value, schema, reviewed_digest=None):
    if not isinstance(value, dict):
        raise WizardError("invalid_plan")
    value = copy.deepcopy(value)
    digest = value.pop("digest", None)
    if value.get("schema") != schema or digest != _hash(value) or (reviewed_digest is not None and reviewed_digest != digest):
        raise WizardError("plan_digest_mismatch")
    return value


def _binding(client, expected=None):
    actual = validate_binding(client.call("binding", {}))
    if expected is not None and actual != validate_binding(expected):
        raise WizardError("binding_mismatch")
    return actual


def _text(value, name, maximum=16384):
    if not isinstance(value, str) or not value.strip() or len(value.encode()) > maximum or "\0" in value:
        raise WizardError("invalid_" + name)
    return value


def _pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise WizardError("duplicate_json_key")
        result[key] = value
    return result


def _parse_json(content):
    if isinstance(content, bytes):
        content = content.decode("utf-8")
    if isinstance(content, str):
        try:
            return json.loads(content, object_pairs_hook=_pairs,
                              parse_constant=lambda _: (_ for _ in ()).throw(WizardError("nonfinite_json")))
        except (json.JSONDecodeError, UnicodeError) as exc:
            raise WizardError("invalid_json") from exc
    _json(content)
    return copy.deepcopy(content)


def preview_native(client, content, *, format, scope, source_name, expected_target=None):
    """Map Markdown sections or a documented JSON note schema to pending intake.

    Markdown is data, including instructions and fenced code. It is never run.
    JSON accepts a list or ``{schema, memories}`` with fields documented below.
    Unknown fields block apply instead of silently dropping them.
    """
    _text(scope, "scope", 256)
    _text(source_name, "source_name", 256)
    if isinstance(content, bytes):
        try:
            content = content.decode("utf-8")
        except UnicodeError as exc:
            raise WizardError("invalid_utf8") from exc
    _json(content)
    binding = _binding(client, expected_target)
    status = client.call("status", {})
    if format not in ("markdown", "json"):
        raise WizardError("unsupported_native_format")
    blockers, warnings, candidates = [], [], []
    if format == "markdown":
        if isinstance(content, bytes):
            content = content.decode("utf-8")
        _text(content, "content", MAX_BYTES)
        # Keep a heading and its complete block together; do not truncate code,
        # qualifications, or infer that a Markdown rule is an accepted fact.
        rows, block, in_fence = [], [], False
        for line in content.splitlines():
            if line.lstrip().startswith(("```", "~~~")):
                in_fence = not in_fence
            if not in_fence and re.match(r"^#{1,6}\s", line) and block:
                rows.append({"text": "\n".join(block).strip(), "kind": "episode"})
                block = []
            block.append(line)
        if block:
            rows.append({"text": "\n".join(block).strip(), "kind": "episode"})
        rows = [row for row in rows if row["text"]]
        warnings.append("markdown_sections_are_untrusted_episodes")
    else:
        parsed = _parse_json(content)
        if isinstance(parsed, dict):
            unknown = set(parsed) - {"schema", "memories"}
            blockers += [{"reason": "unknown_field", "path": key} for key in sorted(unknown)]
            if parsed.get("schema") != NATIVE_SCHEMA:
                blockers.append({"reason": "unsupported_native_schema"})
            rows = parsed.get("memories")
        else:
            rows = parsed
        if not isinstance(rows, list):
            raise WizardError("memories_must_be_array")
    if not 1 <= len(rows) <= 100:
        raise WizardError("native_item_limit")
    allowed = {"id", "text", "scope", "entity", "attribute", "conditions", "kind", "source"}
    seen_ids = set()
    for index, row in enumerate(rows):
        if not isinstance(row, dict):
            blockers.append({"reason": "memory_must_be_object", "index": index})
            continue
        unknown = set(row) - allowed
        blockers += [{"reason": "unknown_field", "path": f"memories[{index}].{key}"} for key in sorted(unknown)]
        body = _text(row.get("text"), "memory_text")
        row_scope = row.get("scope", scope)
        if row_scope != scope:
            blockers.append({"reason": "scope_mismatch", "index": index, "declared_scope": row_scope})
        item_id = row.get("id")
        if item_id is None:
            item_id = hashlib.sha256(body.encode()).hexdigest()[:24]
            warnings.append("content_derived_identity_requires_review_after_edits")
        _text(item_id, "memory_id", 128)
        if item_id in seen_ids:
            blockers.append({"reason": "duplicate_native_id", "index": index})
        seen_ids.add(item_id)
        flags = qualification_flags(body)
        kind = row.get("kind", "episode")
        if kind not in ("assertion", "inference", "preference", "episode", "commitment"):
            blockers.append({"reason": "unsupported_native_kind", "index": index})
        if set(flags) & {"quoted", "hypothetical", "uncertain"} and kind not in ("episode", "inference"):
            blockers.append({"reason": "qualified_text_requires_nonassertion_kind", "index": index})
        conditions = row.get("conditions", "native note; applicability not reviewed")
        key = {
            "entity": row.get("entity", "native:" + source_name),
            "attribute": row.get("attribute", "note:" + item_id),
            "scope": row_scope, "conditions": conditions,
        }
        for value in key.values():
            _text(value, "fact_key", 256)
            if any(ord(c) < 32 or 127 <= ord(c) < 160 for c in value):
                raise WizardError("invalid_fact_key")
        source = _text(row.get("source", "native-import:" + source_name), "source", 256)
        event = "native:" + _hash([source_name, item_id, body, key, kind, source])[:48]
        candidates.append({"body": body, "event_id": event, "origin": "import", "source": source,
                           "kind": kind, "fact_key": key})
    return _seal({
        "schema": "recall-ledger.native-plan/1", "format": format, "scope": scope,
        "source_name": source_name, "target_binding": binding, "target_commit": status["commit_token"],
        "input_digest": _hash(content), "candidates": candidates, "blockers": blockers,
        "warnings": sorted(set(warnings)), "can_apply": not blockers,
        "admission": "pending_only", "execution_authorized": False,
        "atomic": False, "semantics_verified": False,
    })


def _key(prefix, plan_digest, candidate):
    _text(prefix, "idempotency_key", 128)
    return "native-" + _hash([prefix, plan_digest, candidate])[:48]


def apply_native(client, plan, *, reviewed_digest, idempotency_key):
    """Apply the selected frozen plan; resume uses the same plan and key.

    This is a bounded sequence of candidate writes, not an atomic batch. Any
    unrelated target commit makes the plan stale. After a lost reply, committed
    contiguous writes are recovered by their original operation keys.
    """
    p = _verified(plan, "recall-ledger.native-plan/1", reviewed_digest)
    _binding(client, p["target_binding"])
    if not p["can_apply"] or p["blockers"]:
        raise WizardError("native_plan_blocked", p["blockers"])
    keys = [_key(idempotency_key, plan["digest"], candidate) for candidate in p["candidates"]]
    receipts = []
    for key in keys:
        try:
            receipt = client.call("operation", {"method": "candidate_submit", "idempotency_key": key})
        except Exception as exc:
            if getattr(exc, "code", None) == "not_found":
                receipts.append(None)
                continue
            raise
        receipts.append(receipt)
    token = p["target_commit"]
    history, seq = token.rsplit(":", 1)
    sequence = int(seq)
    missing_seen = False
    for receipt in receipts:
        if receipt is None:
            missing_seen = True
        else:
            if missing_seen or receipt.get("commit_token") != f"{history}:{sequence + 1}":
                raise WizardError("native_resume_not_contiguous")
            sequence += 1
    current = client.call("status", {})["commit_token"]
    if current != f"{history}:{sequence}":
        raise WizardError("target_changed", {"expected": f"{history}:{sequence}", "actual": current})
    for index, (candidate, key) in enumerate(zip(p["candidates"], keys)):
        if receipts[index] is not None:
            continue
        try:
            current = client.call("status", {})["commit_token"]
            if current != f"{history}:{sequence}":
                raise WizardError("target_changed")
            receipts[index] = client.call("candidate_submit", dict(candidate, idempotency_key=key))
            sequence += 1
            if receipts[index].get("commit_token") != f"{history}:{sequence}":
                raise WizardError("target_changed_during_pending_write")
        except Exception as exc:
            return {
                "state": "outcome_unknown" if getattr(exc, "outcome_unknown", False) else "partial",
                "receipts": [r for r in receipts if r is not None], "completed": sum(r is not None for r in receipts),
                "total": len(receipts), "error_code": getattr(exc, "code", "transport_error"),
                "outcome_unknown": bool(getattr(exc, "outcome_unknown", False)),
                "failed_index": index, "recovery": {"method": "candidate_submit", "idempotency_key": key},
                "retry_requires_same_plan_and_key": True, "automatic_admission": False,
            }
    return {"state": "applied", "receipts": receipts, "completed": len(receipts), "total": len(receipts),
            "atomic": False, "automatic_admission": False, "execution_authorized": False}


def _scope_issues(bundle, scope, allow_unscoped):
    issues = []
    for collection in ("records", "candidates"):
        for index, item in enumerate(bundle.get("payload", {}).get(collection, [])):
            key = item.get("fact_key")
            if key is None and not allow_unscoped:
                issues.append({"reason": "unscoped_item", "collection": collection, "index": index})
            elif key is not None and key.get("scope") != scope:
                issues.append({"reason": "scope_mismatch", "collection": collection, "index": index,
                               "declared_scope": key.get("scope")})
    return issues


def preview_portable(client, bundle, *, expected_source, scope, expected_target=None, allow_unscoped=False):
    """Combine scope review with the core's read-only, transaction-pinned plan."""
    _text(scope, "scope", 256)
    _json(bundle)
    source = validate_binding(expected_source)
    binding = _binding(client, expected_target)
    core = client.call("bundle_plan", {"bundle": bundle, "expected_source": source})
    blockers = _scope_issues(bundle, scope, allow_unscoped)
    return _seal({
        "schema": "recall-ledger.portable-plan/1", "source_binding": source,
        "target_binding": binding, "scope": scope, "bundle_digest": bundle["digest"],
        "core_plan": core, "blockers": blockers,
        "can_apply": bool(core["can_apply"]) and not blockers,
        "preserves_reviewed_admission_states": True, "execution_authorized": False,
        "semantics_verified": False, "allow_unscoped": allow_unscoped,
    })


def apply_portable(client, bundle, plan, *, reviewed_digest, idempotency_key):
    """Apply exactly the reviewed bundle and target commit; never re-plan."""
    p = _verified(plan, "recall-ledger.portable-plan/1", reviewed_digest)
    _binding(client, p["target_binding"])
    if not p["can_apply"] or p["blockers"]:
        raise WizardError("portable_plan_blocked", p["blockers"])
    if bundle.get("digest") != p["bundle_digest"]:
        raise WizardError("bundle_digest_mismatch")
    return client.call("bundle_apply", {
        "bundle": bundle, "expected_source": p["source_binding"],
        "reviewed_digest": p["core_plan"]["reviewed_digest"],
        "expected_target_commit": p["core_plan"]["target_commit"], "idempotency_key": idempotency_key,
    })


def export_selection(client, record_ids, *, scope, allow_unscoped=False):
    """Check ancestor closure and export selected *current* notes for intake.

    Core portable/1 exports a whole namespace. We do not forge a partial
    portable watermark. This selected JSON is reimported as pending candidates;
    original identity/version links are included only as a separate audit map.
    """
    _text(scope, "scope", 256)
    if not isinstance(record_ids, list) or not 1 <= len(record_ids) <= 100 or any(
        not isinstance(rid, str) or not rid for rid in record_ids
    ) or len(set(record_ids)) != len(record_ids):
        raise WizardError("invalid_record_selection")
    binding = _binding(client)
    records = {}

    def visit(rid, stack):
        if rid in stack:
            raise WizardError("dependency_cycle")
        if rid in records:
            return
        if len(records) + len(stack) >= 100:
            raise WizardError("selection_closure_limit")
        row = client.call("inspect", {"id": rid, "history": False})
        key = row.get("fact_key")
        if key is None and not allow_unscoped:
            raise WizardError("unscoped_item", {"id": rid})
        if key is not None and key["scope"] != scope:
            raise WizardError("scope_mismatch", {"id": rid})
        for ref in row.get("dependencies", []):
            visit(ref["id"], stack | {rid})
            if records[ref["id"]]["revision"] != ref["revision"]:
                raise WizardError("changed_dependency", {"id": ref["id"]})
        records[rid] = row

    for rid in record_ids:
        visit(rid, set())
    status = client.call("status", {})
    manifest = {"schema_version": "recall-ledger.context/1", "namespace": binding["namespace"],
                "commit_token": status["commit_token"],
                "records": [{"id": rid, "revision": row["revision"]} for rid, row in records.items()]}
    check = client.call("validate_context", {"manifest": manifest})
    if check.get("valid") is not True:
        raise WizardError("selection_stale", check)
    notes = []
    for rid, row in records.items():
        note = {"id": rid, "text": row["body"], "scope": scope, "kind": "episode",
                "source": "selected-import:" + binding["history_id"]}
        key = row.get("fact_key")
        if key:
            note.update({k: key[k] for k in ("entity", "attribute", "conditions")})
        notes.append(note)
    return {
        "schema": "recall-ledger.selected-export/1", "source_binding": binding,
        "requested_ids": record_ids, "included_ids": list(records),
        "ancestor_ids": [rid for rid in records if rid not in record_ids],
        "closure_complete": True, "manifest": manifest,
        "native_json": {"schema": NATIVE_SCHEMA, "memories": notes},
        "audit_dependencies": {rid: row.get("dependencies", []) for rid, row in records.items()},
        "portable_apply_supported": False, "transfer_semantics": "pending_candidates_without_native_lineage",
        "execution_authorized": False,
    }


class _CallBridge(Client):
    def __init__(self, delegate, binding):
        super().__init__("/unused", expected_binding=binding)
        self.delegate = delegate

    def call(self, method, params=None):
        _binding(self.delegate, self.expected_binding)
        return self.delegate.call(method, params or {})


def prepare_handoff_packet(client, task, manifest, *, expected_source):
    """Prepare a data-only task packet through a binding-pinned call facade."""
    from .handoff import prepare_handoff
    binding = _binding(client, expected_source)
    return prepare_handoff(_CallBridge(client, binding), task, manifest)


def transfer_handoff(client, packet, *, expected_target, bundle=None):
    """Verify a same-ledger handoff or rebase a previously imported bundle.

    Does not import, launch a host, change its binding, or replay any task.
    """
    from .handoff import inspect_handoff, rebase_handoff, verify_handoff
    binding = _binding(client, expected_target)
    inspect_handoff(packet)
    bridge = _CallBridge(client, binding)
    if packet["evidence"]["binding"] == binding:
        result = copy.deepcopy(packet)
    else:
        if bundle is None:
            raise WizardError("reviewed_import_required")
        result = rebase_handoff(bridge, packet, bundle)
    verification = verify_handoff(bridge, result)
    return {"packet": result, "verification": verification,
            "state": "ready_for_receiver_review" if verification["evidence_valid"] else "stale",
            "execution_authorized": False, "host_binding_changed": False}
