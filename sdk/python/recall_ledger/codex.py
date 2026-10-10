"""Codex local hooks adapter. Configuration generation never grants host trust."""

import argparse
import copy
import json
from pathlib import Path
import re
import shlex
import sys
import time

from . import host_runtime as host
from .client import MemoryError, TransportError


def project_paths(project):
    root = Path(project).expanduser().absolute()
    host.no_symlinks(root)
    root = root.resolve(strict=True)
    if not root.is_dir():
        raise host.HostError("project_must_be_directory")
    return root, root / ".codex" / "recall-ledger"


def disable_native(data):
    """Patch two keys, verifying the complete TOML semantics stay otherwise equal."""
    try:
        import tomllib
    except ImportError as exc:
        raise host.HostError("codex_install_requires_python_3_11") from exc
    text = data.decode("utf-8") if data is not None else ""
    original = tomllib.loads(text)
    expected = copy.deepcopy(original)
    memories = expected.setdefault("memories", {})
    if not isinstance(memories, dict):
        raise host.HostError("unsupported_memory_toml_shape")
    memories.update(generate_memories=False, use_memories=False)
    lines = text.splitlines(keepends=True)
    headers = [i for i, line in enumerate(lines) if re.match(r"^\s*\[memories\]\s*(?:#.*)?$", line)]
    if len(headers) > 1:
        raise host.HostError("unsupported_memory_toml_shape")
    block = "generate_memories = false\nuse_memories = false\n"
    if headers:
        start = headers[0]
        end = next((i for i in range(start + 1, len(lines)) if re.match(r"^\s*\[", lines[i])), len(lines))
        section = [
            line for line in lines[start + 1:end]
            if not re.match(r"^\s*(?:generate_memories|use_memories)\s*=", line)
        ]
        if not lines[start].endswith("\n"):
            lines[start] += "\n"
        candidate = "".join(lines[:start + 1]) + block + "".join(section + lines[end:])
    elif "memories" not in original:
        candidate = text + ("\n" if text and not text.endswith("\n") else "") + "\n[memories]\n" + block
    else:
        # Dotted top-level keys are legal TOML. Conservatively remove these two
        # assignments, prepend replacements, then compare full parsed values.
        remaining = [
            line for line in lines
            if not re.match(r"^\s*memories\.(?:generate_memories|use_memories)\s*=", line)
        ]
        candidate = "memories.generate_memories = false\nmemories.use_memories = false\n" + "".join(remaining)
    try:
        valid = tomllib.loads(candidate) == expected
    except tomllib.TOMLDecodeError:
        valid = False
    if not valid:
        raise host.HostError("unsupported_memory_toml_shape")
    return candidate.encode("utf-8")


def install(project, socket_path, *, memory_mode="persistent", timeout=5.0, max_output_bytes=4096):
    root, state = project_paths(project)
    socket_path = str(Path(socket_path).expanduser().absolute())
    journal = state / "installation.json"
    if journal.exists() and host.read_json(journal)["phase"] in ("installing", "installed", "uninstalling"):
        existing = host.Runtime.load(state)
        if existing.config["socket"] != socket_path or existing.config["memory_mode"] != memory_mode:
            raise host.HostError("installation_binding_changed")
        existing.assert_binding()
        return host.install_files(state, existing.config, {})
    config = host.new_config("codex", root, state, socket_path, memory_mode=memory_mode, timeout=timeout, max_output_bytes=max_output_bytes)
    hooks_path = root / ".codex" / "hooks.json"
    before = host.optional_bytes(hooks_path)
    hooks = json.loads(before) if before is not None else {}
    if not isinstance(hooks, dict) or not isinstance(hooks.setdefault("hooks", {}), dict):
        raise host.HostError("invalid_hooks_file")
    command = shlex.join([sys.executable, "-m", "recall_ledger.codex", "hook", "--project", str(root)])
    for event in ("UserPromptSubmit", "SessionStart", "Stop"):
        entries = hooks["hooks"].setdefault(event, [])
        if not isinstance(entries, list):
            raise host.HostError("invalid_hook_event")
        handler = {"type": "command", "command": command, "timeout": timeout * 3 + 6}
        if event != "Stop":
            # Hard byte cap is enforced on the final serialized stdout envelope.
            handler["additionalContextLimit"] = 0
        group = {"hooks": [handler]}
        if event == "SessionStart":
            group["matcher"] = "^compact$"
        entries.append(group)
    native_path = root / ".codex" / "config.toml"
    native_before = host.optional_bytes(native_path)
    binding_path = state / "binding.json"
    binding_before = host.optional_bytes(binding_path)
    updates = {
        hooks_path: host.encode(hooks),
        native_path: disable_native(native_before),
        binding_path: host.encode(config["expected_binding"]),
    }
    result = host.install_files(state, config, updates, expected_before={
        hooks_path: before, native_path: native_before, binding_path: binding_before,
    })
    result.update({
        "expected_binding": config["expected_binding"], "memory_mode": memory_mode,
        "native_control": "generation_and_use_disabled_for_config_scope",
        "host_trusted": False, "effective_hooks_verified": False, "live_session_verified": False,
        "next_step": "Review this project and exact hook definitions in Codex /hooks; start a new session.",
    })
    return result


def agent_identity(event):
    if event.get("agent_id"):
        return "agent:" + host.identifier(event["agent_id"], "agent_id")
    if event.get("agent_type") or event.get("subagent_id"):
        if event.get("subagent_id"):
            return "agent:" + host.identifier(event["subagent_id"], "subagent_id")
        raise host.HostError("ambiguous_agent_identity")
    transcript = event.get("transcript_path")
    # Opaque path identity only: the transcript contents are never opened.
    if isinstance(transcript, str) and transcript:
        return "transcript:" + host.sha(transcript.encode("utf-8"))
    return "root"


def hook_response(event_name, packet):
    return {"hookSpecificOutput": {"hookEventName": event_name, "additionalContext": host.context_text(packet)}}


def unavailable(event_name, code):
    message = "RecallLedger unavailable (" + code + "); no previous body was reused."
    if event_name in ("SessionStart", "UserPromptSubmit"):
        return {"hookSpecificOutput": {"hookEventName": event_name, "additionalContext": message}, "systemMessage": message}
    return {"systemMessage": message}


def run_hook(runtime, event):
    if not isinstance(event, dict):
        raise host.HostError("invalid_hook_input")
    name = event.get("hook_event_name")
    if name not in ("UserPromptSubmit", "SessionStart", "Stop"):
        return {}
    if name == "SessionStart" and event.get("source") != "compact":
        return {}
    # Another Stop hook can continue the same turn. Do not create a second
    # proposal from the changed completion under the same stable turn ID.
    if name == "Stop" and event.get("stop_hook_active") is True:
        return {}
    if not runtime.active():
        return unavailable(name, "adapter_inactive") if name != "Stop" else {}
    root = Path(runtime.config["project"])
    cwd = event.get("cwd")
    try:
        Path(cwd).resolve().relative_to(root)
    except (TypeError, ValueError):
        raise host.HostError("hook_project_mismatch")
    session = host.identifier(event.get("session_id"), "session_id")
    agent = agent_identity(event)
    mode = runtime.config["memory_mode"]
    if name == "SessionStart":
        return hook_response(name, runtime.refresh(query="", session_id=session, agent_id=agent, memory_mode=mode))
    turn = host.identifier(event.get("turn_id"), "turn_id")
    body = event.get("prompt" if name == "UserPromptSubmit" else "last_assistant_message")
    if name == "UserPromptSubmit" and not isinstance(body, str):
        raise host.HostError("invalid_prompt")
    capture = runtime.capture(
        event_id=name + ":" + turn, session_id=session, agent_id=agent,
        body=body, origin="user_message" if name == "UserPromptSubmit" else "model_inference",
        memory_mode=mode,
    )
    delivery = runtime.flush(memory_mode=mode, deadline=time.monotonic() + runtime.config["timeout"])
    if name == "Stop":
        return {"systemMessage": "RecallLedger capture needs attention."} if delivery.get("error") else {}
    packet = runtime.read(query=body, session_id=session, agent_id=agent, memory_mode=mode)
    packet["capture"] = {"captured": capture["captured"], "delivery_error": delivery.get("error")}
    return hook_response(name, packet)


def uninstall(project, *, enable_native_after_review=False):
    root, state = project_paths(project)
    native_path = root / ".codex" / "config.toml"
    return host.uninstall_files(
        state,
        transform=lambda path, before: disable_native(before) if path == native_path else before,
        restore_native=enable_native_after_review,
    )


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    commands = result.add_subparsers(dest="command", required=True)
    for name in ("install", "hook", "flush", "uninstall"):
        command = commands.add_parser(name)
        command.add_argument("--project", required=True)
        if name == "install":
            command.add_argument("--socket", required=True)
            command.add_argument("--memory-mode", choices=sorted(host.MODES), default="persistent")
            command.add_argument("--timeout", type=float, default=5)
            command.add_argument("--max-output-bytes", type=int, default=4096)
        if name == "uninstall":
            command.add_argument("--enable-native-after-review", action="store_true")
    return result


def main(argv=None):
    args = parser().parse_args(argv)
    event, limit = {}, 4096
    try:
        if args.command == "install":
            result = install(args.project, args.socket, memory_mode=args.memory_mode, timeout=args.timeout, max_output_bytes=args.max_output_bytes)
        elif args.command == "uninstall":
            result = uninstall(args.project, enable_native_after_review=args.enable_native_after_review)
        else:
            _, state = project_paths(args.project)
            runtime = host.Runtime.load(state)
            limit = runtime.config["max_output_bytes"]
            if args.command == "flush":
                result = runtime.flush(memory_mode=runtime.config["memory_mode"])
            else:
                raw = sys.stdin.buffer.read(1024 * 1024 + 1)
                if len(raw) > 1024 * 1024:
                    raise host.HostError("hook_input_too_large")
                event = json.loads(raw)
                result = run_hook(runtime, event)
        sys.stdout.buffer.write(host.bounded_output(result, limit))
        return 0
    except (host.HostError, MemoryError, TransportError, OSError, ValueError, TypeError, KeyError) as exc:
        code = exc.code if isinstance(exc, MemoryError) else str(exc) if isinstance(exc, host.HostError) else type(exc).__name__
        if args.command == "hook":
            name = event.get("hook_event_name") if isinstance(event, dict) else None
            sys.stdout.buffer.write(host.encode(unavailable(name, code)))
            return 0
        sys.stderr.write("RecallLedger Codex error: " + code + "\n")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
