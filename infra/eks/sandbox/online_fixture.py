#!/usr/bin/env python3
"""Credential-free fixtures for one controlled, same-boot online qualification.

The controller owns the online RuntimeClass, host firewall and reboot guards.
It must add GATE_ANNOTATIONS' exact keys to that runtime's pod_annotations
allowlist, verify the Pod's firewall, then atomically write /gate/ready containing
{"pod_uid": "<this Pod UID>", "nonce": "<render nonce>"} with mode 0644 (or
ownership readable by UID 1000). The gate is a fresh disk emptyDir, never a PVC.
The controller must keep the online runtime configuration under /run and require
/run/recall-online-boot-authorized before containerd and kubelet start; after a
reboot, qualification starts again. A Pod gate cannot enforce that host policy.
Mount-hint support was checked in release-20261005.0:
https://github.com/google/gvisor/blob/release-20261005.0/pkg/shim/v1/utils/volumes.go
This fixture does not configure or qualify the host or contact a model provider.
"""

from __future__ import annotations

import argparse
import contextlib
import copy
import datetime
import hmac
import http.client
from http.server import BaseHTTPRequestHandler, HTTPServer
import ipaddress
import json
import os
import re
import signal
import socket
import stat
import subprocess
import sys
import threading
import time
import uuid


SCHEMA = "recall-ledger.online-fixture/1"
ONLINE_RUNTIME = "recall-sandbox-online-qual"
GATE_ANNOTATIONS = {
    "dev.gvisor.empty-dir.gate.force-shared": "true",
    "dev.gvisor.spec.mount.gate.share": "shared",
    "dev.gvisor.spec.mount.gate.type": "bind",
    "dev.gvisor.spec.mount.gate.options": "ro,rprivate",
}
GATE_TIMEOUT = 180
AUDIT_SECONDS = 300
HEALTH_PAYLOAD = b'{"fixture":"recall-ledger-synthetic-canary-v1","synthetic":true}\n'
SCRIPT_IN_POD = "/opt/probe/online_fixture.py"


def emit(event: str, **fields) -> None:
    print(json.dumps({
        "schema": SCHEMA,
        "time": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "event": event, **fields,
    }, sort_keys=True), flush=True)


def ipv4(value: str, *, allow_loopback: bool = False) -> str:
    address = ipaddress.IPv4Address(value)
    if (address.is_unspecified or address.is_multicast or address.is_link_local
            or address.is_reserved or (address.is_loopback and not allow_loopback)):
        raise ValueError("use an explicit unicast IPv4 address")
    return str(address)


def checked_nonce(value: str) -> str:
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9_-]{16,128}", value):
        raise ValueError("nonce must contain 16–128 ASCII letters, digits, underscores or hyphens")
    return value


def render_online(base_initial_job: dict, gateway_ip: str, nonce: str) -> dict:
    """Clone the initial manifest, preserving its PVC, peer, limits and identity."""
    gateway_ip, nonce = ipv4(gateway_ip), checked_nonce(nonce)
    job = copy.deepcopy(base_initial_job)
    if job.get("apiVersion") != "batch/v1" or job.get("kind") != "Job":
        raise ValueError("base_initial_job must be a batch/v1 Job manifest")
    metadata = job["metadata"]
    if not metadata.get("namespace") or any(key in metadata for key in ("uid", "resourceVersion")):
        raise ValueError("supply the original namespaced Job manifest, not a live API object")
    template = job["spec"]["template"]
    pod = template["spec"]
    containers = pod["containers"]
    if len(containers) != 1 or containers[0].get("name") != "probe":
        raise ValueError("base Job must contain exactly the original probe main container")
    main = containers[0]
    init = pod.get("initContainers", [])
    if not any(item.get("name") == "peer" and item.get("restartPolicy") == "Always" for item in init):
        raise ValueError("base Job must contain the native peer sidecar")
    if any(item.get("name") == "network-gate" for item in init):
        raise ValueError("base Job already contains a network gate")
    if any(volume.get("name") == "gate" for volume in pod["volumes"]):
        raise ValueError("base Job already contains a gate volume")
    if not any(volume.get("name") == "probe" and "configMap" in volume for volume in pod["volumes"]):
        raise ValueError("base Job is missing the probe ConfigMap volume")

    metadata["name"] = "probe-online"
    pod["runtimeClassName"] = ONLINE_RUNTIME
    annotations = template.setdefault("metadata", {}).setdefault("annotations", {})
    # CSI paths do not resolve through gVisor's logical-volume-name mount hints.
    for setting in ("share", "type", "options"):
        annotations.pop(f"dev.gvisor.spec.mount.data.{setting}", None)
    annotations.update(GATE_ANNOTATIONS)
    pod["volumes"].append({"name": "gate", "emptyDir": {"sizeLimit": "1Mi"}})
    security = copy.deepcopy(main["securityContext"])
    security.update({"runAsNonRoot": True, "runAsUser": 1000, "runAsGroup": 1000})
    gate = {
        "name": "network-gate", "image": main["image"],
        "command": ["python3", "-u", "-B", SCRIPT_IN_POD, "network-gate", "--nonce", nonce],
        "env": [{"name": "POD_UID", "valueFrom": {
            "fieldRef": {"apiVersion": "v1", "fieldPath": "metadata.uid"}}}],
        "securityContext": security,
        "volumeMounts": [{"name": "gate", "mountPath": "/gate", "readOnly": True},
                         {"name": "probe", "mountPath": "/opt/probe", "readOnly": True}],
        "resources": {"requests": {"cpu": "10m", "memory": "32Mi"},
                      "limits": {"cpu": "50m", "memory": "64Mi"}},
    }
    if "imagePullPolicy" in main:
        gate["imagePullPolicy"] = main["imagePullPolicy"]
    pod["initContainers"] = [gate, *init]
    main["command"] = ["python3", "-u", "-B", SCRIPT_IN_POD, "probe-main",
                       "--gateway-ip", gateway_ip]
    main.pop("args", None)
    return job


def wait_for_gate(nonce: str) -> int:
    nonce = checked_nonce(nonce)
    pod_uid = os.environ.get("POD_UID", "")
    if not pod_uid or str(uuid.UUID(pod_uid)) != pod_uid:
        raise ValueError("POD_UID must come from this Pod's metadata.uid")
    deadline = time.monotonic() + GATE_TIMEOUT
    emit("network_gate_waiting", pod_uid=pod_uid, timeout_seconds=GATE_TIMEOUT)
    while time.monotonic() < deadline:
        try:
            descriptor = os.open("/gate/ready", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        except FileNotFoundError:
            time.sleep(min(0.25, max(0, deadline - time.monotonic())))
            continue
        with os.fdopen(descriptor, "rb") as handle:
            if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
                raise ValueError("gate must be a regular file")
            raw = handle.read(4097)
        if len(raw) > 4096:
            raise ValueError("gate exceeds 4096 bytes")
        ready = json.loads(raw)
        if (not isinstance(ready, dict) or ready.get("pod_uid") != pod_uid
                or not isinstance(ready.get("nonce"), str)
                or not ready["nonce"].isascii()
                or not hmac.compare_digest(ready["nonce"], nonce)):
            raise ValueError("gate does not match this Pod UID and nonce")
        emit("network_gate_released", pod_uid=pod_uid)
        return 0
    emit("network_gate_timeout", status="failed", pod_uid=pod_uid)
    return 1


class SyntheticHandler(BaseHTTPRequestHandler):
    """Only a fixed health response; never forwards requests or accepts data."""

    server_version = "RecallSynthetic/1"
    sys_version = ""

    def reply(self, code: int, body: bytes) -> None:
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("Connection", "close")
        if code == 405:
            self.send_header("Allow", "GET")
        self.end_headers()
        self.close_connection = True
        if self.command != "HEAD":
            self.wfile.write(body)
        emit("synthetic_http_response", port=self.server.server_port,
             method=self.command, status_code=code)

    def do_GET(self) -> None:
        if self.path == "/healthz":
            self.reply(200, HEALTH_PAYLOAD)
        else:
            self.reply(404, b'{"error":"not_found"}\n')

    def reject(self) -> None:
        self.reply(405, b'{"error":"method_not_allowed"}\n')

    do_CONNECT = do_POST = do_PUT = do_DELETE = do_PATCH = do_OPTIONS = do_HEAD = reject

    def send_error(self, code, message=None, explain=None):
        self.reply(code, b'{"error":"invalid_request"}\n')

    def log_message(self, format, *args) -> None:
        pass  # Do not log URLs, bodies, headers or client addresses.


class SyntheticServer(HTTPServer):
    address_family = socket.AF_INET
    request_queue_size = 8

    def get_request(self):
        client, address = super().get_request()
        client.settimeout(3)
        return client, address

    def handle_error(self, request, client_address):
        emit("synthetic_client_error", port=self.server_port)


@contextlib.contextmanager
def listeners(bind: str, ports: tuple[int, ...]):
    servers, threads = [], []
    try:
        for port in ports:
            servers.append(SyntheticServer((bind, port), SyntheticHandler))
        for server in servers:
            thread = threading.Thread(target=server.serve_forever,
                                      kwargs={"poll_interval": 0.2}, daemon=True)
            thread.start()
            threads.append(thread)
        yield servers
    finally:
        for server in servers[:len(threads)]:
            server.shutdown()
        for server in servers:
            server.server_close()
        for thread in threads:
            thread.join(timeout=4)


def check_health(host: str, port: int) -> None:
    connection = http.client.HTTPConnection(host, port, timeout=3)
    try:
        connection.request("GET", "/healthz", headers={"Connection": "close"})
        response = connection.getresponse()
        payload = response.read(len(HEALTH_PAYLOAD) + 1)
        if response.status != 200 or payload != HEALTH_PAYLOAD:
            raise ValueError("synthetic health response mismatch")
    finally:
        connection.close()


def run_check(name: str, arguments: list[str], timeout: int) -> int:
    completed = subprocess.run([sys.executable, "-u", "-B", *arguments],
                               capture_output=True, text=True, timeout=timeout, check=False)
    try:
        document = json.loads(completed.stdout)
        if not isinstance(document, dict) or not document:
            raise ValueError("empty or non-object probe evidence")
    except (json.JSONDecodeError, ValueError):
        emit("invalid_probe_output", check=name, status="failed")
        return completed.returncode or 1
    print(json.dumps(document, sort_keys=True), flush=True)
    emit("probe_command_completed", check=name, return_code=completed.returncode,
         stderr=completed.stderr[-2000:])
    return completed.returncode


def probe_main(gateway_ip: str) -> int:
    gateway_ip = ipv4(gateway_ip)
    peer_code = run_check("peer", ["/opt/probe/peer.py", "check"], 20)
    if peer_code:
        return peer_code
    probe_code = run_check("sandbox", [
        "/opt/probe/sandbox_probe.py", "--phase", "resume", "--work-dir", "/work",
        "--allow-endpoint", f"{gateway_ip}:18080", "--deny-endpoint", f"{gateway_ip}:18081",
    ], 120)
    # A TCP connection alone does not verify that the allowed target is our canary.
    check_health(gateway_ip, 18080)
    with listeners("0.0.0.0", (18082,)):
        check_health("127.0.0.1", 18082)
        emit("inbound_fixture_ready", inbound_fixture_listening=True,
             port=18082, local_healthz="pass", audit_seconds=AUDIT_SECONDS,
             sandbox_probe_return_code=probe_code)
        time.sleep(AUDIT_SECONDS)
    emit("audit_window_completed", status="pass" if probe_code == 0 else "failed",
         sandbox_probe_return_code=probe_code)
    return probe_code


def canary(bind: str) -> int:
    bind = ipv4(bind, allow_loopback=True)
    stop = threading.Event()
    previous = {}
    for signum in (signal.SIGTERM, signal.SIGINT):
        previous[signum] = signal.signal(signum, lambda *_: stop.set())
    try:
        with listeners(bind, (18080, 18081)):
            for port in (18080, 18081):
                check_health(bind, port)
            emit("canary_ready", bind=bind, allowed_control_port=18080,
                 denied_control_port=18081, both_local_healthz="pass",
                 synthetic=True, proxy_enabled=False)
            while not stop.wait(0.5):
                pass
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)
    emit("canary_stopped")
    return 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("canary").add_argument("--bind", required=True)
    commands.add_parser("network-gate").add_argument("--nonce", required=True)
    commands.add_parser("probe-main").add_argument("--gateway-ip", required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == "canary":
            return canary(args.bind)
        if args.command == "network-gate":
            return wait_for_gate(args.nonce)
        return probe_main(args.gateway_ip)
    except (OSError, ValueError, subprocess.TimeoutExpired) as exc:
        emit("fixture_failed", status="failed", error_type=type(exc).__name__,
             message=str(exc)[:300])
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
