#!/usr/bin/env python3
"""Credential-free synthetic Sandbox qualification; no provider or AWS API calls."""

import argparse
import datetime
import errno
import json
import os
from pathlib import Path
import platform
import socket
import sqlite3
import subprocess
import sys
import time
import uuid

SCHEMA = "recall-ledger.sandbox-probe/1"
SCRIPT = str(Path(__file__).resolve())


class JsonArgumentParser(argparse.ArgumentParser):
    def error(self, message):
        print(json.dumps({"schema": SCHEMA, "status": "error", "error": {"code": "invalid_arguments", "message": message}}))
        raise SystemExit(2)


def result(name, status, observed, expected):
    return {"name": name, "status": status, "observed": observed, "expected": expected}


def assess(name, condition, observed, expected):
    return result(name, "pass" if condition else "fail", observed, expected)


def read_text(path):
    return Path(path).read_text(encoding="utf-8", errors="replace")


def mount_for(path, text=None):
    """Return the most specific containing mount, including filesystem type."""
    if text is None:
        text = read_text("/proc/self/mountinfo")
    target = os.path.abspath(path)
    candidates = []
    for line in text.splitlines():
        left, separator, right = line.partition(" - ")
        fields, trailing = left.split(), right.split()
        if not separator or len(fields) < 6 or len(trailing) < 3:
            continue
        mountpoint = fields[4].replace("\\040", " ").replace("\\011", "\t")
        if target == mountpoint or target.startswith(mountpoint.rstrip("/") + "/"):
            candidates.append(
                {
                    "mountpoint": mountpoint,
                    "options": fields[5].split(","),
                    "filesystem": trailing[0],
                }
            )
    return max(candidates, key=lambda item: len(item["mountpoint"])) if candidates else None


def configuration_checks(work_dir):
    checks = []
    try:
        version = read_text("/proc/version").strip()
        checks.append(assess("gvisor_proc_signature", "gvisor" in version.lower(), version, "gVisor signature"))
    except OSError as exc:
        checks.append(result("gvisor_proc_signature", "unknown", type(exc).__name__, "readable /proc/version"))
    checks.append(assess("non_root", os.geteuid() != 0, {"uid": os.getuid(), "euid": os.geteuid()}, "euid != 0"))
    try:
        fields = dict(line.split(":", 1) for line in read_text("/proc/self/status").splitlines() if ":" in line)
        caps = fields.get("CapEff", "").strip()
        checks.append(assess("no_effective_capabilities", bool(caps) and int(caps, 16) == 0, caps, "CapEff == 0"))
        checks.append(assess("no_new_privileges", fields.get("NoNewPrivs", "").strip() == "1",
                             fields.get("NoNewPrivs", "").strip(), "NoNewPrivs == 1"))
    except (OSError, ValueError) as exc:
        checks.append(result("process_security_status", "unknown", type(exc).__name__, "readable process status"))
    for name, candidates in [
        ("no_serviceaccount_token", ["/var/run/secrets/kubernetes.io/serviceaccount/token",
                                     "/run/secrets/kubernetes.io/serviceaccount/token"]),
        ("no_known_host_root_mount", ["/host", "/hostroot", "/host-root", "/rootfs", "/mnt/host"]),
        ("no_runtime_socket", ["/var/run/docker.sock", "/run/docker.sock", "/run/containerd/containerd.sock",
                               "/var/run/containerd/containerd.sock", "/run/crio/crio.sock"]),
    ]:
        found, unknown = [], []
        for candidate in candidates:
            try:
                Path(candidate).lstat()
                found.append(candidate)
            except FileNotFoundError:
                pass
            except OSError:
                unknown.append(candidate)
        checks.append(result(name, "fail" if found else "unknown" if unknown else "pass",
                             {"present_paths": found, "uninspectable_paths": unknown}, "none of the known paths present"))
    for path, name, predicate, expected in [
        ("/", "readonly_root_mount", lambda item: "ro" in item["options"], "root mount is read-only"),
        ("/tmp", "tmp_memory_mount", lambda item: item["filesystem"] == "tmpfs", "/tmp is on tmpfs"),
    ]:
        try:
            mount = mount_for(path)
            checks.append(assess(name, bool(mount) and predicate(mount), mount, expected))
        except OSError as exc:
            checks.append(result(name, "unknown", type(exc).__name__, expected))
    try:
        work_mount = mount_for(work_dir)
    except OSError:
        work_mount = None
    return checks, {"uname": dict(platform.uname()._asdict()), "work_mount": work_mount,
                    "ebs_volume_identity_verified_by_this_probe": False}


def fixture_paths(work_dir):
    directory = Path(work_dir) / "recall-sandbox-probe"
    return directory, directory / "identity.json", directory / "state.sqlite3"


def open_existing_sqlite(path):
    return sqlite3.connect(path.resolve().as_uri() + "?mode=rw", uri=True, timeout=5, isolation_level=None)


def crash_worker(path, identity):
    db = open_existing_sqlite(Path(path))
    db.execute("PRAGMA synchronous=FULL")
    if db.execute("SELECT value FROM probe_state WHERE key='identity'").fetchone() != (identity,):
        return 74
    db.execute("PRAGMA cache_size=8")
    db.execute("BEGIN IMMEDIATE")
    db.execute("UPDATE probe_state SET value=CAST(value AS INTEGER)+1 WHERE key='durable_sequence'")
    db.execute("COMMIT")
    db.execute("BEGIN IMMEDIATE")
    # Spill uncommitted synthetic pages into WAL before an unclean process exit.
    db.executemany("INSERT INTO crash_scratch VALUES(?,?)", [(i, "x" * 8192) for i in range(64)])
    os._exit(73)


def sqlite_probe(work_dir, phase):
    directory, marker_path, database = fixture_paths(work_dir)
    if phase == "initial":
        if marker_path.exists() or database.exists() or directory.exists():
            return result("sqlite_persistence_and_crash_recovery", "fail", {"reason": "fixture_already_exists"}, "initial requires a new fixture")
        if not Path(work_dir).is_dir():
            return result("sqlite_persistence_and_crash_recovery", "fail", {"reason": "work_directory_missing"}, "mounted work directory")
        directory.mkdir(mode=0o700)
        identity = str(uuid.uuid4())
        db = sqlite3.connect(database, timeout=5, isolation_level=None)
        os.chmod(database, 0o600)
        db.execute("PRAGMA journal_mode=WAL")
        db.execute("PRAGMA synchronous=FULL")
        db.executescript(
            "CREATE TABLE probe_state(key TEXT PRIMARY KEY,value TEXT NOT NULL);"
            "CREATE TABLE crash_scratch(key INTEGER PRIMARY KEY,value TEXT NOT NULL);"
        )
        db.executemany("INSERT INTO probe_state VALUES(?,?)", [("identity", identity), ("durable_sequence", "0")])
        db.close()
        marker = {"schema_version": 1, "identity": identity}
        fd = os.open(marker_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(marker, handle, sort_keys=True)
            handle.flush()
            os.fsync(handle.fileno())
        descriptor = os.open(directory, os.O_RDONLY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    else:
        # Missing resume state is a failure. Never silently create replacement state.
        if any(path.is_symlink() for path in [directory, marker_path, database]):
            return result("sqlite_persistence_and_crash_recovery", "fail", {"reason": "fixture_symlink"},
                          "real synthetic fixture files, not symbolic links")
        if not marker_path.is_file() or not database.is_file():
            return result("sqlite_persistence_and_crash_recovery", "fail", {"reason": "resume_state_missing"},
                          "the original identity marker and SQLite database must exist")
        if marker_path.stat().st_size > 4096:
            return result("sqlite_persistence_and_crash_recovery", "fail", {"reason": "invalid_marker"}, "bounded identity marker")
        marker = json.loads(marker_path.read_text(encoding="utf-8"))
        if marker.get("schema_version") != 1:
            return result("sqlite_persistence_and_crash_recovery", "fail", {"reason": "invalid_marker"}, "schema_version 1")
        identity = str(uuid.UUID(marker["identity"]))
    db = open_existing_sqlite(database)
    try:
        stored = db.execute("SELECT value FROM probe_state WHERE key='identity'").fetchone()
        if stored != (identity,):
            return result("sqlite_persistence_and_crash_recovery", "fail", {"reason": "identity_mismatch"}, "same persisted identity")
        journal = db.execute("PRAGMA journal_mode").fetchone()[0]
        db.execute("PRAGMA synchronous=FULL")
        before = int(db.execute("SELECT value FROM probe_state WHERE key='durable_sequence'").fetchone()[0])
        child = subprocess.run([sys.executable, SCRIPT, "--_crash-child", str(database), identity],
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15, check=False)
        wal = Path(str(database) + "-wal")
        wal_bytes = wal.stat().st_size if wal.exists() else 0
        integrity = [row[0] for row in db.execute("PRAGMA integrity_check")]
        after = int(db.execute("SELECT value FROM probe_state WHERE key='durable_sequence'").fetchone()[0])
        scratch = db.execute("SELECT count(*) FROM crash_scratch").fetchone()[0]
        healthy = journal == "wal" and child.returncode == 73 and integrity == ["ok"] and after == before + 1 and scratch == 0 and wal_bytes > 0
        return assess("sqlite_persistence_and_crash_recovery", healthy,
                      {"identity": identity, "phase": phase, "journal_mode": journal, "synchronous": "FULL",
                       "sequence_before": before, "sequence_after": after, "crash_exit": child.returncode,
                       "wal_bytes_after_crash": wal_bytes, "integrity_check": integrity, "uncommitted_rows": scratch},
                      "same identity; committed increment persists; crashed transaction rolls back; integrity ok")
    finally:
        db.close()


def local_communication_probe(work_dir):
    directory = Path(work_dir) / "recall-sandbox-probe"
    if not directory.is_dir():
        return result("local_tcp_and_unix", "fail", "fixture directory absent", "writable fixture directory")
    unix_path = directory / ("socket-" + uuid.uuid4().hex[:8])
    for family, address in [(socket.AF_INET, ("127.0.0.1", 0)), (socket.AF_UNIX, str(unix_path))]:
        listener = socket.socket(family, socket.SOCK_STREAM)
        client = socket.socket(family, socket.SOCK_STREAM)
        try:
            listener.settimeout(2)
            client.settimeout(2)
            listener.bind(address)
            listener.listen(1)
            client.connect(listener.getsockname())
            peer, _ = listener.accept()
            with peer:
                peer.settimeout(2)
                client.sendall(b"synthetic-probe")
                if peer.recv(64) != b"synthetic-probe":
                    raise RuntimeError("local communication payload mismatch")
        finally:
            listener.close()
            client.close()
            if family == socket.AF_UNIX:
                unix_path.unlink(missing_ok=True)
    return result("local_tcp_and_unix", "pass", {"loopback_tcp": True, "unix_socket": True},
                  "synthetic payload delivered over both local transports")


def endpoint(value):
    if value.startswith("["):
        host, delimiter, port = value[1:].partition("]:")
    else:
        host, delimiter, port = value.rpartition(":")
        if ":" in host:
            raise ValueError("IPv6 endpoints must use [address]:port")
    if not delimiter or not host or not port.isdigit() or not 1 <= int(port) <= 65535:
        raise ValueError("endpoint must be host:port or [IPv6]:port")
    if any(character.isspace() for character in host) or len(host) > 253:
        raise ValueError("invalid endpoint host")
    return host, int(port)


def tcp_worker(value, timeout):
    host, port = endpoint(value)
    start = time.monotonic()
    print(json.dumps({"phase": "resolving"}), flush=True)
    try:
        addresses = socket.getaddrinfo(host, port, type=socket.SOCK_STREAM)
    except socket.gaierror:
        print(json.dumps({"outcome": "dns_error"}), flush=True)
        return
    print(json.dumps({"phase": "connecting"}), flush=True)
    outcomes = []
    for family, kind, protocol, _, address in addresses[:8]:
        remaining = timeout - (time.monotonic() - start)
        if remaining <= 0:
            outcomes.append("timeout")
            break
        with socket.socket(family, kind, protocol) as connection:
            connection.settimeout(remaining)
            try:
                connection.connect(address)
                print(json.dumps({"outcome": "connected"}), flush=True)
                return
            except (socket.timeout, TimeoutError):
                outcomes.append("timeout")
            except OSError as exc:
                outcomes.append({
                    errno.ECONNREFUSED: "refused", errno.ENETUNREACH: "unreachable",
                    errno.EHOSTUNREACH: "unreachable", errno.EACCES: "permission_denied",
                    errno.EPERM: "permission_denied",
                }.get(exc.errno, "socket_error"))
    # Any response from a refused port is inconclusive about a deny policy.
    outcome = "refused" if "refused" in outcomes else outcomes[-1] if outcomes else "socket_error"
    print(json.dumps({"outcome": outcome}), flush=True)


def network_semantics(outcome, expect):
    if expect == "allow":
        return "pass" if outcome == "connected" else "fail"
    if outcome == "connected":
        return "fail"
    if outcome in {"timeout", "unreachable", "permission_denied"}:
        return "pass"
    return "unknown"


def tcp_probe(value, expect, name, timeout=2.0):
    endpoint(value)
    start = time.monotonic()
    child = subprocess.Popen([sys.executable, SCRIPT, "--_tcp", value, str(timeout)],
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    timed_out = False
    try:
        stdout, _ = child.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        child.kill()
        stdout, _ = child.communicate()
    events = []
    for line in stdout.decode("utf-8", errors="replace").splitlines():
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    outcome = next((item["outcome"] for item in reversed(events) if "outcome" in item), None)
    if outcome is None:
        connecting = any(item.get("phase") == "connecting" for item in events)
        outcome = "timeout" if timed_out and connecting else "dns_timeout" if timed_out else "probe_error"
    return result(name, network_semantics(outcome, expect),
                  {"endpoint": value, "outcome": outcome, "elapsed_ms": round((time.monotonic() - start) * 1000),
                   "payload_sent": False}, "TCP connects" if expect == "allow" else "TCP connection cannot be established")


def guarded(name, callback):
    try:
        return callback()
    except Exception as exc:
        return result(name, "fail", {"error_type": type(exc).__name__, "message": str(exc)[:300]}, "probe completes")


def main(argv=None):
    parser = JsonArgumentParser(description=__doc__)
    parser.add_argument("--phase", required=True, choices=["initial", "resume"])
    parser.add_argument("--work-dir", default="/work")
    parser.add_argument("--kubernetes-endpoint", help="Override the Service environment endpoint for API deny testing.")
    parser.add_argument("--allow-endpoint", action="append", default=[])
    parser.add_argument("--deny-endpoint", action="append", default=[])
    args = parser.parse_args(argv)
    if len(args.allow_endpoint) + len(args.deny_endpoint) > 16:
        parser.error("at most 16 additional network endpoints")
    for value in [*args.allow_endpoint, *args.deny_endpoint, *([args.kubernetes_endpoint] if args.kubernetes_endpoint else [])]:
        try:
            endpoint(value)
        except ValueError as exc:
            parser.error(str(exc))
    config, runtime = configuration_checks(args.work_dir)
    probes = [
        guarded("sqlite_persistence_and_crash_recovery", lambda: sqlite_probe(args.work_dir, args.phase)),
        guarded("local_tcp_and_unix", lambda: local_communication_probe(args.work_dir)),
    ]
    kubernetes = args.kubernetes_endpoint
    if not kubernetes and os.environ.get("KUBERNETES_SERVICE_HOST"):
        host = os.environ["KUBERNETES_SERVICE_HOST"]
        if ":" in host:
            host = f"[{host}]"
        kubernetes = f"{host}:{os.environ.get('KUBERNETES_SERVICE_PORT', '443')}"
    blocked = [("ipv4_imds_denied", "169.254.169.254:80"), ("external_tcp_denied", "1.1.1.1:443")]
    if kubernetes:
        blocked.append(("kubernetes_api_denied", kubernetes))
    else:
        probes.append(result("kubernetes_api_denied", "unknown", "Kubernetes Service endpoint unavailable", "known API endpoint"))
    blocked.extend((f"additional_deny_{index}", value) for index, value in enumerate(args.deny_endpoint))
    for name, value in blocked:
        probes.append(guarded(name, lambda value=value, name=name: tcp_probe(value, "deny", name)))
    for index, value in enumerate(args.allow_endpoint):
        name = f"allow_canary_{index}"
        probes.append(guarded(name, lambda value=value, name=name: tcp_probe(value, "allow", name)))
    passed = all(check["status"] == "pass" for check in config + probes)
    response = {
        "schema": SCHEMA, "phase": args.phase,
        "recorded_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "status": "pass" if passed else "fail",
        "configuration_assessment": {"checks": config, "runtime_observations": runtime},
        "probes": probes,
        "limits": [
            "Synthetic observations at this instant; not a security guarantee or proof against container/kernel escapes.",
            "Mount observations do not prove EBS identity, exclusive attachment, runtime handler, or node configuration; verify externally.",
            "An unreachable TCP target is not proof NetworkPolicy caused the denial; refused ports and DNS errors remain unknown.",
            "No provider calls, HTTP payloads, real host secret reads, or credential retrieval were performed.",
            "Crash test covers process exit; resume checks persisted state and needs external evidence of the Pod transition. No power-loss or fencing proof.",
        ],
        "qualified_for_open_agent_execution": False,
    }
    print(json.dumps(response, sort_keys=True))
    return 0 if passed else 1


if __name__ == "__main__":
    if len(sys.argv) == 4 and sys.argv[1] == "--_crash-child":
        sys.exit(crash_worker(sys.argv[2], sys.argv[3]))
    if len(sys.argv) == 4 and sys.argv[1] == "--_tcp":
        tcp_worker(sys.argv[2], float(sys.argv[3]))
        sys.exit(0)
    sys.exit(main())
