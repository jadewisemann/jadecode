#!/usr/bin/env bash
# 클라이언트 시험: 창을 띄우지 않고 데이터 계층(--dump)과 창 생성 여부를 확인한다.
#
# 사용: tools/client.sh [워크스페이스]
# 전제: 서버가 떠 있어야 한다(`jadecode server`). 창 시험은 Xvfb가 있으면 한다.
set -uo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
BIN=${JADECODE_CLIENT:-$HERE/target/debug/jadecode-client}
[ -x "$BIN" ] || { echo "no client binary: $BIN" >&2; exit 2; }
WS=${1:-${JADECODE_WORKSPACE:-$(pwd)}}

fail() { echo "{\"ok\":false,\"step\":\"$1\",\"detail\":\"$2\"}"; exit 1; }

DUMP=$("$BIN" --dump "$WS" 2>&1) || fail dump "$DUMP"
printf '%s' "$DUMP" > /tmp/jc-client-dump.json
python3 -c '
import json,sys
d=json.load(open("/tmp/jc-client-dump.json"))
for k in ("projects","cards","instances","messages","pending_permissions","head_seq"):
    assert k in d, k
print(json.dumps({"ok":True,"step":"dump","projects":len(d["projects"]),"cards":len(d["cards"]),"instances":len(d["instances"]),"messages":len(d["messages"]),"pending":len(d["pending_permissions"]),"head_seq":d["head_seq"]},ensure_ascii=False))
' || fail dump_shape "snapshot missing fields"


# 창 시험: Xvfb가 있으면 창이 실제로 만들어지는지 본다(렌더는 GPU가 필요하다).
if command -v xvfb-run >/dev/null && command -v xdotool >/dev/null; then
  cat > /tmp/jc-client-window.sh <<EOF
#!/bin/bash
cd "\$1" || exit 1
"$BIN" > /tmp/jc-client-window.out 2>&1 &
APP=\$!
sleep 10
N=\$(xdotool search --all --name "" 2>/dev/null | while read w; do
  xdotool getwindowgeometry \$w 2>/dev/null | grep -q "1280x820" && echo x
done | wc -l)
kill \$APP 2>/dev/null
echo "\$N"
EOF
  chmod +x /tmp/jc-client-window.sh
  N=$(timeout 90 xvfb-run -a -s "-screen 0 1400x900x24" /tmp/jc-client-window.sh "$WS" 2>/dev/null | tail -1)
  if [ "${N:-0}" -gt 0 ]; then
    echo "{\"ok\":true,\"step\":\"window\",\"windows\":$N}"
  else
    echo "{\"ok\":false,\"step\":\"window\",\"detail\":\"no 1280x820 window\",\"log\":\"$(head -c 200 /tmp/jc-client-window.out 2>/dev/null | tr '\n' ' ')\"}"
    exit 1
  fi
else
  echo "{\"ok\":true,\"step\":\"window\",\"skipped\":\"xvfb-run or xdotool not installed\"}"
fi
