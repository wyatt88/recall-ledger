//! Reviewed, namespace-complete current-state interchange, not arbitrary SQL restore.
//! Digest is SHA256 over serde_json's compact, sorted-key UTF-8 serialization of
//! `payload` ONLY (no newline); `format` is independently required to equal FORMAT.
//! source_sequence is the source store's global committed sequence at the
//! transaction snapshot, not a per-record revision or a target commit token.
//! No historical bodies, vectors, recipes, leases, credentials or receipts travel.
use crate::*;
use std::collections::BTreeSet;

const FORMAT: &str = "recall-ledger.portable/1";
const MAX_BYTES: usize = 512 * 1024;
const MAX_OBJECTS: usize = 1000;

pub(super) fn schema(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE portable_origins(
           namespace TEXT NOT NULL,object_kind TEXT NOT NULL,
           origin_history TEXT NOT NULL,origin_namespace TEXT NOT NULL,origin_id TEXT NOT NULL,
           local_id TEXT NOT NULL,baseline_hash TEXT,
           PRIMARY KEY(namespace,object_kind,origin_history,origin_namespace,origin_id),
           UNIQUE(namespace,object_kind,local_id));
         CREATE TABLE portable_watermarks(
           namespace TEXT NOT NULL,source_history TEXT NOT NULL,source_namespace TEXT NOT NULL,
           sequence INTEGER NOT NULL,digest TEXT NOT NULL,members TEXT NOT NULL,state_hashes TEXT NOT NULL,
           PRIMARY KEY(namespace,source_history,source_namespace));",
    )?;
    Ok(())
}

/// Preserve logical roots before recovery changes the store's commit history.
/// Native objects have no imported synchronization baseline, hence NULL. This
/// scans all namespaces directly and is not limited by portable export bounds.
pub(super) fn freeze_native_origins(db: &Connection) -> Result<()> {
    let history: String = db.query_row("SELECT value FROM meta WHERE key='store_id'", [], |r| {
        r.get(0)
    })?;
    for (table, kind) in [("records", "record"), ("ingestion_candidates", "candidate")] {
        db.execute(
            &format!(
                "INSERT OR IGNORE INTO portable_origins
                 (namespace,object_kind,origin_history,origin_namespace,origin_id,local_id,baseline_hash)
                 SELECT namespace,?1,?2,namespace,id,id,NULL FROM {table} source
                 WHERE NOT EXISTS (SELECT 1 FROM portable_origins p
                   WHERE p.namespace=source.namespace AND p.object_kind=?1 AND p.local_id=source.id)"
            ),
            params![kind, history],
        )?;
    }
    Ok(())
}

pub(super) fn allowed(method: &str) -> Option<&'static [&'static str]> {
    match method {
        "bundle_export" => Some(&[]),
        "bundle_plan" => Some(&["bundle", "expected_source"]),
        "bundle_context" => Some(&["bundle", "expected_source", "manifest"]),
        "bundle_apply" => Some(&[
            "bundle",
            "expected_source",
            "reviewed_digest",
            "expected_target_commit",
            "idempotency_key",
        ]),
        _ => None,
    }
}

pub(super) fn is_mutation(method: &str) -> bool {
    method == "bundle_apply"
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct Identity {
    history_id: String,
    namespace: String,
    id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Dependency {
    identity: Identity,
    revision: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PortableRecord {
    identity: Identity,
    source_id: String,
    base_hash: Option<String>,
    revision: i64,
    body: Option<String>,
    kind: String,
    source: String,
    volatility: String,
    status: String,
    valid_from: Option<i64>,
    valid_until: Option<i64>,
    created_at: i64,
    updated_at: i64,
    fact_key: Option<Value>,
    dependencies: Vec<Dependency>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PortableCandidate {
    identity: Identity,
    source_id: String,
    base_hash: Option<String>,
    event_id: String,
    request_hash: String,
    body: Option<String>,
    origin: String,
    source: String,
    fact_key: Option<Value>,
    kind: String,
    volatility: String,
    valid_from: Option<i64>,
    valid_until: Option<i64>,
    metadata_fields: Vec<String>,
    state: String,
    record: Option<Identity>,
    accepted_revision: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    source_binding: Value,
    source_sequence: i64,
    records: Vec<PortableRecord>,
    candidates: Vec<PortableCandidate>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    format: String,
    payload: Payload,
    digest: String,
}

fn invalid(message: &str) -> Error {
    Error::new("bundle_invalid", message)
}

fn value<T: Serialize>(item: &T) -> Value {
    serde_json::to_value(item).expect("portable structures serialize")
}

fn hash(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}

// Synchronization preconditions are transport metadata, not object state.
fn state_hash<T: Serialize>(item: &T) -> String {
    let mut item = value(item);
    item.as_object_mut().unwrap().remove("base_hash");
    item.as_object_mut().unwrap().remove("source_id");
    hash(&item)
}

fn hash_shape(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn identity(db: &Connection, ns: &str, kind: &str, id: &str) -> Result<(Identity, Option<String>)> {
    let stored = db
        .query_row(
            "SELECT origin_history,origin_namespace,origin_id,baseline_hash FROM portable_origins
         WHERE namespace=?1 AND object_kind=?2 AND local_id=?3",
            params![ns, kind, id],
            |r| {
                Ok((
                    Identity {
                        history_id: r.get(0)?,
                        namespace: r.get(1)?,
                        id: r.get(2)?,
                    },
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    if let Some((identity, baseline)) = stored {
        return Ok((identity, baseline));
    }
    Ok((
        Identity {
            history_id: string(&binding(db, ns)?, "history_id")?.into(),
            namespace: ns.into(),
            id: id.into(),
        },
        None,
    ))
}

fn fact_value(key: Option<String>) -> Result<Option<Value>> {
    key.map(|key| {
        let tuple = parse_stored(&key)?;
        let parts = tuple
            .as_array()
            .filter(|a| a.len() == 4)
            .ok_or_else(|| invalid("Stored fact key is malformed."))?;
        Ok(json!({"entity":parts[0],"attribute":parts[1],"scope":parts[2],"conditions":parts[3]}))
    })
    .transpose()
}

fn fact_text(key: &Option<Value>) -> Result<Option<String>> {
    key.as_ref()
        .map(|key| {
            let object = key
                .as_object()
                .ok_or_else(|| invalid("fact_key must be an object."))?;
            let fields = ["entity", "attribute", "scope", "conditions"];
            if object.len() != 4 || object.keys().any(|name| !fields.contains(&name.as_str())) {
                return Err(invalid("Fact identity requires exactly four fields."));
            }
            let mut tuple = Vec::new();
            for name in fields {
                let text = string(key, name)?;
                validate_name(text, name)?;
                if text.trim().is_empty() {
                    return Err(invalid("Fact identity cannot be blank."));
                }
                tuple.push(text);
            }
            Ok(json!(tuple).to_string())
        })
        .transpose()
}

fn export_payload(db: &Connection, ns: &str) -> Result<Payload> {
    let total: i64 = db.query_row(
        "SELECT (SELECT count(*) FROM records WHERE namespace=?1)
              + (SELECT count(*) FROM ingestion_candidates WHERE namespace=?1)",
        [ns],
        |r| r.get(0),
    )?;
    if total > MAX_OBJECTS as i64 {
        return Err(Error::new(
            "resource_exhausted",
            "Portable namespace exceeds 1000 objects.",
        ));
    }
    let ids = db
        .prepare("SELECT id FROM records WHERE namespace=?1 ORDER BY id")?
        .query_map([ns], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut records = Vec::new();
    for id in ids {
        let row = get(db, ns, &id)?;
        let (identity, base_hash) = identity(db, ns, "record", &id)?;
        let dependencies = dependencies(db, &id)?
            .into_iter()
            .map(|(id, revision)| {
                Ok(Dependency {
                    identity: self::identity(db, ns, "record", &id)?.0,
                    revision,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let key = db
            .query_row(
                "SELECT fact_key FROM fact_bindings WHERE namespace=?1 AND record_id=?2",
                params![ns, id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        records.push(PortableRecord {
            identity,
            source_id: id,
            base_hash,
            revision: row.revision,
            body: (row.status == "active").then_some(row.body),
            kind: row.kind,
            source: row.source,
            volatility: row.volatility,
            status: row.status,
            valid_from: row.valid_from,
            valid_until: row.valid_until,
            created_at: row.created_at,
            updated_at: row.updated_at,
            fact_key: fact_value(key)?,
            dependencies,
        });
    }
    let mut statement = db.prepare(
        "SELECT id,event_id,request_hash,body,origin,source,fact_key,kind,volatility,
         valid_from,valid_until,metadata_fields,state,record_id,accepted_revision,created_at,updated_at
         FROM ingestion_candidates WHERE namespace=?1 ORDER BY id",
    )?;
    let raw = statement
        .query_map([ns], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, Option<i64>>(9)?,
                r.get::<_, Option<i64>>(10)?,
                r.get::<_, String>(11)?,
                r.get::<_, String>(12)?,
                r.get::<_, Option<String>>(13)?,
                r.get::<_, Option<i64>>(14)?,
                r.get::<_, i64>(15)?,
                r.get::<_, i64>(16)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut candidates = Vec::new();
    for (
        id,
        event_id,
        request_hash,
        body,
        origin,
        source,
        key,
        kind,
        volatility,
        start,
        end,
        fields,
        state,
        record,
        accepted_revision,
        created_at,
        updated_at,
    ) in raw
    {
        let (identity, base_hash) = identity(db, ns, "candidate", &id)?;
        candidates.push(PortableCandidate {
            identity,
            source_id: id,
            base_hash,
            event_id,
            request_hash,
            body: if state == "pending" { body } else { None },
            origin,
            source,
            fact_key: fact_value(key)?,
            kind,
            volatility,
            valid_from: start,
            valid_until: end,
            metadata_fields: serde_json::from_str(&fields)
                .map_err(|_| invalid("Invalid stored candidate metadata."))?,
            state,
            record: record
                .map(|id| self::identity(db, ns, "record", &id).map(|p| p.0))
                .transpose()?,
            accepted_revision,
            created_at,
            updated_at,
        });
    }
    records.sort_by(|a, b| a.identity.cmp(&b.identity));
    for record in &mut records {
        record
            .dependencies
            .sort_by(|a, b| a.identity.cmp(&b.identity));
    }
    candidates.sort_by(|a, b| a.identity.cmp(&b.identity));
    let source_sequence = db.query_row(
        "SELECT CAST(value AS INTEGER) FROM meta WHERE key='sequence'",
        [],
        |r| r.get(0),
    )?;
    Ok(Payload {
        source_binding: binding(db, ns)?,
        source_sequence,
        records,
        candidates,
    })
}

fn check_identity(id: &Identity) -> Result<()> {
    validate_binding(&json!({"history_id":id.history_id,"namespace":id.namespace}))?;
    if id.id.len() != 36 || Uuid::parse_str(&id.id).is_err() {
        return Err(invalid("Object identities require UUID IDs."));
    }
    Ok(())
}

fn check_common(item: &Value) -> Result<()> {
    kind(item, "assertion")?;
    validate_name(string(item, "source")?, "source")?;
    if !["stable", "normal", "volatile"].contains(&string(item, "volatility")?) {
        return Err(invalid("Unknown volatility."));
    }
    let start = optional_time(item, "valid_from")?;
    let end = optional_time(item, "valid_until")?;
    if start.zip(end).is_some_and(|(a, b)| a >= b) {
        return Err(invalid("Invalid validity interval."));
    }
    for name in ["created_at", "updated_at"] {
        integer(item, name, -1, 0, i64::MAX)?;
    }
    if item
        .get("base_hash")
        .is_some_and(|v| !v.is_null() && !v.as_str().is_some_and(hash_shape))
    {
        return Err(invalid("Invalid synchronization precondition."));
    }
    Ok(())
}

fn check_payload(payload: &Payload) -> Result<()> {
    validate_binding(&payload.source_binding)?;
    if payload.source_sequence < 0 || payload.records.len() + payload.candidates.len() > MAX_OBJECTS
    {
        return Err(invalid("Snapshot bounds exceeded."));
    }
    let mut records = BTreeMap::new();
    let mut facts = BTreeMap::new();
    let mut source_ids = BTreeSet::new();
    for record in &payload.records {
        check_identity(&record.identity)?;
        if Uuid::parse_str(&record.source_id).is_err()
            || record.source_id.len() != 36
            || !source_ids.insert(&record.source_id)
        {
            return Err(invalid("Invalid or duplicate source record ID."));
        }
        check_common(&value(record))?;
        if record.revision < 1
            || !["active", "stale", "retracted"].contains(&record.status.as_str())
        {
            return Err(invalid("Invalid record revision or state."));
        }
        if record.status == "active" {
            body(&value(record))?;
        } else if record.body.is_some() {
            return Err(invalid("Unavailable records cannot carry bodies."));
        }
        if record.dependencies.len() > MAX_INPUTS {
            return Err(invalid("Too many dependencies."));
        }
        if records.insert(record.identity.clone(), record).is_some() {
            return Err(invalid("Duplicate record identity."));
        }
        if let Some(key) = fact_text(&record.fact_key)? {
            if facts.insert(key, record).is_some() {
                return Err(invalid("Duplicate fact identity."));
            }
        }
    }
    let mut remaining: BTreeSet<_> = records.keys().cloned().collect();
    let mut complete = BTreeSet::new();
    while !remaining.is_empty() {
        let mut ready = Vec::new();
        for id in &remaining {
            let record = records[id];
            let mut seen = BTreeSet::new();
            let mut parents_ready = true;
            for dep in &record.dependencies {
                if dep.revision < 1 || !seen.insert(&dep.identity) {
                    return Err(invalid("Invalid or duplicate dependency."));
                }
                let parent = records
                    .get(&dep.identity)
                    .ok_or_else(|| invalid("Missing dependency."))?;
                if record.status == "active"
                    && (parent.status != "active" || parent.revision != dep.revision)
                {
                    return Err(invalid(
                        "Active artifact has an ineligible captured dependency.",
                    ));
                }
                parents_ready &= complete.contains(&dep.identity);
            }
            if parents_ready {
                ready.push(id.clone());
            }
        }
        if ready.is_empty() {
            return Err(invalid("Dependency cycle."));
        }
        for id in ready {
            remaining.remove(&id);
            complete.insert(id);
        }
    }
    let mut ids = BTreeSet::new();
    let mut events = BTreeSet::new();
    let mut source_ids = BTreeSet::new();
    for candidate in &payload.candidates {
        check_identity(&candidate.identity)?;
        if Uuid::parse_str(&candidate.source_id).is_err()
            || candidate.source_id.len() != 36
            || !source_ids.insert(&candidate.source_id)
        {
            return Err(invalid("Invalid or duplicate source candidate ID."));
        }
        check_common(&value(candidate))?;
        validate_name(&candidate.event_id, "event_id")?;
        if !ids.insert(&candidate.identity)
            || !events.insert(&candidate.event_id)
            || !hash_shape(&candidate.request_hash)
            || ![
                "user_explicit",
                "user_message",
                "tool_observation",
                "model_inference",
                "import",
            ]
            .contains(&candidate.origin.as_str())
            || !["pending", "accepted", "rejected", "withdrawn"].contains(&candidate.state.as_str())
        {
            return Err(invalid("Invalid candidate identity or state."));
        }
        let fields: BTreeSet<_> = candidate.metadata_fields.iter().collect();
        if fields.len() != candidate.metadata_fields.len()
            || fields
                .iter()
                .any(|s| !["kind", "volatility", "valid_from", "valid_until"].contains(&s.as_str()))
        {
            return Err(invalid("Invalid metadata presence mask."));
        }
        let key = fact_text(&candidate.fact_key)?;
        let record = candidate
            .record
            .as_ref()
            .map(|id| {
                records
                    .get(id)
                    .copied()
                    .ok_or_else(|| invalid("Candidate record is missing."))
            })
            .transpose()?;
        match candidate.state.as_str() {
            "pending" => {
                body(&value(candidate))?;
                if record.is_some()
                    || candidate.accepted_revision.is_some()
                    || key
                        .as_ref()
                        .and_then(|k| facts.get(k))
                        .is_some_and(|r| r.status == "retracted")
                {
                    return Err(invalid(
                        "Pending candidate has accepted or withdrawn state.",
                    ));
                }
            }
            "accepted" => {
                let record = record.ok_or_else(|| invalid("Accepted candidate needs a record."))?;
                if candidate.body.is_some()
                    || record.status == "retracted"
                    || candidate
                        .accepted_revision
                        .is_none_or(|r| r < 1 || r > record.revision)
                    || (key.is_some() && key != fact_text(&record.fact_key)?)
                {
                    return Err(invalid("Accepted candidate is inconsistent."));
                }
            }
            "rejected" => {
                if candidate.body.is_some()
                    || record.is_some()
                    || candidate.accepted_revision.is_some()
                {
                    return Err(invalid("Rejected candidate has accepted state."));
                }
            }
            _ => {
                if candidate.body.is_some()
                    || record.is_some_and(|r| r.status != "retracted")
                    || (record.is_none() && candidate.accepted_revision.is_some())
                    || record.is_some_and(|r| {
                        candidate
                            .accepted_revision
                            .is_none_or(|v| v < 1 || v > r.revision)
                    })
                {
                    return Err(invalid("Withdrawn candidate is inconsistent."));
                }
            }
        }
    }
    Ok(())
}

fn parse_bundle(args: &Value) -> Result<Bundle> {
    let input = args
        .get("bundle")
        .ok_or_else(|| invalid("bundle is required."))?;
    if input.to_string().len() > MAX_BYTES {
        return Err(Error::new(
            "resource_exhausted",
            "Portable bundle exceeds 512 KiB.",
        ));
    }
    let bundle: Bundle =
        serde_json::from_value(input.clone()).map_err(|_| invalid("Unsupported bundle shape."))?;
    if bundle.format != FORMAT {
        return Err(invalid("Unsupported portable format."));
    }
    // Reject missing optional fields rather than
    // accidentally digesting a different representation than the caller reviewed.
    if value(&bundle) != *input {
        return Err(invalid("Portable fields must be explicit."));
    }
    if !hash_shape(&bundle.digest) || hash(&value(&bundle.payload)) != bundle.digest {
        return Err(Error::new(
            "digest_mismatch",
            "Portable payload digest does not match.",
        ));
    }
    let expected = args
        .get("expected_source")
        .ok_or_else(|| invalid("expected_source is required."))?;
    validate_binding(expected)?;
    if expected != &bundle.payload.source_binding {
        return Err(Error::new(
            "binding_mismatch",
            "Portable source differs from reviewed source.",
        ));
    }
    check_payload(&bundle.payload)?;
    Ok(bundle)
}

#[derive(Clone)]
struct Mapping {
    identity: Identity,
    source_id: String,
    id: String,
    action: &'static str,
}

struct Plan {
    records: Vec<Mapping>,
    candidates: Vec<Mapping>,
    conflicts: Vec<Value>,
    replayed: bool,
}

fn members(payload: &Payload) -> BTreeSet<String> {
    payload
        .records
        .iter()
        .map(|r| json!(["record", r.identity]).to_string())
        .chain(
            payload
                .candidates
                .iter()
                .map(|c| json!(["candidate", c.identity]).to_string()),
        )
        .collect()
}

fn watermark(
    db: &Connection,
    ns: &str,
    source: &Value,
) -> Result<Option<(i64, String, String, String)>> {
    Ok(db
        .query_row(
            "SELECT sequence,digest,members,state_hashes FROM portable_watermarks
         WHERE namespace=?1 AND source_history=?2 AND source_namespace=?3",
            params![
                ns,
                string(source, "history_id")?,
                string(source, "namespace")?
            ],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?)
}

fn mapping_id(db: &Connection, ns: &str, kind: &str, origin: &Identity) -> Result<String> {
    if let Some(id) = db
        .query_row(
            "SELECT local_id FROM portable_origins
         WHERE namespace=?1 AND object_kind=?2 AND origin_history=?3
         AND origin_namespace=?4 AND origin_id=?5",
            params![ns, kind, origin.history_id, origin.namespace, origin.id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    let target = binding(db, ns)?;
    if target["history_id"] == origin.history_id && ns == origin.namespace {
        return Ok(origin.id.clone());
    }
    // Stable only within this target history/namespace. Root provenance, not a
    // source-local surrogate, determines the ID through arbitrary A -> B -> C hops.
    let digest = Sha256::digest(json!([target, kind, origin]).to_string().as_bytes());
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80; // RFC 9562 custom UUID (version 8).
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(Uuid::from_bytes(bytes).to_string())
}

fn conflict(plan: &mut Plan, kind: &str, identity: &Identity, reason: &str) {
    plan.conflicts
        .push(json!({"object_kind":kind,"identity":identity,"reason":reason}));
}

fn synchronized(
    current_hash: &str,
    source_previous: Option<&String>,
    incoming_base: &Option<String>,
) -> bool {
    source_previous.map(String::as_str) == Some(current_hash)
        || incoming_base.as_deref() == Some(current_hash)
}

fn member(kind: &str, identity: &Identity) -> String {
    json!([kind, identity]).to_string()
}

fn source_hashes(payload: &Payload) -> BTreeMap<String, String> {
    payload
        .records
        .iter()
        .map(|r| (member("record", &r.identity), state_hash(r)))
        .chain(
            payload
                .candidates
                .iter()
                .map(|c| (member("candidate", &c.identity), state_hash(c))),
        )
        .collect()
}

fn record_metadata_equal(a: &PortableRecord, b: &PortableRecord) -> bool {
    a.kind == b.kind
        && a.source == b.source
        && a.volatility == b.volatility
        && a.valid_from == b.valid_from
        && a.valid_until == b.valid_until
        && a.fact_key == b.fact_key
        && a.created_at == b.created_at
}

fn candidate_metadata_equal(a: &PortableCandidate, b: &PortableCandidate) -> bool {
    a.event_id == b.event_id
        && a.request_hash == b.request_hash
        && a.origin == b.origin
        && a.source == b.source
        && a.fact_key == b.fact_key
        && a.kind == b.kind
        && a.volatility == b.volatility
        && a.valid_from == b.valid_from
        && a.valid_until == b.valid_until
        && a.metadata_fields == b.metadata_fields
        && a.created_at == b.created_at
}

fn make_plan(db: &Connection, ns: &str, bundle: &Bundle) -> Result<Plan> {
    let incoming = &bundle.payload;
    let current = export_payload(db, ns)?;
    let mut plan = Plan {
        records: Vec::new(),
        candidates: Vec::new(),
        conflicts: Vec::new(),
        replayed: false,
    };
    let mut previous_hashes = BTreeMap::<String, String>::new();
    if let Some((sequence, digest, previous, hashes)) = watermark(db, ns, &incoming.source_binding)?
    {
        previous_hashes = serde_json::from_str(&hashes)
            .map_err(|_| invalid("Stored source fingerprints are invalid."))?;
        if sequence > incoming.source_sequence {
            plan.conflicts.push(json!({"reason":"stale_bundle"}));
        } else if sequence == incoming.source_sequence {
            if digest != bundle.digest {
                plan.conflicts
                    .push(json!({"reason":"source_sequence_collision"}));
            } else {
                plan.replayed = true;
            }
        } else {
            let previous: BTreeSet<String> = serde_json::from_str(&previous)
                .map_err(|_| invalid("Stored source inventory is invalid."))?;
            if !previous.is_subset(&members(incoming)) {
                plan.conflicts.push(json!({"reason":"missing_tombstones"}));
            }
        }
    }
    let records: BTreeMap<_, _> = current.records.iter().map(|r| (&r.identity, r)).collect();
    let candidates: BTreeMap<_, _> = current
        .candidates
        .iter()
        .map(|c| (&c.identity, c))
        .collect();
    for record in &incoming.records {
        let id = mapping_id(db, ns, "record", &record.identity)?;
        let mut action = "create";
        if let Some(old) = records.get(&record.identity) {
            action = "update";
            if old.source_id != id {
                conflict(
                    &mut plan,
                    "record",
                    &record.identity,
                    "identity_mapping_conflict",
                );
            }
            let current_hash = state_hash(*old);
            if plan.replayed
                || current_hash == state_hash(record)
                || (old.status == "retracted" && record.status == "retracted")
            {
                action = "unchanged";
            } else if old.status == "retracted" {
                conflict(&mut plan, "record", &record.identity, "withdrawn_identity");
            } else if !record_metadata_equal(old, record) {
                conflict(&mut plan, "record", &record.identity, "metadata_conflict");
            } else if record.revision < old.revision {
                conflict(&mut plan, "record", &record.identity, "revision_regression");
            } else if !synchronized(
                &current_hash,
                previous_hashes.get(&member("record", &record.identity)),
                &record.base_hash,
            ) {
                conflict(&mut plan, "record", &record.identity, "concurrent_edit");
            } else if record.status != "retracted" {
                if record.revision == old.revision && record.status == "active" {
                    conflict(&mut plan, "record", &record.identity, "revision_collision");
                }
                if maintenance::is_managed(db, ns, &id)? {
                    conflict(&mut plan, "record", &record.identity, "managed_record");
                }
            }
        } else {
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM records WHERE id=?1)",
                [&id],
                |r| r.get(0),
            )?;
            if exists || plan.replayed {
                conflict(
                    &mut plan,
                    "record",
                    &record.identity,
                    "identity_mapping_conflict",
                );
            }
        }
        if let Some(key) = fact_text(&record.fact_key)? {
            for old in &current.records {
                if old.identity != record.identity
                    && fact_text(&old.fact_key)?.as_ref() == Some(&key)
                {
                    conflict(
                        &mut plan,
                        "record",
                        &record.identity,
                        "fact_identity_collision",
                    );
                }
            }
        }
        plan.records.push(Mapping {
            identity: record.identity.clone(),
            source_id: record.source_id.clone(),
            id,
            action,
        });
    }
    for candidate in &incoming.candidates {
        let id = mapping_id(db, ns, "candidate", &candidate.identity)?;
        let mut action = "create";
        if let Some(old) = candidates.get(&candidate.identity) {
            action = "update";
            let current_hash = state_hash(*old);
            if old.source_id != id {
                conflict(
                    &mut plan,
                    "candidate",
                    &candidate.identity,
                    "identity_mapping_conflict",
                );
            }
            if plan.replayed
                || current_hash == state_hash(candidate)
                || (old.state == "withdrawn" && candidate.state == "withdrawn")
            {
                action = "unchanged";
            } else if !candidate_metadata_equal(old, candidate) {
                conflict(
                    &mut plan,
                    "candidate",
                    &candidate.identity,
                    "candidate_event_conflict",
                );
            } else if !synchronized(
                &current_hash,
                previous_hashes.get(&member("candidate", &candidate.identity)),
                &candidate.base_hash,
            ) {
                conflict(
                    &mut plan,
                    "candidate",
                    &candidate.identity,
                    "concurrent_edit",
                );
            } else if candidate.state == "pending"
                || old.state == "withdrawn"
                || (old.state == "rejected" && candidate.state != "withdrawn")
                || (old.state == "accepted" && candidate.state != "withdrawn")
            {
                conflict(
                    &mut plan,
                    "candidate",
                    &candidate.identity,
                    "admission_regression",
                );
            }
        } else {
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM ingestion_candidates WHERE id=?1)",
                [&id],
                |r| r.get(0),
            )?;
            if exists || plan.replayed {
                conflict(
                    &mut plan,
                    "candidate",
                    &candidate.identity,
                    "identity_mapping_conflict",
                );
            }
        }
        if current
            .candidates
            .iter()
            .any(|old| old.event_id == candidate.event_id && old.identity != candidate.identity)
        {
            conflict(
                &mut plan,
                "candidate",
                &candidate.identity,
                "event_identity_collision",
            );
        }
        // A destination-local withdrawal must also block proposals imported from
        // another branch, including a pending proposal never seen here before.
        if !plan.replayed
            && candidate.state == "pending"
            && candidate.fact_key.is_some()
            && current
                .records
                .iter()
                .any(|old| old.status == "retracted" && old.fact_key == candidate.fact_key)
        {
            conflict(
                &mut plan,
                "candidate",
                &candidate.identity,
                "withdrawn_fact_key",
            );
        }
        plan.candidates.push(Mapping {
            identity: candidate.identity.clone(),
            source_id: candidate.source_id.clone(),
            id,
            action,
        });
    }
    // A newer branch cannot make a descendant eligible when any root has a
    // target-local tombstone. This also covers tombstones absent on that branch.
    for record in &incoming.records {
        if !plan.replayed
            && record.status == "active"
            && record.dependencies.iter().any(|d| {
                records
                    .get(&d.identity)
                    .is_some_and(|old| old.status == "retracted")
            })
        {
            conflict(
                &mut plan,
                "record",
                &record.identity,
                "withdrawn_dependency",
            );
        }
    }
    if current.records.len()
        + current.candidates.len()
        + plan
            .records
            .iter()
            .chain(&plan.candidates)
            .filter(|m| m.action == "create")
            .count()
        > MAX_OBJECTS
    {
        plan.conflicts
            .push(json!({"reason":"target_capacity_exceeded"}));
    }
    Ok(plan)
}

fn report(db: &Connection, bundle: &Bundle, plan: &Plan) -> Result<Value> {
    fn entries(mappings: &[Mapping]) -> Value {
        json!(mappings
            .iter()
            .map(|m| json!({
                "identity":m.identity,"source_id":m.source_id,"id":m.id,"action":m.action
            }))
            .collect::<Vec<_>>())
    }
    fn counts(mappings: &[Mapping]) -> Value {
        json!({
            "create":mappings.iter().filter(|m|m.action=="create").count(),
            "update":mappings.iter().filter(|m|m.action=="update").count(),
            "unchanged":mappings.iter().filter(|m|m.action=="unchanged").count()
        })
    }
    Ok(json!({
        "can_apply":plan.conflicts.is_empty(),"replayed":plan.replayed,"conflicts":plan.conflicts,
        "target_commit":commit_token(db)?,"reviewed_digest":bundle.digest,
        "source_binding":bundle.payload.source_binding,
        "stats":{"records":counts(&plan.records),"candidates":counts(&plan.candidates)},
        "mappings":{"records":entries(&plan.records),"candidates":entries(&plan.candidates)},
        "semantics_verified":false
    }))
}

fn save_origin(db: &Connection, ns: &str, kind: &str, m: &Mapping, baseline: &str) -> Result<()> {
    db.execute(
        "INSERT INTO portable_origins VALUES(?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(namespace,object_kind,origin_history,origin_namespace,origin_id)
         DO UPDATE SET baseline_hash=excluded.baseline_hash",
        params![
            ns,
            kind,
            m.identity.history_id,
            m.identity.namespace,
            m.identity.id,
            m.id,
            baseline
        ],
    )?;
    Ok(())
}

fn apply(db: &Connection, ns: &str, bundle: &Bundle, plan: &Plan, at: i64) -> Result<()> {
    let record_ids: BTreeMap<_, _> = plan.records.iter().map(|m| (&m.identity, &m.id)).collect();
    // Invalidate using the existing graph before changing edges. This propagates
    // to target-local artifacts not present in the source snapshot.
    for (record, m) in bundle.payload.records.iter().zip(&plan.records) {
        if m.action != "update" {
            continue;
        }
        let mut affected = descendants(db, &m.id)?;
        if record.status == "retracted" {
            affected.push(m.id.clone());
            invalidate(db, ns, &affected, true, at)?;
        } else {
            invalidate(db, ns, &affected, false, at)?;
            db.execute(
                "UPDATE jobs SET state='invalidated' WHERE namespace=?1 AND state='prepared'
                 AND id IN (SELECT job_id FROM job_inputs WHERE record_id=?2)",
                params![ns, m.id],
            )?;
            vector::invalidate(db, ns, std::slice::from_ref(&m.id))?;
            maintenance::on_invalidate(db, ns, std::slice::from_ref(&m.id), false, at)?;
        }
    }
    for (record, m) in bundle.payload.records.iter().zip(&plan.records) {
        if m.action != "unchanged" {
            let r = Record {
                id: m.id.clone(),
                revision: record.revision,
                body: record.body.clone().unwrap_or_default(),
                kind: record.kind.clone(),
                source: record.source.clone(),
                volatility: record.volatility.clone(),
                status: record.status.clone(),
                valid_from: record.valid_from,
                valid_until: record.valid_until,
                created_at: record.created_at,
                updated_at: record.updated_at,
            };
            if m.action == "create" {
                insert(db, ns, &r)?;
            } else {
                db.execute(
                    "UPDATE records SET revision=?1,body=?2,status=?3,updated_at=?4 WHERE namespace=?5 AND id=?6",
                    params![r.revision,r.body,r.status,r.updated_at,ns,r.id],
                )?;
                db.execute(
                    "INSERT INTO revisions VALUES(?1,?2,?3,?4,?5)
                     ON CONFLICT(id,revision) DO UPDATE SET body=excluded.body,recorded_at=excluded.recorded_at",
                    params![r.id,r.revision,ns,r.body,r.updated_at],
                )?;
                db.execute(
                    "DELETE FROM record_fts WHERE namespace=?1 AND id=?2",
                    params![ns, r.id],
                )?;
                if r.status == "active" {
                    db.execute(
                        "INSERT INTO record_fts(id,namespace,body) VALUES(?1,?2,?3)",
                        params![r.id, ns, r.body],
                    )?;
                }
            }
            if r.status != "active" {
                db.execute(
                    "DELETE FROM record_fts WHERE namespace=?1 AND id=?2",
                    params![ns, r.id],
                )?;
            }
            if let Some(key) = fact_text(&record.fact_key)? {
                db.execute(
                    "INSERT OR IGNORE INTO fact_bindings VALUES(?1,?2,?3)",
                    params![ns, key, m.id],
                )?;
            }
        }
    }
    for (record, m) in bundle.payload.records.iter().zip(&plan.records) {
        if m.action != "unchanged" {
            db.execute("DELETE FROM edges WHERE child_id=?1", [&m.id])?;
            for dep in &record.dependencies {
                db.execute(
                    "INSERT INTO edges VALUES(?1,?2,?3)",
                    params![m.id, record_ids[&dep.identity], dep.revision],
                )?;
            }
        }
    }
    for (candidate, m) in bundle.payload.candidates.iter().zip(&plan.candidates) {
        if m.action != "unchanged" {
            let key = fact_text(&candidate.fact_key)?;
            let record = candidate.record.as_ref().map(|id| record_ids[id].as_str());
            db.execute(
                "INSERT INTO ingestion_candidates
                 (id,namespace,event_id,request_hash,body,origin,source,fact_key,kind,volatility,
                  valid_from,valid_until,metadata_fields,state,record_id,accepted_revision,created_at,updated_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)
                 ON CONFLICT(id) DO UPDATE SET body=excluded.body,state=excluded.state,
                 record_id=excluded.record_id,accepted_revision=excluded.accepted_revision,updated_at=excluded.updated_at",
                params![m.id,ns,candidate.event_id,candidate.request_hash,candidate.body,
                    candidate.origin,candidate.source,key,candidate.kind,candidate.volatility,
                    candidate.valid_from,candidate.valid_until,json!(candidate.metadata_fields).to_string(),
                    candidate.state,record,candidate.accepted_revision,candidate.created_at,candidate.updated_at],
            )?;
        }
    }
    // A tombstone first seen here has no old target graph to invalidate above.
    // After installing fact bindings, it must also erase destination-local
    // pending/rejected proposals carrying that exact key.
    let tombstones: Vec<_> = bundle
        .payload
        .records
        .iter()
        .zip(&plan.records)
        .filter(|(record, _)| record.status == "retracted")
        .map(|(_, m)| m.id.clone())
        .collect();
    if !tombstones.is_empty() {
        invalidate(db, ns, &tombstones, true, at)?;
    }
    // Record baselines from the actual final state, including invalidation of
    // otherwise unchanged descendants. No content lives in these metadata tables.
    // Install all roots first so graph and candidate references serialize with
    // root provenance rather than the new destination's temporary local IDs.
    for (record, m) in bundle.payload.records.iter().zip(&plan.records) {
        save_origin(db, ns, "record", m, &state_hash(record))?;
    }
    for (candidate, m) in bundle.payload.candidates.iter().zip(&plan.candidates) {
        save_origin(db, ns, "candidate", m, &state_hash(candidate))?;
    }
    let final_state = export_payload(db, ns)?;
    for m in &plan.records {
        let actual = final_state
            .records
            .iter()
            .find(|r| r.source_id == m.id)
            .ok_or_else(|| invalid("Applied record disappeared."))?;
        save_origin(db, ns, "record", m, &state_hash(&actual))?;
    }
    for m in &plan.candidates {
        let actual = final_state
            .candidates
            .iter()
            .find(|r| r.source_id == m.id)
            .ok_or_else(|| invalid("Applied candidate disappeared."))?;
        save_origin(db, ns, "candidate", m, &state_hash(&actual))?;
    }
    let source = &bundle.payload.source_binding;
    db.execute(
        "INSERT INTO portable_watermarks VALUES(?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(namespace,source_history,source_namespace) DO UPDATE SET
         sequence=excluded.sequence,digest=excluded.digest,members=excluded.members,state_hashes=excluded.state_hashes",
        params![ns,string(source,"history_id")?,string(source,"namespace")?,
            bundle.payload.source_sequence,bundle.digest,json!(members(&bundle.payload)).to_string(),
            json!(source_hashes(&bundle.payload)).to_string()],
    )?;
    Ok(())
}

pub(super) fn mutate(
    db: &Connection,
    ns: &str,
    method: &str,
    args: &Value,
    at: i64,
) -> Result<Value> {
    if method != "bundle_apply" {
        return Err(invalid("Unknown portable mutation."));
    }
    let bundle = parse_bundle(args)?;
    if string(args, "reviewed_digest")? != bundle.digest {
        return Err(Error::new(
            "digest_mismatch",
            "Bundle differs from the reviewed digest.",
        ));
    }
    if string(args, "expected_target_commit")? != commit_token(db)? {
        return Err(Error::new(
            "target_changed",
            "Target changed since migration was reviewed; plan again.",
        ));
    }
    let plan = make_plan(db, ns, &bundle)?;
    if !plan.conflicts.is_empty() {
        return Err(Error::new(
            "migration_conflict",
            "Migration has conflicts; inspect a fresh bundle_plan.",
        ));
    }
    let mut result = report(db, &bundle, &plan)?;
    if !plan.replayed {
        apply(db, ns, &bundle, &plan, at)?;
    }
    result["applied"] = json!(true);
    Ok(result)
}

pub(super) fn read(
    db: &Connection,
    ns: &str,
    method: &str,
    args: &Value,
    at: i64,
) -> Result<Value> {
    match method {
        "bundle_export" => {
            let payload = export_payload(db, ns)?;
            check_payload(&payload)?;
            let digest = hash(&value(&payload));
            let bundle = value(&Bundle {
                format: FORMAT.into(),
                payload,
                digest,
            });
            if bundle.to_string().len() > MAX_BYTES {
                return Err(Error::new(
                    "resource_exhausted",
                    "Portable bundle exceeds 512 KiB; no partial snapshot was exported.",
                ));
            }
            Ok(bundle)
        }
        "bundle_plan" => {
            let bundle = parse_bundle(args)?;
            report(db, &bundle, &make_plan(db, ns, &bundle)?)
        }
        "bundle_context" => context(db, ns, args, at),
        _ => Err(invalid("Unknown portable read.")),
    }
}

fn context(db: &Connection, ns: &str, args: &Value, at: i64) -> Result<Value> {
    let bundle = parse_bundle(args)?;
    let source = &bundle.payload.source_binding;
    let saved = watermark(db, ns, source)?
        .ok_or_else(|| Error::new("migration_conflict", "Bundle has not been applied here."))?;
    if saved.0 != bundle.payload.source_sequence || saved.1 != bundle.digest {
        return Err(Error::new(
            "migration_conflict",
            "Only the current imported source snapshot can rebind context.",
        ));
    }
    let manifest = args
        .get("manifest")
        .ok_or_else(|| invalid("manifest is required."))?;
    let object = manifest
        .as_object()
        .ok_or_else(|| invalid("manifest must be an object."))?;
    if object.len() != 4
        || object.keys().any(|k| {
            !["schema_version", "namespace", "commit_token", "records"].contains(&k.as_str())
        })
        || manifest["schema_version"] != "recall-ledger.context/1"
        || manifest["namespace"] != source["namespace"]
    {
        return Err(invalid("Unsupported source context manifest."));
    }
    let (history, sequence) = string(manifest, "commit_token")?
        .rsplit_once(':')
        .ok_or_else(|| invalid("Invalid context commit token."))?;
    let sequence = sequence
        .parse::<i64>()
        .map_err(|_| invalid("Invalid context sequence."))?;
    if source["history_id"] != history || sequence < 0 || sequence > bundle.payload.source_sequence
    {
        return Err(invalid("Context is outside the exported source history."));
    }
    let refs = manifest["records"]
        .as_array()
        .filter(|refs| refs.len() <= 100)
        .ok_or_else(|| invalid("Context must have at most 100 references."))?;
    let target = export_payload(db, ns)?;
    let mut seen = BTreeSet::new();
    let mut rebound = Vec::new();
    for reference in refs {
        let shape = reference
            .as_object()
            .ok_or_else(|| invalid("Invalid context reference."))?;
        if shape.len() != 2
            || shape
                .keys()
                .any(|k| !["id", "revision"].contains(&k.as_str()))
        {
            return Err(invalid("Invalid context reference fields."));
        }
        let id = string(reference, "id")?;
        if !seen.insert(id) {
            return Err(invalid("Duplicate context reference."));
        }
        let revision = integer(reference, "revision", -1, 1, i64::MAX)?;
        let incoming = bundle
            .payload
            .records
            .iter()
            .find(|r| r.source_id == id && r.revision == revision)
            .ok_or_else(|| {
                Error::new(
                    "stale_dependency",
                    "Context reference is absent from the imported snapshot.",
                )
            })?;
        let current = target
            .records
            .iter()
            .find(|r| r.identity == incoming.identity)
            .ok_or_else(|| Error::new("stale_dependency", "Imported record is unavailable."))?;
        if state_hash(current) != state_hash(incoming) {
            return Err(Error::new(
                "stale_dependency",
                "Imported record changed after handoff.",
            ));
        }
        rebound.push(json!({"id":current.source_id,"revision":current.revision}));
    }
    let token = commit_token(db)?;
    let target_manifest = json!({
        "schema_version":"recall-ledger.context/1","namespace":ns,"commit_token":token,"records":rebound
    });
    if validate_context(db, ns, &json!({"manifest":target_manifest}), at)?["valid"] != true {
        return Err(Error::new(
            "stale_dependency",
            "Imported context is no longer eligible.",
        ));
    }
    Ok(
        json!({"manifest":target_manifest,"source_binding":source,"source_digest":bundle.digest,
        "commit_token":token,"evidence_valid":true,"semantics_verified":false}),
    )
}
