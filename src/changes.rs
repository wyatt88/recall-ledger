//! Durable, body-free cache invalidation. Events and receipts commit together.
use crate::*;

pub(super) fn schema(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE change_events(
            namespace TEXT NOT NULL, sequence INTEGER PRIMARY KEY,
            method TEXT NOT NULL, record_ids TEXT NOT NULL);
         CREATE INDEX change_events_scope ON change_events(namespace,sequence);",
    )?;
    Ok(())
}

pub(super) fn append(db: &Connection, ns: &str, method: &str, receipt: &Value) -> Result<()> {
    let ids: Vec<&str> = receipt["id"]
        .as_str()
        .or_else(|| receipt["record_id"].as_str())
        .into_iter()
        .collect();
    db.execute(
        "INSERT INTO change_events(namespace,sequence,method,record_ids)
         VALUES(?1,CAST((SELECT value FROM meta WHERE key='sequence') AS INTEGER),?2,?3)",
        params![ns, method, json!(ids).to_string()],
    )?;
    Ok(())
}

pub(super) fn read(db: &Connection, ns: &str, v: &Value) -> Result<Value> {
    let limit = integer(v, "limit", 100, 1, 500)? as usize;
    let current = commit_token(db)?;
    let (history, _) = current.rsplit_once(':').expect("internal commit token");
    let after = match v.get("after") {
        None => 0,
        Some(token) => {
            check_commit(db, &json!({"minimum_commit":token}))?;
            token
                .as_str()
                .and_then(|t| t.rsplit_once(':'))
                .and_then(|(_, sequence)| sequence.parse::<i64>().ok())
                .ok_or_else(|| Error::new("invalid_request", "Invalid change cursor."))?
        }
    };
    let mut stmt = db.prepare(
        "SELECT sequence,method,record_ids FROM change_events
         WHERE namespace=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(params![ns, after, (limit + 1) as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let more = rows.len() > limit;
    let mut events = Vec::new();
    let mut cursor = current.clone();
    for (sequence, method, ids) in rows.into_iter().take(limit) {
        let token = format!("{history}:{sequence}");
        if more {
            cursor.clone_from(&token);
        }
        events.push(json!({
            "commit_token":token,"method":method,"record_ids":parse_stored(&ids)?,
            "resync_required":true
        }));
    }
    Ok(json!({
        "events":events,"cursor":cursor,"has_more":more,"commit_token":current,
        "resync_required":v.get("after").is_none() || !events.is_empty(),
        "scope":"namespace","record_ids_complete":false,
        "expiry_requires_fresh_read":true
    }))
}
