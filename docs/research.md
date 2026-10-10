# Research basis and open questions

Reference snapshot: 2026-10-10. This document describes relevant methods and the
limited implementation choices they inform. It is not a systematic literature
review, a replication report, or a claim that the cited authors endorse this
project. Recent arXiv papers may be preprints.

## Six technical references

| Fixed paper version | Relevant idea | RecallLedger 0.2 status |
| --- | --- | --- |
| [MemGPT: Towards LLMs as Operating Systems, v2](https://arxiv.org/abs/2310.08560v2), first submitted 2023-10-12 | Separate finite working context from persistent storage; retrieve through explicit operations | Local persistent records and bounded recall. Automated tier management and paging policy are not reproduced. |
| [Hindsight is 20/20: Building Agent Memory that Retains, Recalls, and Reflects, v1](https://arxiv.org/abs/2512.12818v1), 2025-12-14 | Typed memory and multiple retrieval routes | Explicit kinds, temporal validity, local embeddings, and lexical/cosine RRF. Graph expansion and reflection are future work. This does not reproduce the paper's retrieval system. |
| [MEMOREPAIR: Barrier-First Cascade Repair in Agentic Memory, v1](https://arxiv.org/abs/2605.07242v1), 2026-05-08 | Withdraw affected derivations before publishing repaired successors | Input revisions, transitive withdrawal, durable managed-summary repair, and conditional publication are engineering adaptations. Repair optimization and the paper's experiments are not implemented. |
| [MemTrace: State-Consistent Memory for Long-Horizon Coding Agents, v2](https://arxiv.org/abs/2610.04838v2), first submitted 2026-10-04; v2 2026-10-08 | Check whether execution evidence still applies to the current code state | Record revision validation is present. Repository state capture and coding-specific applicability checks are future work. |
| [LongMemEval: Benchmarking Chat Assistants on Long-Term Interactive Memory, v2](https://arxiv.org/abs/2410.10813v2), first submitted 2024-10-14 | Separate extraction, cross-session reasoning, updating, temporal reasoning, and abstention | Guides a future behavioral evaluation. The project has not run this benchmark or bundled its dataset. |
| [MemCalib: Benchmarking and Optimizing Memory Use in LLM Agents, v3](https://arxiv.org/abs/2609.24259v3), first submitted 2026-09-21; v3 2026-10-08 | Distinguish memory underuse from inappropriate overuse | Guides future use-quality metrics. The project has not implemented its training method or reproduced its results. |

Relevant reading included MemGPT §2.1, Hindsight §3.1–3.4 and §4.2.3,
MemoRepair §2.1–2.4, MemTrace §3.1–3.3, LongMemEval's task and evaluation
definitions, and MemCalib's memory-use evaluation and optimization methods.
Method review is not reproduction.

Database transactions, optimistic concurrency, dependency graphs, and idempotency
also have substantial prior systems literature. The project does not claim to
have invented these mechanisms.

## What the implementation claims

The implementation makes a limited, testable contract: a managed derived result
must not be published or served using recorded dependencies that the store knows
are no longer eligible. This depends on complete input reporting within the
managed path.

It does not claim that a recalled assertion is true, that a model follows the
memory correctly, that a source's real-world state has not changed without being
reported, or that all forms of model memory can be revoked.

The protocol is a combination chosen for this project. There is no performance
or originality claim based solely on combining published ideas.

## Candidate research question: group before compressing

Can grouping inputs before summarization reduce future repair cost without
hurting task performance?

A mixed summary of stable preferences and frequently changing plans may require
rebuilding after every plan update. Separating the inputs could reduce that
work, but may also lose useful relationships and increase retrieval overhead.
This is a hypothesis, not an observed advantage.

For a fixed processor and repair policy, a possible objective is:

```text
task loss
+ weighted read cost
+ weighted initial processing cost
+ expected repair cost under future source changes
+ weighted unavailability cost
```

A repair-cost calculation must include transitive derived artifacts and count
each rebuilt artifact once per update batch. It must account for correlated
changes and cross-group summaries. Future change rates are not known perfectly;
use frozen past traces or sensitivity analyses rather than future test events.

Partitioning influence in advance has prior art, including
[Machine Unlearning / SISA](https://arxiv.org/abs/1912.03817).
The existence of this hypothesis does not establish that the proposed approach
is new. External text summaries introduce their own quality, dependency, and
maintenance trade-offs.

The `plan` operation supplies deterministic grouping candidates. The optional
runtime can summarize selected groups and repair their managed outputs, but this
does not measure task quality or optimize the objective above.

## Executable mechanism example

Run the [partition probe](../examples/partition_probe.rs) locally:

```sh
cargo run --locked --example partition_probe
```

The example constructs 12 synthetic sources, alternating six caller-labelled
stable and six volatile records. Each source has 16 Unicode scalar values and
the same 64-character group-input cap applies to all strategies. Fixture-only SQL
sets timestamps to make the input order explicit; ordinary memory operations,
derivation, revision, and visibility checks use the public Store API. Outputs
concatenate the inputs verbatim, including separators. They are not compressed
or model-generated summaries.

The deterministic observations, checked on 2026-10-10, are:

| Strategy | Initial derived records | Withdrawn when six volatile sources change | Withdrawn when all twelve sources change |
| --- | ---: | ---: | ---: |
| `individual` | 12 | 6 | 12 |
| `fixed` | 3 | 3 | 3 |
| `volatility` | 4 | 2 | 4 |

Counts refer to unique affected derived records after the complete change
batch, not the sum of per-revision invalidation receipts. In the six-source case,
fixed grouping also withdraws groups containing all six unchanged sources;
volatility grouping leaves the groups containing unchanged sources readable.
When every source changes, volatility grouping has four affected records
compared with three for fixed grouping.

This supports only the mechanical observation that partitioning changes
dependency coverage, with different initial artifact counts. The artificially
chosen ordering and supplied labels are part of the example, not evidence of
learned volatility prediction or a representative workload. The common character
cap does **not** establish equal total processing cost: initial derived record
counts and body sizes differ. No rebuilding, model inference, task-quality
evaluation, token accounting, or latency measurement is performed. These results
do not establish the research hypothesis above.

## Evaluation design

Three kinds of evidence should remain distinct:

| Evidence | Suitable checks | What it cannot establish alone |
| --- | --- | --- |
| Protocol correctness | Revision races, stale publication, transitive withdrawal, retry recovery, namespace and history checks | Better language-model reasoning or behavior |
| Memory behavior | Current versus historical facts, corrections, missing answers, irrelevant-memory resistance, action tasks | Durability, concurrency correctness, or production readiness |
| Efficiency and portability | Equal-budget native-store comparison, grouping ablations, measured IPC and repair overhead | Universal superiority across hosts, models, or workloads |

For the grouping experiment, compare raw retrieval, one combined summary,
fixed-size groups, topic rules, and volatility rules. Keep the reader, processor,
repair policy, total budget, and evaluation tasks fixed. Include initial
processing, repeated retrieval, model calls, failed attempts, and repairs in cost.

Evaluate current-source leakage and stale-output errors alongside successful
answer coverage and time unavailable. Returning nothing must not appear to be
a successful memory system merely because it avoids stale answers.

Reject or revise the grouping hypothesis if its quality loss exceeds the
predeclared tolerance, its total cost is no better, its benefit disappears
against a strong simple baseline, or its gains depend on future test information.

## Future algorithm candidates

Hindsight's use of multiple retrieval routes motivated the standard
reciprocal-rank fusion component now present in v0.2. It combines lexical and
exact-cosine lists with `k=60`; it does not verify facts or authorize reads.
Implementation of that component is not evidence of task-quality improvement.

MemoRepair supplies a cost/value repair-selection formulation with prerequisite
closure. For a fixed graph and scalar trade-off, maximum-weight closure can be
reduced to minimum cut. This is not a general guarantee of an optimum under every
hard budget or changing dependency model. RecallLedger 0.2 has durable leased
repair tasks for managed summaries, but does not implement that optimizer.

Automatic generation and repair make the grouping hypothesis experimentally
accessible. They do not resolve it. The synthetic partition observations above
remain the original model-free mechanism example, not a new inference benchmark.

The [technical roadmap](roadmap.md) adds a dated comparison with current
official memory-system documentation, proposed host/applicability work, and
strong-baseline evaluation gates. Those plans do not add benchmark results or
extend the implemented guarantees described here.
