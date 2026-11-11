#!/usr/bin/env bash
# 오케스트레이터 깨우기 시험: 워커가 낸 이벤트가 다이제스트 사용자 턴으로 오케스트레이터에 도착하는가.
#
# 사용: tools/wake.sh [모델]
# 전제: `claude` 로그인(구독), `cargo build` 완료.
set -uo pipefail

MODEL=${1:-haiku}
HERE=$(cd "$(dirname "$0")/.." && pwd)
BIN=${JADECODE_BIN:-$HERE/target/debug/jadecode}
MCP=${JADECODE_MCP:-$HERE/target/debug/jadecode-mcp}
[ -x "$BIN" ] || { echo "no jadecode binary" >&2; exit 2; }
export JADECODE_MCP="$MCP"

WS=$(mktemp -d "${TMPDIR:-/tmp}/jc-wake-XXXXXX")
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

"$BIN" init "$WS" --name wake >/dev/null || fail init x
cd "$WS" || fail cd x
"$BIN" server >"$WS/.server.log" 2>&1 &
for _ in $(seq 1 300); do [ -S "$WS/.jadecode/jadecode.sock" ] && break; sleep 0.02; done
[ -S "$WS/.jadecode/jadecode.sock" ] || fail server nosocket
"$BIN" project add api >/dev/null || fail project x
CARD=$("$BIN" card add "orchestrator wake probe" --project api --path implement,verify | tail -1)
BASE=$(basename "$CARD")

# 오케스트레이터를 띄운다. 첫 턴이 끝나야(init + 첫 응답) 깨우기 커서가 잡힌다.
O=$("$BIN" instance start orchestrator --project api --model "$MODEL" \
      | python3 -c 'import json,sys; print(json.load(sys.stdin)["instance"]["id"])')
[ -n "$O" ] || fail orch start

# 커서가 자리 잡을 때까지 조금 기다린다(첫 턴 소요).
sleep 25

# 워커가 낸 것과 같은 모양의 이벤트를 넣는다(실제 워커 없이 깨우기 경로만 본다).
SEQ=$("$BIN" rpc events.append "{\"topic\":\"project/api/card/$BASE\",\"kind\":\"msg.ask\",\"body\":{\"question\":\"wake-probe 신호인가?\",\"summary\":\"WAKE_PROBE\",\"actor\":{\"role\":\"worker\",\"instance\":\"w-test\",\"card\":\"work/api/$BASE\"}}}" \
      | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["seq"])')
[ -n "$SEQ" ] || fail append x

# 오케스트레이터가 다이제스트를 받고 ack를 남길 때까지 기다린다.
DEADLINE=$((SECONDS + 240))
ACK=""
while [ $SECONDS -lt $DEADLINE ]; do
  sleep 4
  ACK=$("$BIN" tail --topics "project/api" --limit 300 | python3 -c '
import json,sys
for l in sys.stdin:
    e=json.loads(l)
    if e["kind"]=="msg.ack" and e["body"].get("actor",{}).get("role")=="orchestrator":
        print(e["body"].get("upto")); break')
  [ -n "$ACK" ] && break
done

DIGESTS=$("$BIN" tail --limit 500 | python3 -c '
import json,sys
n=0
for l in sys.stdin:
    e=json.loads(l)
    if e["kind"]=="instance.wake_digest": n+=1
print(n)')
INSTANCE_OUT=$("$BIN" tail --topics "project/api" --limit 500 | python3 -c '
import json,sys
for l in sys.stdin:
    e=json.loads(l)
    if e["kind"]=="instance.output": print(e["body"].get("text","")[:200]); break')

"$BIN" instance stop "$O" >/dev/null 2>&1

python3 - "$SEQ" "$ACK" "${DIGESTS:-0}" "${INSTANCE_OUT:-}" <<'PY'
import json, sys
seq, ack, digests, out = sys.argv[1:5]
ok = bool(ack) and int(ack or 0) >= int(seq)
print(json.dumps({"ok": ok, "event_seq": int(seq), "orchestrator_ack_upto": int(ack or 0),
                  "wake_digests": int(digests), "orchestrator_first_output": out[:160]}, ensure_ascii=False))
sys.exit(0 if ok else 1)
PY
