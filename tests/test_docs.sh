#!/usr/bin/env bash
# Documentation/bootstrap consistency gate (F10/F11): every file AGENTS.md
# treats as mandatory must exist (case-sensitively), consolidated mechanics
# references exist, no stale role names/scheduling instructions survive, and the
# advertised report/skill contracts match what the scripts implement.
# Runs against the real repo root — no sandbox needed, nothing is written.
# Usage: tests/test_docs.sh   (exit 0 = pass)
# Local deviations from upstream: README.md is GitHub-side only (untracked
# here) and memory/ is tracked here, so the README assertions and the memory/
# exclusion are dropped/kept accordingly.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root=$(cd "$here/.." && pwd)

fail() { echo "FAIL: $*" >&2; exit 1; }

# Managed workspace files live in the embedded template (B1 layout); engine-only
# files stay at the repo root. The CoS document is the template's AGENTS.md.
ws="$root/templates/workspace"

# ── mandatory referenced files exist, case-sensitively (F11) ─────────────────
# AGENTS.md reads identity.md first and mandates these rules/skills.
for f in identity.md rules/_all/brief.md rules/_all/fanout-agent-grid.md \
         rules/_all/worker-timebox.md .claude/skills/herdr-orchestration/SKILL.md; do
  [[ -f $ws/$f ]] || fail "mandatory file missing (case-sensitive): templates/workspace/$f"
done
grep -q 'identity\.md' "$ws/AGENTS.md" || fail "AGENTS.md does not reference identity.md"
echo "ok: identity.md + mandatory rules/_all files + orchestration skill exist"

# ── one authority model + consolidated command/judgment references ─────────
for f in operations.md judgment.md recovery.md; do
  [[ -f $ws/.claude/skills/herdr-orchestration/references/$f ]] \
    || fail "orchestration reference missing: $f"
  grep -qF "references/$f" "$ws/.claude/skills/herdr-orchestration/SKILL.md" \
    || fail "orchestration skill omits reference: $f"
done
# the retired Lead name must not reappear in contracts (amendments name the
# replacement, never the old label)
! grep -rn 'quaqua-[a-z*-]*-orchestrator' "$ws/AGENTS.md" \
    "$ws/models.md" \
  || fail "stale 'quaqua-<slug>-orchestrator' role name survives"
# rename leaves no dangling refs in tracked docs, scripts (incl. the auditor's
# systemd/launchd units), or tests
# (memory/ is tracked here but is dated history; the repo-wide guard, whose walk
# covers crates/ too, lives in the Rust worker_unit suite)
! grep -rn 'super''visor' "$ws/scripts" "$root/tests" \
    --exclude="test_docs.sh" \
  || fail "dangling legacy references survive in docs/scripts"
# acceptance authority: Lead merges into the integration branch, anh iu owns push
grep -q 'quaqua/<slug>-integ' "$ws/AGENTS.md" || fail "integration-branch acceptance missing from AGENTS.md"
echo "ok: consolidated orchestration references; one role naming; acceptance authority consistent"

# ── entrypoints live in role dirs; every scripts/*.sh reference resolves ──────
# Phase 2 moved the entrypoints to scripts/{worker,lead,auditor,workspace}/ (hooks/
# unchanged). A flat file left behind, or any tracked reference to a path that
# does not exist (a stale flat entrypoint path, a typo), fails here. This very
# comment is scanned too: never spell a dead path out in this file.
flat=$(find "$ws/scripts" -maxdepth 1 -name '*.sh' -print)
[[ -z $flat ]] || fail "entrypoint left flat in scripts/ (role dir required): $flat"
# Two roots now: managed refs name the workspace layout (templates/workspace/…),
# engine-only files still name the repo root. A ref resolves if it exists under
# either root; a typo resolves under neither.
# Issue #55: the gate deliberately names the removed wrappers so it can deny
# calls to them; accept exactly those five in the gate's own files, never
# anywhere else (a bare basename here keeps this file out of its own scan).
gate_removed_ref() {
  [[ $1 == scripts/worker/* ]] || return 1
  case "${1##*/}" in
    spawn-worker.sh|prompt-worker.sh|cleanup-worker.sh|grid-workers.sh|worker-cap.sh) return 0 ;;
  esac
  return 1
}

missing=()
while IFS= read -r f; do
  while IFS= read -r ref; do
    ref="scripts/${ref#*scripts/}"
    if [[ $f == crates/quaqua/src/gate/policy.rs || $f == crates/quaqua/tests/gate.rs ]] \
       && gate_removed_ref "$ref"; then
      continue
    fi
    [[ -e $root/$ref || -e $ws/$ref ]] || missing+=("$f: $ref")
  done < <(grep -aoE '(^|[^A-Za-z0-9_./-])scripts/[A-Za-z0-9_./-]+\.sh' "$root/$f" 2>/dev/null | sort -u)
done < <(git -C "$root" ls-files | grep -v -E '^(templates/workspace/scripts/hooks/|templates/workspace/scripts/auditor/|docs/research/)')
(( ${#missing[@]} == 0 )) || fail "dangling scripts/*.sh reference(s): ${missing[*]}"
echo "ok: entrypoints in role dirs; every scripts/*.sh reference resolves"

# ── report contract: worker -> '## Lessons', pipeline reviewer -> 'Verdict:' ──
grep -q '## Lessons' "$ws/rules/_all/brief.md" || fail "brief.md drops the Lessons tail"
grep -q 'Verdict:' "$ws/rules/_all/brief.md" || fail "brief.md ignores the reviewer Verdict tail"
grep -q 'Verdict: clean' "$root/crates/quaqua/src/pipeline/mod.rs" || fail "pipeline lost the Verdict contract"
echo "ok: report-tail contract documented for workers and pipeline reviewers"

# ── scheduling assets: exactly one Auditor entrypoint (F8/F10) ───────────────
[[ -f $ws/scripts/auditor/systemd/quaqua-auditor.service.in && -f $ws/scripts/auditor/systemd/quaqua-auditor.timer \
   && -f $ws/scripts/auditor/launchd/com.quaqua.auditor.plist.in ]] \
  || fail "systemd or launchd auditor scheduling assets missing"
grep -q 'auditor run' "$ws/scripts/auditor/systemd/quaqua-auditor.service.in" \
  || fail "auditor service does not exec quaqua auditor run"
grep -q 'auditor' "$ws/scripts/auditor/launchd/com.quaqua.auditor.plist.in" \
  || fail "launchd template does not exec the same auditor entrypoint"
# README.md is GitHub-side only (not tracked here) — install coverage is
# asserted via the shipped template + installer instead (test_install_auditor.sh).
grep -q 'reports/audit-\*\.md\|reports/audit-' "$ws/AGENTS.md" \
  || fail "AGENTS session-start glob != quaqua auditor output"
echo "ok: auditor entrypoint + scheduling assets agree"

# ── routing consistency: review findings flow through the pipeline ────────────
! grep -q 'to CoS → sude fix\|to CoS -> sude fix' "$ws/models.md" \
  || fail "models.md still routes review findings around the pipeline"
echo "ok: review findings route back to the builder via pipeline.sh"

echo "PASS"
