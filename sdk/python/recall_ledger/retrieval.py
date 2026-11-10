"""Bounded multi-query evidence assembly with explicit index degradation.

Only ``context`` is the budgeted model payload. Diagnostics are for the host UI.
The final manifest is validated after assembly; it is not an execution permit.
"""

import copy
import hashlib
import json
import math
import re
import unicodedata


class RetrievalError(ValueError):
    def __init__(self, code, details=None):
        super().__init__(code)
        self.code, self.details = code, details


class RuleEncoder:
    """Offline hashed word/character features with explicit phrase aliases.

    Useful as an executable lexical baseline and provider example. This is not
    a learned semantic embedding. Its model key pins all feature settings.
    """
    name = "offline_alias_features_not_semantic_embedding"

    def __init__(self, aliases=None, dimensions=256):
        if type(dimensions) is not int or not 32 <= dimensions <= 4096:
            raise RetrievalError("invalid_dimensions")
        self.aliases = dict(aliases or {})
        if any(not isinstance(a, str) or not a or not isinstance(b, str) or not b for a, b in self.aliases.items()):
            raise RetrievalError("invalid_aliases")
        self.dimensions = dimensions
        config = json.dumps({"schema": 1, "dimensions": dimensions, "aliases": self.aliases}, sort_keys=True, ensure_ascii=False)
        self.model_key = "alias-features-" + hashlib.sha256(config.encode()).hexdigest()[:24]

    def __call__(self, text):
        text = unicodedata.normalize("NFKC", text).casefold()
        for source, target in sorted(self.aliases.items(), key=lambda item: -len(item[0])):
            text = text.replace(unicodedata.normalize("NFKC", source).casefold(),
                                unicodedata.normalize("NFKC", target).casefold())
        tokens = re.findall(r"[a-z0-9]+|[\u3400-\u9fff]", text)
        tokens += ["pair:" + a + b for a, b in zip(tokens, tokens[1:])]
        vector = [0.0] * self.dimensions
        for token in tokens:
            digest = hashlib.sha256(token.encode()).digest()
            vector[int.from_bytes(digest[:4], "big") % self.dimensions] += 1.0 if digest[4] % 2 else -1.0
        if not any(vector):
            # Punctuation-only notes must still obey the core's nonzero vector
            # contract, without pretending they carry semantic evidence.
            vector[0] = 1.0
        return vector


def index_evidence(client, *, scope, encoder, model_key, idempotency_key, limit=32, allow_unscoped=False,
                   record_ids=None):
    """Index one bounded batch using an explicitly supplied encoder.

    Call again deliberately for another batch. A failed/unknown write is
    reported with its exact key and is never automatically retried.
    """
    if not isinstance(scope, str) or not scope.strip() or not callable(encoder):
        raise RetrievalError("invalid_index_configuration")
    if type(limit) is not int or not 1 <= limit <= 32:
        raise RetrievalError("invalid_index_limit")
    if record_ids is not None:
        if not isinstance(record_ids, list) or not 1 <= len(record_ids) <= 32 or any(not isinstance(rid, str) or not rid for rid in record_ids):
            raise RetrievalError("invalid_record_ids")
        pending = {"items": [{"id": rid} for rid in dict.fromkeys(record_ids)]}
    else:
        pending = client.call("embedding_candidates", {"model_key": model_key, "limit": limit})
    results = []
    for item in pending["items"]:
        current = client.call("inspect", {"id": item["id"], "history": False})
        key = current.get("fact_key")
        if (not key and not allow_unscoped) or (key and key.get("scope") != scope):
            results.append({"id": item["id"], "state": "excluded", "reason": "unscoped_or_other_scope"})
            continue
        operation_key = "index-" + hashlib.sha256(_render([idempotency_key, model_key, current["id"], current["revision"]]).encode()).hexdigest()[:48]
        try:
            vector = list(encoder(current["body"]))
            if not vector or any(isinstance(x, bool) or not isinstance(x, (int, float)) or not math.isfinite(x) for x in vector):
                raise RetrievalError("invalid_encoder_vector")
            receipt = client.call("embedding_set", {"id": current["id"], "expected_revision": current["revision"],
                                  "model_key": model_key, "vector": vector, "idempotency_key": operation_key})
            results.append({"id": current["id"], "state": "indexed", "receipt": receipt})
        except Exception as exc:
            results.append({"id": current["id"], "state": "outcome_unknown" if getattr(exc, "outcome_unknown", False) else "failed",
                            "error_code": getattr(exc, "code", "encoding_failed"),
                            "recovery": {"method": "embedding_set", "idempotency_key": operation_key}})
            if getattr(exc, "outcome_unknown", False):
                break
    return {"results": results, "index": client.call("embedding_status", {"model_key": model_key}),
            "candidate_pool_limit": limit if record_ids is None else len(record_ids),
            "may_require_explicit_record_ids": record_ids is None and any(r["state"] == "excluded" for r in results),
            "encoder": getattr(encoder, "name", "caller_supplied"), "semantics_verified": False}


def _render(items):
    return json.dumps(items, ensure_ascii=False, separators=(",", ":"), allow_nan=False)


def _view(record):
    return {key: record.get(key) for key in
            ("id", "revision", "body", "kind", "source", "fact_key", "valid_from", "valid_until")} | {
                "trust_boundary": "retrieved_data"}


def recall_evidence(client, queries, *, scope, max_chars=12000, limit=8,
                    mode="hybrid", encoder=None, model_key=None, evidence_groups=None,
                    allow_fallback=True, allow_unscoped=False, condition_context=None):
    """Return complete groups plus dependency ancestors under an exact budget.

    ``encoder(query) -> list[float]`` is opt-in. ``evidence_groups`` maps labels
    to record IDs that must be included together. No group implies no claim of
    question-evidence completeness. ``condition_context`` optionally allowlists
    exact condition strings; otherwise conditions remain explicitly unverified.
    """
    if isinstance(queries, str):
        queries = [queries]
    if not isinstance(queries, (list, tuple)) or not 1 <= len(queries) <= 8 or any(
        not isinstance(q, str) or len(q.encode("utf-8")) > 4096 for q in queries
    ):
        raise RetrievalError("invalid_queries")
    if not isinstance(scope, str) or not scope.strip():
        raise RetrievalError("scope_required")
    if type(max_chars) is not int or not 2 <= max_chars <= 131072 or type(limit) is not int or not 1 <= limit <= 100:
        raise RetrievalError("invalid_budget")
    if mode not in ("lexical", "hybrid", "vector"):
        raise RetrievalError("invalid_mode")
    if condition_context is not None and (
        not isinstance(condition_context, (list, tuple)) or any(not isinstance(x, str) for x in condition_context)
    ):
        raise RetrievalError("invalid_condition_context")
    groups = copy.deepcopy(evidence_groups or {})
    if not isinstance(groups, dict) or len(groups) > 20:
        raise RetrievalError("invalid_evidence_groups")
    for name, ids in groups.items():
        if not isinstance(name, str) or not isinstance(ids, list) or not 1 <= len(ids) <= 100 or any(not isinstance(x, str) or not x for x in ids) or len(set(ids)) != len(ids):
            raise RetrievalError("invalid_evidence_groups")
    records, ranks, provenance, diagnostics, excluded = {}, {}, {}, [], []
    identity = None
    token = None
    index = None
    if mode != "lexical" and encoder is not None and model_key:
        index = client.call("embedding_status", {"model_key": model_key})
    for query_number, query in enumerate(queries):
        effective, reason, vector = mode, None, None
        if mode != "lexical":
            if encoder is None or not model_key:
                effective, reason = "lexical", "query_encoder_not_configured"
            elif not index.get("indexed", 0):
                effective, reason = "lexical", "index_empty"
            else:
                try:
                    vector = list(encoder(query))
                    if not vector or len(vector) > 8192 or any(
                        isinstance(x, bool) or not isinstance(x, (int, float)) or not math.isfinite(x) for x in vector
                    ) or not any(x != 0 for x in vector):
                        raise ValueError("invalid_vector")
                    if index.get("dimensions") is not None and len(vector) != index["dimensions"]:
                        raise ValueError("dimension_mismatch")
                except Exception:
                    effective, reason, vector = "lexical", "query_encoding_failed", None
        if reason and not allow_fallback:
            raise RetrievalError(reason)
        params = {"query": query, "mode": effective, "limit": 100, "max_chars": min(131072, max_chars * 8)}
        if effective != "lexical":
            params.update(model_key=model_key, query_vector=vector)
        try:
            result = client.call("recall", params)
        except Exception as exc:
            if effective == "lexical" or not allow_fallback or getattr(exc, "code", None) != "dimension_mismatch":
                raise
            effective, reason = "lexical", "index_dimension_changed"
            result = client.call("recall", {"query": query, "mode": "lexical", "limit": 100, "max_chars": min(131072, max_chars * 8)})
        manifest = result["manifest"]
        current_identity = (manifest["namespace"], manifest["commit_token"].rsplit(":", 1)[0])
        if identity is not None and current_identity != identity:
            raise RetrievalError("binding_changed")
        identity, token = current_identity, manifest["commit_token"]
        coverage = result.get("coverage", {})
        diagnostics.append({"query": query, "requested_mode": mode, "effective_mode": effective,
                            "degraded": bool(reason) or (effective != "lexical" and not coverage.get("semantic_index_complete", False)),
                            "reason": reason or ("index_partial" if effective != "lexical" and not coverage.get("semantic_index_complete", False) else None),
                            "coverage": coverage})
        for rank, record in enumerate(result["items"]):
            rid = record["id"]
            if rid in records and record["revision"] != records[rid]["revision"]:
                raise RetrievalError("changed_during_retrieval", {"id": rid})
            records[rid] = record
            ranks[rid] = ranks.get(rid, 0.0) + 1 / (60 + rank + 1)
            provenance.setdefault(rid, []).append(query_number)
    inspected = {}
    inspections = 0

    def closure(rid, visiting=None):
        nonlocal inspections
        visiting = set() if visiting is None else set(visiting)
        if rid in visiting:
            raise RetrievalError("dependency_cycle")
        visiting.add(rid)
        if rid not in inspected:
            inspections += 1
            if inspections > 100:
                raise RetrievalError("dependency_read_limit")
            try:
                record = client.call("inspect", {"id": rid, "history": False})
            except Exception as exc:
                if getattr(exc, "code", None) in ("not_found", "stale_dependency", "record_invalid", "retracted", "expired", "not_yet_valid", "invalid_state"):
                    raise RetrievalError("unavailable_evidence", {"id": rid, "reason": exc.code}) from exc
                raise
            if rid in records and records[rid]["revision"] != record["revision"]:
                raise RetrievalError("changed_during_retrieval", {"id": rid})
            if not isinstance(record.get("body"), str) or record.get("applicable_now") is False:
                raise RetrievalError("unavailable_evidence", {"id": rid})
            read_token = record.get("commit_token")
            if read_token and read_token.rsplit(":", 1)[0] != identity[1]:
                raise RetrievalError("binding_changed")
            inspected[rid] = record
        record = inspected[rid]
        key = record.get("fact_key")
        if not key and not allow_unscoped:
            raise RetrievalError("unscoped_evidence", {"id": rid})
        if key and key.get("scope") != scope:
            raise RetrievalError("scope_mismatch", {"id": rid})
        if key and condition_context is not None and key["conditions"] not in condition_context:
            raise RetrievalError("condition_not_selected", {"id": rid})
        result = {}
        for ref in record.get("dependencies", []):
            parents = closure(ref["id"], visiting)
            if parents[ref["id"]]["revision"] != ref["revision"]:
                raise RetrievalError("changed_dependency", {"id": ref["id"]})
            result.update(parents)
        result[rid] = record
        return result

    selected, group_status, reasons = {}, {}, []
    grouped = {rid for ids in groups.values() for rid in ids}
    units = [(name, ids) for name, ids in groups.items()]
    units += [(None, [rid]) for rid in sorted(records, key=lambda rid: (-ranks[rid], rid)) if rid not in grouped]
    for name, ids in units:
        members = {}
        try:
            pending = set(ids)
            while pending:
                for rid in sorted(pending):
                    members.update(closure(rid))
                # Overlapping groups and ancestors share the same atomic unit:
                # no other unit may sneak in half of a rejected evidence group.
                required = {rid for group in groups.values() if set(group) & set(members) for rid in group}
                pending = required - set(members)
        except RetrievalError as exc:
            if exc.code in ("changed_during_retrieval", "changed_dependency", "binding_changed"):
                raise
            excluded.append({"ids": ids, "group": name, "reason": exc.code, "details": exc.details})
            if name is not None:
                group_status[name] = {"complete": False, "reason": exc.code, "required_ids": ids}
            continue
        merged = dict(selected, **members)
        rendered = _render([_view(value) for value in merged.values()])
        if len(merged) > limit or len(rendered) > max_chars:
            excluded.append({"ids": ids, "group": name, "reason": "whole_evidence_group_exceeds_budget"})
            if name is not None:
                group_status[name] = {"complete": False, "reason": "budget", "required_ids": ids}
            continue
        selected = merged
        reasons.append({"ids": ids, "group": name, "reason": "complete_group" if name is not None else "query_match_with_dependencies",
                        "query_indexes": sorted({i for rid in ids for i in provenance.get(rid, [])}),
                        "dependency_ids": sorted(set(members) - set(ids))})
        if name is not None:
            group_status[name] = {"complete": True, "required_ids": ids}
    # Overlapping groups may have supplied a previously excluded group's members.
    for name, ids in groups.items():
        if all(rid in selected for rid in ids):
            group_status[name] = {"complete": True, "required_ids": ids}
    manifest = {"schema_version": "recall-ledger.context/1", "namespace": identity[0], "commit_token": token,
                "records": [{"id": rid, "revision": record["revision"]} for rid, record in selected.items()]}
    validation = client.call("validate_context", {"manifest": manifest})
    valid = validation.get("valid") is True
    items = [_view(record) for record in selected.values()] if valid else []
    return {
        "schema": "recall-ledger.evidence-selection/1", "items": items, "context": _render(items),
        "manifest": manifest, "validation": validation, "usable": valid,
        "budget": {"max_chars": max_chars, "used_chars": len(_render(items)), "unit": "unicode_scalars",
                   "scope": "context_serialized_json", "record_limit": limit},
        "queries": diagnostics, "index": index, "selected": reasons if valid else [],
        "excluded": excluded, "groups": group_status, "degraded": any(q["degraded"] for q in diagnostics),
        "question_evidence_complete": all(g["complete"] for g in group_status.values()) if groups and valid else None,
        "condition_filter_applied": condition_context is not None,
        "conditions_verified": False, "semantics_verified": False,
        "execution_authorized": False,
    }
