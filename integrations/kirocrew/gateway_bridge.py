"""Callable integration helpers for trusted Gateway code.

These functions are RecallLedger examples, not upstream KiroCrew API names.
The caller must freeze identity/privacy from ExecutionContext, select `state`
from its trusted routing table, and admit source text before calling them.
Failures propagate: drop the old external context or stop model dispatch.
"""

from recall_ledger.host_runtime import HostError, Runtime, bounded_output, context_text
from recall_ledger.kirocrew import GatewayBridge


def before_model_dispatch(
    *, state, context, query, admitted_event_id=None, admitted_user_text=None,
    previous_manifest=None,
):
    runtime = Runtime.load(state)
    bridge = GatewayBridge(runtime, context)
    if (admitted_event_id is None) != (admitted_user_text is None):
        raise HostError("incomplete_admitted_event")
    if admitted_event_id is not None:
        bridge.capture(
            event_id="user:" + admitted_event_id,
            body=admitted_user_text,
            origin="user_message",
        )
        bridge.flush()
    packet = bridge.refresh(query=query, manifest=previous_manifest)
    # Validate the exact string that will be handed to context assembly.
    text = context_text(packet) if packet["available"] else ""
    bounded_output({"retrieved_data": text}, runtime.config["max_output_bytes"])
    return {"context": text, "manifest": packet.get("manifest")}


def after_assistant_event(*, state, context, admitted_event_id, admitted_text):
    bridge = GatewayBridge(Runtime.load(state), context)
    proposal = bridge.capture(
        event_id="assistant:" + admitted_event_id,
        body=admitted_text,
        origin="model_inference",
    )
    delivery = bridge.flush()
    return {"proposal": proposal, "delivery": delivery}
