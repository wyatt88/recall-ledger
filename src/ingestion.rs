//! Explicit candidate review and exact, namespace-local fact identities.
//! Origin labels are provenance supplied by the caller, never an acceptance policy.
use crate::*;

const METADATA_FIELDS: [&str; 4] = ["kind", "volatility", "valid_from", "valid_until"];

pub(super) fn schema(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS fact_bindings(
           namespace TEXT NOT NULL,fact_key TEXT NOT NULL,record_id TEXT NOT NULL,
           PRIMARY KEY(namespace,fact_key),UNIQUE(namespace,record_id),
           FOREIGN KEY(record_id) REFERENCES records(id));
         CREATE TABLE IF NOT EXISTS ingestion_candidates(
           id TEXT PRIMARY KEY,namespace TEXT NOT NULL,event_id TEXT NOT NULL,
           request_hash TEXT NOT NULL,body TEXT,origin TEXT NOT NULL,source TEXT NOT NULL,
           fact_key TEXT,kind TEXT NOT NULL,volatility TEXT NOT NULL,
           valid_from INTEGER,valid_until INTEGER,metadata_fields TEXT NOT NULL,
           state TEXT NOT NULL,record_id TEXT,accepted_revision INTEGER,
           created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,
           UNIQUE(namespace,event_id),FOREIGN KEY(record_id) REFERENCES records(id));
         CREATE INDEX IF NOT EXISTS ingestion_candidates_scope
           ON ingestion_candidates(namespace,state,created_at,id);
         CREATE INDEX IF NOT EXISTS ingestion_candidates_fact
           ON ingestion_candidates(namespace,fact_key);
         CREATE INDEX IF NOT EXISTS ingestion_candidates_record
           ON ingestion_candidates(namespace,record_id);",
    )?;
    Ok(())
}

pub(super) fn allowed(method: &str) -> Option<&'static [&'static str]> {
    match method {
        "candidate_submit" => Some(&[
            "event_id",
            "body",
            "origin",
            "source",
            "fact_key",
            "kind",
            "volatility",
            "valid_from",
            "valid_until",
            "idempotency_key",
        ]),
        "candidate_list" => Some(&["state", "limit"]),
        "candidate_inspect" => Some(&["candidate_id"]),
        "candidate_resolve" => Some(&[
            "candidate_id",
            "decision",
            "expected_revision",
            "idempotency_key",
        ]),
        "fact_get" => Some(&["fact_key"]),
        _ => None,
    }
}

pub(super) fn is_mutation(method: &str) -> bool {
    matches!(method, "candidate_submit" | "candidate_resolve")
}

fn nonempty_name(value: &str, label: &str) -> Result<()> {
    validate_name(value, label)?;
    if value.trim().is_empty() {
        return Err(Error::new("invalid_request", format!("Invalid {label}.")));
    }
    Ok(())
}

// Serialize the exact tuple, not a delimiter-joined string or normalized text.
// Object key order cannot affect identity; case, Unicode and whitespace can.
fn fact_key(value: &Value) -> Result<String> {
    let object = value.as_object().ok_or_else(|| {
        Error::new(
            "invalid_request",
            "fact_key must contain entity, attribute, scope and conditions.",
        )
    })?;
    let fields = ["entity", "attribute", "scope", "conditions"];
    if object.len() != fields.len() || object.keys().any(|key| !fields.contains(&key.as_str())) {
        return Err(Error::new(
            "invalid_request",
            "fact_key requires exactly entity, attribute, scope and conditions.",
        ));
    }
    let mut tuple = Vec::new();
    for field in fields {
        let text = string(value, field)?;
        nonempty_name(text, field)?;
        tuple.push(text);
    }
    Ok(json!(tuple).to_string())
}

fn key_value(key: &str) -> Result<Value> {
    let value = parse_stored(key)?;
    let tuple = value
        .as_array()
        .filter(|tuple| tuple.len() == 4 && tuple.iter().all(Value::is_string))
        .ok_or_else(|| Error::new("database_error", "Invalid stored fact identity."))?;
    Ok(json!({
        "entity":tuple[0],"attribute":tuple[1],"scope":tuple[2],"conditions":tuple[3]
    }))
}

// Fact identities are immutable for a record through the public mutation paths:
// accepting the same key revises the body; a different key creates a new record.
// Portable updates likewise reject metadata changes, including this key. Readers
// must still fetch the binding in their snapshot instead of inferring it from
// a record's body, kind, source label, or a candidate's proposed identity.
pub(super) fn record_fact_key(db: &Connection, ns: &str, id: &str) -> Result<Option<Value>> {
    let key: Option<String> = db
        .query_row(
            "SELECT fact_key FROM fact_bindings WHERE namespace=?1 AND record_id=?2",
            params![ns, id],
            |row| row.get(0),
        )
        .optional()?;
    key.as_deref().map(key_value).transpose()
}

fn binding(db: &Connection, ns: &str, key: &str) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT record_id FROM fact_bindings WHERE namespace=?1 AND fact_key=?2",
            params![ns, key],
            |row| row.get(0),
        )
        .optional()?)
}

#[derive(Debug)]
struct Candidate {
    id: String,
    event_id: String,
    request_hash: String,
    body: Option<String>,
    origin: String,
    source: String,
    fact_key: Option<String>,
    kind: String,
    volatility: String,
    valid_from: Option<i64>,
    valid_until: Option<i64>,
    metadata_fields: String,
    state: String,
    record_id: Option<String>,
    accepted_revision: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

fn candidate(db: &Connection, ns: &str, id: &str) -> Result<Candidate> {
    db.query_row(
        "SELECT id,event_id,request_hash,body,origin,source,fact_key,kind,volatility,
         valid_from,valid_until,metadata_fields,state,record_id,accepted_revision,created_at,updated_at
         FROM ingestion_candidates WHERE namespace=?1 AND id=?2",
        params![ns, id],
        |row| {
            Ok(Candidate {
                id: row.get(0)?,
                event_id: row.get(1)?,
                request_hash: row.get(2)?,
                body: row.get(3)?,
                origin: row.get(4)?,
                source: row.get(5)?,
                fact_key: row.get(6)?,
                kind: row.get(7)?,
                volatility: row.get(8)?,
                valid_from: row.get(9)?,
                valid_until: row.get(10)?,
                metadata_fields: row.get(11)?,
                state: row.get(12)?,
                record_id: row.get(13)?,
                accepted_revision: row.get(14)?,
                created_at: row.get(15)?,
                updated_at: row.get(16)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| Error::new("not_found", "Candidate not found."))
}

fn receipt(candidate: &Candidate) -> Value {
    json!({
        "candidate_id":candidate.id,"event_id":candidate.event_id,"state":candidate.state,
        "record_id":candidate.record_id,"accepted_revision":candidate.accepted_revision,
        "revision":candidate.accepted_revision
    })
}

fn key_retracted(db: &Connection, ns: &str, key: Option<&str>) -> Result<bool> {
    if let Some(key) = key {
        if let Some(id) = binding(db, ns, key)? {
            return Ok(get(db, ns, &id)?.status == "retracted");
        }
    }
    Ok(false)
}

fn render(
    db: &Connection,
    ns: &str,
    candidate: &Candidate,
    at: i64,
    include_body: bool,
) -> Result<Value> {
    let mut value = receipt(candidate);
    value["origin"] = json!(candidate.origin);
    value["source"] = json!(candidate.source);
    value["kind"] = json!(candidate.kind);
    value["volatility"] = json!(candidate.volatility);
    value["valid_from"] = json!(candidate.valid_from);
    value["valid_until"] = json!(candidate.valid_until);
    value["metadata_fields"] = parse_stored(&candidate.metadata_fields)?;
    value["fact_key"] = candidate
        .fact_key
        .as_deref()
        .map(key_value)
        .transpose()?
        .unwrap_or(Value::Null);
    value["created_at"] = json!(candidate.created_at);
    value["updated_at"] = json!(candidate.updated_at);
    value["trust_boundary"] = json!("retrieved_data");
    value["body_available"] = json!(false);
    value["body_included"] = json!(false);

    // Read-side checks provide defense in depth even if a future caller forgets
    // the erasure hook. No API exposes a stored proposal after its key is withdrawn.
    if candidate.state == "withdrawn" || key_retracted(db, ns, candidate.fact_key.as_deref())? {
        value["state"] = json!("withdrawn");
        value["body_unavailable_reason"] = json!("source_retracted");
        return Ok(value);
    }
    match candidate.state.as_str() {
        "pending" => {
            value["body_source"] = json!("candidate_proposal");
            value["body_available"] = json!(candidate.body.is_some());
            if include_body {
                if let Some(text) = &candidate.body {
                    value["body"] = json!(text);
                    value["body_included"] = json!(true);
                }
            }
        }
        "accepted" => {
            let id = candidate
                .record_id
                .as_deref()
                .ok_or_else(|| Error::new("database_error", "Accepted candidate has no record."))?;
            let record = get(db, ns, id)?;
            value["current_revision"] = json!(record.revision);
            value["record_changed_since_acceptance"] =
                json!(candidate.accepted_revision != Some(record.revision));
            value["body_source"] = json!("current_record");
            value["current_record_metadata"] = json!({
                "kind":record.kind,"source":record.source,"volatility":record.volatility,
                "valid_from":record.valid_from,"valid_until":record.valid_until
            });
            if record.status == "retracted" {
                value["state"] = json!("withdrawn");
                value["body_unavailable_reason"] = json!("source_retracted");
            } else if valid(db, ns, &record, at)? {
                value["body_available"] = json!(true);
                if include_body {
                    value["body"] = json!(record.body);
                    value["body_included"] = json!(true);
                }
            } else {
                value["body_unavailable_reason"] = json!("record_not_currently_valid");
            }
        }
        "rejected" => value["body_unavailable_reason"] = json!("candidate_rejected"),
        _ => return Err(Error::new("database_error", "Invalid candidate state.")),
    }
    Ok(value)
}

fn submit(db: &Connection, ns: &str, v: &Value, at: i64) -> Result<Value> {
    let event_id = string(v, "event_id")?;
    nonempty_name(event_id, "event_id")?;
    let text = body(v)?;
    let origin = string(v, "origin")?;
    if ![
        "user_explicit",
        "user_message",
        "tool_observation",
        "model_inference",
        "import",
    ]
    .contains(&origin)
    {
        return Err(Error::new("invalid_request", "Unknown candidate origin."));
    }
    let source = string(v, "source")?;
    nonempty_name(source, "source")?;
    let key = v.get("fact_key").map(fact_key).transpose()?;
    let memory_kind = kind(v, "assertion")?;
    let volatility = optional_string(v, "volatility", "normal")?;
    if !["stable", "normal", "volatile"].contains(&volatility) {
        return Err(Error::new("invalid_request", "Unknown volatility."));
    }
    let start = optional_time(v, "valid_from")?;
    let end = optional_time(v, "valid_until")?;
    if start.zip(end).is_some_and(|(start, end)| start >= end) {
        return Err(Error::new(
            "invalid_request",
            "valid_until must be after valid_from.",
        ));
    }
    let provided: Vec<_> = METADATA_FIELDS
        .iter()
        .filter(|field| v.get(**field).is_some())
        .copied()
        .collect();
    // Omitted metadata inherits the canonical record when accepting an update,
    // so the presence mask is part of the event's semantic payload.
    let normalized = json!({
        "event_id":event_id,"body":text,"origin":origin,"source":source,"fact_key":key,
        "kind":memory_kind,"volatility":volatility,"valid_from":start,"valid_until":end,
        "metadata_fields":provided
    });
    let fingerprint = format!("{:x}", Sha256::digest(normalized.to_string().as_bytes()));
    let prior: Option<String> = db
        .query_row(
            "SELECT id FROM ingestion_candidates WHERE namespace=?1 AND event_id=?2",
            params![ns, event_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = prior {
        let prior = candidate(db, ns, &id)?;
        if fingerprint != prior.request_hash {
            return Err(Error::new(
                "event_conflict",
                "event_id already identifies a different candidate payload.",
            ));
        }
        return Ok(receipt(&prior));
    }
    if key_retracted(db, ns, key.as_deref())? {
        return Err(Error::new(
            "fact_retracted",
            "Fact identity was withdrawn; automatic resurrection is not supported.",
        ));
    }
    let id = Uuid::new_v4().to_string();
    db.execute(
        "INSERT INTO ingestion_candidates
         (id,namespace,event_id,request_hash,body,origin,source,fact_key,kind,volatility,
          valid_from,valid_until,metadata_fields,state,record_id,accepted_revision,created_at,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'pending',NULL,NULL,?14,?14)",
        params![
            id,
            ns,
            event_id,
            fingerprint,
            text,
            origin,
            source,
            key,
            memory_kind,
            volatility,
            start,
            end,
            json!(provided).to_string(),
            at
        ],
    )?;
    Ok(receipt(&candidate(db, ns, &id)?))
}

fn check_metadata(candidate: &Candidate, record: &Record) -> Result<()> {
    let provided = parse_stored(&candidate.metadata_fields)?;
    let provided = provided
        .as_array()
        .ok_or_else(|| Error::new("database_error", "Invalid stored metadata presence."))?;
    for field in provided {
        let matches = match field.as_str() {
            Some("kind") => candidate.kind == record.kind,
            Some("volatility") => candidate.volatility == record.volatility,
            Some("valid_from") => candidate.valid_from == record.valid_from,
            Some("valid_until") => candidate.valid_until == record.valid_until,
            _ => {
                return Err(Error::new(
                    "database_error",
                    "Invalid stored metadata field.",
                ))
            }
        };
        if !matches {
            return Err(Error::new(
                "fact_metadata_conflict",
                "Explicit metadata differs from the existing fact; use a distinct identity or conditions.",
            ));
        }
    }
    Ok(())
}

fn resolve(db: &Connection, ns: &str, v: &Value, at: i64) -> Result<Value> {
    let id = string(v, "candidate_id")?;
    nonempty_name(id, "candidate_id")?;
    let decision = string(v, "decision")?;
    if !["accept", "reject"].contains(&decision) {
        return Err(Error::new(
            "invalid_request",
            "decision must be accept or reject.",
        ));
    }
    let expected = if v.get("expected_revision").is_some() {
        Some(integer(v, "expected_revision", -1, 1, i64::MAX)?)
    } else {
        None
    };
    if decision == "reject" && expected.is_some() {
        return Err(Error::new(
            "invalid_request",
            "expected_revision applies only to accepting an existing fact.",
        ));
    }
    let current = candidate(db, ns, id)?;
    if current.state != "pending" {
        return Err(Error::new(
            "candidate_state_conflict",
            "Candidate has already been resolved or withdrawn.",
        ));
    }
    if decision == "reject" {
        db.execute(
            "UPDATE ingestion_candidates SET state='rejected',body=NULL,updated_at=?1
             WHERE namespace=?2 AND id=?3",
            params![at, ns, id],
        )?;
        return Ok(receipt(&candidate(db, ns, id)?));
    }
    let existing = current
        .fact_key
        .as_deref()
        .map(|key| binding(db, ns, key))
        .transpose()?
        .flatten();
    let text = current
        .body
        .as_deref()
        .ok_or_else(|| Error::new("candidate_state_conflict", "Candidate body is unavailable."))?;
    let result = if let Some(record_id) = existing {
        let record = get(db, ns, &record_id)?;
        if record.status == "retracted" {
            return Err(Error::new(
                "fact_retracted",
                "Fact identity was withdrawn; automatic resurrection is not supported.",
            ));
        }
        if expected != Some(record.revision) {
            return Err(Error::new(
                "revision_conflict",
                "Accepting an existing fact requires its current expected_revision.",
            ));
        }
        check_metadata(&current, &record)?;
        crate::mutate(
            db,
            ns,
            "revise",
            &json!({"id":record_id,"expected_revision":record.revision,"body":text}),
            at,
        )?
    } else {
        if expected.is_some() {
            return Err(Error::new(
                "revision_conflict",
                "No existing fact matches expected_revision.",
            ));
        }
        let result = crate::mutate(
            db,
            ns,
            "remember",
            &json!({
                "body":text,"source":current.source,"kind":current.kind,
                "volatility":current.volatility,
                "valid_from":current.valid_from,"valid_until":current.valid_until
            }),
            at,
        )?;
        if let Some(key) = &current.fact_key {
            db.execute(
                "INSERT INTO fact_bindings(namespace,fact_key,record_id) VALUES(?1,?2,?3)",
                params![ns, key, string(&result, "id")?],
            )?;
        }
        result
    };
    let record_id = string(&result, "id")?;
    let revision = integer(&result, "revision", -1, 1, i64::MAX)?;
    db.execute(
        "UPDATE ingestion_candidates SET state='accepted',body=NULL,record_id=?1,
         accepted_revision=?2,updated_at=?3 WHERE namespace=?4 AND id=?5",
        params![record_id, revision, at, ns, id],
    )?;
    Ok(receipt(&candidate(db, ns, id)?))
}

pub(super) fn mutate(db: &Connection, ns: &str, method: &str, v: &Value, at: i64) -> Result<Value> {
    match method {
        "candidate_submit" => submit(db, ns, v, at),
        "candidate_resolve" => resolve(db, ns, v, at),
        _ => Err(Error::new(
            "method_not_found",
            "Unknown ingestion mutation.",
        )),
    }
}

pub(super) fn read(db: &Connection, ns: &str, method: &str, v: &Value, at: i64) -> Result<Value> {
    match method {
        "candidate_inspect" => {
            let id = string(v, "candidate_id")?;
            nonempty_name(id, "candidate_id")?;
            let mut value = render(db, ns, &candidate(db, ns, id)?, at, true)?;
            value["commit_token"] = json!(commit_token(db)?);
            Ok(value)
        }
        "candidate_list" => {
            let state = v.get("state").map(|_| string(v, "state")).transpose()?;
            if state.is_some_and(|state| {
                !["pending", "accepted", "rejected", "withdrawn"].contains(&state)
            }) {
                return Err(Error::new("invalid_request", "Unknown candidate state."));
            }
            let limit = integer(v, "limit", 50, 1, 100)?;
            let total: i64 = db.query_row(
                "SELECT count(*) FROM ingestion_candidates
                 WHERE namespace=?1 AND (?2 IS NULL OR state=?2)",
                params![ns, state],
                |row| row.get(0),
            )?;
            let mut stmt = db.prepare(
                "SELECT id FROM ingestion_candidates
                 WHERE namespace=?1 AND (?2 IS NULL OR state=?2)
                 ORDER BY created_at,id LIMIT ?3",
            )?;
            let ids = stmt
                .query_map(params![ns, state, limit], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let candidates = ids
                .iter()
                .map(|id| render(db, ns, &candidate(db, ns, id)?, at, false))
                .collect::<Result<Vec<_>>>()?;
            Ok(json!({
                "candidates":candidates,"total":total,"truncated":total > limit,
                "commit_token":commit_token(db)?,"trust_boundary":"retrieved_data"
            }))
        }
        "fact_get" => {
            let key = fact_key(
                v.get("fact_key")
                    .ok_or_else(|| Error::new("invalid_request", "fact_key is required."))?,
            )?;
            let id =
                binding(db, ns, &key)?.ok_or_else(|| Error::new("not_found", "Fact not found."))?;
            let mut value = crate::read(db, ns, "inspect", &json!({"id":id}), at)?;
            value["fact_key"] = key_value(&key)?;
            value["record_id"] = json!(id);
            Ok(value)
        }
        _ => Err(Error::new("method_not_found", "Unknown ingestion read.")),
    }
}

/// Run inside the same transaction as record erasure. Keep identity tombstones
/// and fingerprints so event retries cannot reimport a withdrawn proposal.
pub(super) fn on_retract(db: &Connection, ns: &str, ids: &[String]) -> Result<()> {
    let at = now();
    for id in ids {
        db.execute(
            "UPDATE ingestion_candidates SET body=NULL,state='withdrawn',updated_at=?1
             WHERE namespace=?2 AND
             (record_id=?3 OR fact_key IN
               (SELECT fact_key FROM fact_bindings WHERE namespace=?2 AND record_id=?3))",
            params![at, ns, id],
        )?;
    }
    Ok(())
}
