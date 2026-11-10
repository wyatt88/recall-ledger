//! Read-only change explanations and conservative historical evidence.
//! Events contain metadata only; bodies remain in the withdrawal-aware
//! revisions table. A historical event never grants permission to resurrect it.
use crate::*;

const MAX_RESPONSE_BYTES: usize = 512 * 1024;
const MAX_PREVIEW_NODES: usize = 1000;
const MAX_CHAIN: usize = 100;
const SCAN_LIMIT: i64 = 500;

pub(super) fn schema(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS history_events(
           event_id INTEGER PRIMARY KEY AUTOINCREMENT,
           namespace TEXT NOT NULL,record_id TEXT NOT NULL,revision INTEGER NOT NULL,
           event_type TEXT NOT NULL,status_at_event TEXT NOT NULL,
           recorded_at INTEGER NOT NULL,occurred_at INTEGER,
           valid_from INTEGER,valid_until INTEGER,commit_sequence INTEGER,history_id TEXT,
           method TEXT NOT NULL,dependencies TEXT,time_quality TEXT NOT NULL,
           finalized INTEGER NOT NULL,
           FOREIGN KEY(record_id) REFERENCES records(id));
         CREATE INDEX IF NOT EXISTS history_events_scope
           ON history_events(namespace,event_id);
         CREATE INDEX IF NOT EXISTS history_events_record
           ON history_events(namespace,record_id,event_id);
         CREATE INDEX IF NOT EXISTS history_events_pending
           ON history_events(namespace,finalized);
         INSERT INTO history_events(namespace,record_id,revision,event_type,status_at_event,
           recorded_at,valid_from,valid_until,method,time_quality,finalized)
         SELECT r.namespace,r.id,r.revision,'revision','active',r.recorded_at,
           c.valid_from,c.valid_until,'legacy','legacy_revision',1
         FROM revisions r JOIN records c ON c.id=r.id
         WHERE NOT EXISTS(SELECT 1 FROM history_events e
           WHERE e.namespace=r.namespace AND e.record_id=r.id AND e.revision=r.revision)
         ORDER BY r.recorded_at,r.id,r.revision;
         INSERT INTO history_events(namespace,record_id,revision,event_type,status_at_event,
           recorded_at,valid_from,valid_until,method,time_quality,finalized)
         SELECT namespace,id,revision,status,status,updated_at,
           valid_from,valid_until,'legacy','legacy_current_state',1 FROM records c
         WHERE status!='active' AND NOT EXISTS(SELECT 1 FROM history_events e
           WHERE e.record_id=c.id AND e.event_type=c.status);
         CREATE TRIGGER IF NOT EXISTS history_revision_insert AFTER INSERT ON revisions
         BEGIN
           INSERT INTO history_events(namespace,record_id,revision,event_type,status_at_event,
             recorded_at,valid_from,valid_until,method,time_quality,finalized)
           SELECT NEW.namespace,NEW.id,NEW.revision,'revision',status,
             NEW.recorded_at,valid_from,valid_until,'pending','local_commit',0
           FROM records WHERE id=NEW.id;
         END;
         CREATE TRIGGER IF NOT EXISTS history_status_update AFTER UPDATE OF status ON records
         WHEN OLD.status!=NEW.status AND NEW.status IN ('stale','retracted')
         BEGIN
           INSERT INTO history_events(namespace,record_id,revision,event_type,status_at_event,
             recorded_at,valid_from,valid_until,method,time_quality,finalized)
           VALUES(NEW.namespace,NEW.id,NEW.revision,NEW.status,NEW.status,
             NEW.updated_at,NEW.valid_from,NEW.valid_until,'pending','local_commit',0);
         END;",
    )?;
    // Only the current legacy revision has a recoverable dependency snapshot.
    // Inferring old edges from today's graph would silently revive old summaries.
    let mut stmt = db.prepare(
        "SELECT e.event_id,e.record_id FROM history_events e JOIN records r ON r.id=e.record_id
         WHERE e.method='legacy' AND e.event_type='revision' AND e.revision=r.revision
           AND e.dependencies IS NULL",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (event_id, id) in rows {
        db.execute(
            "UPDATE history_events SET dependencies=?1 WHERE event_id=?2",
            params![json!(dependencies(db, &id)?).to_string(), event_id],
        )?;
    }
    Ok(())
}

/// Complete trigger-captured metadata in the same transaction as the mutation,
/// receipt and change feed. Imports use the LOCAL admission time, never the
/// originating store's potentially old updated_at as local knowledge time.
pub(super) fn finalize(
    db: &Connection,
    ns: &str,
    method: &str,
    args: &Value,
    receipt: &Value,
    at: i64,
) -> Result<()> {
    let mut stmt = db.prepare(
        "SELECT event_id,record_id,revision,event_type FROM history_events
         WHERE namespace=?1 AND finalized=0 ORDER BY event_id",
    )?;
    let pending = stmt
        .query_map([ns], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let primary_id = receipt["id"]
        .as_str()
        .or_else(|| receipt["record_id"].as_str());
    for (event_id, id, revision, event_type) in pending {
        let r = get(db, ns, &id)?;
        let deps = (event_type == "revision" && r.revision == revision)
            .then(|| dependencies(db, &id).map(|deps| json!(deps).to_string()))
            .transpose()?;
        let occurred_at = if matches!(method, "remember" | "revise")
            && primary_id == Some(id.as_str())
            && args.get("occurred_at").is_some()
        {
            optional_time(args, "occurred_at")?
        } else {
            db.query_row(
                "SELECT occurred_at FROM history_events WHERE namespace=?1 AND record_id=?2
                 AND event_id<?3 AND event_type='revision' ORDER BY event_id DESC LIMIT 1",
                params![ns, id, event_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten()
        };
        db.execute(
            "UPDATE history_events SET recorded_at=?1,occurred_at=?2,
             commit_sequence=CAST((SELECT value FROM meta WHERE key='sequence') AS INTEGER),
             history_id=(SELECT value FROM meta WHERE key='store_id'),
             method=?3,dependencies=?4,finalized=1 WHERE event_id=?5",
            params![at, occurred_at, method, deps, event_id],
        )?;
    }
    Ok(())
}

#[derive(Debug)]
struct Event {
    event_id: i64,
    id: String,
    revision: i64,
    event_type: String,
    status: String,
    recorded_at: i64,
    occurred_at: Option<i64>,
    valid_from: Option<i64>,
    valid_until: Option<i64>,
    commit_sequence: Option<i64>,
    method: String,
    dependencies: Option<String>,
    time_quality: String,
    history_id: Option<String>,
}

const EVENT_COLUMNS: &str = "e.event_id,e.record_id,e.revision,e.event_type,e.status_at_event,
    e.recorded_at,e.occurred_at,e.valid_from,e.valid_until,e.commit_sequence,e.method,
    e.dependencies,e.time_quality,e.history_id";

fn event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        event_id: row.get(0)?,
        id: row.get(1)?,
        revision: row.get(2)?,
        event_type: row.get(3)?,
        status: row.get(4)?,
        recorded_at: row.get(5)?,
        occurred_at: row.get(6)?,
        valid_from: row.get(7)?,
        valid_until: row.get(8)?,
        commit_sequence: row.get(9)?,
        method: row.get(10)?,
        dependencies: row.get(11)?,
        time_quality: row.get(12)?,
        history_id: row.get(13)?,
    })
}

fn event_dependencies(event: &Event) -> Result<Option<Vec<(String, i64)>>> {
    event
        .dependencies
        .as_deref()
        .map(|text| {
            serde_json::from_str(text).map_err(|_| {
                Error::new("database_error", "Invalid historical dependency metadata.")
            })
        })
        .transpose()
}

fn body_withheld_reason(
    db: &Connection,
    ns: &str,
    event: &Event,
    record: &Record,
    effective_at: i64,
) -> Result<Option<&'static str>> {
    if record.status == "retracted" {
        return Ok(Some("withdrawn"));
    }
    if record.status != "active" {
        return Ok(Some("stale_dependency"));
    }
    if event.status != "active" || event.event_type != "revision" {
        return Ok(Some("not_active_at_event"));
    }
    if event.valid_from.is_some_and(|t| t > effective_at)
        || event.valid_until.is_some_and(|t| effective_at >= t)
    {
        return Ok(Some("outside_validity_at_time"));
    }
    if !valid(db, ns, record, effective_at)? {
        return Ok(Some("current_ancestry_not_eligible"));
    }
    let Some(mut stack) = event_dependencies(event)? else {
        return Ok(Some("historical_dependencies_unknown"));
    };
    let mut seen = HashSet::new();
    while let Some((id, captured_revision)) = stack.pop() {
        if !seen.insert((id.clone(), captured_revision)) {
            continue;
        }
        if seen.len() > MAX_ANCESTORS {
            return Err(Error::new(
                "resource_exhausted",
                "Historical dependency limit exceeded.",
            ));
        }
        let parent = get(db, ns, &id)?;
        // Use current status/revision even for a historical cutoff. Historical
        // visibility is never a way around a withdrawn or corrected ancestor.
        if parent.status != "active" || parent.revision != captured_revision {
            return Ok(Some("ancestor_changed_or_withdrawn"));
        }
        if !temporally_valid(&parent, effective_at) {
            return Ok(Some("ancestor_outside_validity_at_time"));
        }
        stack.extend(dependencies(db, &id)?);
    }
    Ok(None)
}

fn event_value(
    db: &Connection,
    ns: &str,
    event: &Event,
    effective_at: i64,
    now: i64,
) -> Result<Value> {
    let record = get(db, ns, &event.id)?;
    let reason = body_withheld_reason(db, ns, event, &record, effective_at)?;
    let usable =
        reason.is_none() && record.revision == event.revision && valid(db, ns, &record, now)?;
    let mut value = json!({
        "event_id":event.event_id,"type":event.event_type,"id":event.id,
        "revision":event.revision,"current_revision":record.revision,
        "status_at_event":event.status,"current_status":record.status,
        "recorded_at":event.recorded_at,"occurred_at":event.occurred_at,
        "valid_from":event.valid_from,"valid_until":event.valid_until,
        "commit_sequence":event.commit_sequence,"history_id":event.history_id,"method":event.method,
        "time_quality":event.time_quality,"kind":record.kind,
        "historical_only":!usable,"usable_as_current":usable,
        "body_withheld_reason":reason,
        "trust_boundary":"retrieved_data","semantics_verified":false
    });
    if reason.is_none() {
        let body: String = db.query_row(
            "SELECT body FROM revisions WHERE namespace=?1 AND id=?2 AND revision=?3",
            params![ns, event.id, event.revision],
            |row| row.get(0),
        )?;
        value["body"] = json!(body);
        value["source"] = json!(record.source);
        value["fact_key"] = json!(ingestion::record_fact_key(db, ns, &event.id)?);
    }
    Ok(value)
}

fn limited(value: Value) -> Result<Value> {
    if value.to_string().len() > MAX_RESPONSE_BYTES {
        return Err(Error::new(
            "resource_exhausted",
            "Complete explanation exceeds 512 KiB; query a narrower record. No source text was truncated.",
        ));
    }
    Ok(value)
}

pub(super) fn impact_preview(db: &Connection, ns: &str, v: &Value, at: i64) -> Result<Value> {
    let id = string(v, "id")?;
    let root = get(db, ns, id)?;
    if root.status == "retracted" {
        return Err(Error::new("not_found", "Record not found."));
    }
    if v.get("expected_revision").is_some()
        && integer(v, "expected_revision", -1, 1, i64::MAX)? != root.revision
    {
        return Err(Error::new("revision_conflict", "Record revision changed."));
    }
    let root_valid = valid(db, ns, &root, at)?;
    let mut root_value = record_value(db, ns, &root)?;
    if !root_valid {
        root_value.as_object_mut().unwrap().remove("body");
        root_value["body_withheld_reason"] = json!("not_currently_eligible");
    }
    root_value["applicable_now"] = json!(root_valid);
    let mut stack = vec![id.to_owned()];
    let mut seen = HashSet::new();
    let mut sources = Vec::new();
    let mut guards = Vec::new();
    while let Some(current_id) = stack.pop() {
        if !seen.insert(current_id.clone()) {
            continue;
        }
        if seen.len() > MAX_PREVIEW_NODES {
            return Err(Error::new(
                "resource_exhausted",
                "Preview exceeds 1000 registered ancestors.",
            ));
        }
        let record = get(db, ns, &current_id)?;
        let deps = dependencies(db, &current_id)?;
        for (parent_id, captured_revision) in &deps {
            let parent = get(db, ns, parent_id)?;
            guards.push(json!({
                "child_id":current_id,"parent_id":parent_id,
                "captured_revision":captured_revision,"current_revision":parent.revision,
                "applicable_now":*captured_revision==parent.revision && valid(db,ns,&parent,at)?
            }));
            stack.push(parent_id.clone());
        }
        if deps.is_empty() {
            let eligible = valid(db, ns, &record, at)?;
            let is_source = ["assertion", "preference", "episode", "commitment"]
                .contains(&record.kind.as_str());
            let mut source = json!({
                "id":record.id,"revision":record.revision,"kind":record.kind,
                "status":record.status,"applicable_now":eligible,
                "provenance":"registered_leaf","source_assertion":is_source
            });
            if eligible && is_source {
                source["body"] = json!(record.body);
                source["source"] = json!(record.source);
                source["fact_key"] = json!(ingestion::record_fact_key(db, ns, &current_id)?);
            } else {
                source["body_withheld_reason"] = json!(if is_source {
                    "not_currently_eligible"
                } else {
                    "source_evidence_missing"
                });
            }
            sources.push(source);
        }
    }
    sources.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let affected = descendants(db, id)?;
    if affected.len() > MAX_PREVIEW_NODES {
        return Err(Error::new(
            "resource_exhausted",
            "Preview exceeds 1000 registered descendants.",
        ));
    }
    let mut descendants = Vec::new();
    for child in affected {
        let r = get(db, ns, &child)?;
        descendants.push(json!({
            "id":r.id,"revision":r.revision,"kind":r.kind,"status":r.status,
            "applicable_now":valid(db,ns,&r,at)?
        }));
    }
    let mut stmt = db.prepare(&format!(
        "SELECT {EVENT_COLUMNS} FROM history_events e
         WHERE e.namespace=?1 AND e.record_id=?2 AND e.event_type='revision'
         ORDER BY e.event_id LIMIT ?3"
    ))?;
    let events = stmt
        .query_map(params![ns, id, (MAX_CHAIN + 1) as i64], event_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if events.len() > MAX_CHAIN {
        return Err(Error::new(
            "resource_exhausted",
            "Revision chain exceeds 100; use paginated timeline.",
        ));
    }
    let chain = events
        .iter()
        .map(|e| event_value(db, ns, e, e.recorded_at, at))
        .collect::<Result<Vec<_>>>()?;
    let mutable_count = descendants
        .iter()
        .filter(|r| r["status"] != "retracted")
        .count();
    let managed = maintenance::is_managed(db, ns, id)?;
    let pending_jobs: i64 = db.query_row(
        "WITH RECURSIVE affected(id) AS (
           SELECT ?2 UNION SELECT child_id FROM edges JOIN affected ON parent_id=affected.id)
         SELECT COUNT(DISTINCT j.id) FROM jobs j JOIN job_inputs i ON i.job_id=j.id
         WHERE j.namespace=?1 AND j.state='prepared' AND i.record_id IN (SELECT id FROM affected)",
        params![ns, id],
        |row| row.get(0),
    )?;
    limited(json!({
        "record":root_value,"sources":sources,"dependencies":guards,"revision_chain":chain,
        "descendants":descendants,"registered_dependencies_only":true,
        "effects":{
            "revise":{"allowed":root_valid && !managed,"invalidated_descendants":mutable_count,
                "blocked_reason":if managed {Some("managed_record")} else if !root_valid {Some("stale_dependency")} else {None}},
            "retract":{"allowed":true,"withdrawn_descendants":mutable_count,"physical_erasure":false},
            "prepared_jobs_affected":pending_jobs
        },
        "preview_only":true,"requires_revision_check_on_write":true,
        "commit_token":commit_token(db)?,"checked_at":at,
        "trust_boundary":"retrieved_data","semantics_verified":false
    }))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: i64,
    namespace: String,
    commit_token: String,
    query_hash: String,
    as_of: Option<i64>,
    before: i64,
}

pub(super) fn timeline(db: &Connection, ns: &str, v: &Value, at: i64) -> Result<Value> {
    let limit = integer(v, "limit", 50, 1, 100)? as usize;
    let query = optional_string(v, "query", "")?;
    if query.len() > 4096 {
        return Err(Error::new(
            "invalid_request",
            "Timeline query exceeds 4096 bytes.",
        ));
    }
    let as_of = optional_time(v, "as_of")?;
    if as_of.is_some_and(|t| t > at) {
        return Err(Error::new(
            "invalid_request",
            "as_of cannot be later than the current time.",
        ));
    }
    let token = commit_token(db)?;
    let query_hash = format!("{:x}", Sha256::digest(query.as_bytes()));
    let before = if let Some(cursor) = v.get("cursor") {
        let text = cursor
            .as_str()
            .filter(|s| s.len() <= 8192)
            .ok_or_else(|| Error::new("invalid_request", "Invalid timeline cursor."))?;
        let cursor: Cursor = serde_json::from_str(text)
            .map_err(|_| Error::new("invalid_request", "Invalid timeline cursor."))?;
        if cursor.version != 1
            || cursor.namespace != ns
            || cursor.query_hash != query_hash
            || cursor.as_of != as_of
            || cursor.before < 1
        {
            return Err(Error::new(
                "invalid_request",
                "Timeline cursor does not match this query.",
            ));
        }
        if cursor.commit_token != token {
            return Err(Error::new(
                "stale_cursor",
                "Ledger changed; restart the timeline query.",
            ));
        }
        cursor.before
    } else {
        i64::MAX
    };
    // With as_of, choose the last locally recorded event for each record as of
    // that second, instead of treating every earlier revision as still current.
    let mut stmt = db.prepare(&format!(
        "SELECT {EVENT_COLUMNS} FROM history_events e
         WHERE e.namespace=?1 AND e.event_id<?2 AND e.finalized=1
           AND (?3 IS NULL OR (e.recorded_at<=?3 AND NOT EXISTS(
             SELECT 1 FROM history_events later
             WHERE later.namespace=e.namespace AND later.record_id=e.record_id
               AND later.event_id>e.event_id AND later.recorded_at<=?3)))
         ORDER BY e.event_id DESC LIMIT ?4"
    ))?;
    let rows = stmt
        .query_map(params![ns, before, as_of, SCAN_LIMIT + 1], event_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut events = Vec::new();
    let mut last = before;
    let mut scanned = 0;
    let mut used_bytes = 0;
    let mut has_more = false;
    let normalized_query = query.to_lowercase();
    for (index, event) in rows.iter().enumerate() {
        if index == SCAN_LIMIT as usize || events.len() == limit {
            has_more = true;
            break;
        }
        let value = event_value(db, ns, event, as_of.unwrap_or(event.recorded_at), at)?;
        // Search ONLY permitted rendered fields. Matching suppressed historical
        // bodies would disclose withdrawn text through hits/counts.
        let matches = normalized_query.is_empty()
            || value
                .get("body")
                .and_then(Value::as_str)
                .is_some_and(|s| s.to_lowercase().contains(&normalized_query))
            || event.id.to_lowercase().contains(&normalized_query);
        if matches {
            let bytes = value.to_string().len();
            if used_bytes + bytes > MAX_RESPONSE_BYTES - 16_384 {
                has_more = true;
                break;
            }
            used_bytes += bytes;
            events.push(value);
        }
        scanned += 1;
        last = event.event_id;
    }
    let next_cursor = has_more.then(|| {
        serde_json::to_string(&Cursor {
            version: 1,
            namespace: ns.into(),
            commit_token: token.clone(),
            query_hash,
            as_of,
            before: last,
        })
        .expect("serializable cursor")
    });
    limited(json!({
        "events":events,"next_cursor":next_cursor,"has_more":has_more,
        "as_of":as_of,"mode":if as_of.is_some() {"knowledge_at_time"} else {"event_history"},
        "order":"newest_commit_first","commit_token":token,"checked_at":at,"scanned":scanned,
        "time_unit":"unix_seconds","current_withdrawals_enforced":true,
        "historical_evidence_is_not_current_context":true,
        "legacy_history_complete":false,"trust_boundary":"retrieved_data"
    }))
}
