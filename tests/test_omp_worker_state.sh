#!/usr/bin/env bash
# Unit and integration test for omp-worker-state.ts extension.
# Usage: tests/test_omp_worker_state.sh (exit 0 = pass)
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd -P)"
ext="$root/templates/workspace/scripts/hooks/omp-worker-state.ts"
T=$(mktemp -d "${TMPDIR:-/tmp}/test-omp-state.XXXXXX")
trap 'rm -rf "$T"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
unset OMPCODE

[[ -f $ext ]] || fail "missing extension at $ext"

# Check version pinning headers
grep -q 'TARGETED_OMP_VERSION = "18.15.0"' "$ext" || fail "missing TARGETED_OMP_VERSION 18.15.0"
grep -q 'QUAQUA_EXTENSION_ID=omp-worker-state' "$ext" || fail "missing QUAQUA_EXTENSION_ID"

# Run node/bun tests on the extension behavior
bun -e '
import { default as initExtension } from "'"$ext"'";
import fs from "node:fs";
import path from "node:path";
import assert from "node:assert";

const testDir = "'"$T"'";
const stateFile = path.join(testDir, ".worker.state");
const errorFile = path.join(testDir, ".worker.error");

process.env.QUAQUA_STATE_FILE = stateFile;
process.env.QUAQUA_ERROR_FILE = errorFile;
delete process.env.OMPCODE;
class MockEmitter {
  constructor() {
    this.handlers = new Map();
  }
  on(event, handler) {
    if (!this.handlers.has(event)) {
      this.handlers.set(event, []);
    }
    this.handlers.get(event).push(handler);
  }
  emit(event, ...args) {
    const list = this.handlers.get(event) || [];
    for (const h of list) {
      h(...args);
    }
  }
}

const pi = new MockEmitter();
initExtension(pi);

function readState() {
  return fs.readFileSync(stateFile, "utf8").trim();
}

function readError() {
  return fs.readFileSync(errorFile, "utf8").trim();
}

// 1. session_start -> idle
pi.emit("session_start", {}, { isIdle: () => true });
assert.match(readState(), /^idle \d+$/);

// 2. agent_start -> working
pi.emit("agent_start");
assert.match(readState(), /^working \d+$/);

// 3. tool_approval_requested -> blocked ... permission
pi.emit("tool_approval_requested");
assert.match(readState(), /^blocked \d+ permission$/);

// 4. tool_approval_resolved -> working
pi.emit("tool_approval_resolved");
assert.match(readState(), /^working \d+$/);

// 5. plan_review_requested -> blocked ... plan
pi.emit("plan_review_requested");
assert.match(readState(), /^blocked \d+ plan$/);

// 6. plan_review_resolved -> working
pi.emit("plan_review_resolved");
assert.match(readState(), /^working \d+$/);

// 7. tool_execution_start (ask) -> blocked ... question
pi.emit("tool_execution_start", { toolName: "ask" });
assert.match(readState(), /^blocked \d+ question$/);

// 8. tool_execution_end (ask) -> working
pi.emit("tool_execution_end", { toolName: "ask" });
assert.match(readState(), /^working \d+$/);

// 9. agent_end with willContinue: true -> remains working
pi.emit("agent_end", { willContinue: true });
assert.match(readState(), /^working \d+$/);

// 10. agent_end clean finish -> idle
pi.emit("agent_end", {});
assert.match(readState(), /^idle \d+$/);

// 11. credential_disabled -> blocked signed-out + fatal 401 0 (0 retries)
pi.emit("credential_disabled");
assert.match(readState(), /^blocked \d+ signed-out$/);
assert.equal(readError(), "fatal 401 0");
fs.unlinkSync(errorFile);

// 12. agent_end with terminal error (connection refused, 0 retries run)
pi.emit("agent_start");
pi.emit("agent_end", {
  messages: [{
    role: "assistant",
    stopReason: "error",
    errorMessage: "Error: Couldn\x27t connect to 127.0.0.1:8899 (connection refused). Check that the server is running and reachable."
  }]
});
assert.match(readState(), /^idle \d+$/);
assert.equal(readError(), "retryable 0 0");
fs.unlinkSync(errorFile);

// 13. agent_end with terminal 429 rate limit (exhausted after 10 retries)
pi.emit("agent_start");
pi.emit("agent_end", {
  messages: [{
    role: "assistant",
    stopReason: "error",
    errorStatus: 429,
    errorMessage: "Retry budget exhausted after 10 retries: 429 Rate limit reached"
  }]
});
assert.equal(readError(), "retryable 429 10");
fs.unlinkSync(errorFile);

// 13b. Pinning regex for singular retry: "failed after 1 retry: 429"
pi.emit("agent_start");
pi.emit("agent_end", {
  messages: [{
    role: "assistant",
    stopReason: "error",
    errorStatus: 429,
    errorMessage: "Error: Retry failed after 1 retry: 429: Too Many Requests"
  }]
});
assert.equal(readError(), "retryable 429 1");
fs.unlinkSync(errorFile);

// 13c. Pinning agent_start count: 2 agent_start calls = 1 retry run
pi.emit("agent_start");
pi.emit("agent_start");
pi.emit("agent_end", {
  messages: [{
    role: "assistant",
    stopReason: "error",
    errorMessage: "Connection dropped"
  }]
});
assert.equal(readError(), "retryable 0 1");
fs.unlinkSync(errorFile);

// 14. agent_end with terminal 401 auth error -> blocked signed-out + fatal 401 0
pi.emit("agent_start");
pi.emit("agent_end", {
  messages: [{
    role: "assistant",
    stopReason: "error",
    errorStatus: 401,
    errorMessage: "401 Incorrect API key provided: sk-fake."
  }]
});
assert.match(readState(), /^blocked \d+ signed-out$/);
assert.equal(readError(), "fatal 401 0");
fs.unlinkSync(errorFile);

// 15. Nested session ignores events
process.env.OMPCODE = "1";
const piNested = new MockEmitter();
initExtension(piNested);
piNested.emit("agent_start");
// stateFile was still signed-out from step 14
assert.match(readState(), /^blocked \d+ signed-out$/);

console.log("ok: all omp-worker-state mock events pass");
'

chmod +x "$ext"
echo "PASS test_omp_worker_state"
