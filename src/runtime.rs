//! Exact-source evidence repair, optional model execution and bounded maintenance.

use crate::{
    integer, models::Config, optional_string, string, validate_name, Error, Result, Store,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const EVIDENCE_PROCESSOR: &str = "recall-ledger.evidence/1";

/// Model calls happen between Store transactions, never inside a SQLite writer.
/// A daemon owns one maintenance loop; each client has its own Runtime.
pub struct Runtime {
    store: Store,
    models: Option<Config>,
    expected_binding: Option<Value>,
}

impl Runtime {
    pub fn new(store: Store, models: Option<Config>) -> Result<Self> {
        if let Some(config) = &models {
            config.validate()?;
        }
        Ok(Self {
            store,
            models,
            expected_binding: None,
        })
    }

    pub fn execute(&mut self, namespace: &str, method: &str, params: Value) -> Result<Value> {
        validate_name(namespace, "namespace")?;
        match method {
            "recall" => self.recall(namespace, params),
            "summarize" => {
                fields(&params, &["inputs", "max_chars", "idempotency_key"])?;
                let config = self.config()?;
                let processor = config.summary_key()?;
                let mut args = params;
                args["processor"] = json!(processor);
                let mut result = self.store_execute(namespace, "summary_create", args)?;
                result["receipt_method"] = json!("summary_create");
                Ok(result)
            }
            "consolidate" => {
                fields(&params, &["inputs", "max_chars", "idempotency_key"])?;
                let mut result = self.store_execute(namespace, "evidence_create", params)?;
                result["receipt_method"] = json!("evidence_create");
                Ok(result)
            }
            "maintain" => self.maintain(namespace, params),
            "maintenance_status" => self.maintenance_status(namespace, params),
            "status" => {
                let mut result = self.store_execute(namespace, method, params)?;
                let config = self.models.clone().unwrap_or_default();
                result["capabilities"]["model_calls"] =
                    json!(config.embedding.is_some() || config.summary.is_some());
                result["capabilities"]["strict_evidence"] = json!(true);
                result["consolidation"] = json!({
                    "default_policy":"evidence","evidence_processor":EVIDENCE_PROCESSOR,
                    "evidence_model_required":false,"evidence_fidelity":"source_exact",
                    "model_fidelity":"unchecked","semantic_truth_verified":false
                });
                result["models"] = json!({
                    "embedding_configured":config.embedding.is_some(),
                    "summary_configured":config.summary.is_some(),
                    "embedding_key":config.embedding_key().ok(),
                    "summary_key":config.summary_key().ok(),
                    "health_checked":false,
                    "revision_attestation":"operator_declared"
                });
                if let Ok(key) = config.embedding_key() {
                    let mut progress = self.store_execute(
                        namespace,
                        "embedding_status",
                        json!({"model_key":key}),
                    )?;
                    progress.as_object_mut().unwrap().remove("items");
                    result["embedding"] = progress;
                }
                Ok(result)
            }
            "embedding_status" | "embedding_retry" => {
                let mut args = params;
                if args.is_object() && args.get("model_key").is_none() {
                    args["model_key"] = json!(self.config()?.embedding_key()?);
                }
                self.store_execute(namespace, method, args)
            }
            "operation" if params["method"] == "summarize" => {
                let mut args = params;
                args["method"] = json!("summary_create");
                self.store_execute(namespace, method, args)
            }
            "operation" if params["method"] == "consolidate" => {
                let mut args = params;
                args["method"] = json!("evidence_create");
                self.store_execute(namespace, method, args)
            }
            _ => self.store_execute(namespace, method, params),
        }
    }

    /// Pin every Store transaction in this runtime operation, including reads
    /// after model inference. Checking a separate preflight connection is not
    /// sufficient when a daemon socket has been rebound to another destination.
    pub fn execute_bound(
        &mut self,
        namespace: &str,
        method: &str,
        params: Value,
        expected_binding: Option<&Value>,
    ) -> Result<Value> {
        if let Some(binding) = expected_binding {
            crate::validate_binding(binding)?;
        }
        let previous = self.expected_binding.take();
        self.expected_binding = expected_binding.cloned();
        let result = (|| {
            if self.expected_binding.is_some() {
                // Reject before model work as well as before actual Store use.
                self.store_execute(namespace, "binding", json!({}))?;
            }
            self.execute(namespace, method, params)
        })();
        self.expected_binding = previous;
        result
    }

    fn store_execute(&mut self, namespace: &str, method: &str, params: Value) -> Result<Value> {
        self.store
            .execute_bound(namespace, method, params, self.expected_binding.as_ref())
    }

    fn config(&self) -> Result<Config> {
        self.models.clone().ok_or_else(|| {
            Error::new(
                "model_not_configured",
                "Configure a local model provider before using this operation.",
            )
        })
    }

    fn recall(&mut self, namespace: &str, mut params: Value) -> Result<Value> {
        fields(
            &params,
            &[
                "query",
                "limit",
                "max_chars",
                "minimum_commit",
                "mode",
                "model_key",
                "query_vector",
            ],
        )?;
        // The embedded/raw-vector API remains useful for other encoders. Such
        // calls never silently select a configured, potentially different model.
        if params.get("query_vector").is_some() {
            return self.store_execute(namespace, "recall", params);
        }
        if params.get("model_key").is_some() {
            return Err(Error::new(
                "invalid_request",
                "Supply model_key with query_vector, or omit both for configured retrieval.",
            ));
        }
        let configured = self
            .models
            .as_ref()
            .is_some_and(|config| config.embedding.is_some());
        let query = optional_string(&params, "query", "")?.to_owned();
        let default_mode = if configured && !query.trim().is_empty() {
            "hybrid"
        } else {
            "lexical"
        };
        let mode = optional_string(&params, "mode", default_mode)?.to_owned();
        if !["lexical", "vector", "hybrid"].contains(&mode.as_str()) {
            return Err(Error::new("invalid_request", "Unknown retrieval mode."));
        }
        params["mode"] = json!(mode);
        if mode == "lexical" {
            return self.store_execute(namespace, "recall", params);
        }
        if query.trim().is_empty() {
            return Err(Error::new(
                "invalid_request",
                "Configured semantic retrieval requires a nonempty query.",
            ));
        }
        // Validate scope, consistency token and budgets before spending model
        // work. Re-read after inference; this snapshot is never returned later.
        let mut lexical = params.clone();
        lexical["mode"] = json!("lexical");
        self.store_execute(namespace, "recall", lexical.clone())?;
        let config = self.config()?;
        let key = config.embedding_key()?;
        match config.embed(&query, "query") {
            Ok(vector) => {
                params["model_key"] = json!(key);
                params["query_vector"] = json!(vector);
                self.store_execute(namespace, "recall", params)
            }
            Err(error) if mode == "hybrid" => {
                let mut result = self.store_execute(namespace, "recall", lexical)?;
                result["coverage"]["requested_mode"] = json!("hybrid");
                result["coverage"]["degraded"] = json!(true);
                result["coverage"]["reason"] = json!(error.code);
                result["coverage"]["configured_model_key"] = json!(key);
                Ok(result)
            }
            Err(error) => Err(error),
        }
    }

    fn maintain(&mut self, namespace: &str, params: Value) -> Result<Value> {
        fields(
            &params,
            &[
                "max_embeddings",
                "max_summaries",
                "auto_summary_min",
                "strategy",
                "max_group_chars",
                "max_summary_chars",
                "summary_policy",
            ],
        )?;
        let max_embeddings = integer(&params, "max_embeddings", 4, 0, 32)? as usize;
        let max_summaries = integer(&params, "max_summaries", 1, 0, 8)? as usize;
        let auto_min = integer(&params, "auto_summary_min", 4, 0, 64)? as usize;
        let max_group_chars = integer(&params, "max_group_chars", 12_000, 1, 16_000)?;
        let policy = optional_string(&params, "summary_policy", "evidence")?;
        if !["evidence", "model"].contains(&policy) {
            return Err(Error::new("invalid_request", "Unknown summary policy."));
        }
        let (default_chars, maximum_chars) = if policy == "evidence" {
            (16_384, 65_536)
        } else {
            (2000, 16_384)
        };
        let max_summary_chars = integer(
            &params,
            "max_summary_chars",
            default_chars,
            1,
            maximum_chars,
        )?;
        let strategy = optional_string(&params, "strategy", "fixed")?;
        if !["individual", "fixed", "topic", "volatility"].contains(&strategy) {
            return Err(Error::new("invalid_request", "Unknown grouping strategy."));
        }
        let config = self.models.clone().unwrap_or_default();
        let mut actions = Vec::new();
        let mut errors = Vec::new();
        let mut evidence_processed = 0;
        let mut model_processed = 0;
        // Cap automatic discovery independently of inference, including when
        // callers choose max_summaries=0 to enqueue only.
        if auto_min > 0 && (policy == "evidence" || config.summary.is_some()) {
            let processor = if policy == "evidence" {
                EVIDENCE_PROCESSOR.to_owned()
            } else {
                config.summary_key()?
            };
            let creation_method = if policy == "evidence" {
                "evidence_create"
            } else {
                "summary_create"
            };
            let plan = self.store_execute(
                namespace,
                "plan",
                json!({"strategy":strategy,"max_group_chars":max_group_chars,"unmanaged_only":true}),
            )?;
            for group in plan["groups"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|group| {
                    group["inputs"]
                        .as_array()
                        .is_some_and(|ids| ids.len() >= auto_min)
                })
                .take(8)
            {
                let mut inputs = group["inputs"].as_array().cloned().unwrap_or_default();
                inputs.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
                let fingerprint = json!([inputs, processor, max_summary_chars]);
                let key = format!(
                    "auto-summary-{:x}",
                    Sha256::digest(fingerprint.to_string().as_bytes())
                );
                let mut arguments = json!({
                    "inputs":inputs,"max_chars":max_summary_chars,"idempotency_key":key
                });
                if policy == "model" {
                    arguments["processor"] = json!(processor);
                }
                match self.store_execute(namespace, creation_method, arguments) {
                    Ok(receipt) => actions.push(json!({
                        "kind":if policy=="evidence" {"evidence_queued"} else {"summary_queued"},
                        "fidelity_contract":if policy=="evidence" {"source_exact"} else {"unchecked"},
                        "receipt":receipt
                    })),
                    Err(error) => errors.push(json!({
                        "kind":if policy=="evidence" {"evidence_queue"} else {"summary_queue"},
                        "code":error.code
                    })),
                }
            }
        } else if auto_min > 0 && policy == "model" {
            errors.push(json!({"kind":"summary_queue","code":"model_not_configured"}));
        }
        // Repairs precede indexing so their newly published revisions can be
        // embedded in this tick. Strict tasks never require a model provider.
        // A processor filter prevents an incompatible worker claiming a task
        // and turning merely unavailable configuration into a failed recipe.
        if max_summaries > 0 {
            evidence_processed = self.process_queue(
                namespace,
                EVIDENCE_PROCESSOR,
                None,
                max_summaries,
                &mut actions,
                &mut errors,
            )?;
        }
        if let Some(summary) = &config.summary {
            let processor = config.summary_key()?;
            if evidence_processed < max_summaries {
                model_processed = self.process_queue(
                    namespace,
                    &processor,
                    Some((&config, summary.timeout_seconds + 30)),
                    max_summaries - evidence_processed,
                    &mut actions,
                    &mut errors,
                )?;
            }
        }
        if config.embedding.is_some() && max_embeddings > 0 {
            let key = config.embedding_key()?;
            let candidates = self.store_execute(
                namespace,
                "embedding_candidates",
                json!({"model_key":key,"limit":max_embeddings}),
            )?;
            for item in candidates["items"].as_array().into_iter().flatten() {
                let id = string(item, "id")?;
                let revision = item["revision"].clone();
                let attempt = Uuid::new_v4().to_string();
                match config.embed(string(item, "body")?, "document") {
                    Ok(vector) => match self.store_execute(
                        namespace,
                        "embedding_set",
                        json!({"id":id,"expected_revision":revision,"model_key":key,
                            "vector":vector,"idempotency_key":attempt}),
                    ) {
                        Ok(receipt) => actions.push(json!({"kind":"embedded","receipt":receipt})),
                        Err(error) => {
                            // A provider can change dimensions even when its
                            // declared revision has not changed. Persist that
                            // failure too, otherwise every tick would infer the
                            // same unpublishable vector without backoff.
                            let recorded = if ["dimension_mismatch", "invalid_vector"]
                                .contains(&error.code.as_str())
                            {
                                self.store_execute(
                                    namespace,
                                    "embedding_fail",
                                    json!({"id":id,"expected_revision":revision,"model_key":key,
                                        "error_code":error.code,"idempotency_key":format!("fail-{attempt}")}),
                                ).is_ok()
                            } else {
                                false
                            };
                            errors
                                .push(json!({"kind":"embedding_commit","id":id,"code":error.code,
                                "failure_recorded":recorded}));
                        }
                    },
                    Err(error) => {
                        let recorded = self.store_execute(
                            namespace,
                            "embedding_fail",
                            json!({"id":id,"expected_revision":revision,"model_key":key,
                                "error_code":error.code,"idempotency_key":attempt}),
                        );
                        errors.push(json!({"kind":"embedding","id":id,"code":error.code,
                            "failure_recorded":recorded.is_ok()}));
                    }
                }
            }
        }
        Ok(json!({
            "actions":actions,"errors":errors,
            "summary_policy":policy,"max_summary_chars":max_summary_chars,
            "processed":{"evidence":evidence_processed,"model":model_processed},
            "maintenance":self.maintenance_status(namespace,json!({"limit":100}))?,
            "status":self.execute(namespace,"status",json!({}))?
        }))
    }

    fn process_queue(
        &mut self,
        namespace: &str,
        processor: &str,
        model: Option<(&Config, u64)>,
        maximum: usize,
        actions: &mut Vec<Value>,
        errors: &mut Vec<Value>,
    ) -> Result<usize> {
        let strict = processor == EVIDENCE_PROCESSOR;
        let worker = format!("local-{}", Uuid::new_v4());
        let mut processed = 0;
        for _ in 0..maximum {
            let claim = self.store_execute(
                namespace,
                "maintenance_claim",
                json!({
                    "worker":worker,"processor":processor,
                    "lease_seconds":model.map_or(60, |(_, lease)| lease),
                    "idempotency_key":Uuid::new_v4().to_string()
                }),
            )?;
            let failed = claim["failed_tasks"].as_u64().unwrap_or(0);
            let deferred = claim["deferred_tasks"].as_u64().unwrap_or(0);
            if failed > 0 || deferred > 0 {
                // Preserve bounded queue changes even when no lease was
                // obtained. The paginated status may omit the affected rows.
                actions.push(json!({
                    "kind":"maintenance_queue_scan","processor":processor,
                    "failed_tasks":failed,"deferred_tasks":deferred
                }));
            }
            if claim["claimed"] != true {
                break;
            }
            processed += 1;
            let task_id = string(&claim, "task_id")?.to_owned();
            let lease = string(&claim, "lease_token")?.to_owned();
            let outcome = if strict {
                self.run_evidence(namespace, &task_id, &lease)
            } else {
                self.run_summary(
                    namespace,
                    model
                        .ok_or_else(|| Error::new("model_not_configured", "No matching model."))?
                        .0,
                    processor,
                    &task_id,
                    &lease,
                )
            };
            match outcome {
                Ok(receipt) => actions.push(json!({
                    "kind":if strict {"evidence_published"} else {"summary_published"},
                    "fidelity_contract":if strict {"source_exact"} else {"unchecked"},
                    "receipt":receipt
                })),
                Err(error) => {
                    // A concurrent invalidation can revoke this lease. Failure
                    // reporting is fenced and cannot damage a newer generation.
                    let reported = self.store_execute(
                        namespace,
                        "maintenance_fail",
                        json!({
                            "task_id":task_id,"lease_token":lease,"error_code":error.code,
                            "retryable":error.retryable,"idempotency_key":format!("fail-{lease}")
                        }),
                    );
                    errors.push(json!({
                        "kind":if strict {"evidence"} else {"summary"},
                        "task_id":task_id,"code":error.code,"failure_recorded":reported.is_ok()
                    }));
                }
            }
        }
        Ok(processed)
    }

    fn maintenance_status(&mut self, namespace: &str, params: Value) -> Result<Value> {
        let mut result = self.store_execute(namespace, "maintenance_status", params)?;
        let model_key = self
            .models
            .as_ref()
            .and_then(|config| config.summary_key().ok());
        for task in result["tasks"].as_array_mut().into_iter().flatten() {
            let processor = task["processor"].as_str().unwrap_or("");
            let strict = processor == EVIDENCE_PROCESSOR;
            let available = strict || model_key.as_deref() == Some(processor);
            task["runtime_processor_available"] = json!(available);
            task["fidelity_contract"] = json!(if strict { "source_exact" } else { "unchecked" });
            if !available && ["pending", "leased"].contains(&task["state"].as_str().unwrap_or("")) {
                task["waiting_reason"] = json!("matching_model_configuration_required");
            }
        }
        Ok(result)
    }

    fn run_evidence(&mut self, namespace: &str, task_id: &str, lease: &str) -> Result<Value> {
        let job = self.store_execute(
            namespace,
            "maintenance_job",
            json!({"task_id":task_id,"lease_token":lease}),
        )?;
        if job["processor"] != EVIDENCE_PROCESSOR {
            return Err(Error::new(
                "processor_mismatch",
                "Expected exact-source evidence task.",
            ));
        }
        let body = string(&job, "evidence_body")?;
        // The Store reconstructs and compares the complete capsule again in its
        // publication transaction. No summarizer, truncation or rewrite occurs.
        self.store_execute(
            namespace,
            "maintenance_complete",
            json!({
                "task_id":task_id,"lease_token":lease,"body":body,
                "idempotency_key":format!("complete-{lease}")
            }),
        )
    }

    fn run_summary(
        &mut self,
        namespace: &str,
        config: &Config,
        processor: &str,
        task_id: &str,
        lease: &str,
    ) -> Result<Value> {
        let job = self.store_execute(
            namespace,
            "maintenance_job",
            json!({"task_id":task_id,"lease_token":lease}),
        )?;
        if job["processor"] != processor {
            return Err(Error::new(
                "model_revision_mismatch",
                "Recipe is pinned to a different summary configuration; restore it before retrying.",
            ));
        }
        let inputs = job["inputs"]
            .as_array()
            .ok_or_else(|| Error::new("database_error", "Invalid maintenance inputs."))?
            .iter()
            .map(|input| {
                let text = string(input, "body")?;
                // A qualified fact may store every limitation outside its body.
                // Keep those fields in the untrusted text envelope. Config's
                // combined byte/request limits cover the complete encoding.
                let text = if input.get("fact_key").is_some_and(|key| !key.is_null()) {
                    json!({
                        "body":text,"fact_key":input["fact_key"],
                        "kind":input["kind"],"source":input["source"],
                        "valid_from":input["valid_from"],"valid_until":input["valid_until"]
                    })
                    .to_string()
                } else {
                    text.to_owned()
                };
                Ok((string(input, "id")?.to_owned(), text))
            })
            .collect::<Result<Vec<_>>>()?;
        let max_chars = integer(&job, "max_chars", 2000, 1, 16_384)? as usize;
        let body = config.summarize(&inputs, max_chars)?;
        self.store_execute(
            namespace,
            "maintenance_complete",
            json!({"task_id":task_id,"lease_token":lease,"body":body,
                "idempotency_key":format!("complete-{lease}")}),
        )
    }
}

fn fields(params: &Value, allowed: &[&str]) -> Result<()> {
    let args = params
        .as_object()
        .ok_or_else(|| Error::new("invalid_request", "Parameters must be an object."))?;
    if args.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(Error::new("invalid_request", "Unknown parameter."));
    }
    Ok(())
}
