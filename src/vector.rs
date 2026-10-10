//! Exact cosine vectors kept in the same transactional database as source revisions.
use super::*;

pub(super) fn schema(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE vector_models(namespace TEXT NOT NULL,model_key TEXT NOT NULL,dimensions INTEGER NOT NULL,
           PRIMARY KEY(namespace,model_key));
         CREATE TABLE embeddings(namespace TEXT NOT NULL,record_id TEXT NOT NULL,revision INTEGER NOT NULL,
           model_key TEXT NOT NULL,vector BLOB NOT NULL,PRIMARY KEY(namespace,record_id,model_key),
           FOREIGN KEY(record_id) REFERENCES records(id));
         CREATE TABLE embedding_failures(namespace TEXT NOT NULL,record_id TEXT NOT NULL,revision INTEGER NOT NULL,
           model_key TEXT NOT NULL,attempts INTEGER NOT NULL,next_attempt INTEGER NOT NULL,error_code TEXT NOT NULL,
           PRIMARY KEY(namespace,record_id,model_key));"
    )?;
    Ok(())
}

pub(super) fn invalidate(db: &Connection, ns: &str, ids: &[String]) -> Result<()> {
    for id in ids {
        db.execute(
            "DELETE FROM embeddings WHERE namespace=?1 AND record_id=?2",
            params![ns, id],
        )?;
        db.execute(
            "DELETE FROM embedding_failures WHERE namespace=?1 AND record_id=?2",
            params![ns, id],
        )?;
    }
    Ok(())
}

fn normalized(value: &Value) -> Result<Vec<f64>> {
    let array = value
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 16384)
        .ok_or_else(|| {
            Error::new(
                "invalid_vector",
                "Vector requires 1 to 16384 finite numbers.",
            )
        })?;
    let values: Vec<f64> = array
        .iter()
        .map(|v| {
            v.as_f64()
                .filter(|n| n.is_finite())
                .ok_or_else(|| Error::new("invalid_vector", "Vector requires finite numbers."))
        })
        .collect::<Result<_>>()?;
    let scale = values.iter().fold(0.0_f64, |s, x| s.max(x.abs()));
    if scale == 0.0 {
        return Err(Error::new(
            "invalid_vector",
            "Zero vectors are not supported.",
        ));
    }
    let norm = values
        .iter()
        .map(|x| (x / scale).powi(2))
        .sum::<f64>()
        .sqrt();
    Ok(values.iter().map(|x| (x / scale) / norm).collect())
}

fn dimensions(db: &Connection, ns: &str, key: &str) -> Result<Option<usize>> {
    Ok(db
        .query_row(
            "SELECT dimensions FROM vector_models WHERE namespace=?1 AND model_key=?2",
            params![ns, key],
            |r| r.get(0),
        )
        .optional()?)
}

pub(super) fn mutate(db: &Connection, ns: &str, method: &str, v: &Value, at: i64) -> Result<Value> {
    let key = string(v, "model_key")?;
    validate_name(key, "model_key")?;
    if method == "embedding_retry" {
        let count = db.execute(
            "DELETE FROM embedding_failures WHERE namespace=?1 AND model_key=?2",
            params![ns, key],
        )?;
        return Ok(json!({"model_key":key,"reset_failures":count}));
    }
    let id = string(v, "id")?;
    let record = get(db, ns, id)?;
    require_valid(db, ns, &record, at)?;
    let revision = integer(v, "expected_revision", -1, 1, i64::MAX)?;
    if record.revision != revision {
        return Err(Error::new(
            "revision_conflict",
            "Embedding source revision changed.",
        ));
    }
    if method == "embedding_fail" {
        let code = string(v, "error_code")?;
        validate_name(code, "error_code")?;
        let previous: i64 = db.query_row(
            "SELECT attempts FROM embedding_failures WHERE namespace=?1 AND record_id=?2 AND revision=?3 AND model_key=?4",
            params![ns,id,revision,key], |r| r.get(0)).optional()?.unwrap_or(0);
        let attempts = (previous + 1).min(5);
        let next = if attempts == 5 {
            i64::MAX
        } else {
            at + 5 * (1_i64 << (attempts - 1))
        };
        db.execute(
            "INSERT OR REPLACE INTO embedding_failures VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![ns, id, revision, key, attempts, next, code],
        )?;
        return Ok(
            json!({"id":id,"model_key":key,"attempts":attempts,"state":if attempts==5 {"failed"} else {"retry_wait"},"next_attempt":next}),
        );
    }
    let vector = normalized(v.get("vector").unwrap_or(&Value::Null))?;
    if dimensions(db, ns, key)?.is_some_and(|n| n != vector.len()) {
        return Err(Error::new(
            "dimension_mismatch",
            "Model key is already bound to another vector dimension.",
        ));
    }
    let bytes: Vec<u8> = vector
        .iter()
        .flat_map(|x| (*x as f32).to_le_bytes())
        .collect();
    db.execute(
        "INSERT OR IGNORE INTO vector_models VALUES(?1,?2,?3)",
        params![ns, key, vector.len()],
    )?;
    db.execute(
        "INSERT OR REPLACE INTO embeddings VALUES(?1,?2,?3,?4,?5)",
        params![ns, id, revision, key, bytes],
    )?;
    db.execute(
        "DELETE FROM embedding_failures WHERE namespace=?1 AND record_id=?2 AND model_key=?3",
        params![ns, id, key],
    )?;
    Ok(
        json!({"id":id,"revision":revision,"model_key":key,"dimensions":vector.len(),"index_state":"semantic_ready"}),
    )
}

fn has_vector(db: &Connection, ns: &str, r: &Record, key: &str) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM embeddings WHERE namespace=?1 AND record_id=?2 AND revision=?3 AND model_key=?4)",
        params![ns,r.id,r.revision,key], |row| row.get(0))?)
}

pub(super) fn read(db: &Connection, ns: &str, method: &str, v: &Value, at: i64) -> Result<Value> {
    let key = string(v, "model_key")?;
    validate_name(key, "model_key")?;
    let records = readable(db, ns, at)?;
    let mut pending = 0;
    let mut indexed = 0;
    let mut failed = 0;
    let mut waiting = 0;
    let mut items = Vec::new();
    let mut bytes = 0;
    let limit = if method == "embedding_candidates" {
        integer(v, "limit", 4, 1, 32)? as usize
    } else {
        0
    };
    for record in records.iter() {
        if has_vector(db, ns, record, key)? {
            indexed += 1;
            continue;
        }
        pending += 1;
        let failure:Option<(i64,i64)>=db.query_row("SELECT attempts,next_attempt FROM embedding_failures WHERE namespace=?1 AND record_id=?2 AND revision=?3 AND model_key=?4",
            params![ns,record.id,record.revision,key],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        if let Some((attempts, next)) = failure {
            if attempts >= 5 {
                failed += 1;
                continue;
            }
            if next > at {
                waiting += 1;
                continue;
            }
        }
        let item = json!(record);
        let size = item.to_string().len();
        if items.len() < limit && bytes + size < 512 * 1024 {
            items.push(item);
            bytes += size;
        }
    }
    Ok(
        json!({"model_key":key,"eligible":records.len(),"indexed":indexed,"pending":pending,
        "failed":failed,"retry_wait":waiting,"items":items,"dimensions":dimensions(db,ns,key)?,"commit_token":commit_token(db)?}),
    )
}

/// Candidates are already namespace/revision/ancestry/time filtered by the caller.
/// Return exact scores, not approximate-nearest-neighbour results.
pub(super) fn scores(
    db: &Connection,
    ns: &str,
    key: &str,
    query: &Value,
    records: &[Record],
) -> Result<BTreeMap<String, f64>> {
    validate_name(key, "model_key")?;
    let query = normalized(query)?;
    if dimensions(db, ns, key)?.is_some_and(|n| n != query.len()) {
        return Err(Error::new(
            "dimension_mismatch",
            "Query vector does not match model dimension.",
        ));
    }
    let mut output = BTreeMap::new();
    let mut stmt=db.prepare("SELECT vector FROM embeddings WHERE namespace=?1 AND record_id=?2 AND revision=?3 AND model_key=?4")?;
    for record in records {
        let bytes: Option<Vec<u8>> = stmt
            .query_row(params![ns, record.id, record.revision, key], |row| {
                row.get(0)
            })
            .optional()?;
        if let Some(bytes) = bytes {
            if bytes.len() != query.len() * 4 {
                return Err(Error::new("database_error", "Invalid stored vector."));
            }
            let mut dot = 0.0;
            for (chunk, q) in bytes.as_chunks::<4>().0.iter().zip(&query) {
                let x = f32::from_le_bytes(*chunk) as f64;
                if !x.is_finite() {
                    return Err(Error::new("database_error", "Invalid stored vector."));
                }
                dot += x * q;
            }
            output.insert(record.id.clone(), dot.clamp(-1.0, 1.0));
        }
    }
    Ok(output)
}
