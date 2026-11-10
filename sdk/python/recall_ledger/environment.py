"""Bounded host observations; never executes a command or infers tool versions.

Roots and tool versions must come from the trusted host. A snapshot describes
observations at one time, not a sandbox, authentication decision or permission.
"""

import copy
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import stat
from typing import Mapping

from .client import MemoryError, validate_binding

SCHEMA = "recall-ledger.environment/1"
MAX_FILE_BYTES = 8 * 1024 * 1024
MAX_ITEMS = 128


def _text(value, name, maximum=256):
    if (not isinstance(value, str) or not value.strip()
            or len(value.encode("utf-8")) > maximum
            or any(ord(char) < 32 for char in value)):
        raise ValueError("invalid_" + name)
    return value


def _time(value):
    """Return an explicit instant; naive dates/times are refused."""
    if not isinstance(value, str):
        raise ValueError("timestamp_requires_timezone")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as exc:
        raise ValueError("invalid_timestamp") from exc
    if parsed.tzinfo is None or parsed.utcoffset() is None:
        raise ValueError("timestamp_requires_timezone")
    return parsed.astimezone(timezone.utc)


def _stamp(value):
    return _time(value).isoformat().replace("+00:00", "Z")


def _json(value):
    result = json.dumps(value, ensure_ascii=False, sort_keys=True,
                        separators=(",", ":"), allow_nan=False)
    if len(result.encode("utf-8")) > 65536:
        raise ValueError("record_too_large")
    return result


def _digest(value):
    return hashlib.sha256(_json(value).encode("utf-8")).hexdigest()


def _sha(value):
    return isinstance(value, str) and len(value) == 64 and all(c in "0123456789abcdef" for c in value)


def _relative(value):
    _text(value, "relative_path", 2048)
    path = Path(value)
    if path.is_absolute() or any(part in (".", "..") for part in value.split("/")):
        raise ValueError("path_outside_workspace")
    return path


def _hash_file(root, relative):
    # Walk open directory descriptors so symlink substitutions cannot redirect
    # an observation outside the host-selected root after a path precheck.
    root = Path(root)
    if not root.is_absolute() or any(p.is_symlink() for p in (root, *root.parents)):
        raise ValueError("unsafe_workspace_root")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    directory = os.open(str(root), flags | os.O_DIRECTORY)
    try:
        parts = _relative(relative).parts
        for part in parts[:-1]:
            next_fd = os.open(part, flags | os.O_DIRECTORY, dir_fd=directory)
            os.close(directory)
            directory = next_fd
        fd = os.open(parts[-1], flags | getattr(os, "O_NONBLOCK", 0), dir_fd=directory)
        try:
            before = os.fstat(fd)
            if not stat.S_ISREG(before.st_mode) or before.st_size > MAX_FILE_BYTES:
                return {"status": "unknown", "reason": "unsupported_or_large_file"}
            digest = hashlib.sha256()
            total = 0
            while True:
                block = os.read(fd, min(65536, MAX_FILE_BYTES + 1 - total))
                if not block:
                    break
                total += len(block)
                if total > MAX_FILE_BYTES:
                    return {"status": "unknown", "reason": "file_too_large"}
                digest.update(block)
            after = os.fstat(fd)
            if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (
                    after.st_size, after.st_mtime_ns, after.st_ctime_ns):
                return {"status": "unknown", "reason": "changed_during_observation"}
            return {"status": "observed", "sha256": digest.hexdigest()}
        finally:
            os.close(fd)
    finally:
        os.close(directory)


def capture_environment(*, roots, files, tool_versions, observed_at):
    """Hash selected files under host-owned roots and record supplied versions.

    ``files`` is ``[{"root": "project", "path": "package-lock.json"}]``.
    Paths to roots are not persisted. Missing/unsafe files are unknown; invalid
    relative paths and malformed root selections are rejected before reading.
    """
    observed_at = _stamp(observed_at)
    if not isinstance(roots, Mapping) or not roots or len(roots) > MAX_ITEMS:
        raise ValueError("invalid_workspace_roots")
    for alias, root in roots.items():
        _text(alias, "root_alias")
        if not isinstance(root, (str, Path)) or not Path(root).is_absolute():
            raise ValueError("invalid_workspace_root")
    if not isinstance(files, (list, tuple)) or len(files) > MAX_ITEMS:
        raise ValueError("invalid_files")
    if not isinstance(tool_versions, Mapping) or len(tool_versions) > MAX_ITEMS:
        raise ValueError("invalid_tool_versions")
    tools = {}
    for name, version in tool_versions.items():
        tools[_text(name, "tool")] = _text(version, "tool_version")
    selected, seen = [], set()
    for item in files:
        if not isinstance(item, dict) or set(item) != {"root", "path"}:
            raise ValueError("invalid_file_selection")
        alias, path = _text(item["root"], "root_alias"), item["path"]
        _relative(path)
        if alias not in roots or (alias, path) in seen:
            raise ValueError("unknown_or_duplicate_file")
        seen.add((alias, path))
        selected.append((alias, path))
    observations = []
    for alias, path in selected:
        try:
            result = _hash_file(roots[alias], path)
        except FileNotFoundError:
            result = {"status": "unknown", "reason": "missing_file"}
        except (OSError, ValueError):
            result = {"status": "unknown", "reason": "unreadable_or_unsafe_file"}
        observations.append(dict(result, root=alias, path=path))
    return {"schema": SCHEMA, "observed_at": observed_at,
            "files": observations, "tools": tools}


def _environment(value):
    if not isinstance(value, dict) or value.get("schema") != SCHEMA:
        raise ValueError("invalid_environment_schema")
    _time(value.get("observed_at"))
    files, tools = value.get("files"), value.get("tools")
    if not isinstance(files, list) or len(files) > MAX_ITEMS or not isinstance(tools, dict) or len(tools) > MAX_ITEMS:
        raise ValueError("invalid_environment")
    indexed = {}
    for item in files:
        if not isinstance(item, dict):
            raise ValueError("invalid_file_observation")
        key = (_text(item.get("root"), "root_alias"), str(_relative(item.get("path"))))
        if key in indexed or item.get("status") not in ("observed", "unknown"):
            raise ValueError("invalid_file_observation")
        if item["status"] == "observed" and not _sha(item.get("sha256")):
            raise ValueError("invalid_file_digest")
        indexed[key] = item
    for name, version in tools.items():
        _text(name, "tool")
        _text(version, "tool_version")
    return indexed, tools


def check_environment(requirements, snapshot, *, now, max_age_seconds=86400):
    """Return applicable/mismatch/unknown; missing evidence never matches.

    ``requirements`` is the original environment snapshot. ``snapshot`` must be
    freshly captured by the current host. Extra current observations are ignored.
    """
    current = _time(now)
    if type(max_age_seconds) is not int or not 0 <= max_age_seconds <= 31536000:
        raise ValueError("invalid_observation_age")
    wanted_files, wanted_tools = _environment(requirements)
    actual_files, actual_tools = _environment(snapshot)
    checks = []
    age = (current - _time(snapshot["observed_at"])).total_seconds()
    stale = age < 0 or age > max_age_seconds
    if stale:
        checks.append({"kind": "observation_time", "status": "unknown",
                       "reason": "future_or_stale_observation"})
    for key, wanted in wanted_files.items():
        actual = actual_files.get(key)
        status = "unknown"
        if not stale and wanted["status"] == "observed" and actual and actual["status"] == "observed":
            status = "applicable" if wanted["sha256"] == actual["sha256"] else "mismatch"
        checks.append({"kind": "file", "root": key[0], "path": key[1], "status": status})
    for name, version in wanted_tools.items():
        actual = actual_tools.get(name)
        status = ("unknown" if stale or actual is None else
                  "applicable" if actual == version else "mismatch")
        checks.append({"kind": "tool", "tool": name, "status": status})
    if not wanted_files and not wanted_tools:
        checks.append({"kind": "requirements", "status": "unknown", "reason": "empty_requirements"})
    states = {item["status"] for item in checks}
    status = "mismatch" if "mismatch" in states else "unknown" if "unknown" in states else "applicable"
    return {"status": status, "checks": checks, "checked_at": _stamp(now),
            "permissions_granted": False}


class _BoundStore:
    """Private common utilities. All content calls require a pinned client."""

    def __init__(self, client, *, expected_binding):
        self.client = client
        self.expected_binding = validate_binding(expected_binding)
        # Duck-typed clients must expose this property and enforce the binding
        # transactionally, just as Client does. A preflight read alone is unsafe.
        if getattr(client, "expected_binding", None) != self.expected_binding:
            raise ValueError("client_binding_mismatch")

    def _call(self, method, params=None):
        if getattr(self.client, "expected_binding", None) != self.expected_binding:
            raise ValueError("client_binding_mismatch")
        return self.client.call(method, params or {})

    def _record(self, record_id, schema):
        raw = self._call("inspect", {"id": _text(record_id, "record_id")})
        try:
            body = json.loads(raw["body"])
        except (KeyError, TypeError, json.JSONDecodeError) as exc:
            raise ValueError("invalid_record_body") from exc
        if not isinstance(body, dict) or body.get("schema") != schema or body.get("binding") != self.expected_binding:
            raise ValueError("record_type_or_binding_mismatch")
        return dict(body, id=raw["id"], revision=raw["revision"],
                    dependencies=raw.get("dependencies", []))

    def _remember(self, body, *, kind, source, idempotency_key):
        body = dict(copy.deepcopy(body), binding=self.expected_binding)
        return self._call("remember", {"body": _json(body), "kind": kind,
                                      "source": source, "idempotency_key": _text(idempotency_key, "idempotency_key")})

    def _revise(self, record, body, *, expected_revision, idempotency_key):
        if type(expected_revision) is not int or expected_revision < 1:
            raise ValueError("invalid_expected_revision")
        if record["revision"] != expected_revision:
            raise MemoryError("revision_conflict", "Record revision changed.")
        return self._call("revise", {"id": record["id"], "expected_revision": expected_revision,
                                    "body": _json(body), "idempotency_key": _text(idempotency_key, "idempotency_key")})

    def _catalog(self, schema, *, limit=100):
        if type(limit) is not int or not 1 <= limit <= 1000:
            raise ValueError("invalid_limit")
        ids, after = set(), None
        # A bounded full feed scan avoids recall ranking hiding older due tasks.
        # Hosts with larger histories must implement an indexed feed consumer.
        for _ in range(20):
            params = {"limit": 500}
            if after is not None:
                params["after"] = after
            page = self._call("changes", params)
            for event in page["events"]:
                ids.update(event.get("record_ids", []))
            if not page["has_more"]:
                break
            after = page["cursor"]
        else:
            raise ValueError("catalog_scan_limit_exceeded")
        records = []
        for record_id in sorted(ids):
            try:
                record = self._record(record_id, schema)
            except MemoryError as exc:
                if exc.code in ("not_found", "stale_dependency"):
                    continue
                raise
            except ValueError as exc:
                if str(exc) in ("record_type_or_binding_mismatch", "invalid_record_body"):
                    continue
                raise
            records.append(record)
        return {"items": records[:limit], "has_more": len(records) > limit,
                "total": len(records)}
