#!/usr/bin/env python3
"""Synthetic adapter qualification with real Rust IPC, never a live Agent.

No model configuration, provider requests, AWS calls, or existing user profile
access. Initial runs require a new work directory. Resume uses its saved DBs and
bindings and refuses to initialize a missing database.
"""

import argparse
import fcntl
import hashlib
import importlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import time
import uuid

SCHEMA = "recall-ledger.host-memory-probe/1"
ADAPTERS = ("claude-code", "chatgpt", "kirocrew")
DATABASE_SCHEMA = 4


class ProbeError(Exception):
    pass


def check(report, name, condition):
    report["checks"].append({"name": name, "status": "pass" if condition else "fail"})
    if not condition:
        raise ProbeError(name)


def sha_file(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def sdk_fingerprint(root):
    package = Path(root) / "recall_ledger"
    files = sorted(package.rglob("*.py"))
    if not files:
        raise ProbeError("sdk_package_missing")
    digest = hashlib.sha256()
    for path in files:
        digest.update(path.relative_to(package).as_posix().encode() + b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def save_new(path, value):
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as stream:
        json.dump(value, stream, sort_keys=True)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    fd = os.open(Path(path).parent, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def load_sdk(root):
    root = Path(root).resolve(strict=True)
    package = root / "recall_ledger"
    if not (package / "__init__.py").is_file():
        raise ProbeError("sdk_package_missing")
    sys.path.insert(0, str(root))
    modules = {name: importlib.import_module("recall_ledger" + suffix) for name, suffix in (
        ("client", ""), ("claude", ".claude_code"), ("codex", ".codex"),
        ("runtime", ".host_runtime"), ("kiro", ".kirocrew"),
    )}
    for module in modules.values():
        if not Path(module.__file__).resolve().is_relative_to(package):
            raise ProbeError("sdk_import_does_not_match_requested_root")
    return modules


def verify_existing_database(path, expected_binding, expected_pending=None):
    """Read-only checks happen before Rust can migrate/initialize any file."""
    path = Path(path)
    if not path.is_file() or path.is_symlink():
        raise ProbeError("resume_database_missing_or_symlink")
    with path.open("rb") as stream:
        if path.stat().st_size < 100 or stream.read(16) != b"SQLite format 3\0":
            raise ProbeError("resume_database_empty_or_invalid_header")
    if not expected_binding:
        raise ProbeError("resume_expected_binding_missing")
    try:
        db = sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True, timeout=2)
        try:
            db.execute("PRAGMA query_only=ON")
            db.execute("PRAGMA trusted_schema=OFF")
            if db.execute("PRAGMA user_version").fetchone()[0] != DATABASE_SCHEMA:
                raise ProbeError("resume_database_schema_mismatch")
            tables = {row[0] for row in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
            if not {"meta", "records", "revisions", "ingestion_candidates", "operations"}.issubset(tables):
                raise ProbeError("resume_database_required_tables_missing")
            if db.execute("PRAGMA quick_check").fetchall() != [("ok",)]:
                raise ProbeError("resume_database_integrity_failed")
            identity = db.execute("SELECT value FROM meta WHERE key='store_id'").fetchone()
            if identity != (expected_binding["history_id"],):
                raise ProbeError("resume_database_history_mismatch")
            if db.execute("SELECT 1 FROM records WHERE namespace=? LIMIT 1",
                          (expected_binding["namespace"],)).fetchone() is None:
                raise ProbeError("resume_database_namespace_missing")
            if expected_pending is not None:
                if not expected_pending:
                    raise ProbeError("resume_pending_manifest_empty")
                for prior in expected_pending:
                    row = db.execute(
                        "SELECT event_id,state,body FROM ingestion_candidates WHERE namespace=? AND id=?",
                        (expected_binding["namespace"], prior["candidate_id"]),
                    ).fetchone()
                    if (row is None or row[:2] != (prior["event_id"], "pending")
                            or not isinstance(row[2], str)
                            or hashlib.sha256(row[2].encode()).hexdigest() != prior["body_sha256"]):
                        raise ProbeError("resume_original_pending_candidate_changed")
        finally:
            db.close()
    except sqlite3.Error as exc:
        raise ProbeError("resume_database_readonly_check_failed") from exc


class Daemon:
    def __init__(self, binary, root, namespace, sdk, *, expected_binding=None, expected_pending=None):
        self.binary, self.root, self.namespace, self.sdk = binary, Path(root), namespace, sdk
        self.database = self.root / "memory.db"
        self.socket = self.root / "s"
        self.process = None
        self.log = None
        self.client = None
        self.expected_binding = expected_binding
        self.expected_pending = expected_pending

    def start(self, *, existing, namespace=None):
        if self.process is not None:
            raise ProbeError("daemon_already_running")
        if existing:
            verify_existing_database(self.database, self.expected_binding, self.expected_pending)
        if len(os.fsencode(self.socket)) > 100:
            raise ProbeError("socket_path_too_long")
        if self.socket.is_symlink():
            raise ProbeError("socket_symlink")
        self.socket.unlink(missing_ok=True)
        log_path = self.root / "daemon.log"
        self.log = os.fdopen(os.open(log_path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600), "ab")
        self.process = subprocess.Popen(
            [str(self.binary), "--db", str(self.database), "--namespace", namespace or self.namespace,
             "serve", "--socket", str(self.socket), "--no-worker"],
            stdin=subprocess.DEVNULL, stdout=self.log, stderr=self.log,
        )
        self.client = self.sdk["client"].Client(str(self.socket), timeout=2)
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise ProbeError("daemon_exited_during_startup")
            try:
                binding = self.client.binding()
                if not existing:
                    self.expected_binding = binding
                return binding
            except self.sdk["client"].TransportError:
                time.sleep(0.025)
        raise ProbeError("daemon_startup_timeout")

    def stop(self):
        if self.process is not None:
            process, self.process = self.process, None
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        if self.log is not None:
            self.log.close()
            self.log = None


class Adapter:
    def __init__(self, name, root, daemon, sdk, sdk_root, *, install):
        self.name, self.root, self.daemon, self.sdk = name, Path(root), daemon, sdk
        self.project = self.root / "project"
        if install:
            self.project.mkdir(mode=0o700)
        elif not self.project.is_dir() or self.project.is_symlink():
            raise ProbeError("adapter_project_missing_or_symlink")
        self.install_receipt = None
        if name == "claude-code":
            if install:
                self.install_receipt = sdk["claude"].install(str(self.project), str(daemon.socket), timeout=2)
            self.config = sdk["claude"].load_config(str(self.project))
            self.state = Path(self.config["state"])
        elif name == "chatgpt":
            if install:
                env = dict(os.environ, PYTHONPATH=str(sdk_root), PYTHONDONTWRITEBYTECODE="1")
                result = subprocess.run(
                    [sys.executable, "-B", "-m", "recall_ledger.chatgpt", "install",
                     "--project", str(self.project), "--socket", str(daemon.socket), "--timeout", "2"],
                    env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE, timeout=20, check=False,
                )
                if result.returncode:
                    raise ProbeError("chatgpt_alias_install_failed")
                self.install_receipt = json.loads(result.stdout)
            self.state = self.project / ".codex" / "recall-ledger"
            self.runtime = sdk["runtime"].Runtime.load(self.state)
            self.config = self.runtime.config
        else:
            self.state = self.root / "bridge"
            if install:
                self.install_receipt = sdk["kiro"].install(
                    str(self.state), str(daemon.socket), host_instance_id="fixture-gateway",
                    member_id="fixture-member", store_id="fixture-store", binary=str(daemon.binary),
                )
            self.runtime = sdk["runtime"].Runtime.load(self.state)
            self.config = self.runtime.config
        if self.config["expected_binding"] != daemon.client.binding():
            raise ProbeError("saved_adapter_binding_mismatch")

    def bridge(self, session):
        return self.sdk["kiro"].GatewayBridge(
            self.runtime, self.sdk["kiro"].FrozenContext(
                "fixture-gateway", "fixture-member", "fixture-store",
                "persistent", session, "root",
            ),
        )

    def event(self, text, session, turn):
        return {"hook_event_name": "UserPromptSubmit", "cwd": str(self.project),
                "session_id": session, "turn_id": turn, "prompt": text}

    def turn(self, text, session, turn):
        if self.name == "claude-code":
            return self.sdk["claude"].run_hook(self.config, self.event(text, session, turn))
        if self.name == "chatgpt":
            return self.sdk["codex"].run_hook(self.runtime, self.event(text, session, turn))
        bridge = self.bridge(session)
        bridge.capture(event_id=turn, body=text)
        bridge.flush()
        return bridge.read(query=text)

    def read(self, query, session):
        # New synthetic session ID; never claim an actual Host session started.
        if self.name in ("claude-code", "chatgpt"):
            return self.turn(query, session, "read")
        return self.bridge(session).read(query=query)

    def capture_only(self, text, session, turn):
        if self.name == "claude-code":
            return self.sdk["claude"].capture(self.config, self.event(text, session, turn))
        if self.name == "chatgpt":
            return self.runtime.capture(event_id=turn, session_id=session, agent_id="root",
                                        body=text, origin="user_message", memory_mode="persistent")
        return self.bridge(session).capture(event_id=turn, body=text)

    def flush(self):
        if self.name == "claude-code":
            with self.sdk["claude"].locked(self.state):
                return self.sdk["claude"].flush_outbox(
                    self.config, self.sdk["claude"].client_for(self.config), deadline=time.monotonic() + 2,
                )
        return self.runtime.flush(memory_mode="persistent")


def packet(response):
    if "items" in response:
        return response
    text = response.get("hookSpecificOutput", {}).get("additionalContext", "")
    if "\n" not in text:
        raise ProbeError("adapter_did_not_return_memory_packet")
    value = json.loads(text.split("\n", 1)[1])
    if not isinstance(value.get("items"), list):
        raise ProbeError("adapter_memory_packet_invalid")
    return value


def bodies(response):
    return [item["body"] for item in packet(response)["items"]]


def pending_with_body(client, body):
    listing = client.candidate_list(state="pending", limit=100)
    if listing.get("truncated"):
        raise ProbeError("unexpected_candidate_list_truncation")
    return [row for row in listing["candidates"]
            if client.candidate_inspect(row["candidate_id"]).get("body") == body]


def accept_body(client, body, key):
    rows = pending_with_body(client, body)
    if len(rows) != 1:
        raise ProbeError("expected_one_pending_candidate")
    return client.candidate_resolve(rows[0]["candidate_id"], "accept", idempotency_key=key)


def pending_snapshot(client):
    listing = client.candidate_list(state="pending", limit=100)
    if listing.get("truncated"):
        raise ProbeError("unexpected_candidate_list_truncation")
    output = []
    for row in listing["candidates"]:
        candidate = client.candidate_inspect(row["candidate_id"])
        if candidate["state"] != "pending" or not isinstance(candidate.get("body"), str):
            raise ProbeError("pending_snapshot_changed")
        output.append({"candidate_id": row["candidate_id"], "event_id": row["event_id"],
                       "body_sha256": hashlib.sha256(candidate["body"].encode()).hexdigest()})
    return sorted(output, key=lambda item: item["candidate_id"])


def initial_adapter(name, root, daemon, sdk, sdk_root, report):
    adapter = Adapter(name, root, daemon, sdk, sdk_root, install=True)
    report["configuration_control"] = {
        "live_native_suppression_verified": False,
        "installer_receipt": {k: adapter.install_receipt[k] for k in (
            "native_control", "host_trusted", "effective_hooks_verified",
            "live_session_verified", "gateway_wired", "native_memory",
        ) if adapter.install_receipt and k in adapter.install_receipt},
    }
    token = uuid.uuid4().hex
    stable_key, changing_key = "stable" + token, "mutable" + token
    stable_body = stable_key + " value " + uuid.uuid4().hex
    original = changing_key + " value " + uuid.uuid4().hex
    revised = changing_key + " value " + uuid.uuid4().hex
    binding = daemon.client.binding()

    adapter.turn(stable_body, "source", "stable")
    adapter.turn(stable_body, "source", "stable")
    check(report, "raw_capture_is_one_pending_candidate",
          len(pending_with_body(daemon.client, stable_body)) == 1)
    check(report, "pending_not_recalled_in_new_synthetic_session",
          stable_body not in bodies(adapter.read(stable_key, "before-admission")))
    admitted = accept_body(daemon.client, stable_body, "assisted-stable")
    report["assisted_actions"].append("candidate_resolve:accept_stable")
    check(report, "assisted_accept_visible_in_new_synthetic_session",
          stable_body in bodies(adapter.read(stable_key, "after-admission")))

    adapter.turn(original, "source", "mutable")
    mutable = accept_body(daemon.client, original, "assisted-mutable")
    report["assisted_actions"].append("candidate_resolve:accept_mutable")
    previous = packet(adapter.read(changing_key, "before-correction"))
    check(report, "mutable_original_visible", original in [item["body"] for item in previous["items"]])
    daemon.client.revise(mutable["record_id"], expected_revision=1, body=revised,
                         idempotency_key="assisted-revise")
    report["assisted_actions"].append("revise:explicit_record_id")
    current = bodies(adapter.read(changing_key, "after-correction"))
    check(report, "assisted_correction_visible_old_body_absent", revised in current and original not in current)
    check(report, "old_manifest_invalid_after_correction",
          not daemon.client.validate_context(previous["manifest"])["valid"])
    daemon.client.retract(mutable["record_id"], expected_revision=2, idempotency_key="assisted-retract")
    report["assisted_actions"].append("retract:explicit_record_id")
    check(report, "assisted_retraction_absent_from_fresh_read",
          revised not in bodies(adapter.read(changing_key, "after-retraction")))
    check(report, "stable_fact_survives_unrelated_retraction",
          stable_body in bodies(adapter.read(stable_key, "stable-after-retraction")))

    daemon.stop()
    offline_body = "offline" + uuid.uuid4().hex
    adapter.capture_only(offline_body, "offline", "original-event")
    queued = list((adapter.state / "outbox").glob("*.json"))
    check(report, "offline_capture_saved_under_original_binding",
          len(queued) == 1 and json.loads(queued[0].read_text())["expected_binding"] == binding)
    queued_bytes = queued[0].read_bytes()
    daemon.start(existing=True)
    check(report, "same_database_restart_preserves_binding", daemon.client.binding() == binding)
    first, second = adapter.flush(), adapter.flush()
    check(report, "offline_delivery_once_after_restart",
          first["delivered"] == 1 and second["delivered"] == 0
          and len(pending_with_body(daemon.client, offline_body)) == 1)
    check(report, "offline_payload_remains_pending",
          offline_body not in [item["body"] for item in daemon.client.recall(offline_body, mode="lexical")["items"]])
    report["offline_queue_sha256"] = hashlib.sha256(queued_bytes).hexdigest()

    # Same socket, deliberately wrong namespace: a real daemon request must
    # fail the saved binding, not merely a Python mock's expectation.
    daemon.stop()
    daemon.start(existing=True, namespace=daemon.namespace + "-foreign")
    foreign_body = stable_key + " FOREIGN " + uuid.uuid4().hex
    daemon.client.remember(foreign_body, idempotency_key="foreign-fixture")
    error_code, response = None, {}
    try:
        response = adapter.read(stable_key, "foreign-binding")
    except (sdk["client"].MemoryError, sdk["runtime"].HostError) as exc:
        error_code = getattr(exc, "code", str(exc))
    serialized = json.dumps(response, sort_keys=True)
    check(report, "wrong_namespace_binding_blocks_read_and_delivery",
          (error_code == "binding_mismatch" or "binding_mismatch" in serialized)
          and foreign_body not in serialized and stable_body not in serialized
          and daemon.client.candidate_list()["total"] == 0)
    daemon.stop()
    daemon.start(existing=True)
    check(report, "original_history_usable_after_binding_fault",
          daemon.client.binding() == binding
          and stable_body in bodies(adapter.read(stable_key, "returned-to-original")))
    return {"adapter": name, "directory": Path(root).name, "namespace": daemon.namespace,
            "binding": binding, "stable_key": stable_key, "stable_body": stable_body,
            "stable_record_id": admitted["record_id"], "retracted_key": changing_key,
            "retracted_bodies": [original, revised], "mutable_record_id": mutable["record_id"],
            "pending_candidates": pending_snapshot(daemon.client)}


def resume_adapter(saved, root, daemon, sdk, sdk_root, report):
    check(report, "persisted_history_identity_unchanged", daemon.client.binding() == saved["binding"])
    # Do this before adapter.read: Claude/Codex read hooks capture new queries.
    current_pending = pending_snapshot(daemon.client)
    check(report, "persisted_pending_candidates_unchanged_before_adapter_calls",
          bool(saved["pending_candidates"])
          and all(item in current_pending for item in saved["pending_candidates"]))
    adapter = Adapter(saved["adapter"], root, daemon, sdk, sdk_root, install=False)
    session = "resumed-" + uuid.uuid4().hex
    response = packet(adapter.read(saved["stable_key"], session))
    check(report, "persisted_stable_fact_in_new_synthetic_session",
          any(item["id"] == saved["stable_record_id"] and item["body"] == saved["stable_body"]
              for item in response["items"]))
    current = bodies(adapter.read(saved["retracted_key"], session + "-withdrawn"))
    check(report, "retracted_fact_not_resurrected_after_resume",
          not any(body in current for body in saved["retracted_bodies"]))


def run_probe(*, binary, sdk_root, work_dir, phase, adapter):
    binary, sdk_root = Path(binary).resolve(strict=True), Path(sdk_root).resolve(strict=True)
    work_dir = Path(work_dir).absolute()
    if work_dir.is_symlink():
        raise ProbeError("work_dir_symlink")
    names = list(ADAPTERS) if adapter == "all" else [adapter]
    fingerprints = {"binary_sha256": sha_file(binary), "sdk_python_sha256": sdk_fingerprint(sdk_root),
                    "probe_sha256": sha_file(__file__)}
    if phase == "initial":
        work_dir.mkdir(mode=0o700, exist_ok=False)
    elif not work_dir.is_dir():
        raise ProbeError("resume_work_dir_missing")
    work_dir = work_dir.resolve(strict=True)
    manifest_path = work_dir / "fixture-state.json"
    with (work_dir / "probe.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if phase == "resume":
            if not manifest_path.is_file() or manifest_path.is_symlink():
                raise ProbeError("resume_manifest_missing_or_symlink")
            saved = json.loads(manifest_path.read_text())
            if saved.get("schema_version") != SCHEMA or saved.get("fingerprints") != fingerprints:
                raise ProbeError("resume_schema_or_fingerprint_mismatch")
            if [item["adapter"] for item in saved["adapters"]] != names:
                raise ProbeError("resume_adapter_selection_mismatch")
        else:
            saved = {"schema_version": SCHEMA, "run_id": str(uuid.uuid4()),
                     "fingerprints": fingerprints, "adapters": []}
        sdk = load_sdk(sdk_root)
        output = {
            "schema_version": SCHEMA, "run_id": saved["run_id"], "phase": phase,
            "test_kind": "adapter_fixture", "live_host_executed": False,
            "replacement_status": "blocked_autonomous_admission",
            "fingerprints": fingerprints, "model_configuration": "none",
            "daemon_worker": False, "retrieval_mode": "lexical",
            "model_provider_requests_by_probe": 0,
            "native_memory_replacement_verified": False,
            "limits": ["Synthetic Host events; no real Claude, ChatGPT App or KiroCrew process.",
                       "Assisted mutations supplied by this fixture; no autonomous extraction/admission.",
                       "Same-container daemon subprocess; no cross-container sidecar qualification.",
                       "Resume verifies supplied filesystem state; caller must establish PVC/Pod provenance."],
            "adapters": [], "status": "pass", "cleanup": {"all_daemons_stopped": True,
                                                       "fixture_state_retained": True},
        }
        for index, name in enumerate(names):
            prior = saved["adapters"][index] if phase == "resume" else None
            root = work_dir / ("a" + str(index))
            if phase == "initial":
                root.mkdir(mode=0o700)
            elif not root.is_dir() or root.is_symlink():
                raise ProbeError("resume_adapter_directory_missing_or_symlink")
            namespace = prior["namespace"] if prior else "probe-" + uuid.uuid4().hex
            report = {"adapter": name, "test_kind": "adapter_fixture", "checks": [],
                      "assisted_actions": [], "status": "pass",
                      "live_host_executed": False, "replacement_status": "blocked_autonomous_admission"}
            output["adapters"].append(report)
            daemon = Daemon(binary, root, namespace, sdk,
                            expected_binding=prior["binding"] if prior else None,
                            expected_pending=prior["pending_candidates"] if prior else None)
            try:
                daemon.start(existing=phase == "resume")
                if phase == "initial":
                    saved["adapters"].append(initial_adapter(name, root, daemon, sdk, sdk_root, report))
                else:
                    resume_adapter(prior, root, daemon, sdk, sdk_root, report)
            except Exception as exc:
                report["status"], output["status"] = "fail", "fail"
                report["error"] = {"type": type(exc).__name__, "code": str(exc)[:240]}
            finally:
                try:
                    daemon.stop()
                except Exception as exc:
                    output["cleanup"]["all_daemons_stopped"] = False
                    report["status"], output["status"] = "fail", "fail"
                    report["cleanup_error"] = type(exc).__name__
        if phase == "initial" and output["status"] == "pass":
            save_new(manifest_path, saved)
        output["durable_manifest_written"] = manifest_path.is_file()
        return output


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--sdk-root", required=True, type=Path, help="Directory containing recall_ledger package")
    parser.add_argument("--work-dir", required=True, type=Path, help="Must not exist for initial")
    parser.add_argument("--phase", choices=("initial", "resume"), default="initial")
    parser.add_argument("--adapter", choices=("all",) + ADAPTERS, default="all")
    args = parser.parse_args(argv)
    try:
        result = run_probe(binary=args.binary, sdk_root=args.sdk_root, work_dir=args.work_dir,
                           phase=args.phase, adapter=args.adapter)
    except Exception as exc:
        result = {"schema_version": SCHEMA, "status": "fail", "test_kind": "adapter_fixture",
                  "live_host_executed": False, "replacement_status": "blocked_autonomous_admission",
                  "error": {"type": type(exc).__name__, "code": str(exc)[:240]}}
    print(json.dumps(result, sort_keys=True))
    return 0 if result["status"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
