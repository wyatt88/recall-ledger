//! Deterministic source preservation, not factual or semantic verification.
//! Every selected ancestor remains a version/withdrawal guard. Only complete
//! leaf assertions enter the capsule body; derived prose is never summarized.
use crate::*;

pub(crate) const PROCESSOR: &str = "recall-ledger.evidence/1";
const MAX_NODES: usize = 64;
const MAX_DEPTH: usize = 32;
const DEFAULT_CHARS: i64 = 16_384;

#[derive(Serialize)]
pub(super) struct SourceAssertion {
    #[serde(flatten)]
    record: Record,
    // The exact identity can carry the only stated scope or condition. It is
    // part of the source assertion, not inferred prose or a semantic guarantee.
    // None deliberately serializes as null, rather than omitting this field.
    fact_key: Option<Value>,
}

pub(super) struct Capsule {
    pub roots: Vec<SourceAssertion>,
    pub guards: Vec<Record>,
    pub body: String,
    pub coverage: Value,
}

impl Capsule {
    pub fn references(&self) -> Value {
        json!(self
            .guards
            .iter()
            .map(|r| json!({"id":r.id,"revision":r.revision}))
            .collect::<Vec<_>>())
    }
    pub fn fidelity(&self) -> Value {
        json!({
            "mode":"source_preserving","verified":true,"semantics_verified":false,
            "source_body_policy":"verbatim_complete","recursive_summary":false,
            "canonical_format":PROCESSOR
        })
    }
    fn response(&self, db: &Connection) -> Result<Value> {
        Ok(json!({
            "processor":PROCESSOR,"roots":self.roots,"dependencies":self.references(),
            "body":self.body,"coverage":self.coverage,"fidelity":self.fidelity(),
            "trust_boundary":"retrieved_data","commit_token":commit_token(db)?
        }))
    }
}

pub(super) fn max_chars(v: &Value) -> Result<i64> {
    integer(v, "max_chars", DEFAULT_CHARS, 1, 65_536)
}

fn input_ids(v: &Value) -> Result<Vec<String>> {
    let inputs = v
        .get("inputs")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty() && a.len() <= MAX_INPUTS)
        .ok_or_else(|| Error::new("invalid_request", "Evidence requires 1 to 64 record IDs."))?;
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for item in inputs {
        let id = item
            .as_str()
            .ok_or_else(|| Error::new("invalid_request", "Input must be a record ID."))?;
        validate_name(id, "record ID")?;
        if !seen.insert(id) {
            return Err(Error::new("invalid_request", "Duplicate evidence input."));
        }
        result.push(id.to_owned());
    }
    Ok(result)
}

struct Graph<'a> {
    db: &'a Connection,
    ns: &'a str,
    at: i64,
    records: BTreeMap<String, Record>,
    roots: BTreeMap<String, Record>,
    visiting: HashSet<String>,
    heights: BTreeMap<String, usize>,
    root_bytes: usize,
}

impl Graph<'_> {
    fn visit(&mut self, id: &str, captured: Option<i64>, depth: usize) -> Result<usize> {
        if depth > MAX_DEPTH {
            return Err(Error::new(
                "resource_exhausted",
                "Evidence graph exceeds depth 32.",
            ));
        }
        if self.visiting.contains(id) {
            return Err(Error::new(
                "invalid_dependency",
                "Evidence graph contains a cycle.",
            ));
        }
        let record = if let Some(r) = self.records.get(id) {
            r.clone()
        } else {
            if self.records.len() >= MAX_NODES {
                return Err(Error::new(
                    "resource_exhausted",
                    "Evidence graph exceeds 64 unique records.",
                ));
            }
            let r = get(self.db, self.ns, id)?;
            if r.body.len() > MAX_BODY {
                return Err(Error::new(
                    "resource_exhausted",
                    "Evidence source exceeds UTF-8 byte bounds.",
                ));
            }
            self.records.insert(id.to_owned(), r.clone());
            r
        };
        if record.status == "retracted" {
            return Err(Error::new("not_found", "Evidence source was withdrawn."));
        }
        if record.status != "active"
            || !temporally_valid(&record, self.at)
            || captured.is_some_and(|revision| revision != record.revision)
        {
            return Err(Error::new(
                "stale_dependency",
                "Evidence source is not current and valid.",
            ));
        }
        if let Some(height) = self.heights.get(id) {
            if depth + height > MAX_DEPTH {
                return Err(Error::new(
                    "resource_exhausted",
                    "Evidence graph exceeds depth 32.",
                ));
            }
            return Ok(*height);
        }
        self.visiting.insert(id.to_owned());
        let deps = dependencies(self.db, id)?;
        if deps.len() > MAX_NODES {
            return Err(Error::new(
                "resource_exhausted",
                "Evidence dependency bound exceeded.",
            ));
        }
        let mut height = 0;
        if deps.is_empty() {
            // These are source assertions in this ledger, not a claim that the
            // external event or user statement is true. A kind/source label on a
            // node with edges never makes it a leaf.
            if !["assertion", "preference", "episode", "commitment"].contains(&record.kind.as_str())
            {
                return Err(Error::new(
                    "evidence_sources_missing",
                    "A leaf summary or inference has no verifiable source path.",
                ));
            }
            self.root_bytes = self
                .root_bytes
                .checked_add(record.body.len())
                .filter(|size| *size <= MAX_BODY)
                .ok_or_else(|| {
                    Error::new(
                        "resource_exhausted",
                        "Evidence roots exceed 65536 UTF-8 bytes.",
                    )
                })?;
            self.roots.insert(id.to_owned(), record);
        } else {
            for (parent, revision) in deps {
                height = height.max(1 + self.visit(&parent, Some(revision), depth + 1)?);
            }
        }
        self.visiting.remove(id);
        self.heights.insert(id.to_owned(), height);
        Ok(height)
    }
}

pub(super) fn build(
    db: &Connection,
    ns: &str,
    inputs: &[String],
    max_chars: i64,
    at: i64,
) -> Result<Capsule> {
    if inputs.is_empty() || inputs.len() > MAX_INPUTS || !(1..=65_536).contains(&max_chars) {
        return Err(Error::new(
            "invalid_request",
            "Invalid evidence inputs or character budget.",
        ));
    }
    let mut graph = Graph {
        db,
        ns,
        at,
        records: BTreeMap::new(),
        roots: BTreeMap::new(),
        visiting: HashSet::new(),
        heights: BTreeMap::new(),
        root_bytes: 0,
    };
    let mut seen = HashSet::new();
    for id in inputs {
        if !seen.insert(id) {
            return Err(Error::new("invalid_request", "Duplicate evidence input."));
        }
        graph.visit(id, None, 0)?;
    }
    // Read fact bindings in the same transaction as the graph. Besides body
    // equality, audit/complete compare the canonical structured identity again;
    // no previously rendered source object or caller-supplied key is trusted.
    let roots = graph
        .roots
        .into_values()
        .map(|record| {
            Ok(SourceAssertion {
                fact_key: ingestion::record_fact_key(db, ns, &record.id)?,
                record,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let guards: Vec<_> = graph.records.into_values().collect();
    let refs: Vec<_> = guards
        .iter()
        .map(|r| json!({"id":r.id,"revision":r.revision}))
        .collect();
    let body = json!({
        "schema_version":PROCESSOR,"namespace":ns,"source_assertions":roots,
        "guards":refs,"source_body_policy":"verbatim_complete",
        "semantics_verified":false,"trust_boundary":"retrieved_data"
    })
    .to_string();
    // Both limits apply to the entire canonical capsule, including fact keys
    // and all other metadata, not just the leaf bodies counted below.
    let chars = body.chars().count();
    if body.len() > MAX_BODY || chars > max_chars as usize {
        return Err(Error::new("resource_exhausted","Complete evidence capsule exceeds its character or UTF-8 byte budget; no truncation is allowed."));
    }
    let coverage = json!({
        "status":"complete","root_count":roots.len(),"guard_count":guards.len(),
        "root_utf8_bytes":graph.root_bytes,"output_utf8_bytes":body.len(),
        "output_chars":chars,"max_chars":max_chars,"max_utf8_bytes":MAX_BODY,
        "max_graph_nodes":MAX_NODES,"max_graph_depth":MAX_DEPTH,"truncated":false
    });
    Ok(Capsule {
        roots,
        guards,
        body,
        coverage,
    })
}

pub(super) fn allowed(method: &str) -> Option<&'static [&'static str]> {
    match method {
        "evidence_sources" => Some(&["inputs", "max_chars"]),
        "evidence_create" => Some(&["inputs", "max_chars", "idempotency_key"]),
        "evidence_inspect" => Some(&["id"]),
        _ => None,
    }
}

pub(super) fn create(db: &Connection, ns: &str, v: &Value, at: i64) -> Result<Value> {
    let mut args = v.clone();
    args["processor"] = json!(PROCESSOR);
    args["max_chars"] = json!(max_chars(v)?);
    maintenance::mutate(db, ns, "summary_create", &args, at)
}

pub(super) fn read(db: &Connection, ns: &str, method: &str, v: &Value, at: i64) -> Result<Value> {
    match method {
        "evidence_sources" => build(db, ns, &input_ids(v)?, max_chars(v)?, at)?.response(db),
        "evidence_inspect" => audit(db, ns, string(v, "id")?, at),
        _ => Err(Error::new("method_not_found", "Unknown evidence method.")),
    }
}

fn audit(db: &Connection, ns: &str, id: &str, at: i64) -> Result<Value> {
    let record = get(db, ns, id)?;
    require_valid(db, ns, &record, at)?;
    let recipe: Option<(String,String,i64,String)> = db.query_row(
        "SELECT id,processor,max_chars,status FROM summary_recipes WHERE namespace=?1 AND output_id=?2",
        params![ns,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    ).optional()?;
    let deps = dependencies(db, id)?;
    let mode = if deps.is_empty()
        && ["assertion", "preference", "episode", "commitment"].contains(&record.kind.as_str())
    {
        "source_assertion"
    } else {
        "unchecked_derivation"
    };
    let mut result = json!({
        "id":id,"revision":record.revision,"verified":false,
        "fidelity":{"mode":mode,"verified":false,"semantics_verified":false,
            "reason":"no_local_evidence_recipe"},
        "trust_boundary":"retrieved_data","commit_token":commit_token(db)?
    });
    let Some((recipe, processor, max_chars, status)) = recipe else {
        return Ok(result);
    };
    if processor != PROCESSOR || status != "active" {
        return Ok(result);
    }
    let inputs = db
        .prepare("SELECT record_id FROM summary_inputs WHERE recipe_id=?1 ORDER BY ordinal")?
        .query_map([&recipe], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let capsule = match build(db, ns, &inputs, max_chars, at) {
        Ok(capsule) => capsule,
        Err(error)
            if [
                "resource_exhausted",
                "evidence_sources_missing",
                "invalid_dependency",
            ]
            .contains(&error.code.as_str()) =>
        {
            result["fidelity"]["reason"] = json!(error.code);
            return Ok(result);
        }
        Err(error) => return Err(error),
    };
    let expected: BTreeMap<_, _> = capsule
        .guards
        .iter()
        .map(|r| (r.id.clone(), r.revision))
        .collect();
    if deps.into_iter().collect::<BTreeMap<_, _>>() != expected {
        result["fidelity"]["reason"] = json!("dependency_mismatch");
    } else if record.kind != "summary" || record.source != PROCESSOR || record.body != capsule.body
    {
        result["fidelity"]["reason"] = json!("content_mismatch");
    } else {
        result["verified"] = json!(true);
        result["fidelity"] = capsule.fidelity();
        result["dependencies"] = capsule.references();
        result["body"] = json!(capsule.body);
        result["roots"] = json!(capsule.roots);
        result["coverage"] = capsule.coverage;
    }
    Ok(result)
}
