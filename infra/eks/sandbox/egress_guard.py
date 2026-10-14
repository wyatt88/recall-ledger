#!/usr/bin/env python3
"""Node-local, one-trial egress guard for the controlled EKS qualification.

Run in the dedicated node's host network namespace as root. The trusted
controller must keep the trial behind an init gate until install/verify succeeds,
stop the Pod before remove, and requalify after any node/Pod/interface restart.
This is not a Kubernetes admission policy or a continuous firewall reconciler.
"""

from __future__ import annotations

import argparse
import contextlib
from dataclasses import asdict, dataclass
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import shlex
import stat
import subprocess
import sys
import tempfile
from typing import Callable


STATE_DIR = Path("/run/recall-ledger-egress")
GATEWAY_PORT = 18080


class GuardError(RuntimeError):
    """Qualification failed; the controller must not release the trial."""


class Runner:
    def __call__(self, args: list[str]) -> subprocess.CompletedProcess[str]:
        if args[0] not in {"ip", "iptables", "ip6tables"}:
            raise GuardError("unsupported executable")
        return subprocess.run(
            args, text=True, capture_output=True, timeout=20, check=False,
            env={**os.environ, "LC_ALL": "C"},
        )


@dataclass(frozen=True)
class Config:
    pod_ip: str
    gateway_ip: str
    veth: str

    def __post_init__(self) -> None:
        for value in (self.pod_ip, self.gateway_ip):
            try:
                address = ipaddress.IPv4Address(value)
            except ipaddress.AddressValueError as exc:
                raise GuardError("pod-ip and gateway-ip must be plain IPv4 addresses") from exc
            if address.is_loopback or address.is_multicast or address.is_unspecified or address.is_link_local:
                raise GuardError("pod-ip and gateway-ip must be unicast non-link-local addresses")
        if self.pod_ip == self.gateway_ip:
            raise GuardError("pod-ip and gateway-ip must differ")
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,14}", self.veth):
            raise GuardError("invalid Linux interface name")
        if self.veth == "lo":
            raise GuardError("loopback cannot be guarded")

    @property
    def key(self) -> str:
        encoded = json.dumps(asdict(self), sort_keys=True).encode()
        return hashlib.sha256(encoded).hexdigest()[:12]


@dataclass(frozen=True)
class Binding:
    tool: str
    table: str
    hook: str
    chain: str
    jump: tuple[str, ...]
    rules: tuple[tuple[str, ...], ...]


def bindings(config: Config) -> list[Binding]:
    """Use pre-DNAT egress and post-DNAT ingress/output hooks."""
    result = []
    pod = f"{config.pod_ip}/32"
    gateway = f"{config.gateway_ip}/32"
    for tool in ("iptables", "ip6tables"):
        for suffix, table, hook, direction in (
            ("E", "mangle", "PREROUTING", "-i"),
            ("I", "filter", "FORWARD", "-o"),
            ("H", "filter", "OUTPUT", "-o"),
        ):
            chain = f"RLEG_{config.key}_{suffix}"
            allowed: list[tuple[str, ...]] = []
            if tool == "iptables" and suffix == "E":
                allowed.append((
                    "-s", pod, "-d", gateway, "-p", "tcp", "-m", "tcp",
                    "--dport", str(GATEWAY_PORT), "-j", "RETURN",
                ))
            if tool == "iptables" and suffix == "H":
                allowed.append((
                    "-s", gateway, "-d", pod, "-p", "tcp", "-m", "tcp",
                    "--sport", str(GATEWAY_PORT), "-m", "conntrack",
                    "--ctstate", "ESTABLISHED", "--ctdir", "REPLY", "-j", "RETURN",
                ))
            result.append(Binding(
                tool, table, hook, chain,
                (direction, config.veth, "-m", "comment",
                 "--comment", f"recall-ledger:{config.key}", "-j", chain),
                tuple(allowed + [("-j", "DROP")]),
            ))
    return result


class Guard:
    def __init__(
        self, config: Config, state_dir: Path = STATE_DIR,
        runner: Callable | None = None, boot_id: Callable | None = None,
        netns_id: Callable | None = None,
    ) -> None:
        self.config = config
        self.state_dir = state_dir
        self.state_path = state_dir / f"{config.veth}.json"
        self.runner = runner or Runner()
        self.boot_id = boot_id or (
            lambda: Path("/proc/sys/kernel/random/boot_id").read_text().strip()
        )
        self.netns_id = netns_id or (lambda: os.stat("/proc/self/ns/net").st_ino)
        self.bindings = bindings(config)

    def run(self, *args: str) -> str:
        result = self.runner(list(args))
        if result.returncode:
            raise GuardError(f"{args[0]} failed ({result.returncode}): {result.stderr.strip()}")
        return result.stdout

    def ip_json(self, *args: str) -> list[dict]:
        try:
            value = json.loads(self.run("ip", "-j", *args))
        except json.JSONDecodeError as exc:
            raise GuardError("ip returned invalid JSON") from exc
        if not isinstance(value, list) or not all(isinstance(item, dict) for item in value):
            raise GuardError("ip returned an unexpected result")
        return value

    def identity(self) -> dict:
        routes = self.ip_json("route", "get", self.config.pod_ip)
        if len(routes) != 1 or routes[0].get("dev") != self.config.veth or routes[0].get("gateway"):
            raise GuardError("pod route does not directly resolve to the requested veth")
        for family in ("-4", "-6"):
            defaults = self.ip_json(family, "route", "show", "default")
            if any(
                item.get("dev") == self.config.veth
                or any(hop.get("dev") == self.config.veth for hop in item.get("nexthops", []))
                for item in defaults
            ):
                raise GuardError("refusing to guard a default-route interface")
        links = self.ip_json("-d", "link", "show", "dev", self.config.veth)
        if len(links) != 1:
            raise GuardError("expected exactly one live host veth")
        link = links[0]
        if link.get("ifname") != self.config.veth or link.get("linkinfo", {}).get("info_kind") != "veth":
            raise GuardError("requested interface is not a veth")
        if not isinstance(link.get("ifindex"), int) or not isinstance(link.get("link_index"), int):
            raise GuardError("veth ifindex/peer identity unavailable")
        if not re.fullmatch(r"[0-9a-fA-F]{2}(?::[0-9a-fA-F]{2}){5}", link.get("address", "")):
            raise GuardError("veth MAC identity unavailable")
        addresses = self.ip_json("-4", "address", "show")
        local_ips = {
            address.get("local")
            for interface in addresses
            for address in interface.get("addr_info", [])
        }
        if self.config.gateway_ip not in local_ips:
            raise GuardError("gateway-ip is not a local node address")
        if self.config.pod_ip in local_ips:
            raise GuardError("pod-ip must not be a local node address")
        return {
            "boot_id": self.boot_id(), "netns_inode": self.netns_id(),
            "ifindex": link["ifindex"], "peer_ifindex": link["link_index"],
            "mac": link["address"].lower(),
        }

    def ipt(self, binding: Binding, *args: str) -> str:
        return self.run(binding.tool, "-w", "5", "-t", binding.table, *args)

    def chain_rules(self, binding: Binding, chain: str) -> list[tuple[str, ...]] | None:
        # iptables-nft can report an absent named chain as "incompatible".
        # Never classify that error as absence. A successful, complete table
        # listing with no exact declaration is the only absence evidence.
        result = self.runner([binding.tool, "-w", "5", "-t", binding.table, "-S"])
        if result.returncode or result.stderr.strip():
            raise GuardError(f"{binding.tool} read failed ({result.returncode}): {result.stderr.strip()}")
        declared: set[str] = set()
        rules: dict[str, list[tuple[str, ...]]] = {}
        for line in result.stdout.splitlines():
            words = shlex.split(line)
            if len(words) == 2 and words[0] == "-N":
                name = words[1]
            elif len(words) == 3 and words[0] == "-P" and words[2] in {"ACCEPT", "DROP"}:
                name = words[1]
            elif len(words) >= 4 and words[0] == "-A" and words[1] in declared:
                rules[words[1]].append(tuple(words[2:]))
                continue
            else:
                raise GuardError(f"unexpected {binding.tool} rule readback")
            if name in declared:
                raise GuardError(f"duplicate {binding.tool} chain declaration")
            declared.add(name)
            rules[name] = []
        return rules.get(chain)

    def save(self, state: dict) -> None:
        fd, temporary = tempfile.mkstemp(prefix=".state-", dir=self.state_dir)
        try:
            with os.fdopen(fd, "w") as handle:
                json.dump(state, handle, sort_keys=True, indent=2)
                handle.write("\n")
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(temporary, self.state_path)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)

    def load(self) -> dict | None:
        if not self.state_path.exists():
            return None
        self.check_owned(self.state_path, directory=False)
        try:
            state = json.loads(self.state_path.read_text())
        except (json.JSONDecodeError, OSError) as exc:
            raise GuardError("cannot read saved guard state") from exc
        if not isinstance(state, dict) or state.get("version") != 1 or state.get("config") != asdict(self.config):
            raise GuardError("guard state does not match this request")
        expected = [asdict(binding) for binding in self.bindings]
        if state.get("bindings") != json.loads(json.dumps(expected)):
            raise GuardError("saved rule ownership does not match this guard version")
        return state

    @staticmethod
    def check_owned(path: Path, *, directory: bool) -> None:
        metadata = path.lstat()
        expected = stat.S_ISDIR if directory else stat.S_ISREG
        if not expected(metadata.st_mode) or metadata.st_uid != os.geteuid() or metadata.st_mode & 0o077:
            raise GuardError("guard state must be owner-only, owned by this administrator, and not a symlink")

    @contextlib.contextmanager
    def locked(self):
        self.state_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.check_owned(self.state_dir, directory=True)
        lock_path = self.state_dir / ".lock"
        fd = os.open(lock_path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        try:
            self.check_owned(lock_path, directory=False)
            fcntl.flock(fd, fcntl.LOCK_EX)
            yield
        finally:
            os.close(fd)

    def inspect(self, state: dict) -> dict:
        if state.get("phase") != "installed":
            raise GuardError("guard installation is incomplete; keep the trial gated")
        if state.get("identity") != self.identity():
            raise GuardError("node/network/interface identity changed; a new qualification is required")
        readback = []
        for binding in self.bindings:
            chain = self.chain_rules(binding, binding.chain)
            hook = self.chain_rules(binding, binding.hook)
            if chain != list(binding.rules):
                raise GuardError(f"guard chain changed or missing: {binding.tool}/{binding.chain}")
            if not hook or hook[0] != binding.jump or hook.count(binding.jump) != 1:
                raise GuardError(f"guard jump missing, duplicated or not first: {binding.tool}/{binding.hook}")
            readback.append({
                "tool": binding.tool, "table": binding.table, "hook": binding.hook,
                "chain": binding.chain, "rules": len(chain), "first_jump_verified": True,
            })
        return {"status": "verified", "identity": state["identity"], "rules": readback}

    def install(self) -> dict:
        with self.locked():
            state = self.load()
            if state is not None:
                return self.inspect(state)
            if list(self.state_dir.glob("*.json")):
                raise GuardError("another trial guard is recorded on this dedicated node")
            identity = self.identity()
            for binding in self.bindings:
                if self.chain_rules(binding, binding.chain) is not None:
                    raise GuardError("unowned chain collision; refusing to adopt or flush it")
                if self.chain_rules(binding, binding.hook) is None:
                    raise GuardError("required iptables hook is unavailable")
            state = {
                "version": 1, "phase": "installing", "config": asdict(self.config),
                "identity": identity, "bindings": [asdict(binding) for binding in self.bindings],
            }
            self.save(state)
            # Establish all deny paths before adding either permission. A failed
            # install keeps its state and rules for inspection; it never rolls open.
            for binding in self.bindings:
                self.ipt(binding, "-N", binding.chain)
                self.ipt(binding, "-A", binding.chain, "-j", "DROP")
                self.ipt(binding, "-I", binding.hook, "1", *binding.jump)
            if self.identity() != identity:
                raise GuardError("interface changed during installation; keep the trial gated")
            for binding in self.bindings:
                for rule in reversed(binding.rules[:-1]):
                    self.ipt(binding, "-I", binding.chain, "1", *rule)
            state["phase"] = "installed"
            self.save(state)
            return self.inspect(state)

    def verify(self) -> dict:
        with self.locked():
            state = self.load()
            if state is None:
                raise GuardError("no guard state for this trial")
            return self.inspect(state)

    def remove(self) -> dict:
        with self.locked():
            state = self.load()
            if state is None:
                return {"status": "already_removed"}
            # Never create an unguarded window for a live or reused interface.
            if any(link.get("ifname") == self.config.veth for link in self.ip_json("-d", "link", "show")):
                raise GuardError("stop/delete the trial Pod and wait for its veth to disappear before remove")
            state["phase"] = "removing"
            self.save(state)
            for binding in reversed(self.bindings):
                hook = self.chain_rules(binding, binding.hook)
                if hook is None:
                    raise GuardError("required iptables hook disappeared during removal")
                for _ in range(hook.count(binding.jump)):
                    self.ipt(binding, "-D", binding.hook, *binding.jump)
                chain = self.chain_rules(binding, binding.chain)
                if chain is None:
                    continue
                for rule in binding.rules:
                    for _ in range(chain.count(rule)):
                        self.ipt(binding, "-D", binding.chain, *rule)
                remaining = self.chain_rules(binding, binding.chain)
                if remaining:
                    raise GuardError("unknown rules remain in owned chain; refusing to flush them")
                self.ipt(binding, "-X", binding.chain)
            self.state_path.unlink()
            return {"status": "removed"}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("install", "verify", "remove"))
    parser.add_argument("--pod-ip", required=True)
    parser.add_argument("--gateway-ip", required=True)
    parser.add_argument("--veth", required=True)
    args = parser.parse_args(argv)
    try:
        if os.geteuid() != 0:
            raise GuardError("run as root in the dedicated node host network namespace")
        guard = Guard(Config(args.pod_ip, args.gateway_ip, args.veth))
        result = getattr(guard, args.action)()
        print(json.dumps(result, sort_keys=True))
        return 0
    except (GuardError, OSError, subprocess.TimeoutExpired) as exc:
        print(json.dumps({"status": "failed", "error": str(exc)}), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
