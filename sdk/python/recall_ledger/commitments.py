"""Durable conditional reminders, with a single non-expiring delivery claim.

Polling never sends anything. A claim reserves one outbox item; after a crash
it is *not* automatically reclaimed. The host must reconcile unknown delivery,
deduplicate by delivery_id and recheck cancellation immediately before sending.
"""

import copy
from .environment import _BoundStore, _digest, _stamp, _text, _time

SCHEMA = "recall-ledger.commitment/1"


def _conditions(value):
    if not isinstance(value, list) or len(value) > 32:
        raise ValueError("invalid_conditions")
    seen = set()
    for item in value:
        if not isinstance(item, dict) or set(item) != {"signal", "equals"}:
            raise ValueError("invalid_condition")
        signal = _text(item["signal"], "signal")
        target = item["equals"]
        if signal in seen or target is None or type(target) not in (str, int, bool):
            raise ValueError("invalid_condition_value")
        if isinstance(target, str):
            _text(target, "condition_value", 1024)
        seen.add(signal)
    return copy.deepcopy(value)


def _body(record):
    return {key: copy.deepcopy(value) for key, value in record.items()
            if key not in ("id", "revision", "dependencies")}


def _matches(conditions, signals):
    if not isinstance(signals, dict) or len(signals) > 128:
        raise ValueError("invalid_signals")
    return all(item["signal"] in signals
               and type(signals[item["signal"]]) is type(item["equals"])
               and signals[item["signal"]] == item["equals"] for item in conditions)


class CommitmentStore(_BoundStore):
    def create(self, *, title, due_at, conditions, created_at, idempotency_key):
        title = _text(title, "title", 4096)
        due_at, created_at = _stamp(due_at), _stamp(created_at)
        conditions = _conditions(conditions)
        body = {"schema": SCHEMA, "title": title, "due_at": due_at,
                "created_at": created_at, "updated_at": created_at,
                "conditions": conditions, "state": "pending", "delivery": None,
                "permissions_granted": False}
        return self._remember(body, kind="commitment", source=SCHEMA,
                              idempotency_key=idempotency_key)

    def inspect(self, record_id):
        record = self._record(record_id, SCHEMA)
        if record.get("state") not in ("pending", "completed", "cancelled"):
            raise ValueError("invalid_commitment_state")
        _time(record.get("due_at"))
        _time(record.get("created_at"))
        _conditions(record.get("conditions"))
        return record

    def list(self, *, limit=100):
        return self._catalog(SCHEMA, limit=limit)

    def poll(self, *, now, signals, limit=100):
        """Read eligible pending reminders; it neither claims nor delivers."""
        instant = _time(now)
        if not isinstance(signals, dict):
            raise ValueError("invalid_signals")
        if type(limit) is not int or not 1 <= limit <= 1000:
            raise ValueError("invalid_limit")
        result = self._catalog(SCHEMA, limit=1000)
        if result["has_more"]:
            raise ValueError("poll_catalog_limit_exceeded")
        due = [record for record in result["items"]
               if record["state"] == "pending" and record["delivery"] is None
               and _time(record["due_at"]) <= instant
               and _time(record["updated_at"]) <= instant
               and _matches(record["conditions"], signals)]
        due.sort(key=lambda r: (_time(r["due_at"]), r["id"]))
        return {"items": due[:limit], "has_more": len(due) > limit,
                "checked_at": _stamp(now), "permissions_granted": False}

    def claim(self, record_id, *, expected_revision, delivery_id, now, signals, idempotency_key):
        """CAS one persistent claim. Repeated claims never grant deliver=True."""
        delivery_id = _text(delivery_id, "delivery_id")
        now = _stamp(now)
        record = self.inspect(record_id)
        if _time(now) < _time(record["updated_at"]):
            raise ValueError("claim_before_latest_update")
        if record["state"] != "pending":
            return {"claimed": False, "deliver": False, "reason": record["state"], "item": record}
        if record["delivery"] is not None:
            return {"claimed": False, "deliver": False, "reason": "already_claimed", "item": record}
        if _time(record["due_at"]) > _time(now) or not _matches(record["conditions"], signals):
            return {"claimed": False, "deliver": False, "reason": "not_due", "item": record}
        body = _body(record)
        body["delivery"] = {"id": delivery_id,
                            "outbox_id": _digest({"binding": self.expected_binding,
                                                 "record_id": record_id, "delivery_id": delivery_id}),
                            "state": "claimed", "claimed_at": now,
                            "acknowledged_at": None}
        body["updated_at"] = now
        self._revise(record, body, expected_revision=expected_revision, idempotency_key=idempotency_key)
        current = self.inspect(record_id)
        deliver = current["state"] == "pending" and current["delivery"]["id"] == delivery_id and current["delivery"]["state"] == "claimed"
        return {"claimed": True, "deliver": deliver, "item": current,
                "permissions_granted": False}

    def delivery(self, record_id, *, delivery_id, now, signals):
        """Fresh preflight for a host reconciling its own delivery_id."""
        record = self.inspect(record_id)
        delivery = record["delivery"]
        ready = (record["state"] == "pending" and delivery is not None
                 and delivery["id"] == _text(delivery_id, "delivery_id")
                 and delivery["state"] == "claimed"
                 and _time(record["due_at"]) <= _time(now)
                 and _time(record["updated_at"]) <= _time(now)
                 and _matches(record["conditions"], signals))
        return {"deliver": ready, "item": record, "permissions_granted": False,
                "external_deduplication_required": True}

    def ack(self, record_id, *, delivery_id, now, idempotency_key):
        record = self.inspect(record_id)
        now = _stamp(now)
        delivery = record["delivery"]
        if record["state"] != "pending" or delivery is None or delivery["id"] != delivery_id:
            raise ValueError("delivery_not_active")
        if delivery["state"] == "acknowledged":
            return {"acknowledged": True, "already_acknowledged": True, "item": record}
        if _time(now) < _time(delivery["claimed_at"]):
            raise ValueError("ack_before_claim")
        body = _body(record)
        body["delivery"]["state"] = "acknowledged"
        body["delivery"]["acknowledged_at"] = now
        body["updated_at"] = now
        self._revise(record, body, expected_revision=record["revision"], idempotency_key=idempotency_key)
        return {"acknowledged": True, "already_acknowledged": False, "item": self.inspect(record_id)}

    def _finish(self, record_id, *, state, expected_revision, now, idempotency_key):
        record = self.inspect(record_id)
        now = _stamp(now)
        if record["state"] == state:
            return {"changed": False, "item": record}
        if record["state"] != "pending":
            raise ValueError("commitment_already_terminal")
        if _time(now) < _time(record["updated_at"]):
            raise ValueError("transition_before_latest_update")
        body = _body(record)
        body["state"], body["updated_at"] = state, now
        body[state + "_at"] = now
        self._revise(record, body, expected_revision=expected_revision, idempotency_key=idempotency_key)
        return {"changed": True, "item": self.inspect(record_id)}

    def cancel(self, record_id, *, expected_revision, now, idempotency_key):
        return self._finish(record_id, state="cancelled", expected_revision=expected_revision,
                            now=now, idempotency_key=idempotency_key)

    def complete(self, record_id, *, expected_revision, now, idempotency_key):
        """Record trusted host/user completion; memory grants no action rights."""
        return self._finish(record_id, state="completed", expected_revision=expected_revision,
                            now=now, idempotency_key=idempotency_key)

    def update(self, record_id, *, expected_revision, now, idempotency_key,
               title=None, due_at=None, conditions=None):
        """Edit an unclaimed pending reminder; claimed deliveries need reconciliation."""
        record = self.inspect(record_id)
        now = _stamp(now)
        if record["state"] != "pending" or record["delivery"] is not None:
            raise ValueError("commitment_not_editable")
        if _time(now) < _time(record["updated_at"]):
            raise ValueError("transition_before_latest_update")
        if title is None and due_at is None and conditions is None:
            raise ValueError("empty_commitment_update")
        body = _body(record)
        if title is not None:
            body["title"] = _text(title, "title", 4096)
        if due_at is not None:
            body["due_at"] = _stamp(due_at)
        if conditions is not None:
            body["conditions"] = _conditions(conditions)
        body["updated_at"] = now
        self._revise(record, body, expected_revision=expected_revision, idempotency_key=idempotency_key)
        return self.inspect(record_id)
