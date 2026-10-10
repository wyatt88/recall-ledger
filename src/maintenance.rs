//! Durable managed-summary recipes. Model execution always happens outside transactions.
use super::*;

const MAX_FAILURES: i64 = 5;
// Worst-case JSON string escaping still fits the transport frame comfortably.
const MAX_INPUT_BYTES: usize = 65_536;

pub(super) fn schema(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS summary_recipes(
           id TEXT PRIMARY KEY,namespace TEXT NOT NULL,output_id TEXT NOT NULL UNIQUE,
           processor TEXT NOT NULL,max_chars INTEGER NOT NULL,status TEXT NOT NULL,
           created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL);
         CREATE INDEX IF NOT EXISTS summary_recipes_scope ON summary_recipes(namespace,status);
         CREATE TABLE IF NOT EXISTS summary_inputs(
           recipe_id TEXT NOT NULL,record_id TEXT NOT NULL,ordinal INTEGER NOT NULL,
           PRIMARY KEY(recipe_id,record_id),FOREIGN KEY(recipe_id) REFERENCES summary_recipes(id),
           FOREIGN KEY(record_id) REFERENCES records(id));
         CREATE INDEX IF NOT EXISTS summary_inputs_parent ON summary_inputs(record_id);
         CREATE TABLE IF NOT EXISTS maintenance_tasks(
           id TEXT PRIMARY KEY,recipe_id TEXT NOT NULL UNIQUE,namespace TEXT NOT NULL,
           generation INTEGER NOT NULL,state TEXT NOT NULL,attempts INTEGER NOT NULL,
           claim_count INTEGER NOT NULL,next_attempt_at INTEGER NOT NULL,
           lease_token TEXT,lease_until INTEGER,worker TEXT,output_revision INTEGER,
           error_code TEXT,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,
           FOREIGN KEY(recipe_id) REFERENCES summary_recipes(id));
         CREATE INDEX IF NOT EXISTS maintenance_tasks_ready ON maintenance_tasks(namespace,state,next_attempt_at);
         CREATE TABLE IF NOT EXISTS maintenance_inputs(
           task_id TEXT NOT NULL,record_id TEXT NOT NULL,revision INTEGER NOT NULL,ordinal INTEGER NOT NULL,
           PRIMARY KEY(task_id,record_id),FOREIGN KEY(task_id) REFERENCES maintenance_tasks(id),
           FOREIGN KEY(record_id) REFERENCES records(id));",
    )?;
    Ok(())
}

pub(super) fn is_managed(db: &Connection, ns: &str, id: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM summary_recipes WHERE namespace=?1 AND output_id=?2)",
        params![ns, id],
        |row| row.get(0),
    )?)
}

pub(super) fn assigned(db: &Connection, ns: &str, id: &str) -> Result<bool> {
    // Automatic discovery must respect a cancelled/withdrawn summary, including
    // when every original source remains valid. Explicit summarize can create a
    // new recipe, but a polling worker must not recreate it without that intent.
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM summary_inputs i JOIN summary_recipes r ON r.id=i.recipe_id
         WHERE r.namespace=?1 AND i.record_id=?2)",
        params![ns, id],
        |row| row.get(0),
    )?)
}

#[derive(Debug)]
struct Task {
    id: String,
    recipe_id: String,
    output_id: String,
    processor: String,
    max_chars: i64,
    recipe_status: String,
    generation: i64,
    state: String,
    attempts: i64,
    claim_count: i64,
    next_attempt_at: i64,
    lease_token: Option<String>,
    lease_until: Option<i64>,
    worker: Option<String>,
    output_revision: Option<i64>,
    error_code: Option<String>,
}

fn task(db: &Connection, ns: &str, id: &str) -> Result<Task> {
    db.query_row(
        "SELECT t.id,t.recipe_id,r.output_id,r.processor,r.max_chars,r.status,
         t.generation,t.state,t.attempts,t.claim_count,t.next_attempt_at,t.lease_token,
         t.lease_until,t.worker,t.output_revision,t.error_code
         FROM maintenance_tasks t JOIN summary_recipes r ON r.id=t.recipe_id
         WHERE t.namespace=?1 AND r.namespace=?1 AND t.id=?2",
        params![ns, id],
        |r| {
            Ok(Task {
                id: r.get(0)?,
                recipe_id: r.get(1)?,
                output_id: r.get(2)?,
                processor: r.get(3)?,
                max_chars: r.get(4)?,
                recipe_status: r.get(5)?,
                generation: r.get(6)?,
                state: r.get(7)?,
                attempts: r.get(8)?,
                claim_count: r.get(9)?,
                next_attempt_at: r.get(10)?,
                lease_token: r.get(11)?,
                lease_until: r.get(12)?,
                worker: r.get(13)?,
                output_revision: r.get(14)?,
                error_code: r.get(15)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| Error::new("not_found", "Maintenance task not found."))
}

fn lease_guard(t: &Task, v: &Value, at: i64) -> Result<()> {
    let token = string(v, "lease_token")?;
    if t.recipe_status != "active"
        || t.state != "leased"
        || t.lease_token.as_deref() != Some(token)
        || t.lease_until.is_none_or(|end| end <= at)
    {
        return Err(Error::new(
            "lease_conflict",
            "Maintenance lease expired, changed, or was cancelled.",
        ));
    }
    Ok(())
}

fn input_ids(db: &Connection, recipe: &str) -> Result<Vec<String>> {
    let mut stmt =
        db.prepare("SELECT record_id FROM summary_inputs WHERE recipe_id=?1 ORDER BY ordinal")?;
    let rows = stmt.query_map([recipe], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn current_inputs(db: &Connection, ns: &str, recipe: &str, at: i64) -> Result<Vec<Record>> {
    input_ids(db, recipe)?
        .iter()
        .map(|id| {
            let record = get(db, ns, id)?;
            require_valid(db, ns, &record, at)?;
            Ok(record)
        })
        .collect()
}

fn captured_inputs(db: &Connection, ns: &str, t: &Task, at: i64) -> Result<Vec<Record>> {
    let mut stmt = db.prepare(
        "SELECT record_id,revision FROM maintenance_inputs WHERE task_id=?1 ORDER BY ordinal",
    )?;
    let refs = stmt
        .query_map([&t.id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if refs.is_empty() {
        return Err(Error::new(
            "stale_dependency",
            "Task has no captured inputs.",
        ));
    }
    let mut result = Vec::with_capacity(refs.len());
    for (id, revision) in refs {
        let record = get(db, ns, &id)?;
        if record.revision != revision {
            return Err(Error::new(
                "stale_dependency",
                "A captured input revision changed.",
            ));
        }
        require_valid(db, ns, &record, at)?;
        result.push(record);
    }
    Ok(result)
}

fn check_input_budget(inputs: &[Record]) -> Result<()> {
    let bytes = inputs.iter().try_fold(0usize, |sum, r| {
        sum.checked_add(r.body.len())
            .and_then(|sum| sum.checked_add(r.id.len()))
            .filter(|total| *total <= MAX_INPUT_BYTES)
    });
    if bytes.is_none() {
        return Err(Error::new(
            "resource_exhausted",
            "Summary inputs exceed 65536 UTF-8 bytes; create smaller recipes.",
        ));
    }
    Ok(())
}

fn output_revision(db: &Connection, ns: &str, id: &str) -> Result<Option<i64>> {
    match get(db, ns, id) {
        Ok(r) if r.status == "retracted" => Err(Error::new(
            "not_found",
            "The managed summary was withdrawn.",
        )),
        Ok(r) => Ok(Some(r.revision)),
        Err(error) if error.code == "not_found" => Ok(None),
        Err(error) => Err(error),
    }
}

fn task_metadata(t: &Task) -> Value {
    json!({
        "task_id":t.id,"recipe_id":t.recipe_id,"output_id":t.output_id,
        "processor":t.processor,"max_chars":t.max_chars,"generation":t.generation,
        "state":t.state,"attempts":t.attempts,"claim_count":t.claim_count,
        "next_attempt_at":t.next_attempt_at,"lease_until":t.lease_until,
        "worker":t.worker,"error_code":t.error_code
    })
}

pub(super) fn mutate(db: &Connection, ns: &str, method: &str, v: &Value, at: i64) -> Result<Value> {
    match method {
        "summary_create" => create(db, ns, v, at),
        "maintenance_claim" => claim(db, ns, v, at),
        "maintenance_complete" => complete(db, ns, v, at),
        "maintenance_fail" => fail(db, ns, v, at),
        "maintenance_retry" => retry(db, ns, v, at),
        _ => Err(Error::new(
            "method_not_found",
            "Unknown maintenance mutation.",
        )),
    }
}

fn create(db: &Connection, ns: &str, v: &Value, at: i64) -> Result<Value> {
    let input_values = v
        .get("inputs")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::new("invalid_request", "inputs must be an array."))?;
    if input_values.is_empty() || input_values.len() > MAX_INPUTS {
        return Err(Error::new(
            "invalid_request",
            "Summary recipes require 1 to 64 inputs.",
        ));
    }
    let processor = string(v, "processor")?;
    validate_name(processor, "processor")?;
    let max_chars = integer(v, "max_chars", 2000, 1, 16_384)?;
    let mut seen = HashSet::new();
    let mut inputs = Vec::new();
    for value in input_values {
        let id = value
            .as_str()
            .ok_or_else(|| Error::new("invalid_request", "Input must be a record ID."))?;
        if !seen.insert(id) {
            return Err(Error::new("invalid_request", "Duplicate input ID."));
        }
        let r = get(db, ns, id)?;
        require_valid(db, ns, &r, at)?;
        inputs.push(r);
    }
    check_input_budget(&inputs)?;
    let recipe_id = Uuid::new_v4().to_string();
    let task_id = Uuid::new_v4().to_string();
    let output_id = Uuid::new_v4().to_string();
    db.execute(
        "INSERT INTO summary_recipes VALUES(?1,?2,?3,?4,?5,'active',?6,?6)",
        params![recipe_id, ns, output_id, processor, max_chars, at],
    )?;
    for (ordinal, r) in inputs.iter().enumerate() {
        db.execute(
            "INSERT INTO summary_inputs VALUES(?1,?2,?3)",
            params![recipe_id, r.id, ordinal as i64],
        )?;
    }
    db.execute(
        "INSERT INTO maintenance_tasks(id,recipe_id,namespace,generation,state,attempts,
         claim_count,next_attempt_at,created_at,updated_at)
         VALUES(?1,?2,?3,1,'pending',0,0,?4,?4,?4)",
        params![task_id, recipe_id, ns, at],
    )?;
    Ok(json!({
        "recipe_id":recipe_id,"task_id":task_id,"output_id":output_id,
        "state":"pending","generation":1,"processor":processor,"max_chars":max_chars
    }))
}

fn claim(db: &Connection, ns: &str, v: &Value, at: i64) -> Result<Value> {
    let worker = string(v, "worker")?;
    validate_name(worker, "worker")?;
    let seconds = integer(v, "lease_seconds", 120, 1, 3600)?;
    let mut stmt = db.prepare(
        "SELECT t.id FROM maintenance_tasks t JOIN summary_recipes r ON r.id=t.recipe_id
         WHERE t.namespace=?1 AND r.status='active' AND t.attempts<5
           AND ((t.state='pending' AND t.next_attempt_at<=?2)
             OR (t.state='leased' AND t.lease_until<=?2))
         ORDER BY t.created_at,t.rowid",
    )?;
    let candidates = stmt
        .query_map(params![ns, at], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for id in candidates {
        let t = task(db, ns, &id)?;
        let inputs = match current_inputs(db, ns, &t.recipe_id, at) {
            Ok(inputs) => inputs,
            Err(error) if ["not_found", "stale_dependency"].contains(&error.code.as_str()) => {
                continue;
            }
            Err(error) => return Err(error),
        };
        let revision = match output_revision(db, ns, &t.output_id) {
            Ok(revision) => revision,
            Err(error) if error.code == "not_found" => continue,
            Err(error) => return Err(error),
        };
        let token = Uuid::new_v4().to_string();
        let until = at + seconds;
        db.execute("DELETE FROM maintenance_inputs WHERE task_id=?1", [&id])?;
        for (ordinal, r) in inputs.iter().enumerate() {
            db.execute(
                "INSERT INTO maintenance_inputs VALUES(?1,?2,?3,?4)",
                params![id, r.id, r.revision, ordinal as i64],
            )?;
        }
        db.execute(
            "UPDATE maintenance_tasks SET state='leased',lease_token=?1,lease_until=?2,
             worker=?3,output_revision=?4,claim_count=claim_count+1,updated_at=?5 WHERE id=?6",
            params![token, until, worker, revision, at, id],
        )?;
        let mut receipt = task_metadata(&task(db, ns, &id)?);
        receipt["claimed"] = json!(true);
        receipt["lease_token"] = json!(token);
        receipt["output_revision"] = json!(revision);
        receipt["dependencies"] = json!(inputs
            .iter()
            .map(|r| json!({"id":r.id,"revision":r.revision}))
            .collect::<Vec<_>>());
        return Ok(receipt);
    }
    Ok(json!({"claimed":false}))
}

fn complete(db: &Connection, ns: &str, v: &Value, at: i64) -> Result<Value> {
    let t = task(db, ns, string(v, "task_id")?)?;
    lease_guard(&t, v, at)?;
    let inputs = captured_inputs(db, ns, &t, at)?;
    check_input_budget(&inputs)?;
    let text = body(v)?;
    if text.chars().count() > t.max_chars as usize {
        return Err(Error::new(
            "invalid_request",
            "Summary exceeds the recipe's max_chars budget.",
        ));
    }
    let old_revision = output_revision(db, ns, &t.output_id)?;
    if old_revision != t.output_revision {
        return Err(Error::new(
            "revision_conflict",
            "Managed output changed after the task was claimed.",
        ));
    }
    let revision = match old_revision {
        None => {
            let r = Record {
                id: t.output_id.clone(),
                revision: 1,
                body: text.into(),
                kind: "summary".into(),
                source: t.processor.clone(),
                volatility: "normal".into(),
                status: "active".into(),
                valid_from: None,
                valid_until: None,
                created_at: at,
                updated_at: at,
            };
            insert(db, ns, &r)?;
            1
        }
        Some(previous) => {
            let revision = previous
                .checked_add(1)
                .ok_or_else(|| Error::new("resource_exhausted", "Revision limit reached."))?;
            let affected = descendants(db, &t.output_id)?;
            invalidate(db, ns, &affected, false, at)?;
            db.execute(
                "UPDATE records SET revision=?1,body=?2,status='active',updated_at=?3
                 WHERE namespace=?4 AND id=?5 AND status!='retracted'",
                params![revision, text, at, ns, t.output_id],
            )?;
            db.execute(
                "INSERT INTO revisions VALUES(?1,?2,?3,?4,?5)",
                params![t.output_id, revision, ns, text, at],
            )?;
            db.execute(
                "DELETE FROM record_fts WHERE id=?1 AND namespace=?2",
                params![t.output_id, ns],
            )?;
            db.execute(
                "INSERT INTO record_fts(id,namespace,body) VALUES(?1,?2,?3)",
                params![t.output_id, ns, text],
            )?;
            db.execute("DELETE FROM edges WHERE child_id=?1", [&t.output_id])?;
            revision
        }
    };
    for input in inputs {
        db.execute(
            "INSERT INTO edges VALUES(?1,?2,?3)",
            params![t.output_id, input.id, input.revision],
        )?;
    }
    db.execute(
        "UPDATE jobs SET state='invalidated' WHERE namespace=?1 AND state='prepared'
         AND id IN (SELECT job_id FROM job_inputs WHERE record_id=?2)",
        params![ns, t.output_id],
    )?;
    // Recipes that have not published yet have no output edges to invalidate.
    // The active output itself is excluded by on_invalidate; its consumers are queued.
    on_invalidate(db, ns, std::slice::from_ref(&t.output_id), false, at)?;
    db.execute(
        "UPDATE maintenance_tasks SET state='succeeded',lease_token=NULL,lease_until=NULL,
         worker=NULL,error_code=NULL,updated_at=?1 WHERE id=?2",
        params![at, t.id],
    )?;
    db.execute("DELETE FROM maintenance_inputs WHERE task_id=?1", [&t.id])?;
    Ok(json!({
        "task_id":t.id,"recipe_id":t.recipe_id,"id":t.output_id,"output_id":t.output_id,
        "revision":revision,"generation":t.generation,"state":"succeeded",
        "index_state":"lexical_ready"
    }))
}

fn fail(db: &Connection, ns: &str, v: &Value, at: i64) -> Result<Value> {
    let t = task(db, ns, string(v, "task_id")?)?;
    lease_guard(&t, v, at)?;
    let code = string(v, "error_code")?;
    // Store a machine code, never model output, provider errors, or input text.
    if code.is_empty()
        || code.len() > 64
        || !code
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
    {
        return Err(Error::new(
            "invalid_request",
            "error_code must contain 1 to 64 lowercase ASCII letters, digits, or underscores.",
        ));
    }
    let retryable = v
        .get("retryable")
        .and_then(Value::as_bool)
        .ok_or_else(|| Error::new("invalid_request", "retryable must be a boolean."))?;
    let attempts = t.attempts + 1;
    let state = if retryable && attempts < MAX_FAILURES {
        "pending"
    } else {
        "failed"
    };
    let next = at + (1_i64 << attempts.min(8));
    db.execute(
        "UPDATE maintenance_tasks SET state=?1,attempts=?2,next_attempt_at=?3,error_code=?4,
         lease_token=NULL,lease_until=NULL,worker=NULL,updated_at=?5 WHERE id=?6",
        params![state, attempts, next, code, at, t.id],
    )?;
    db.execute("DELETE FROM maintenance_inputs WHERE task_id=?1", [&t.id])?;
    Ok(
        json!({"task_id":t.id,"generation":t.generation,"state":state,
        "attempts":attempts,"next_attempt_at":next,"error_code":code}),
    )
}

fn retry(db: &Connection, ns: &str, v: &Value, at: i64) -> Result<Value> {
    let t = task(db, ns, string(v, "task_id")?)?;
    if t.recipe_status != "active" || t.state != "failed" {
        return Err(Error::new(
            "task_state_conflict",
            "Only failed active tasks can be retried.",
        ));
    }
    // A withdrawn input/output is terminal, even if an operator retries the task.
    output_revision(db, ns, &t.output_id)?;
    for id in input_ids(db, &t.recipe_id)? {
        if get(db, ns, &id)?.status == "retracted" {
            return Err(Error::new("not_found", "A summary input was withdrawn."));
        }
    }
    db.execute(
        "UPDATE maintenance_tasks SET state='pending',generation=generation+1,attempts=0,
         next_attempt_at=?1,error_code=NULL,updated_at=?1 WHERE id=?2",
        params![at, t.id],
    )?;
    Ok(json!({"task_id":t.id,"state":"pending","generation":t.generation+1}))
}

pub(super) fn read(db: &Connection, ns: &str, method: &str, v: &Value, at: i64) -> Result<Value> {
    match method {
        "maintenance_job" => {
            let t = task(db, ns, string(v, "task_id")?)?;
            lease_guard(&t, v, at)?;
            let inputs = captured_inputs(db, ns, &t, at)?;
            check_input_budget(&inputs)?;
            if output_revision(db, ns, &t.output_id)? != t.output_revision {
                return Err(Error::new(
                    "revision_conflict",
                    "Managed output changed after the task was claimed.",
                ));
            }
            let mut result = task_metadata(&t);
            result["inputs"] = json!(inputs);
            result["trust_boundary"] = json!("retrieved_data");
            result["commit_token"] = json!(commit_token(db)?);
            Ok(result)
        }
        "maintenance_status" => {
            let limit = integer(v, "limit", 100, 1, 500)?;
            let mut stmt = db.prepare(
                "SELECT id FROM maintenance_tasks WHERE namespace=?1 ORDER BY created_at,rowid LIMIT ?2",
            )?;
            let ids = stmt
                .query_map(params![ns, limit], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut tasks = Vec::new();
            for id in ids {
                let t = task(db, ns, &id)?;
                let mut item = task_metadata(&t);
                item["inputs"] = json!(input_ids(db, &t.recipe_id)?);
                item["lease_expired"] =
                    json!(t.state == "leased" && t.lease_until.is_some_and(|end| end <= at));
                tasks.push(item);
            }
            let mut stmt = db.prepare(
                "SELECT state,count(*) FROM maintenance_tasks WHERE namespace=?1 GROUP BY state",
            )?;
            let counts = stmt
                .query_map([ns], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                .collect::<std::result::Result<BTreeMap<_, _>, _>>()?;
            let total: i64 = counts.values().sum();
            Ok(json!({"tasks":tasks,"counts":counts,"total":total,
                "truncated":total>limit,"commit_token":commit_token(db)?}))
        }
        _ => Err(Error::new("method_not_found", "Unknown maintenance read.")),
    }
}

/// Schedule managed dependents, including recipes with no published output yet.
/// The caller supplies changed source IDs as well as invalidated descendants.
pub(super) fn on_invalidate(
    db: &Connection,
    ns: &str,
    ids: &[String],
    erase: bool,
    at: i64,
) -> Result<()> {
    let mut stack = ids.to_vec();
    let mut visited = HashSet::new();
    let mut recipes = HashSet::new();
    while let Some(id) = stack.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let include_self = erase || get(db, ns, &id).is_ok_and(|r| r.status != "active");
        let mut stmt = db.prepare(
            "SELECT r.id,r.output_id FROM summary_recipes r
             WHERE r.namespace=?1 AND r.status='active'
               AND ((?2 AND r.output_id=?3) OR r.id IN
                    (SELECT recipe_id FROM summary_inputs WHERE record_id=?3))",
        )?;
        let affected = stmt
            .query_map(params![ns, include_self, id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (recipe, output) in affected {
            if !recipes.insert(recipe.clone()) {
                continue;
            }
            if recipes.len() > MAX_ANCESTORS {
                return Err(Error::new(
                    "resource_exhausted",
                    "Managed dependency traversal limit exceeded.",
                ));
            }
            let state = if erase { "cancelled" } else { "pending" };
            db.execute(
                "UPDATE summary_recipes SET status=CASE WHEN ?1 THEN 'cancelled' ELSE status END,
                 updated_at=?2 WHERE id=?3",
                params![erase, at, recipe],
            )?;
            db.execute(
                "UPDATE maintenance_tasks SET state=?1,generation=generation+1,attempts=0,
                 next_attempt_at=?2,lease_token=NULL,lease_until=NULL,worker=NULL,
                 output_revision=NULL,error_code=NULL,updated_at=?2 WHERE recipe_id=?3",
                params![state, at, recipe],
            )?;
            db.execute(
                "DELETE FROM maintenance_inputs WHERE task_id IN
                 (SELECT id FROM maintenance_tasks WHERE recipe_id=?1)",
                [&recipe],
            )?;
            // This also covers unpublished recipes and consumers not yet represented in edges.
            db.execute(
                "UPDATE records SET status=?1,body=CASE WHEN ?2 THEN '' ELSE body END,
                 updated_at=?3 WHERE namespace=?4 AND id=?5 AND status!='retracted'",
                params![
                    if erase { "retracted" } else { "stale" },
                    erase,
                    at,
                    ns,
                    output
                ],
            )?;
            db.execute(
                "DELETE FROM record_fts WHERE namespace=?1 AND id=?2",
                params![ns, output],
            )?;
            vector::invalidate(db, ns, std::slice::from_ref(&output))?;
            if erase {
                db.execute(
                    "UPDATE revisions SET body='' WHERE namespace=?1 AND id=?2",
                    params![ns, output],
                )?;
            }
            stack.push(output);
        }
    }
    Ok(())
}
