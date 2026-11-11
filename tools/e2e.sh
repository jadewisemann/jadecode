#!/usr/bin/env bash
# 끝에서 끝까지 시험: 워크스페이스 → 카드 → 워커 인스턴스 → MCP 보고 → 단계 이동.
#
# 사용: tools/e2e.sh [모델] [카드 지시]
# 전제: `claude` 로그인(구독), `cargo build` 완료.
# 결과: 마지막에 JSON 한 줄. 실패하면 종료 코드 1.
set -uo pipefail

MODEL=${1:-haiku}
TASK=${2:-"Create a file greet.sh that prints exactly: hello jadecode. Make it executable."}
HERE=$(cd "$(dirname "$0")/.." && pwd)
BIN=${JADECODE_BIN:-$HERE/target/debug/jadecode}
MCP=${JADECODE_MCP:-$HERE/target/debug/jadecode-mcp}
[ -x "$BIN" ] || { echo "no jadecode binary at $BIN" >&2; exit 2; }
[ -x "$MCP" ] || { echo "no jadecode-mcp at $MCP" >&2; exit 2; }
export JADECODE_MCP="$MCP"

WS=$(mktemp -d "${TMPDIR:-/tmp}/jc-e2e-XXXXXX")
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

fail() { echo "{\"ok\":false,\"step\":\"$1\",\"detail\":\"$2\"}" ; exit 1; }

"$BIN" init "$WS" --name e2e >/dev/null || fail init "init failed"
cd "$WS" || fail cd "$WS"
"$BIN" server >"$WS/.server.log" 2>&1 &
for _ in $(seq 1 300); do [ -S "$WS/.jadecode/jadecode.sock" ] && break; sleep 0.02; done
[ -S "$WS/.jadecode/jadecode.sock" ] || fail server "no socket"

"$BIN" project add api >/dev/null || fail project "project add"
CARD=$("$BIN" card add "$TASK" --project api --path implement,verify | tail -1)
[ -d "$CARD" ] || fail card "card add: $CARD"
CARDREL="work/api/$(basename "$CARD")"

START=$("$BIN" instance start worker --project api --card "$(basename "$CARD")" --model "$MODEL")
INST=$(echo "$START" | python3 -c 'import json,sys; print(json.load(sys.stdin)["instance"]["id"])')
[ -n "$INST" ] || fail instance "start: $START"

# 워커가 일을 끝낼 때까지 기다린다(최대 6분).
DEADLINE=$((SECONDS + 360))
APPROVED=0
REPORTED=""
while [ $SECONDS -lt $DEADLINE ]; do
  sleep 3
  KINDS=$("$BIN" tail --topics "project/api/card/$(basename "$CARD")" --limit 200 | python3 -c '
import json,sys
ks=[]
for l in sys.stdin:
    e=json.loads(l); ks.append(e["kind"])
print(",".join(ks))')
  case "$KINDS" in
    *msg.report_done*) REPORTED=done; break;;
    *msg.report*blocked*) REPORTED=blocked; break;;
    *instance.failed*) fail worker "instance.failed";;
  esac
  # 사람처럼 권한 요청에 답한다(첫 요청만). 정책 밖 요청은 e2e에서 승인한다.
  PEND=$("$BIN" permission list | head -1 | cut -f1)
  if [ -n "${PEND:-}" ]; then
    "$BIN" permission allow "$PEND" "e2e auto-allow" >/dev/null 2>&1
    APPROVED=$((APPROVED+1))
  fi
done
[ -n "$REPORTED" ] || fail timeout "no report within 6min"

# 파일이 실제로 생겼는지(지시가 그랬다면)
FILE_OK=null
if [ -f "$CARD/greet.sh" ]; then
  if [ -x "$CARD/greet.sh" ]; then FILE_OK=true; else FILE_OK=missing_exec; fi
fi

# 보고가 오면 카드 상태가 waiting_human/blocked로 바뀌어 있어야 한다(MCP report가 상태 칸도 쓴다).
STATUS=$("$BIN" card list | awk -v c="$CARDREL" '$4==c{print $3}')

# 단계 이동(RPC)도 되는지
NEXT=$("$BIN" rpc card.advance "{\"card\":\"$(basename "$CARD")\"}" 2>&1)
CHECK=$("$BIN" check | python3 -c 'import json,sys; print(json.load(sys.stdin)["ok"])')

"$BIN" instance stop "$INST" >/dev/null 2>&1
echo "permissions_approved=$APPROVED" >&2

python3 - "$INST" "$REPORTED" "$STATUS" "$FILE_OK" "$NEXT" "$CHECK" "$MODEL" <<'PY'
import json, sys
inst, reported, status, file_ok, nxt, check, model = sys.argv[1:8]
try:
    adv = json.loads(nxt)
except Exception:
    adv = {"raw": nxt}
stage = (adv.get("result") or {}).get("stage") or adv.get("stage")
ok = reported == "done" and str(check).lower() == "true" and bool(stage)
print(json.dumps({
    "ok": bool(ok), "instance": inst, "model": model, "report": reported,
    "card_status_after_report": status, "greet_sh": file_ok,
    "advance_stage": (adv.get("result") or {}).get("stage") or adv.get("stage"), "journal_check_ok": check,
}, ensure_ascii=False))
sys.exit(0 if ok else 1)
PY
