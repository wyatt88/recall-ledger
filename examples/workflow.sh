#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

project_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary="${RECALL_LEDGER_BIN:-$project_dir/target/release/recall-ledger}"
if [[ ! -x "$binary" ]]; then
  printf '%s\n' "Build the binary first: cargo build --release --locked" >&2
  exit 1
fi
command -v python3 >/dev/null

demo_dir="$(mktemp -d "${TMPDIR:-/tmp}/recall-ledger-demo.XXXXXXXX")"
chmod 700 "$demo_dir"
trap 'rm -rf -- "$demo_dir"' EXIT

call() {
  "$binary" --db "$demo_dir/memory.sqlite3" --namespace workflow-demo \
    call "$1" --params "$2"
}

field() {
  python3 -c 'import json,sys; print(json.load(sys.stdin)[sys.argv[1]])' "$1"
}

expect_error() {
  local expected_code="$1"
  local method="$2"
  local arguments="$3"
  if call "$method" "$arguments" >"$demo_dir/output.json" 2>"$demo_dir/error.json"; then
    printf 'Expected %s from %s, but the call succeeded.\n' "$expected_code" "$method" >&2
    exit 1
  fi
  python3 - "$demo_dir/error.json" "$expected_code" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as handle:
    error = json.load(handle)["error"]
assert error["code"] == sys.argv[2], error
PY
}

source="$(call remember '{"body":"The project language is undecided.","kind":"commitment","idempotency_key":"source-create"}')"
source_id="$(printf '%s' "$source" | field id)"
prepare_args="$(python3 - "$source_id" <<'PY'
import json
import sys
print(json.dumps({"inputs": [sys.argv[1]], "processor": "synthetic-demo-v1",
                  "idempotency_key": "prepare-old"}))
PY
)"
prepared="$(call prepare "$prepare_args")"
old_job_id="$(printf '%s' "$prepared" | field job_id)"

revise_args="$(python3 - "$source_id" <<'PY'
import json
import sys
print(json.dumps({"id": sys.argv[1], "expected_revision": 1,
                  "body": "The project uses Rust.", "idempotency_key": "choose-rust"}))
PY
)"
call revise "$revise_args" >/dev/null
stale_args="$(python3 - "$old_job_id" <<'PY'
import json
import sys
print(json.dumps({"job_id": sys.argv[1], "body": "The language is still undecided.",
                  "idempotency_key": "publish-old"}))
PY
)"
expect_error stale_dependency publish "$stale_args"
printf '%s\n' "PASS: a late result prepared from the old revision was rejected."

prepare_args="$(python3 - "$source_id" <<'PY'
import json
import sys
print(json.dumps({"inputs": [sys.argv[1]], "processor": "synthetic-demo-v1",
                  "idempotency_key": "prepare-current"}))
PY
)"
prepared="$(call prepare "$prepare_args")"
new_job_id="$(printf '%s' "$prepared" | field job_id)"
publish_args="$(python3 - "$new_job_id" <<'PY'
import json
import sys
print(json.dumps({"job_id": sys.argv[1], "body": "Rust is the selected project language.",
                  "idempotency_key": "publish-current"}))
PY
)"
summary="$(call publish "$publish_args")"
summary_id="$(printf '%s' "$summary" | field id)"
call recall '{"query":"Rust","max_chars":1000}' >"$demo_dir/recall.json"
python3 - "$demo_dir/recall.json" "$summary_id" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as handle:
    result = json.load(handle)
assert any(item["id"] == sys.argv[2] for item in result["items"]), result
PY
printf '%s\n' "PASS: a result prepared from the current revision was published and recalled."

retract_args="$(python3 - "$source_id" <<'PY'
import json
import sys
print(json.dumps({"id": sys.argv[1], "expected_revision": 2,
                  "idempotency_key": "retract-source"}))
PY
)"
call retract "$retract_args" >/dev/null
inspect_args="$(python3 - "$summary_id" <<'PY'
import json
import sys
print(json.dumps({"id": sys.argv[1], "history": True}))
PY
)"
expect_error not_found inspect "$inspect_args"
printf '%s\n' "PASS: retracting a source blocked reading its derived result."
printf '%s\n' "Synthetic workflow complete; its temporary database will be removed."
