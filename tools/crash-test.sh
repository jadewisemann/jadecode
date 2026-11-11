#!/usr/bin/env bash
# 크래시 반복 시험: 서버에 이벤트를 밀어 넣는 중 `kill -9`를 여러 번 하고,
# 매번 다시 띄워 일지·색인이 맞는지(`jadecode check`) 확인한다.
#
# 사용: tools/crash-test.sh [반복횟수] [이벤트수]
# 결과: 마지막에 `{"rounds":N,"ok":true,...}` 한 줄(JSON). 실패하면 종료 코드 1.
set -uo pipefail

ROUNDS=${1:-8}
EVENTS=${2:-80}
BIN=${JADECODE_BIN:-$(cd "$(dirname "$0")/.." && pwd)/target/debug/jadecode}
if [ ! -x "$BIN" ]; then
  echo "no binary at $BIN (cargo build 먼저)" >&2
  exit 2
fi
command -v jq >/dev/null || { echo "jq 필요" >&2; exit 2; }

WS=$(mktemp -d "${TMPDIR:-/tmp}/jc-crash-XXXXXX")
trap 'pkill -f "jadecode server --socket $WS" 2>/dev/null; rm -rf "$WS"' EXIT
"$BIN" init "$WS" --name crash >/dev/null
( cd "$WS" && "$BIN" project add p1 >/dev/null && "$BIN" card add c1 --project p1 --path refine,implement >/dev/null )
CARD=$( cd "$WS" && "$BIN" card list | head -1 | cut -f1 )

start_server() {
  ( cd "$WS" && exec "$BIN" server >"$WS/.server.log" 2>&1 ) &
  SRV=$!
  for _ in $(seq 1 200); do
    [ -S "$WS/.jadecode/jadecode.sock" ] && return 0
    sleep 0.02
  done
  echo "server did not start" >&2; exit 3
}

lost=0
for r in $(seq 1 "$ROUNDS"); do
  start_server
  # 이벤트를 밀어 넣는 중에 죽인다.
  (
    for i in $(seq 1 "$EVENTS"); do
      printf '{"jsonrpc":"2.0","id":%d,"method":"card.append_event","params":{"card":"'"$CARD"'","kind":"msg.note","body":{"round":%d,"i":%d}}}\n' "$i" "$r" "$i"
    done
  ) | ( cd "$WS" && "$BIN" rpc-stream 2>/dev/null ) >/dev/null 2>&1 &
  sleep "0.$(printf '%03d' $((RANDOM % 300 + 50)))"
  kill -9 "$SRV" 2>/dev/null
  wait "$SRV" 2>/dev/null
  # 재시작해서 복구 결과를 본다.
  start_server
  OK=$( ( cd "$WS" && "$BIN" check ) | jq -r .ok )
  if [ "$OK" != "true" ]; then
    echo "round $r: check failed" >&2
    ( cd "$WS" && "$BIN" check ) >&2
    lost=$((lost + 1))
  fi
  kill "$SRV" 2>/dev/null; wait "$SRV" 2>/dev/null
done

TOTAL=$( ( cd "$WS" && "$BIN" tail --limit 100000 ) | wc -l )
HEAD=$( ( cd "$WS" && "$BIN" check ) | jq -r .last_seq )
GAPS=$( ( cd "$WS" && "$BIN" check ) | jq -r .seq_gaps )
jq -nc --argjson rounds "$ROUNDS" --argjson bad "$lost" --argjson events "$TOTAL" \
  --argjson head_seq "$HEAD" --argjson gaps "$GAPS" \
  '{rounds:$rounds, failed_rounds:$bad, journal_lines:$events, head_seq:$head_seq, seq_gaps:$gaps, ok:($bad==0 and $gaps==0)}'
[ "$lost" -eq 0 ] && [ "$GAPS" -eq 0 ]
