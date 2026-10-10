"""Trusted KiroCrew Gateway bridge; not an automatically registered App/V2 backend."""

import argparse
from dataclasses import asdict, dataclass
import json
import os
from pathlib import Path
import sys

from . import host_runtime as host
from .client import MemoryError, TransportError


@dataclass(frozen=True)
class FrozenContext:
    host_instance_id: str
    member_id: str
    store_id: str
    memory_mode: str
    session_key: str
    agent_id: str

    def validate(self):
        for key, value in asdict(self).items():
            if key == "memory_mode":
                host.privacy(value)
            else:
                host.identifier(value, key)


class GatewayBridge:
    """Construct in trusted Gateway code from its frozen ExecutionContext.

    Never expose the context constructor or configuration path as model-provided
    tool arguments. A member/store pair selects one already-bound local daemon.
    """

    def __init__(self, runtime, context):
        if not isinstance(context, FrozenContext):
            raise host.HostError("frozen_gateway_context_required")
        context.validate()
        expected = runtime.config["gateway_binding"]
        for key in ("host_instance_id", "member_id", "store_id"):
            if getattr(context, key) != expected[key]:
                raise host.HostError("gateway_binding_mismatch")
        self.runtime, self.context = runtime, context

    def capture(self, *, event_id, body, origin="user_message"):
        context = self.context
        return self.runtime.capture(
            event_id=event_id, session_id=context.session_key, agent_id=context.agent_id,
            body=body, origin=origin, memory_mode=context.memory_mode,
        )

    def flush(self):
        return self.runtime.flush(memory_mode=self.context.memory_mode)

    def read(self, *, query):
        return self.runtime.read(
            query=query, session_id=self.context.session_key, agent_id=self.context.agent_id,
            memory_mode=self.context.memory_mode,
        )

    def refresh(self, *, query, manifest=None):
        return self.runtime.refresh(
            query=query, session_id=self.context.session_key, agent_id=self.context.agent_id,
            memory_mode=self.context.memory_mode, manifest=manifest,
        )


def install(state, socket_path, *, host_instance_id, member_id, store_id, binary):
    state = Path(state).expanduser().absolute()
    host.no_symlinks(state)
    parent = state.parent.resolve(strict=True)
    state = parent / state.name
    socket_path = str(Path(socket_path).expanduser().absolute())
    selected = {
        "host_instance_id": host.identifier(host_instance_id, "host_instance_id"),
        "member_id": host.identifier(member_id, "member_id"),
        "store_id": host.identifier(store_id, "store_id"),
    }
    journal = state / "installation.json"
    if journal.exists() and host.read_json(journal)["phase"] in ("installing", "installed", "uninstalling"):
        runtime = host.Runtime.load(state)
        if runtime.config["gateway_binding"] != selected or runtime.config["socket"] != socket_path:
            raise host.HostError("installation_binding_changed")
        runtime.assert_binding()
        return host.install_files(state, runtime.config, {})
    config = host.new_config("kirocrew", parent, state, socket_path)
    config["gateway_binding"] = selected
    binding_path = state / "binding.json"
    example_path = state / "mcp.example.json"
    originals = {
        binding_path: host.optional_bytes(binding_path),
        example_path: host.optional_bytes(example_path),
    }
    example = {
        "mcpServers": {
            "recall-ledger": {
                "command": str(binary),
                "args": ["mcp", "--socket", socket_path, "--binding-file", str(binding_path)],
            }
        }
    }
    result = host.install_files(state, config, {
        binding_path: host.encode(config["expected_binding"]),
        example_path: host.encode(example),
    }, expected_before=originals)
    result.update({
        "gateway_binding": selected, "expected_binding": config["expected_binding"],
        "mcp_example": str(state / "mcp.example.json"),
        "gateway_wired": False, "native_memory": "coexists_no_v2_replacement",
    })
    return result


def load_context(path):
    path = Path(path)
    host.no_symlinks(path)
    stat = path.stat()
    if stat.st_uid != os.getuid() or stat.st_mode & 0o077:
        raise host.HostError("gateway_context_file_must_be_owner_private")
    value = host.read_json(path)
    if set(value) != set(FrozenContext.__dataclass_fields__):
        raise host.HostError("invalid_gateway_context")
    return FrozenContext(**value)


def execute(bridge, envelope):
    if not isinstance(envelope, dict):
        raise host.HostError("invalid_gateway_envelope")
    operation = envelope.get("operation")
    allowed = {
        "capture": {"operation", "event_id", "body", "origin"},
        "read": {"operation", "query"},
        "refresh": {"operation", "query", "manifest"},
        "flush": {"operation"},
    }
    if operation not in allowed or set(envelope) - allowed[operation]:
        raise host.HostError("unsupported_gateway_fields")
    if operation == "capture":
        return bridge.capture(event_id=envelope.get("event_id"), body=envelope.get("body"), origin=envelope.get("origin", "user_message"))
    if operation == "read":
        return bridge.read(query=envelope.get("query", ""))
    if operation == "refresh":
        return bridge.refresh(query=envelope.get("query", ""), manifest=envelope.get("manifest"))
    return bridge.flush()


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    commands = result.add_subparsers(dest="command", required=True)
    for name in ("install", "call", "uninstall"):
        command = commands.add_parser(name)
        command.add_argument("--state", required=True)
        if name == "install":
            for field in ("socket", "host-instance-id", "member-id", "store-id", "binary"):
                command.add_argument("--" + field, required=True)
        if name == "call":
            command.add_argument("--context-file", required=True)
    return result


def main(argv=None):
    args = parser().parse_args(argv)
    try:
        if args.command == "install":
            result = install(args.state, args.socket, host_instance_id=args.host_instance_id, member_id=args.member_id, store_id=args.store_id, binary=args.binary)
            limit = 4096
        elif args.command == "uninstall":
            result = host.uninstall_files(Path(args.state))
            limit = 4096
        else:
            runtime = host.Runtime.load(Path(args.state))
            bridge = GatewayBridge(runtime, load_context(args.context_file))
            raw = sys.stdin.buffer.read(1024 * 1024 + 1)
            if len(raw) > 1024 * 1024:
                raise host.HostError("gateway_input_too_large")
            result = execute(bridge, json.loads(raw))
            limit = runtime.config["max_output_bytes"]
        sys.stdout.buffer.write(host.bounded_output(result, limit))
        return 0
    except (host.HostError, MemoryError, TransportError, OSError, ValueError, TypeError, KeyError) as exc:
        code = exc.code if isinstance(exc, MemoryError) else str(exc) if isinstance(exc, host.HostError) else type(exc).__name__
        # This CLI is for a Gateway subprocess, not ScriptHookStore Stop stdout.
        sys.stderr.write("RecallLedger KiroCrew error: " + code + "\n")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
