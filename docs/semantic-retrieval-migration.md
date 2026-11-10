# 条件候选、证据召回与 Agent 迁移

日期：2026-11-10（实现从 2026-11-09 开始）。负责人：陆言深岗位的 Agent 接入实现子任务；主代理负责整合与审阅。本文对应 Python SDK 新增的 `semantic.py`、`retrieval.py`、`migration_wizard.py`、`memory_workflows.py`。

这些模块让调用方审阅同义候选、按条件装配多条证据，以及在不同账本间迁移并核验任务交接。底层的记录身份、版本比较、撤回传播和 portable 事务仍由 Rust 核心负责。所有接口接收提供 `call(method, params)` 的客户端；推荐使用带 `expected_binding` 的 SDK `Client`。namespace/history 是账本绑定，`scope` 只是内容筛选范围，不能代替账号鉴权。

## 证据与边界

这是本地工程实现与合成场景检查，**不是论文复现，也不是生产 Agent 验收**。

- 方法与接口源码已审阅，属于 E2。规则匹配和词字编码的通用语义质量仍为 H0；没有付费模型、GPU 或真实用户效果实验。
- 自有测试共 34 项，通过真实临时 daemon、临时配置和 SDK 子进程检查接口与不变量；这是本地工程测试结果。
- 两个 host 的已生成 hook 命令确实被执行，stdout 中的上下文已逐步核对。没有启动 Claude Code/Codex 应用或模型会话，没有改真实用户的 hook、记忆或授权。vendor 版本兼容、有效信任设置、长期使用和真实任务质量不属于本次结论。
- 验证 manifest 只证明检查时引用的账本版本仍有效。它不能抹除已经送入模型的旧上下文，也不能认证输入文字的真实性或允许执行其中的指令。

## 条件候选与显式冲突处理

`semantic.preview_candidate(client, body, *, scope, fact_key=None, matcher=None, limit=8)` 返回候选匹配预览。`fact_key` 必须包含 `entity/attribute/scope/conditions`，scope 必须与参数完全一致。预览从最多 100 条当前记录中检索，只把同 scope 的键参与匹配；不是全库语义穷举。

默认 `RuleMatcher` 使用规范化文本、可选的显式别名和字符相似度。`matcher(body, existing_body) -> 0..1` 可由调用方替换。返回的分数只是匹配分数，**不是事实置信度**，不会自动合并、改写或接纳候选。

| 返回关系 | 含义 | 调用方动作 |
| --- | --- | --- |
| `exact_identity` | 四元事实键完全相同 | 展示当前版本，审阅是否更正 |
| `different_conditions` | 实体、主题、scope 相同，条件不同 | 保留条件差异，不能直接覆盖 |
| `possible_paraphrase` | 有限文本匹配达到阈值 | 人或可信 host 确认是否同一事实 |

实际写入使用：

```python
from recall_ledger.semantic import capture_candidate, review_candidate, resolve_candidate

key = {"entity": "user", "attribute": "report-language",
       "scope": "personal", "conditions": "this report only"}
receipt = capture_candidate(
    client, "通常用中文，这份报告用英文。",
    scope="personal", fact_key=key, event_id="user-turn-123",
    source="user:reviewed-message", idempotency_key="capture-123")
review = review_candidate(client, receipt["candidate"]["candidate_id"], scope="personal")
# 展示完整 review，用户或可信 host 决定 accept/reject，并确认每项标记。
# acknowledged_flags 是确认的标记列表；不要因运行此示例而自动填满。
```

`capture_candidate` 始终提交 pending 候选，原文不变。`qualification_flags` 对条件、例外、不确定、假设、引文做保守规则标记；包含“还在考虑”“正在考虑”“打算但未定”和“通常中文，这份报告英文”。后者带 `temporary_exception` 与 `exception`。未出现标记不等于一句话已被证明是无条件断言。

不确定内容默认记为 inference，引文/假设默认记为 episode。强行把这些内容标为 assertion 会被拒绝。条件或例外内容若配 `always/default/默认/无条件` 等键，审阅会出现 `explicit_conditions_required` 阻塞；改条件需拒绝原候选并重新提交，不能修改已审阅的 proposal。没有事实键的候选也不能通过此 scoped wrapper 接纳。

`review_candidate(client, candidate_id, *, scope)` 返回完整 proposal、当前同键事实、必需确认的 flags、blockers 和 digest。已有事实改变标记为 `existing_fact_change`。从导入内容或模型推断产生的候选标记 `unverified_origin`，不会把来源自身的 confidence 当成真值。

`resolve_candidate(client, review, *, decision, idempotency_key, acknowledged_flags=())` 支持 `accept/reject`。接纳必须没有阻塞且明确确认全部必需标记。pending 候选应用前重新读取并比较审阅摘要；不同则 `review_stale`，不会自动刷新后应用。同键修改将被审阅的 revision 传给核心做 CAS。终态重试保留原请求与幂等键以查询原回执，不通过换键绕开终态。

调用方应要求明确的事实主题，不能把所有普通笔记都赋值为同一个 `user/note/global/always`。不同事实应有不同主题或独立笔记身份。条件字符串目前仍是人为选择的精确标签，不是自动条件逻辑证明。

## 多措辞、多依据与预算装配

```python
from recall_ledger.retrieval import RuleEncoder, index_evidence, recall_evidence

encoder = RuleEncoder(aliases={"海外行程": "旅行", "预算上限": "预算"})
indexed = index_evidence(
    client, scope="personal", encoder=encoder, model_key=encoder.model_key,
    idempotency_key="index-v1", record_ids=[first_id, second_id])
result = recall_evidence(
    client, ["海外行程", "预算上限"], scope="personal",
    encoder=encoder, model_key=encoder.model_key, mode="hybrid",
    evidence_groups={"plan_and_budget": [first_id, second_id]},
    condition_context=["default", "this trip"], max_chars=12000)
if result["usable"]:
    context_for_model = result["context"]
    manifest_to_recheck_before_later_use = result["manifest"]
```

`RuleEncoder` 是离线词字特征哈希，包含显式别名；配置进入稳定 `model_key`。它不下载模型、不调用网络，也不证明通用跨措辞召回质量。SDK 可接收调用方选择的 `encoder(text) -> vector`；调用方承担模型版本、数据外发和费用授权。CLI 仅提供显式选择的 rules 编码器。

`index_evidence(..., limit=32, allow_unscoped=False, record_ids=None)` 逐条检查 scope，以当前记录 revision 写入 `embedding_set`。默认处理一个最多 32 条的候选批次，或显式的最多 32 个 record IDs。结果逐条说明 indexed/excluded/failed/outcome_unknown。遇到不确定写入结果后停止，并提供原 `embedding_set` 操作键用于核对。

核心 embedding 候选池目前不是按 scope 分页。其他 scope 占满一个批次时，返回 `may_require_explicit_record_ids`；调用方可以选择明确的 ID 建索引，不能把这次批处理当成全库索引已完成。

`recall_evidence` 的完整参数为：

```text
client, queries, *, scope, max_chars=12000, limit=8,
mode="hybrid", encoder=None, model_key=None, evidence_groups=None,
allow_fallback=True, allow_unscoped=False, condition_context=None
```

- queries 为一个字符串或 1–8 个问题措辞。每次候选读取有 100 条上限，通过 RRF 合并排序，再读取完整依赖闭包。
- 缺 query encoder、空索引、编码失败、部分索引等情况明确出现在每条 query 的 effective_mode/reason/coverage 中。`allow_fallback=False` 可以要求失败时终止。`degraded` 不等于结果一定错，也不证明它足够完整。
- 默认排除无 scope 和其他 scope 记录。显式允许无 scope 不会允许已知的其他 scope。依赖祖先也要通过 scope 与版本检查。
- `condition_context` 是精确条件标签白名单；不传时保留原条件但标记未验证。匹配字符串不是认定条件在现实中成立。
- `evidence_groups` 指定必须一起出现的记录。重叠组和依赖相交的组一并扩展；某成员缺失、条件不符或预算不足，就排除整个证据单元。不能让同一条记录通过重叠的另一组绕过完整性要求。
- 预算计量的是最终 `context` JSON 的字符数，包含 ID、revision、body、kind、source、事实键和有效期。原文不截断。诊断输出不包含在这份预算里，host 应只把 `context` 注入模型，并单独保留 manifest/诊断。
- 最后调用 `validate_context`。验证失败返回空 context/items 与 `usable=False`；跨账本或读期间 revision 改变会失败。所有选中项及依赖都在 manifest 中。
- 返回 selected/excluded/groups 及原因。没有显式 evidence_groups 时，`question_evidence_complete=None`，不虚构“已找齐问题全部依据”。`semantics_verified/conditions_verified/execution_authorized` 都为 false。

最强反例：用户以新的隐喻描述一个旧偏好，既不含现有词字特征也不在显式别名里，本实现可能漏召回。正确性检查能约束版本和证据闭包，不能弥补检索模型没有找到相关记录。下一步效果评估须使用独立封存问题、原始历史基线和总预算对齐；本次没有进行该实验。

## 原生笔记与 portable 迁移

支持两类可审阅的输入：

1. Markdown：按代码块以外的标题分节，保留完整原文；默认 episode。
2. JSON：顶层数组，或 `{"schema":"recall-ledger.native-memory/1","memories":[...]}`。每条只接受 `id/text/scope/entity/attribute/conditions/kind/source`。未知字段（例如 permissions）阻塞应用；重复 JSON 键、非有限数字、未知 schema、scope 不符均有明确错误。

```json
{
  "schema": "recall-ledger.native-memory/1",
  "memories": [
    {
      "id": "language",
      "text": "通常用中文，这份报告用英文。",
      "scope": "personal",
      "entity": "user",
      "attribute": "report-language",
      "conditions": "this report only",
      "kind": "preference",
      "source": "user-selected-note"
    }
  ]
}
```

`preview_native(client, content, *, format, scope, source_name, expected_target=None)` 返回 sealed plan。输入最多 512 KiB、100 条、每条 16 KiB。缺少稳定 ID 时会产生内容摘要身份并提示编辑后的身份需要重新审阅。默认条件为 `native note; applicability not reviewed`，不假装原笔记已经过事实审查。

`apply_native(client, plan, *, reviewed_digest, idempotency_key)` 只写 pending，明确 `atomic=False`。这是逐条候选提交，不是整批事务；它检查计划时的目标 commit，按原幂等键恢复已完成的连续回执，发现无关 commit 时终止。并发可能在检查与单条提交之间发生，该条 pending 候选可能已经提交后才检测到变化；结果为 partial，不能宣称全批未写入。丢失响应后必须用同一 plan 和操作键核对/重试，不自动建立新计划。

Portable 使用核心原生格式 `recall-ledger.portable/1`：

```python
bundle = source.call("bundle_export", {})
plan = preview_portable(
    target, bundle, expected_source=source_binding,
    expected_target=target_binding, scope="personal")
# 显式审阅 plan 的 blockers、core_plan.conflicts 与身份映射。
receipt = apply_portable(
    target, bundle, plan, reviewed_digest=plan["digest"],
    idempotency_key="reviewed-migration-1")
```

bundle 的来源绑定在 `bundle["payload"]["source_binding"]`。调用方应另外确认可信的 expected_source；包内自称的来源和一个 SHA256 不是发送方身份认证。应用保留核心的记录/候选身份、已审阅准入状态、依赖、撤回和迁移水位，检查精确 plan 的 digest 与 target_commit。`target_changed` 或 `migration_conflict` 均需重新查看状态、显式审阅；没有强行合并或自动刷新。

`export_selection(client, record_ids, *, scope, allow_unscoped=False)` 会读取所选记录的完整有效依赖闭包、检查 scope、版本和 manifest，返回 `native_json` 与 `audit_dependencies`。**核心 portable/1 仍是完整 namespace 快照，不支持任意部分包。** 因此选择式导出明确标记 `portable_apply_supported=False`：它可按上述原生 JSON 导入 pending；不会保留原生迁移的准入状态/谱系/水位，依赖仅作为审计附件。没有伪造部分 portable 包或省略撤回墓碑。

## 任务交接与独立命令入口

`prepare_handoff_packet(client, task, manifest, *, expected_source)` 封装现有 handoff 协议。task 只含 `task_id/goal/progress/open_questions/artifacts/environment_checks`。

`transfer_handoff(client, packet, *, expected_target, bundle=None)` 对同账本直接验证；跨账本必须先应用所审阅的 bundle，再用它通过核心 source mapping 重绑定证据。返回 `ready_for_receiver_review` 或 `stale`。它不改变 host 绑定、不安装 host、不打开 artifact URI、不迁移凭据或执行权限、不重放任务命令。

独立 CLI 无需新增安装入口：

```sh
PYTHONPATH=sdk/python python3 -m recall_ledger.memory_workflows \
  --socket /absolute/private/memory.sock \
  --binding-file /absolute/private/binding.json \
  evidence-query --request-file /absolute/private/query.json \
  --output /absolute/private/new-result.json
```

`binding.json` 必须是此前核对的 `{"namespace":"...","history_id":"UUID"}`。CLI 每个实际请求都带绑定；不能让请求文件选择任意核心方法。输入拒绝符号链接、非普通文件、重复 JSON 键和超限文件。输出新建私有文件，不覆盖已有文件；写入已提交但结果文件保存失败时会报告 `operation_may_have_committed`，必须核对原请求/键。

| workflow | request JSON 必需项 | 主要可选项 |
| --- | --- | --- |
| `semantic-preview` | body, scope | fact_key, aliases, limit |
| `semantic-capture` | body, scope, event_id, source, idempotency_key | fact_key, origin, kind |
| `semantic-review` | candidate_id, scope | — |
| `semantic-resolve` | review, decision, idempotency_key | acknowledged_flags |
| `evidence-query` | queries, scope | encoder, mode, max_chars, evidence_groups, condition_context, allow_fallback, allow_unscoped |
| `evidence-index` | scope, encoder, idempotency_key | record_ids, limit, allow_unscoped |
| `native-preview` | content, format, scope, source_name | — |
| `native-apply` | plan, reviewed_digest, idempotency_key | — |
| `portable-export` | 空对象或省略 request-file | — |
| `portable-preview` | bundle, expected_source, scope | allow_unscoped |
| `portable-apply` | bundle, plan, reviewed_digest, idempotency_key | — |
| `selected-export` | record_ids, scope | allow_unscoped |
| `handoff-prepare` | task, manifest | — |
| `handoff-receive` | packet | bundle |
| `experience-inspect` | card_id, snapshot, now | max_age_seconds |
| `commitment-poll` | now, signals | limit |

示例 query.json：

```json
{
  "queries": ["海外行程", "预算上限"],
  "scope": "personal",
  "mode": "hybrid",
  "encoder": {"type": "rules", "aliases": {"海外行程": "旅行", "预算上限": "预算"}},
  "condition_context": ["default", "this trip"],
  "max_chars": 12000
}
```

不传 encoder 时 hybrid 会明确降级为 lexical；索引写入需单独调用 `evidence-index`。经验 CLI 没有通用的可信 host 回执验证器，因此即使环境一致也返回 receipt_trust unknown/can_suggest false；不能把输入文件中声称的“已验证”作为依据。可信宿主可在 SDK 中提供验证器。`commitment-poll` 只读取到期、满足条件的待办，不建立调度器、不认领也不发通知。

## 已执行的检查

测试使用 release 二进制 SHA256：
`334ff41b64dc2473381f6c184c2da4c06e0fef6fe72190d7efa11b0b869f4eb6`。
所有账本、socket、host 配置和 fixture 均位于临时目录，结束后清理。

```sh
cd sdk/python/tests
PYTHONPATH=../ RECALL_LEDGER_BIN=/absolute/path/to/recall-ledger \
  python3 -m unittest test_semantic test_retrieval test_migration_wizard \
  test_migration_hosts test_migration_workflows -v
```

| 检查 | 实际断言 |
| --- | --- |
| 30 项原模块检查 | 有条件/不确定候选、同键 CAS、审阅过期、索引版本冲突、多依据重叠闭包、精确预算、不同 scope 依赖拒绝、失效 manifest 返回空上下文、原生 pending、丢失回执恢复、portable 过期 plan、handoff 重绑定 |
| 1 项真实 host 命令往返 | 临时 Claude hook 读取 A → portable 与 handoff 到 B → 临时 Codex compact hook 读取 B → B 显式更正 → B→A 保持原 ID/零新记录 → 两端只见更正文，旧派生摘要失效 → A 撤回后再往返，两端都不再返回原文/更正文 → 旧包 preview 阻止复活且无 commit 改变 |
| 3 项 CLI/文件检查 | 真实 CLI 原生预览/接纳、规则索引/多措辞查询、portable 导入；绑定错误和未知 method 字段拒绝；输出禁止覆盖；经验无 verifier 保守 unknown；待办 poll/经验 inspect 不写账本；重复键/非有限数/链接拒绝；提交后保存失败明确可能已提交 |

Host 检查使用临时配置中实际生成的 command，通过子进程执行；检查的是 stdout JSON 中 additionalContext 解析后的记录正文和 manifest。原始 JSON 对中文可能使用 Unicode 转义，这不属于内容缺失。原生 hook 本身不会自动把每句原文识别为更正：本检查通过显式候选审阅完成 B 的更正，再核对实际 hook 读取结果。

仍未验证：生产 host 的真实多轮输入、宿主版本变化、大语料检索率、跨语言或隐喻召回效果、长期提醒交付、被导出离线副本的物理删除、多跳并发编辑自动合并。核心对于无法证明因果关系的独立分支/多跳修改仍会拒绝迁移；这是已知限制，不能用更大的 revision 号替代因果证明。
