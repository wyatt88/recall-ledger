"""Reviewable identity suggestions; every capture remains a pending proposal.

The default matcher is a bounded lexical heuristic, not an entailment model.
Callers supply the authenticated scope and explicitly choose exact fact keys.
"""

import copy
import hashlib
import json
import math
import re
import unicodedata
from difflib import SequenceMatcher


class SemanticError(ValueError):
    def __init__(self, code, details=None):
        super().__init__(code)
        self.code, self.details = code, details


def _canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False)


def _digest(value):
    return hashlib.sha256(_canonical(value).encode("utf-8")).hexdigest()


def _text(value, name, maximum=16384):
    if not isinstance(value, str) or not value.strip() or len(value.encode("utf-8")) > maximum:
        raise SemanticError("invalid_" + name)
    if any(unicodedata.category(c) == "Cc" for c in value if c not in "\n\t"):
        raise SemanticError("invalid_" + name)
    return value


def _key(value, scope):
    _text(scope, "scope", 256)
    if value is None:
        return None
    if not isinstance(value, dict) or set(value) != {"entity", "attribute", "scope", "conditions"}:
        raise SemanticError("invalid_fact_key")
    for item in value.values():
        _text(item, "fact_key", 256)
        if any(unicodedata.category(c) == "Cc" for c in item):
            raise SemanticError("invalid_fact_key")
    if value["scope"] != scope:
        raise SemanticError("scope_mismatch")
    return copy.deepcopy(value)


def qualification_flags(body):
    """Signals for review only; absence of a signal does not prove assertion."""
    patterns = {
        "uncertain": r"可能|也许|大概|不确定|尚未确认|还在考虑|正在考虑|仍在考虑|考虑中|还没决定|尚未决定|尚未确定|打算.{0,30}(?:未定|没定|未确定|没确定)|\b(maybe|perhaps|uncertain|might|considering|undecided)\b",
        "quoted": r"引用|转述|原话|[“”「」]|\b(quote|quoted|said)\b",
        "hypothetical": r"假设|假如|设想|\b(hypothetical|suppose|imagine)\b",
        "conditional": r"如果|只有|当.+时|前提|除非|\b(if|unless|provided that|only when)\b",
        "exception": r"除了|除外|但不|除非|\b(except|excluding|unless)\b",
    }
    flags = [name for name, pattern in patterns.items() if re.search(pattern, body, re.I)]
    if re.search(r"(?:通常|平时|一般|默认).{0,120}(?:这次|本次|这份|这回|当前这|仅此)|(?:这次|本次|这份|这回).{0,120}(?:通常|平时|一般|默认)", body, re.S):
        flags.extend(["temporary_exception", "exception"])
    return sorted(set(flags))


class RuleMatcher:
    """NFKC lexical similarity with optional caller-supplied phrase aliases."""
    name = "lexical_rules_not_semantic_entailment"

    def __init__(self, aliases=None):
        self.aliases = dict(aliases or {})
        for a, b in self.aliases.items():
            _text(a, "alias", 256)
            _text(b, "alias", 256)

    def normalize(self, value):
        value = unicodedata.normalize("NFKC", value).casefold()
        for source, target in sorted(self.aliases.items(), key=lambda item: -len(item[0])):
            value = value.replace(source.casefold(), target.casefold())
        return "".join(c for c in value if c.isalnum())

    def __call__(self, body, record):
        a, b = self.normalize(body), self.normalize(record["body"])
        if not a or not b:
            return 0.0
        # Bound quadratic comparison for user-provided long documents.
        return SequenceMatcher(None, a[:2048], b[:2048], autojunk=False).ratio()


def preview_candidate(client, body, *, scope, fact_key=None, matcher=None, limit=8):
    """Read candidate identity suggestions without changing memory.

    A match is never selected automatically. Different conditions stay distinct.
    The pool is limited to 100 current records; coverage exposes that bound.
    """
    _text(body, "body")
    key = _key(fact_key, scope)
    if type(limit) is not int or not 1 <= limit <= 20:
        raise SemanticError("invalid_limit")
    matcher = matcher or RuleMatcher()
    pool = client.call("recall", {"query": "", "mode": "lexical", "limit": 100, "max_chars": 131072})
    matches, excluded = [], []
    for record in pool.get("items", []):
        other = record.get("fact_key")
        if not other or other.get("scope") != scope:
            excluded.append({"id": record["id"], "reason": "unscoped_or_other_scope"})
            continue
        exact = key == other if key else False
        same_slot = bool(key and all(key[p] == other[p] for p in ("entity", "attribute", "scope")))
        score = matcher(body, copy.deepcopy(record))
        if isinstance(score, bool) or not isinstance(score, (float, int)) or not math.isfinite(score) or not 0 <= score <= 1:
            raise SemanticError("invalid_matcher_score")
        if exact or same_slot or score >= 0.35:
            matches.append({
                "id": record["id"], "revision": record["revision"], "body": record["body"],
                "fact_key": other, "similarity": score, "score_is_probability": False,
                "relation": "exact_identity" if exact else "different_conditions" if same_slot else "possible_paraphrase",
                "body_differs": body != record["body"], "requires_explicit_selection": True,
            })
    matches.sort(key=lambda m: (m["relation"] != "exact_identity", m["relation"] != "different_conditions", -m["similarity"], m["id"]))
    return {
        "schema": "recall-ledger.semantic-preview/1", "body": body, "fact_key": key,
        "scope": scope, "flags": qualification_flags(body), "matches": matches[:limit],
        "excluded": excluded, "coverage": pool.get("coverage", {}),
        "pool_limit": 100, "matching_method": getattr(matcher, "name", "caller_supplied_matcher"),
        "semantics_verified": False, "automatic_admission": False,
    }


def capture_candidate(client, body, *, scope, event_id, source, idempotency_key,
                      fact_key=None, origin="user_message", kind=None):
    """Capture exact text; lexical signals cannot promote it to a trusted fact."""
    _text(body, "body")
    key = _key(fact_key, scope)
    flags = qualification_flags(body)
    suggested_kind = "episode" if {"quoted", "hypothetical"} & set(flags) else "inference" if "uncertain" in flags else "assertion"
    kind = kind or suggested_kind
    if ({"quoted", "hypothetical"} & set(flags) and kind != "episode") or ("uncertain" in flags and kind not in ("inference", "episode")):
        raise SemanticError("qualified_text_requires_nonassertion_kind")
    params = {"body": body, "event_id": event_id, "source": source, "origin": origin,
              "kind": kind, "idempotency_key": idempotency_key}
    if key is not None:
        params["fact_key"] = key
    receipt = client.call("candidate_submit", params)
    return {"candidate": receipt, "flags": flags, "kind": kind, "scope": scope,
            "automatic_admission": False, "requires_review": receipt.get("state") == "pending"}


def review_candidate(client, candidate_id, *, scope):
    """Freeze proposal and exact current fact revision for explicit resolution."""
    proposal = client.call("candidate_inspect", {"candidate_id": candidate_id})
    if proposal.get("state") != "pending" or not isinstance(proposal.get("body"), str):
        raise SemanticError("candidate_not_pending")
    key = _key(proposal.get("fact_key"), scope)
    flags = qualification_flags(proposal["body"])
    blockers = []
    if key is None:
        blockers.append("fact_key_required_for_scoped_admission")
    if {"conditional", "exception"} & set(flags):
        if not key or key["conditions"].casefold() in ("default", "always", "unconditional", "默认", "无条件"):
            blockers.append("explicit_conditions_required")
    if {"quoted", "hypothetical"} & set(flags) and proposal.get("kind") != "episode":
        blockers.append("qualified_text_requires_nonassertion_kind")
    if "uncertain" in flags and proposal.get("kind") not in ("inference", "episode"):
        blockers.append("qualified_text_requires_nonassertion_kind")
    current = None
    if key:
        try:
            current = client.call("fact_get", {"fact_key": key})
        except Exception as exc:
            if getattr(exc, "code", None) != "not_found":
                raise
    if current is not None:
        flags.append("existing_fact_change" if current["body"] != proposal["body"] else "duplicate_wording")
    if proposal.get("origin") in ("import", "model_inference"):
        flags.append("unverified_origin")
    review = {
        "schema": "recall-ledger.candidate-review/1", "candidate_id": candidate_id, "scope": scope,
        "proposal": {field: proposal.get(field) for field in
                     ("body", "fact_key", "kind", "origin", "source", "valid_from", "valid_until", "metadata_fields")},
        "current": None if current is None else
            {"id": current["id"], "revision": current["revision"], "body": current["body"], "fact_key": key},
        "required_acknowledgements": sorted(set(flags)), "blockers": blockers,
        "semantics_verified": False,
    }
    review["digest"] = _digest(review)
    return review


def resolve_candidate(client, review, *, decision, idempotency_key, acknowledged_flags=()):
    """Apply one reviewed decision. Never refresh a stale review or revise a key."""
    review = copy.deepcopy(review)
    if not isinstance(review, dict) or review.get("schema") != "recall-ledger.candidate-review/1":
        raise SemanticError("invalid_review")
    digest = review.pop("digest", None)
    if digest != _digest(review):
        raise SemanticError("review_digest_mismatch")
    if decision not in ("accept", "reject"):
        raise SemanticError("invalid_decision")
    if decision == "accept":
        if review["blockers"]:
            raise SemanticError("admission_blocked", review["blockers"])
        missing = set(review["required_acknowledgements"]) - set(acknowledged_flags)
        if missing:
            raise SemanticError("acknowledgement_required", sorted(missing))
    proposal = client.call("candidate_inspect", {"candidate_id": review["candidate_id"]})
    if proposal.get("state") == "pending":
        current = review_candidate(client, review["candidate_id"], scope=review["scope"])
        if current["digest"] != digest:
            raise SemanticError("review_stale", current)
    # A repeated call on a terminal candidate reaches the core with the original
    # parameters/key, so the core can recover its receipt or reject a new write.
    params = {"candidate_id": review["candidate_id"], "decision": decision, "idempotency_key": idempotency_key}
    if decision == "accept" and review["current"] is not None:
        params["expected_revision"] = review["current"]["revision"]
    return client.call("candidate_resolve", params)
