# 环境适用检查、经验卡与条件提醒

这些 Python SDK 模块沿用核心的版本、CAS、`prepare/publish` 和依赖失效契约，不执行工具、不调用模型、不建立调度器，也不从记忆取得外部操作权限。经验与承诺分别使用宿主选择的独立 namespace；不要与用户事实混用同一个客户端。

## 接入与身份

```python
from recall_ledger.client import Client
from recall_ledger.environment import capture_environment, check_environment
from recall_ledger.experience import ExperienceStore
from recall_ledger.commitments import CommitmentStore

# expected_binding 必须来自宿主此前确认并保存的绑定。
client = Client(socket_path, expected_binding=expected_binding)
experiences = ExperienceStore(
    client, expected_binding=expected_binding,
    receipt_verifier=host_verify_receipt,
)
```

所有模块只调用 `client.call(method, params)`。替代客户端须暴露相同的 `expected_binding`，并在每次实际请求中强制验证 namespace/history；仅在构造时调用 `binding` 不足以防止后续 socket 被替换。模块不自动重新绑定，namespace 也不是认证。

## 1. 环境适用检查

```python
snapshot = capture_environment(
    roots={"task": host_selected_absolute_root},
    files=[{"root": "task", "path": "requirements.lock"}],
    tool_versions={"test-runner": "1.0"},
    observed_at="2026-11-09T10:00:00Z",
)
result = check_environment(
    original_snapshot, snapshot, now="2026-11-09T10:01:00Z",
    max_age_seconds=86400,
)
```

`roots` 由可信宿主选择。目录必须是绝对路径；建议宿主先解析自己的合法根目录。文件选择只允许根目录下相对路径，拒绝 `..`、绝对路径和 symlink 跳转。采用目录文件描述符逐层打开，单文件最多 8 MiB、最多 128 个文件；不读取任意绝对路径，不运行命令探测版本。锁文件与普通文件按同一规则计算 SHA-256。

返回快照：

```json
{
  "schema": "recall-ledger.environment/1",
  "observed_at": "2026-11-09T10:00:00Z",
  "files": [{"root": "task", "path": "requirements.lock", "status": "observed", "sha256": "..."}],
  "tools": {"test-runner": "1.0"}
}
```

返回检查为 `{status, checks, checked_at, permissions_granted:false}`：

- `applicable`：全部要求都有新鲜观察，文件 hash 与工具版本一致。
- `mismatch`：存在已知不一致。
- `unknown`：没有要求、文件缺失／不可读／过大、版本未提供，或当前快照太旧／来自未来。未知不能被当作匹配。

所有时间必须带明确时区，存储输出统一为 UTC。观察是一个时间点，不保证下一次工具运行前文件没有变化；有副作用的执行必须由宿主再次检查。

## 2. 可验证经验卡

### 工具回执

`record_receipt(receipt, *, idempotency_key)` 接收宿主完成的工具回执。`receipt_verifier` 必须对完整回执返回严格 `True`：例如验证宿主签名，或与受保护的工具运行日志逐字段比对。**不能用 `lambda _: True`、LLM 声称成功或用户粘贴的 `verified:true` 替代。** SDK 自己没有宿主日志或私钥，真实性边界由宿主提供。

回执至少包含：

```json
{
  "schema": "recall-ledger.tool-receipt/1",
  "receipt_id": "host-run-123",
  "host_id": "host-installation",
  "tool": "test-runner",
  "tool_version": "1.0",
  "status": "succeeded",
  "input_sha256": "<64 lowercase hex>",
  "output_sha256": "<64 lowercase hex>",
  "started_at": "2026-11-09T09:59:00Z",
  "completed_at": "2026-11-09T10:00:00Z",
  "exit_code": 0,
  "checks": [{"name": "input-and-result-validation", "passed": true}],
  "source_refs": [{"id": "same-namespace-input-record", "revision": 1}]
}
```

失败／取消回执可保留用于反馈，但不能派生成成功经验。成功必须为 `succeeded`、exit code 0、非空且全部通过的检查，并得到宿主验证。宿主还应验证输入摘要确实对应那次工具运行，检查项目足够支持所宣称的成功。

`source_refs` 可省略。提供时，回执通过 `prepare/publish` 绑定同 namespace 的输入版本；输入变更会传递失效回执、经验卡和后续反馈。省略时只持久化经过核验的回执，不宣称已追踪全部输入依赖。跨 namespace 不自动复制用户事实；需要宿主显式建立合法的同域输入记录或受控导入。

### 派生、使用与反馈

```python
receipt = experiences.record_receipt(host_receipt, idempotency_key="receipt-123")
card = experiences.derive_card(
    [receipt["id"]], title="这套配置下的测试方法",
    steps=["执行宿主记录的测试步骤", "核对完整测试结果"],
    environment=snapshot, idempotency_key="card-123",
)
current = experiences.inspect(
    card["id"], snapshot=fresh_host_snapshot, now=host_now,
)
```

派生必须有 1–64 份成功回执、已观察的非空环境要求，而且快照中的工具版本与回执一致。读取之后、准备之前的版本变化也会拒绝；提交时由核心再次检查。卡片原始 body 包含 `title`、`steps`、`environment`、`receipt_refs`、`created_at`、`verification`；实际依赖必须与 `receipt_refs` 一致，复制 JSON 却没有核心依赖的记录不能通过检查。

`inspect` 返回 `applicability`、`receipt_trust`、`can_suggest`。未提供验证器、验证器拒绝、环境不明或变化时不允许推荐。`list(limit=100)` 只供浏览，均标为待检查。成功回执验证不证明步骤的概括正确，更不证明它适用于其他任务；`semantic_generalization_verified` 恒为 false。

```python
experiences.feedback(
    card["id"], outcome="failed", receipt_id=verified_failure_receipt_id,
    recorded_at=host_now, idempotency_key="feedback-123",
)
experiences.revoke(
    card["id"], expected_revision=card["revision"],
    idempotency_key="revoke-123",
)
```

反馈必须有匹配结果的可信回执，不自动转化为训练样本或提升置信度。`list_receipts` 返回回执清单。撤回卡片或来源沿用核心传递失效，不承诺物理介质擦除。

## 3. 条件提醒与承诺

用另一个固定 namespace 的客户端创建 `CommitmentStore(client, expected_binding=...)`。

```python
item = commitments.create(
    title="草稿就绪后审阅", due_at="2026-11-09T18:00:00+08:00",
    conditions=[{"signal": "draft_ready", "equals": True}],
    created_at=host_now, idempotency_key="commitment-123",
)
due = commitments.poll(now=host_now, signals={"draft_ready": True})
```

条件是最多 32 个、全部满足的精确相等条件；值仅支持字符串、整数、布尔值，类型必须一致。缺失信号不满足条件；不接受表达式、脚本或模型自动解释。时间到达与全部条件满足后才进入 `poll`。空条件表示单纯按时间到期。信号与当前时间必须由宿主提供。

记录包含 `state: pending|completed|cancelled`、UTC `due_at`、`conditions`、`delivery` 和创建／更新时间。`list`、`inspect` 供管理。`update(id, expected_revision, now, idempotency_key, title=None, due_at=None, conditions=None)` 可修改尚未领取的待办；已经领取或进入终态的记录不能改期复活。

### 持久领取与交付边界

```python
claim = commitments.claim(
    item["id"], expected_revision=item["revision"], delivery_id=host_delivery_id,
    now=host_now, signals=host_signals, idempotency_key="claim-123",
)
```

`claim` 用 CAS 将唯一领取写入同一条承诺，并返回 `{claimed, deliver, item}`。`item.delivery` 包含调用方 `id`、全局限定的 `outbox_id`、`state:claimed`、`claimed_at`。重复调用返回 `deliver:false`；重启后 `poll` 不会再次排出已领取任务。没有自动超时回收或无限重发。

宿主只在首次领取成功后进入自身交付流程。交付前再调用：

```python
preflight = commitments.delivery(
    item["id"], delivery_id=host_delivery_id, now=host_now, signals=host_signals,
)
```

该操作重新检查取消／完成、条件、时间和领取身份。它不是发送权限。外部端以 `outbox_id` 去重，宿主仍执行自身授权。投递完成后调用 `ack(id, delivery_id, now, idempotency_key)`；再次确认不会重复写。提醒已读不自动表示工作完成。

**本模块提供持久单次领取，不声称跨数据库与外部服务的 exactly-once。** 若领取后进程崩溃、发送结果未知，保留 `claimed`，宿主应对照自身 outbox 与外部接收回执人工／受控恢复，不能简单重发。取消与外部发送不是一个事务：取消能让后续检查失败，不能撤回已经完成的发送。应用内提醒也应按 `outbox_id` 保留同一展示项目。

`complete(id, expected_revision, now, idempotency_key)` 和 `cancel(...)` 由用户或可信宿主明确调用；它们保留交付历史并停止后续排出。CAS 冲突要求重新读取，不能覆写较新的领取或取消。

## 有界目录与验证

列表使用核心变更流发现本模块记录，每次最多扫描 10,000 条事件；超过上限明确报错，不静默漏掉更早的到期待办。清单最多 1,000 条，普通列表返回 `has_more`；轮询若全集超过 1,000 条则拒绝。更大部署需要宿主维护经过绑定校验的变更流索引。导入记录必须重新核对目标绑定与回执信任，不能把迁移后原样 JSON 当成本地验证。

可在临时数据库运行：

```sh
cd sdk/python
RECALL_LEDGER_BIN=/absolute/path/to/recall-ledger \
PYTHONPATH=.:tests python3 -m unittest test_environment test_experience test_commitments -v
```

测试覆盖实际 daemon 的依赖失效、来源变化竞态、重启、取消、完成、未知领取回执、同数据库 namespace 隔离；环境单元测试覆盖文件／版本变化、时区、缺失证据和路径越界。该检查属于本地实现验证，不是论文复现、真实宿主提醒验收或长期经验有效性证明。
