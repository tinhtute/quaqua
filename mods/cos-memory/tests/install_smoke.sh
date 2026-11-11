#!/usr/bin/env bash
# R14: the mod is a local marketplace that validates, declares no permission
# change and no network, and carries no cap, secret pattern or char count of its
# own (R11). Never touches the live ~/.claude or ~/quaqua.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
mod=$(cd "$here/.." && pwd)
market=$(cd "$mod/.." && pwd)

fails=0
ok() { printf 'ok   %s\n' "$1"; }
bad() { printf 'FAIL %s\n' "$1"; fails=$((fails + 1)); }

json() { node -e "const d=JSON.parse(require('fs').readFileSync(process.argv[1],'utf8'));$2" "$1"; }

# 1. The marketplace manifest names the plugin and points at its folder.
if json "$market/.claude-plugin/marketplace.json" \
  "const p=d.plugins.find(x=>x.name==='cos-memory');if(!p)throw new Error('no cos-memory plugin');if(p.source!=='./cos-memory')throw new Error('source '+p.source)" 2>/dev/null; then
  ok 'marketplace.json lists cos-memory -> ./cos-memory'
else
  bad 'marketplace.json shape'
fi

# 2. The plugin manifest carries identity only: no permissions, no network, no config knobs.
if json "$mod/.claude-plugin/plugin.json" \
  "if(d.name!=='cos-memory')throw new Error('name');if(!d.version)throw new Error('version');if(!d.description)throw new Error('description');for(const k of ['permissions','userConfig','mcpServers','network','allowedTools'])if(k in d)throw new Error('declares '+k)" 2>/dev/null; then
  ok 'plugin.json is identity only (no permissions, no userConfig, no network)'
else
  bad 'plugin.json shape'
fi

# 3. The hooks manifest names the module that exists (resolved beside hooks.json).
if json "$mod/hooks/hooks.json" \
  "const m=d.modules;if(!Array.isArray(m)||m.length!==1)throw new Error('modules');if(m[0]!=='./register.js')throw new Error('module '+m[0])" 2>/dev/null \
  && [ -f "$mod/hooks/register.js" ]; then
  ok 'hooks.json -> ./register.js (hooks/register.js)'
else
  bad 'hooks.json'
fi

# 4. The module loads as ESM and exports register().
if node --input-type=module -e "
  const m = await import('file://$mod/hooks/register.js')
  if (typeof m.register !== 'function') throw new Error('register is not a function')
" 2>/dev/null; then
  ok 'hooks/register.js loads as ESM and exports register()'
else
  bad 'hooks/register.js does not load'
fi

# 5. No cap number, no secret pattern and no char counting in the mod's own source.
srcs=("$mod/hooks/register.js" "$mod"/src/*.js)
if grep -nE '\b(2200|1375)\b' "${srcs[@]}" >/dev/null 2>&1; then
  bad 'a memory cap literal appears in the mod source'
else
  ok 'no cap literal in the mod source'
fi

if grep -nE 'sk-[A-Za-z0-9]{8,}|AKIA[0-9A-Z]{16}|BEGIN [A-Z ]*PRIVATE KEY' "${srcs[@]}" >/dev/null 2>&1; then
  bad 'a secret pattern appears in the mod source'
else
  ok 'no secret pattern in the mod source'
fi

if grep -nE 'charCodeAt|codePointAt|Array\.from\([^)]*\)\.length' "${srcs[@]}" >/dev/null 2>&1; then
  bad 'the mod counts characters'
else
  ok 'the mod counts no characters'
fi

# 6. The engine's own validator accepts both manifests.
if command -v claude >/dev/null 2>&1; then
  if claude plugin validate "$market" >/dev/null 2>&1; then
    ok 'claude plugin validate: marketplace'
  else
    bad 'claude plugin validate: marketplace'
  fi
  if claude plugin validate "$mod" >/dev/null 2>&1; then
    ok 'claude plugin validate: plugin'
  else
    bad 'claude plugin validate: plugin'
  fi
  if claude plugin test "$mod" >/dev/null 2>&1; then
    ok 'claude plugin test: the engine loads the module and serves its tools'
  else
    bad 'claude plugin test: the module does not load'
  fi
else
  printf 'skip claude plugin validate/test (claude not on PATH)\n'
fi

if [ "$fails" -ne 0 ]; then
  printf '\n%d check(s) failed\n' "$fails"
  exit 1
fi
printf '\nall checks passed\n'
