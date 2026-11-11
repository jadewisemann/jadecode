#!/usr/bin/env bash
# 여러 에이전트 백엔드 시험: claude / opencode / omp / pi 각각으로 카드 하나를 끝까지 돌린다.
#
# 사용: tools/agents.sh [에이전트...]   (기본: 설치된 것 전부)
# 전제: 각 CLI 로그인 또는 API 키. pi는 DEEPSEEK_API_KEY 등이 필요하다.
# 결과: 에이전트마다 JSON 한 줄. 하나라도 실패하면 종료 코드 1.
set -uo pipefail

HERE=$(cd "$(dirname "$0")/.." && pwd)
BIN=${JADECODE_BIN:-$HERE/target/release/jadecode}
[ -x "$BIN" ] || BIN=$HERE/target/debug/jadecode
MCP=${JADECODE_MCP:-$HERE/target/release/jadecode-mcp}
[ -x "$MCP" ] || MCP=$HERE/target/debug/jadecode-mcp
[ -x "$BIN" ] || { echo "no jadecode binary" >&2; exit 2; }
[ -x "$MCP" ] || { echo "no jadecode-mcp" >&2; exit 2; }
export JADECODE_MCP="$MCP" JADECODE_BIN="$BIN"

AGENTS=("$@")
if [ ${#AGENTS[@]} -eq 0 ]; then AGENTS=(claude opencode omp pi); fi

WS=$(mktemp -d "${TMPDIR:-/tmp}/jc-agents-XXXXXX")
KEEP=${JADECODE_KEEP:-0}
cleanup() {
  if [ "$KEEP" = "1" ]; then echo "kept: $WS" >&2; return; fi
  if [ -f "$WS/.jadecode/server.json" ]; then
    PID=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$WS/.jadecode/server.json")
    [ -n "${PID:-}" ] && kill "$PID" 2>/dev/null
  fi
  rm -rf "$WS"
}
trap cleanup EXIT

"$BIN" init "$WS" --name agents >/dev/null || exit 3
cd "$WS" || exit 3
"$BIN" server >"$WS/.server.log" 2>&1 &
for _ in $(seq 1 300); do [ -S "$WS/.jadecode/jadecode.sock" ] && break; sleep 0.02; done
"$BIN" project add api >/dev/null || exit 3

FAILED=0
for A in "${AGENTS[@]}"; do
  READY=$("$BIN" agents | awk -v a="$A" '$1==a{print $3}')
  if [ "$READY" != "yes" ]; then
    echo "{\"agent\":\"$A\",\"ok\":false,\"skip\":\"not installed\"}"
    continue
  fi
  MODEL=${JADECODE_TEST_MODEL:-}
  EXTRA=()
  case "$A" in
    pi) EXTRA=(--model "${MODEL:-deepseek/deepseek-v4-flash}") ;;
    claude) EXTRA=(--model "${MODEL:-haiku}") ;;
    *) [ -n "$MODEL" ] && EXTRA=(--model "$MODEL") ;;
  esac
  CARD=$("$BIN" card add "$A worker probe" --project api --path implement | tail -1)
  B=$(basename "$CARD")
  FILE="probe-$A.txt"
  python3 - "$CARD/task.md" "$FILE" "$A" <<'PY'
import sys
p, fname, agent = sys.argv[1:4]
s = open(p).read()
s = s.split('\n## 목표\n')[0] + f"""
## 목표

`{fname}` 파일을 만든다. 내용은 `agent ok: {agent}` 한 줄.

## 완료 조건

- {fname} 파일이 존재하고 내용이 정확하다.
- 끝나면 report(status=done, summary=...) 도구로 반드시 보고한다.
"""
open(p, 'w').write(s)
PY
  sleep 1
  "$BIN" instance start worker --project api --card "$B" --agent "$A" "${EXTRA[@]}" >/dev/null 2>&1
  REPORT=""
  DEADLINE=$((SECONDS + 420))
  while [ $SECONDS -lt $DEADLINE ]; do
    sleep 4
    P=$("$BIN" permission list 2>/dev/null | head -1 | cut -f1)
    [ -n "${P:-}" ] && "$BIN" permission allow "$P" "agents.sh auto" >/dev/null 2>&1
    REPORT=$("$BIN" tail --topics "project/api/card/$B" --limit 400 2>/dev/null | python3 -c '
import json,sys
r=""
for l in sys.stdin:
    e=json.loads(l)
    if e["kind"]=="msg.report_done": r="done"
    elif e["kind"]=="msg.report" and e["body"].get("status")=="blocked": r="blocked"
print(r)')
    [ -n "$REPORT" ] && break
  done
  CONTENT=""
  [ -f "$CARD/$FILE" ] && CONTENT=$(head -c 60 "$CARD/$FILE")
  [ "$REPORT" = "done" ] && [ -n "$CONTENT" ] || FAILED=$((FAILED + 1))
  python3 - "$A" "$REPORT" "$FILE" "$CONTENT" <<'PY'
import json, sys
agent, report, fname, content = sys.argv[1:5]
ok = report == "done" and bool(content)
print(json.dumps({"agent": agent, "ok": ok, "report": report or "(none)",
                  "file": fname, "content": content[:60]}, ensure_ascii=False))
sys.exit(0 if ok else 1)
PY
  for i in $("$BIN" instance list 2>/dev/null | cut -f1); do
    "$BIN" instance stop "$i" >/dev/null 2>&1
  done
done

echo "{\"failed\":$FAILED}" >&2
exit $([ "$FAILED" -eq 0 ] && echo 0 || echo 1)
