# What supports reliable long-term memory?

Review date: 2026-10-11; publication window: 2023-10-11 through 2026-10-11.
This is a targeted primary-source review, not an exhaustive systematic review.
Relevant methods and experiments were read (**E2**). Scores are author reports;
no paper reproduction or long-term real-host acceptance was performed.

The reviewed papers support mechanisms for reducing loss and limiting error
propagation. They do not establish that a general agent will preserve all
meaning indefinitely or that task quality will never decrease.

| Fixed primary source | Useful support | Boundary |
| --- | --- | --- |
| [MemGPT, v2](https://arxiv.org/html/2310.08560v2) | Separate limited working context from searchable history | Finite dialogue/document experiments; the summary-only comparison has less accessible information |
| [A-MEM, v11](https://arxiv.org/html/2502.12110v11) | Structured notes, links and evolving organization improve tested retrieval tasks | Evolution can introduce mistakes; more retrieval is not uniformly better |
| [ACE, v3](https://arxiv.org/html/2510.04618v3) | Itemized incremental updates address collapse from repeated whole-context rewriting | Harmful reflection at every adaptation step reduced FiNER accuracy to 66.7 versus a 70.7 base model |
| [Hindsight, v1](https://arxiv.org/html/2512.12818v1) | Separate facts, experiences and opinions; combine retrieval channels | Opinion strength is not calibrated truth; v1 leaves retrieval token budget as a placeholder and mixes baseline sources |
| [MemoRepair, v1](https://arxiv.org/html/2605.07242v1) | Withdraw affected artifacts before repairing and republishing | Requires complete influence provenance and suitable validators; removing 1% of influence edges produced 17.7% Leak in its ablation |
| [MemSecBench, v1](https://arxiv.org/html/2607.27080v1) | Evaluate successful removal together with preservation of benign memory | Selective repair was 56.1% versus 86.3% target removal, conditioned on successful poisoning; controlled tests are not years of use |

These are design inputs and counterexamples, not endorsements of RecallLedger.
Paper guarantees must retain their assumptions; a store-level isolation theorem
does not become a theorem about meaning, belief or all future agent behavior.

## Engineering implications

RecallLedger already records source revisions, invalidates known descendants,
gates publication, keeps candidate admission explicit, and checks manifests
against current memory. Portable import adds origin identity, source-specific
synchronization checks, conflict refusal and withdrawal preservation. Handoff
adds current evidence checks before a receiving host uses a transferred task.

Those contracts address **known state and dependency consistency**. They do not
validate generated summaries against source meaning, discover hidden
dependencies, retract a host's already-delivered prompt, or prove performance
superiority over strong native memory.

The 2026-10-12 development implementation adds
[source-rooted evidence capsules](fidelity.md): deterministic complete source
text, recorded-closure guards, transaction-time reconstruction, refusal on
insufficient budgets, and model-free repair. This is a narrow way to exclude
new generative distortion from this representation path. It trades compression
and availability for directly checkable retention; it is not a proof of truth.

Further research on evidence-anchored memory evolution should:

- Preserve permitted source evidence and conditions separately from generated
  interpretations; a hash only checks identity, not semantic support.
- Generate reviewable patches instead of repeatedly replacing the complete
  memory with a summary.
- Require both deterministic lifecycle checks and semantic regression checks
  before promoting generated long-term conclusions.
- Measure stable-fact retention, outdated-answer adoption, withdrawal exposure,
  benign-memory preservation and total maintenance cost over fixed checkpoints.

Semantic checks and longitudinal task non-degradation remain **H0 proposals**.
The implemented source-byte and lifecycle regression runner is an engineering
test, not a semantic quality result.
Compare native memory, raw-history retrieval, recursive summaries and
RecallLedger under equal information access and total budget. Freeze the
held-out histories, checkpoints, non-inferiority margins and stopping rules
before evaluation. Accelerated event sequences must not be described as
real deployment lasting years.

A defensible outcome is bounded: “Under the declared protocol and duration,
quality stayed within the specified tolerance.” It is not “the agent never
forgets or degrades.” See [product goals](product-goals.md) for the separate
longitudinal and cross-brand acceptance designs.
