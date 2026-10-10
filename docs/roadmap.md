# Technical direction and evidence gates

Reviewed 2026-10-10 against v0.2.0,
`86ffb86ec7f49270226144b133222ebcfe5fa22d`.

The target is a local memory framework that remains useful when facts change,
agents hand work to each other, and background processing fails. The project
has not established state-of-the-art performance. This roadmap describes the
work and comparisons needed to earn a narrower, reproducible claim.

**v0.3 implementation update:** candidate intake and exact fact identity,
`changes`, context manifests/validation, consistent backup/new-history restore,
managed Markdown publication/read gates, and Claude Code project hooks are now
implemented. See [the current contract](CONTRACT.md),
[validation](VALIDATION-v0.3.md) and [review](REVIEW-v0.3.md). Hooks are tested
with fixtures and actual local daemon processes; live Claude session acceptance,
other framework backends, and the research proposals below remain open.

**Original review status:** the v0.2 mechanisms were implemented; new interfaces
below were proposals at the time of review. No new behavioral benchmark, host acceptance run, or performance
experiment was executed for this document. A roadmap is not a frozen experimental
protocol or a deployment commitment.

## The technical thesis

Memory has to answer three different questions:

1. Is this record still eligible under its recorded dependencies?
2. Does its evidence apply to the current task and environment?
3. Is providing or rebuilding it worth the task and processing cost?

v0.2 addresses the first question inside its managed store and worker paths.
The next engineering priority is to carry that contract into the host's actual
context. The next application priority is to check the second question for a
narrow class of coding and tool-use memories. The third question needs controlled
experiments before a learned policy is justified.

A Git history can show what changed. A vector index can find related material.
The ledger's current contribution is enforcing what may still be read or
published after a reported change. None of these alone establishes factual
truth or good Agent decisions.

## Strong systems we must compare with

Official method descriptions inspected on 2026-10-10:

| System | Relevant existing capability | Consequence for our claims |
| --- | --- | --- |
| [Letta MemFS](https://docs.letta.com/concepts/memfs) | Git-backed memory, contextual and on-demand files, concurrent background memory work | File-based memory is not inherently unversioned or passive |
| [Letta shared repositories](https://docs.letta.com/agent-sdk/repositories) | History and content-hash update preconditions; this SDK path requires the cloud backend | Version checks and shared memory are not unique to RecallLedger |
| [Mem0](https://github.com/mem0ai/mem0) | Its April 2026 method description uses additive extraction, entity linking, and multiple retrieval signals | Compare total cost with a strong accumulation/retrieval strategy, not just repeated summarization |
| [Mem0 update](https://docs.mem0.ai/core-concepts/memory-operations/update) | Explicit edits update memory and indexes, including in OSS | ADD-only ingestion must not be misrepresented as an inability to correct records |
| [Graphiti and Zep](https://help.getzep.com/zep-vs-graphiti) | Temporal graph extraction, invalidation, and hybrid retrieval; OSS and managed services differ | Time fields, graphs, and invalidation alone are not novel |
| [Zep facts](https://help.getzep.com/facts) | Distinguishes when a fact is learned from when it applies | Temporal metadata does not establish that a claim is true |

These are floating documentation snapshots, not installed-system audits or a
leaderboard. Pin implementations before comparisons. Do not transfer proprietary
cloud scores or governance features to an OSS baseline. In particular, the Mem0
README attributes its reported scores to its managed platform; we do not use
those numbers as a head-to-head result.

The [paper mapping](research.md) provides separate, fixed-version method
references. Neither that mapping nor the product survey is an exhaustive
novelty search.

## Priority 1: make corrections reach the host

**Deliverable:** one fully traced host adapter, one controlled Markdown
projection, then a second host sharing the same project memory.

The proposed components are:

- A durable change outbox committed with record mutations. Consumers resume by
  cursor; an expired cursor requires a full resynchronization.
- A context package carrying IDs, revisions, scope, budget, and a dependency
  manifest. Record the conservative set of inputs delivered to processing, not
  just the citations a model chooses to mention.
- A host capability declaration: per-request replacement, startup-only loading,
  controlled file projection, or tools-only access.
- Versioned projections with freshness checks and conflict detection.
  [The native memory design](native-memory.md) specifies cutover and rollback.

Possible interfaces include `compile_context`, `validate_context`, and
`changes(after_cursor)`. **These names are proposals, not callable v0.2 methods.**
Compilation must capture a coherent set of dependencies; a returned commit lower
bound alone is not a snapshot across several reads or a model call.

Test the complete trace: A writes, B reads, A corrects/retracts, B restarts or
compacts, and B obtains a refreshed package. An offline B must report that it has
not synchronized. A change acknowledgement proves adapter progress, not that a
model forgot text it already received.

The first adapter must also verify the host's actual MCP protocol and hook
behavior. v0.2 targets `2025-11-25`; a tools listing is not cross-host acceptance.
Do not label every MCP-compatible application as a supported managed-memory host.

**Strongest counterexample:** one native host can already provide adequate
versioning and shared state. If an external service adds only synchronization
cost, use the native backend or the embedded Rust library.

## Priority 2: check when experience can be reused

**Deliverable:** a coding/tool-memory adapter with machine-checkable
preconditions, initially file hashes, dependency-lock hashes, and tool versions.

“The tests passed last week” is an observation about a particular execution.
It is not evidence that today's checkout passes. A valid ledger revision can
still describe an inapplicable environment.

Keep observations, assertions, and proposed procedures distinct. Preserve the
command, verified result, relevant environment evidence, observation time, and
checker version. An applicability check should return `applicable`, `mismatch`,
or `unknown`, with the conditions it actually checked. A missing observation
must not silently count as a match.

An entire Git commit changing need not invalidate every experience. An unchanged
commit also says nothing about uncommitted files, remote state, or changed
permissions. Start with explicit relevant dependencies and measure both missed
changes and unnecessary rejection.

Possible interfaces are `capture_observation` and `check_applicability`;
both remain proposals. The host collects evidence under its existing
authorization. The memory core does not gain arbitrary tool execution rights.
Dependency eligibility, environmental applicability, semantic fidelity, and
permission to act remain separate checks.

Alternative support sets may later help structured assertions survive withdrawal
of one source. They must not be used to retain arbitrary free-text summaries
merely because another source sounds similar, or to bypass a user's withdrawal.

**Strongest counterexample:** rereading the current environment may be cheaper
and safer than maintaining these checks. Stable preferences may not need them
at all. Compare against fresh tool reads before adding complexity.

## Priority 3: test whether compression is worth maintaining

**Research hypothesis:** grouping inputs using past changes and semantic
relationships before summarization can reduce future total maintenance cost
without an unacceptable loss of task quality.

The candidate objective includes:

```text
task loss
+ initial processing
+ repeated reads and context construction
+ future repair, failed work, and retries
+ loss of useful availability while results are withdrawn
```

Report the individual quantities and quality–cost curves. A weighted sum only
has meaning after its units and weights have been specified.

Compare raw records with no persistent summaries, fixed-size groups, time/topic
groups, matched-size random groups, and a policy using past change information.
Use the same retrieval, reader, summarizer, repair rules, and total resource
budget. Include no-change, correlated-change, all-sources-change, and cross-group
reasoning cases. Future test changes must not determine the groups.

Count initial calls, query embeddings, summaries, maintenance, cancelled work,
failures, and retries. Count shared rebuild work once per actual operation.
Fewer invalidated records alone is not a cost result. Returning nothing is not
a quality result.

The [existing partition example](research.md#executable-mechanism-example)
only demonstrates dependency coverage. SISA, truth maintenance, incremental
computation, and MemoRepair already supply related principles. The proposed
quality/repair trade-off is an open question, not a claim of first invention.

Defer learned write/read policies and sophisticated repair optimization until a
replayable trace and strong deterministic baselines exist. An eager repair policy,
a demand-driven policy with current-source fallback, and no summaries are useful
first comparisons. The fixed-graph closure formulation discussed in the
[research notes](research.md#future-algorithm-candidates) does not solve the whole
dynamic memory policy.

**Strongest counterexample:** raw structured facts or additive retrieval may
cost less and preserve more detail. Correlated changes can eliminate the benefit
of partitioning. Keep the simpler policy if the hypothesis fails.

## Delivery gates

Engineering work and evaluation preparation can overlap. Each gate needs named
implementation and review owners, a fixed version, and inspectable artifacts.
These are acceptance requirements; they have not been passed by writing them.

| Gate | Deliverable | Required evidence / stopping condition |
| --- | --- | --- |
| G0 — definition | Baseline versions, task/data split, hardware/model lock, budget, frozen metrics and review | Stop comparative claims if strong baselines, fair budgets, or independent test data are missing |
| G1 — controlled integration | One native host and managed MD path; later a second sharing host | Trace all declared writes, reads, corrections, compaction and restart paths; disclose any uncontrolled cache or duplicate writer |
| G2 — useful current memory | Development tasks, source fidelity checks, applicability checks, end-to-end graders | A correct store must still supply useful evidence; reject tests that all systems trivially pass or that rely on Agent self-scoring |
| G3 — method validation | Held-out tasks, strong baselines, cost accounting, critical ablations | Meet frozen quality/coverage requirements and a predeclared benefit; abandon a policy whose gain disappears against a strong simple baseline |
| G4 — operating envelope | Capacity, mixed load, recovery, upgrade/migration, and host compatibility records | Bound tail latency, resource growth and repair backlog; unsupported sizes/hosts remain explicitly unsupported |
| G5 — reproducibility | Runnable versions/configs, legal data access, traces, scoring scripts, failures | Independent rerun within declared tolerances; report scope and negative results, not a universal rank |

### Baselines and attribution

Use two tracks. A **controlled mechanism track** keeps the Agent, models, input
events, tools, and budget fixed. A **complete system track** allows sensible
native strategies within comparable resource limits. Do not attribute different
reader models or extra background inference to a memory algorithm.

Required baseline families:

- Native Markdown with search, version metadata, concurrency protection and
  lifecycle hooks.
- A framework-native persistent store with its supported retrieval and updates.
- Strong lexical/vector retrieval with current-version and deletion filtering.
- At least one current memory framework that can be run under the stated
  deployment constraints. Unavailable cloud-only configurations belong in a
  separate track, not the losing column.
- A native/simple backend augmented with the same correction and repair contract,
  to distinguish algorithm benefits from missing baseline correctness.

Provide every system the same observed corrections and withdrawals. Split by
complete project/user and time, not random individual questions from the same
history. Keep hidden answers, future events and evaluation tools out of memory.
Use fixed public benchmarks such as LongMemEval for diagnosis alongside new,
held-out change trajectories; public benchmark scores alone are insufficient.

### Metrics that must travel together

| Question | Measure |
| --- | --- |
| Did a withdrawal protect later managed reads? | Ineligible current-content exposure in reads started after the committed change; late publication and resurrection events |
| Did the Agent complete useful work? | End-to-end task success, sufficient-evidence coverage, stale/irrelevant-memory misuse, unnecessary rejection |
| How long did correction hurt availability? | Time to valid evidence, time to repaired/indexed summary, unavailable-task time over the change trace |
| Was it affordable? | Total processing and every failed attempt; cost per valid successful task; CPU/GPU time, memory and disk growth |
| Was it responsive? | Store-only and end-to-end p50/p95/p99 separately, queue time, errors and useful throughput |

Zero observed leakage is not proof of zero risk. Always report exposure counts,
workload coverage and uncertainty. Timeouts and empty results remain in the
success denominator. Explicitly requested historical answers are distinct from
mistaken use of superseded facts as current; history must never bypass a
retraction. Previously delivered or in-flight host context is measured separately
from reads started after the change.

Candidate development targets are a task-success noninferiority margin of one
percentage point and a 20% reduction in total cost per successful task on
preselected changing workloads. These are **unfrozen hypotheses, not measured
results or SLO promises**. Choose final thresholds and sample size from task value,
development variance and independent review before opening the held-out set.
Report paired uncertainty at the project/user level, including model variance.
If the candidate thresholds are adopted, freeze an interval decision rule:
the task-success difference's lower bound must be at least -0.01; a claim of
at least 20% savings requires the cost-ratio upper bound to be at most 0.80,
not merely a favorable point estimate. Fix confidence levels and any multiple
comparison correction in G0.
Safety failures cannot be offset by average gains on easier tasks.

### Scale, reliability, and boundaries

Start with measured small workloads, then consider 1K, 10K, 100K and 1M records
at declared dimensions, history depth, dependency fan-out, and concurrent load.
These are candidate experiments, not current capacity claims. v0.2 uses exact
vector scans. Profile batch reads, dependency traversal and indexing before
choosing ANN; any approximate index must preserve eligibility and withdrawal.

Use enough requests and independent process runs for tail estimates. Include
queueing, cold starts, repair storms, provider failures, disk pressure, and
restart during mutation. Keep errors and timeouts visible rather than dropping
them from latency results. Backups, clone identity, restore and upgrades need
their own tested protocols.

Current namespaces support trusted local scoping, not hostile multi-tenant
isolation. A separate authorization/OS-isolation design is required before
claiming the latter. Test adversarial memory text and benign lookalikes against
the host's data/instruction boundary; declaring `retrieved_data` is not a proof
that a model will respect it.

## When a leading-framework claim would be justified

Only after the relevant gates pass, report a claim scoped to specific models,
hosts, workloads, hardware and budgets, with effect sizes and uncertainty. A
useful result might be lower stale-memory misuse at comparable task quality and
total cost on frequently corrected, cross-host work.

If native memory matches that result with less operating complexity, document
it and narrow the project. The goal is a dependable and useful memory system;
feature count and an external daemon do not establish superiority.
