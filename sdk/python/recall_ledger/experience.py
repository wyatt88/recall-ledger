"""Receipt-backed, revocable experience cards.

A host verifier is mandatory for receipt admission and derivation. This module
does not trust model self-evaluation, execute steps, or grant tool permissions.
"""

import copy
from .environment import (
    _BoundStore, _digest, _environment, _json, _sha, _stamp, _text, _time,
    check_environment,
)
from .client import MemoryError

RECEIPT_SCHEMA = "recall-ledger.tool-receipt/1"
CARD_SCHEMA = "recall-ledger.experience/1"
FEEDBACK_SCHEMA = "recall-ledger.experience-feedback/1"


def _receipt(value, verifier, *, success=False):
    if not isinstance(value, dict) or value.get("schema") != RECEIPT_SCHEMA:
        raise ValueError("invalid_tool_receipt")
    value = copy.deepcopy(value)
    for field in ("receipt_id", "host_id", "tool", "tool_version"):
        _text(value.get(field), field)
    if value.get("status") not in ("succeeded", "failed", "cancelled"):
        raise ValueError("receipt_not_terminal")
    if not _sha(value.get("input_sha256")) or not _sha(value.get("output_sha256")):
        raise ValueError("receipt_requires_input_output_digest")
    if _time(value.get("completed_at")) < _time(value.get("started_at")):
        raise ValueError("invalid_receipt_time_order")
    if type(value.get("exit_code")) is not int:
        raise ValueError("receipt_requires_exit_code")
    checks = value.get("checks")
    if not isinstance(checks, list) or not checks or len(checks) > 128:
        raise ValueError("receipt_requires_checks")
    for check in checks:
        if not isinstance(check, dict) or type(check.get("passed")) is not bool:
            raise ValueError("invalid_receipt_check")
        _text(check.get("name"), "check_name")
    refs = value.get("source_refs", [])
    if not isinstance(refs, list) or len(refs) > 64:
        raise ValueError("invalid_receipt_source_refs")
    seen = set()
    for ref in refs:
        if not isinstance(ref, dict) or set(ref) != {"id", "revision"}:
            raise ValueError("invalid_receipt_source_ref")
        _text(ref["id"], "source_id")
        if ref["id"] in seen or type(ref["revision"]) is not int or ref["revision"] < 1:
            raise ValueError("invalid_receipt_source_ref")
        seen.add(ref["id"])
    successful = value["status"] == "succeeded"
    if successful and (value["exit_code"] != 0 or not all(c["passed"] for c in checks)):
        raise ValueError("inconsistent_success_receipt")
    if success and not successful:
        raise ValueError("experience_requires_successful_receipt")
    if not callable(verifier) or verifier(copy.deepcopy(value)) is not True:
        raise ValueError("unverified_host_receipt")
    _json(value)
    return value


class ExperienceStore(_BoundStore):
    def __init__(self, client, *, expected_binding, receipt_verifier=None):
        super().__init__(client, expected_binding=expected_binding)
        self.receipt_verifier = receipt_verifier

    def record_receipt(self, receipt, *, idempotency_key):
        """Admit a terminal receipt authenticated/checked by the trusted host."""
        receipt = _receipt(receipt, self.receipt_verifier)
        body = {"schema": RECEIPT_SCHEMA, "receipt": receipt}
        refs = receipt.get("source_refs", [])
        if not refs:
            return self._remember(body, kind="episode", source=RECEIPT_SCHEMA,
                                  idempotency_key=idempotency_key)
        prefix = "experience-receipt:" + _digest({"key": _text(idempotency_key, "idempotency_key"),
                                                "binding": self.expected_binding})
        prepared = self._call("prepare", {"inputs": [ref["id"] for ref in refs],
                                          "processor": RECEIPT_SCHEMA, "idempotency_key": prefix + ":prepare"})
        if {(ref["id"], ref["revision"]) for ref in prepared["dependencies"]} != {
                (ref["id"], ref["revision"]) for ref in refs}:
            raise MemoryError("stale_dependency", "Tool input revisions no longer match.")
        body["binding"] = self.expected_binding
        return self._call("publish", {"job_id": prepared["job_id"], "body": _json(body),
                                      "kind": "episode", "idempotency_key": prefix + ":publish"})

    def derive_card(self, receipt_ids, *, title, steps, environment, idempotency_key):
        """Publish an interpretation with revision-bound successful receipts."""
        if not isinstance(receipt_ids, (list, tuple)) or not 1 <= len(receipt_ids) <= 64 or len(set(receipt_ids)) != len(receipt_ids):
            raise ValueError("invalid_receipt_ids")
        _text(title, "title", 1024)
        if not isinstance(steps, list) or not 1 <= len(steps) <= 64:
            raise ValueError("invalid_experience_steps")
        for step in steps:
            _text(step, "step", 4096)
        _environment(environment)
        if check_environment(environment, environment, now=environment["observed_at"])["status"] != "applicable":
            raise ValueError("experience_requires_observed_environment")
        inputs = [self._record(record_id, RECEIPT_SCHEMA) for record_id in receipt_ids]
        receipts = [_receipt(record["receipt"], self.receipt_verifier, success=True) for record in inputs]
        for receipt in receipts:
            if environment["tools"].get(receipt["tool"]) != receipt["tool_version"]:
                raise ValueError("receipt_environment_mismatch")
        body = {
            "schema": CARD_SCHEMA, "binding": self.expected_binding,
            "title": title, "steps": copy.deepcopy(steps),
            "environment": copy.deepcopy(environment),
            "created_at": _stamp(max((r["completed_at"] for r in receipts), key=_time)),
            "receipt_refs": [{"id": r["id"], "revision": r["revision"]} for r in inputs],
            "verification": {"host_receipts_verified": True,
                             "source_dependencies_tracked": sum(len(r.get("source_refs", [])) for r in receipts),
                             "semantic_generalization_verified": False},
            "permissions_granted": False,
        }
        _json(body)
        key = _text(idempotency_key, "idempotency_key")
        # Include the caller intent in both keys; keys stay <=256 bytes.
        prefix = "experience:" + _digest({"key": key, "binding": self.expected_binding})
        prepared = self._call("prepare", {"inputs": list(receipt_ids), "processor": CARD_SCHEMA,
                                          "idempotency_key": prefix + ":prepare"})
        captured = {(d["id"], d["revision"]) for d in prepared["dependencies"]}
        if captured != {(r["id"], r["revision"]) for r in inputs}:
            raise MemoryError("stale_dependency", "Receipt changed during derivation.")
        return self._call("publish", {"job_id": prepared["job_id"], "body": _json(body),
                                      "kind": "inference", "idempotency_key": prefix + ":publish"})

    def inspect(self, card_id, *, snapshot, now, max_age_seconds=86400):
        card = self._record(card_id, CARD_SCHEMA)
        if card.get("receipt_refs") != card["dependencies"]:
            # Core prepare sorts inputs. Compare as sets after validating shape.
            try:
                matches = {(r["id"], r["revision"]) for r in card["receipt_refs"]} == {
                    (r["id"], r["revision"]) for r in card["dependencies"]}
            except (KeyError, TypeError):
                matches = False
            if not matches:
                raise ValueError("experience_dependency_mismatch")
        applicability = check_environment(card["environment"], snapshot, now=now,
                                           max_age_seconds=max_age_seconds)
        # Recheck trust at use time. A stored boolean is not authentication.
        trust = "verified"
        if not callable(self.receipt_verifier):
            trust = "unknown"
        else:
            for ref in card["receipt_refs"]:
                source = self._record(ref["id"], RECEIPT_SCHEMA)
                if source["revision"] != ref["revision"]:
                    raise MemoryError("stale_dependency", "Receipt revision changed.")
                try:
                    _receipt(source["receipt"], self.receipt_verifier, success=True)
                except ValueError:
                    trust = "unknown"
        if trust != "verified" and applicability["status"] == "applicable":
            applicability["status"] = "unknown"
        return dict(card, applicability=applicability, receipt_trust=trust,
                    can_suggest=applicability["status"] == "applicable" and trust == "verified",
                    permissions_granted=False)

    def list(self, *, limit=100):
        """Unassessed card metadata; callers must inspect before suggesting."""
        result = self._catalog(CARD_SCHEMA, limit=limit)
        for item in result["items"]:
            item["applicability"] = {"status": "unknown", "reason": "fresh_environment_required"}
            item["can_suggest"] = False
        return result

    def list_receipts(self, *, limit=100):
        return self._catalog(RECEIPT_SCHEMA, limit=limit)

    def feedback(self, card_id, *, outcome, receipt_id, recorded_at, idempotency_key):
        """Attach a verified terminal outcome, without promoting card truth."""
        if outcome not in ("succeeded", "failed", "cancelled"):
            raise ValueError("invalid_feedback_outcome")
        recorded_at = _stamp(recorded_at)
        card = self._record(card_id, CARD_SCHEMA)
        source = self._record(receipt_id, RECEIPT_SCHEMA)
        receipt = _receipt(source["receipt"], self.receipt_verifier)
        if receipt["status"] != outcome or _time(recorded_at) < _time(receipt["completed_at"]):
            raise ValueError("feedback_receipt_mismatch")
        prefix = "experience-feedback:" + _digest({"key": _text(idempotency_key, "idempotency_key"),
                                                 "binding": self.expected_binding})
        prepared = self._call("prepare", {"inputs": [card_id, receipt_id],
                                          "processor": FEEDBACK_SCHEMA, "idempotency_key": prefix + ":prepare"})
        if {(r["id"], r["revision"]) for r in prepared["dependencies"]} != {
                (card_id, card["revision"]), (receipt_id, source["revision"])}:
            raise MemoryError("stale_dependency", "Feedback source changed.")
        body = {"schema": FEEDBACK_SCHEMA, "binding": self.expected_binding,
                "card_id": card_id, "card_revision": card["revision"],
                "receipt_id": receipt_id, "outcome": outcome, "recorded_at": recorded_at,
                "semantic_generalization_verified": False}
        return self._call("publish", {"job_id": prepared["job_id"], "body": _json(body),
                                      "kind": "episode", "idempotency_key": prefix + ":publish"})

    def revoke(self, card_id, *, expected_revision, idempotency_key):
        card = self._record(card_id, CARD_SCHEMA)
        if card["revision"] != expected_revision:
            raise MemoryError("revision_conflict", "Experience revision changed.")
        return self._call("retract", {"id": card_id, "expected_revision": expected_revision,
                                     "idempotency_key": _text(idempotency_key, "idempotency_key")})
