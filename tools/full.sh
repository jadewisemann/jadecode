#!/usr/bin/env bash
# 전체 흐름 시험: 사람이 오케스트레이터에게 말하면, 오케스트레이터가 카드를 만들고
# 워커를 띄우고, 워커가 일을 끝내 보고하면 오케스트레이터가 깨어나 처리한다.
#
# 사용: tools/full.sh [모델]
# 전제: `claude` 로그인(구독), `cargo build` 완료. 시간이 걸린다(기본 최대 10분).
set -uo pipefail

MODEL=${1:-haiku}
HERE=$(cd "$(dirname "$0")/.." && pwd)
BIN=${JADECODE_BIN:-$HERE/target/debug/jadecode}
MCP=${JADECODE_MCP:-$HERE/target/debug/jadecode-mcp}
[ -x "$BIN" ] || { echo "no jadecode binary" >&2; exit 2; }
[ -x "$MCP" ] || { echo "no jadecode-mcp" >&2; exit 2; }
export JADECODE_MCP="$MCP"

WS=$(mktemp -d "${TMPDIR:-/tmp}/jc-full-XXXXXX")
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
fail() { echo "{\"ok\":false,\"step\":\"$1\",\"detail\":\"$2\"}"; exit 1; }

"$BIN" init "$WS" --name full >/dev/null || fail init x
cd "$WS" || fail cd x
"$BIN" server >"$WS/.server.log" 2>&1 &
for _ in $(seq 1 300); do [ -S "$WS/.jadecode/jadecode.sock" ] && break; sleep 0.02; done
[ -S "$WS/.jadecode/jadecode.sock" ] || fail server nosocket
"$BIN" project add api >/dev/null || fail project x

# 1) 오케스트레이터를 띄운다.
O=$("$BIN" instance start orchestrator --project api --model "$MODEL" \
      | python3 -c 'import json,sys; print(json.load(sys.stdin)["instance"]["id"])')
[ -n "$O" ] || fail orch start
sleep 20   # 첫 턴(브리프 → 응답)이 끝날 시간

# 2) 사람이 오케스트레이터에게 말한다: 카드 하나 만들고 워커로 돌려라.
"$BIN" instance send "$O" "API 프로젝트에 카드 하나를 만들어 진행해라. 카드 제목은 'write hello.txt', 경로는 implement 하나. 파일은 카드 폴더에 hello.txt로 내용 'hello from jadecode'를 쓰는 일이다. 워커(worker, 모델 $MODEL)를 띄워 그 카드에서 일하게 하고, 워커가 끝내면 결과를 확인해라." >/dev/null \
  || fail send "instance.send"

# 3) 오케스트레이터가 카드를 만들고 워커를 띄울 때까지 기다린다.
DEADLINE=$((SECONDS + 300))
CARD=""
WORKER=""
while [ $SECONDS -lt $DEADLINE ]; do
  sleep 4
  CARD=$("$BIN" card list 2>/dev/null | head -1 | cut -f4)
  WORKER=$("$BIN" instance list 2>/dev/null | awk -F'\t' '$2=="worker"{print $1; exit}')
  # 권한 요청이 오면 사람처럼 승인한다(e2e).
  PEND=$("$BIN" permission list 2>/dev/null | head -1 | cut -f1)
  [ -n "${PEND:-}" ] && "$BIN" permission allow "$PEND" "full.sh auto-allow" >/dev/null 2>&1
  [ -n "$CARD" ] && [ -n "$WORKER" ] && break
done
[ -n "$CARD" ] || fail orchestrator "카드가 만들어지지 않았다"
[ -n "$WORKER" ] || fail orchestrator "워커가 띄워지지 않았다"

# 4) 워커가 보고할 때까지 기다린다(권한은 계속 승인).
REPORT=""
DEADLINE=$((SECONDS + 420))
while [ $SECONDS -lt $DEADLINE ]; do
  sleep 4
  PEND=$("$BIN" permission list 2>/dev/null | head -1 | cut -f1)
  [ -n "${PEND:-}" ] && "$BIN" permission allow "$PEND" "full.sh auto-allow" >/dev/null 2>&1
  REPORT=$("$BIN" tail --topics "project/api/card/$(basename "$CARD")" --limit 400 2>/dev/null \
    | python3 -c '
import json,sys
r=""
for l in sys.stdin:
    e=json.loads(l)
    if e["kind"]=="msg.report_done": r="done"
    elif e["kind"]=="msg.report" and e["body"].get("status")=="blocked": r="blocked"
print(r)')
  [ -n "$REPORT" ] && break
done
[ -n "$REPORT" ] || fail worker "워커 보고 없음"

# 5) 오케스트레이터가 깨어나 보고를 처리했는지(다이제스트·ack) 확인한다.
DIGEST=""
ACK=""
DEADLINE=$((SECONDS + 240))
while [ $SECONDS -lt $DEADLINE ]; do
  sleep 4
  PEND=$("$BIN" permission list 2>/dev/null | head -1 | cut -f1)
  [ -n "${PEND:-}" ] && "$BIN" permission allow "$PEND" "full.sh auto-allow" >/dev/null 2>&1
  DIGEST=$("$BIN" tail --limit 800 2>/dev/null | python3 -c '
import json,sys
n=0
for l in sys.stdin:
    e=json.loads(l)
    if e["kind"]=="instance.wake_digest": n+=1
print(n)')
  ACK=$("$BIN" tail --topics project/api --limit 800 2>/dev/null | python3 -c '
import json,sys
a=0
for l in sys.stdin:
    e=json.loads(l)
    if e["kind"]=="msg.ack" and e["body"].get("actor",{}).get("role")=="orchestrator":
        a=e["body"].get("upto",0)
print(a)')
  [ "${ACK:-0}" -gt 0 ] && break
done

FILE_OK=null
[ -f "$WS/$CARD/hello.txt" ] && FILE_OK=true
CONTENT=""
[ -f "$WS/$CARD/hello.txt" ] && CONTENT=$(head -c 60 "$WS/$CARD/hello.txt")
CHECK=$("$BIN" check | python3 -c 'import json,sys; print(str(json.load(sys.stdin)["ok"]).lower())')

"$BIN" instance list 2>/dev/null | awk -F'\t' '{print $1}' | while read -r i; do
  [ -n "$i" ] && "$BIN" instance stop "$i" >/dev/null 2>&1
done

python3 - "${DIGEST:-0}" "${ACK:-0}" "$REPORT" "$FILE_OK" "$CONTENT" "$CHECK" "$CARD" <<'PY'
import json, sys
digest, ack, report, file_ok, content, check, card = sys.argv[1:8]
ok = (
    report == "done"
    and int(digest or 0) > 0
    and int(ack or 0) > 0
    and check == "true"
    and file_ok == "true"
)
print(json.dumps({
    "ok": ok, "card": card, "worker_report": report, "hello_txt": file_ok,
    "hello_txt_content": content[:60], "wake_digests": int(digest or 0),
    "orchestrator_ack_upto": int(ack or 0), "journal_check_ok": check,
}, ensure_ascii=False))
sys.exit(0 if ok else 1)
PY
