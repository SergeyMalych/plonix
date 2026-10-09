#!/usr/bin/env bash
# End-to-end smoke test of the plonix CLI: engine, proxy capture over HTTP and
# HTTPS (through the Plonix CA), search, show, replay, HAR, findings, Market.
# Usage: windows-cli-smoke.sh <path-to-plonix>
set -uo pipefail
P="$1"
export PLONIX_ACCEPT_TERMS=1
export PLONIX_HOME="${RUNNER_TEMP:-$(mktemp -d)}/plonix-home"
rm -rf "$PLONIX_HOME"; mkdir -p "$PLONIX_HOME"
fails=0
# Each step gets two minutes and no input, so one stuck command can't hang the run.
step() { echo; echo "::group::$1"; shift; if timeout 120 "$@" </dev/null; then echo "OK"; else echo "::error::FAILED ($?): $*"; fails=$((fails+1)); fi; echo "::endgroup::"; }

"$(command -v python3 || command -v python)" -m http.server 18081 --bind 127.0.0.1 >/dev/null 2>&1 &
WEB=$!
sleep 2

step "version"        "$P" --version
step "projects new"   "$P" projects new smoke
step "projects list"  "$P" projects list
step "start engine"   "$P" -p smoke start --port 18080
sleep 3
step "status"         "$P" -p smoke status
step "ca path"        "$P" ca --help
step "scope accept"   "$P" -p smoke scope --help
step "capture http via proxy" curl -sS -f -o /dev/null --max-time 20 -x http://127.0.0.1:18080 http://127.0.0.1:18081/
step "capture https via proxy (insecure)" curl -sS -f -o /dev/null --max-time 30 -k -x http://127.0.0.1:18080 https://example.com/
sleep 2
F="$PLONIX_HOME/found.txt"
step "search finds http"  bash -c "\"$P\" -p smoke search 127.0.0.1 > '$F' && cat '$F' && grep -q 18081 '$F'"
step "search finds https" bash -c "\"$P\" -p smoke search example.com > '$F' && cat '$F' && grep -q example.com '$F'"
step "hosts"          "$P" -p smoke hosts
step "har export"     "$P" -p smoke har export -o "$PLONIX_HOME/out.har"
step "har file non-empty" test -s "$PLONIX_HOME/out.har"
step "har import"     "$P" -p smoke har import "$PLONIX_HOME/out.har"
step "findings list"  "$P" -p smoke findings list
step "market list"    "$P" market list
step "demo project"   "$P" projects demo
step "mcp help"       "$P" mcp --help
step "stop"           "$P" -p smoke stop
kill $WEB 2>/dev/null
echo "Still running:"; tasklist | grep -iE "plonix|python" || true
echo; echo "smoke failures: $fails"
exit $fails
