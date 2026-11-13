#!/usr/bin/env bash
# macOS 배포용 DMG를 만든다. 열면 jadecode.app 과 /Applications 바로가기가 나온다.
#
# 사용:
#   tools/make-dmg.sh <jadecode.app> [출력 dmg]
#   tools/make-dmg.sh --check <dmg>          # 만든 DMG 검사(맥 없이도 구조만 확인)
#
# 환경변수:
#   VERSION   볼륨 이름에 넣을 버전 (기본: 앱 Info.plist 의 CFBundleShortVersionString)
#
# macOS의 hdiutil 이 필요하다. 리눅스에서는 안내하고 종료한다(3).
# 서명·공증은 자리만: CODESIGN_IDENTITY 가 있으면 DMG에도 서명한다.
set -euo pipefail

HERE=$(cd "$(dirname "$0")/.." && pwd)
APP_NAME="jadecode.app"
VOL_BASE="jadecode"

usage() {
  cat >&2 <<'EOS'
usage: tools/make-dmg.sh <jadecode.app> [출력 dmg]
       tools/make-dmg.sh --check <dmg>

  기본 출력: dist/jadecode-<버전>-macos-<arch>.dmg
EOS
}

die() { printf '오류: %s\n' "$*" >&2; exit 1; }
info() { printf '%s\n' "$*"; }

plist_value() { # $1 = plist, $2 = key
  /usr/libexec/PlistBuddy -c "Print :$2" "$1" 2>/dev/null \
    || python3 - "$1" "$2" <<'PY' 2>/dev/null
import plistlib, sys
d = plistlib.load(open(sys.argv[1], 'rb'))
v = d.get(sys.argv[2], "")
print(v)
PY
}

check_dmg() {
  local dmg="$1" rc=0
  [ -f "$dmg" ] || { printf 'FAIL 없음: %s\n' "$dmg" >&2; return 1; }
  # UDZO(DMG) 헤더: 파일 끝 512바이트에 'koly' 매직이 있다.
  if python3 - "$dmg" <<'PY'
import os, sys
p = sys.argv[1]
size = os.path.getsize(p)
if size < 1024:
    print("FAIL 파일이 너무 작다")
    sys.exit(1)
with open(p, 'rb') as f:
    f.seek(-512, 2)
    tail = f.read(512)
ok = tail[0:4] == b'koly'
print(('ok   ' if ok else 'FAIL ') + 'DMG koly trailer')
sys.exit(0 if ok else 1)
PY
  then :; else rc=1; fi
  local size
  size=$(wc -c <"$dmg" | tr -d ' ')
  if [ "$size" -lt 1000000 ]; then
    printf 'FAIL DMG가 너무 작다(%s 바이트)\n' "$size"; rc=1
  else
    printf 'ok   크기 %s 바이트\n' "$size"
  fi
  [ "$rc" -eq 0 ] && info "검사 통과: $dmg"
  return "$rc"
}

# ---------- --check ----------
if [ "${1:-}" = "--check" ]; then
  [ $# -eq 2 ] || { usage; exit 2; }
  if check_dmg "$2"; then exit 0; else exit 1; fi
fi

# ---------- 인자 ----------
APP="${1:-}"
OUT="${2:-}"
[ -n "$APP" ] || { usage; exit 2; }
[ -d "$APP" ] || die "앱 번들이 아니다: $APP"
APP=$(cd "$(dirname "$APP")" && pwd)/$(basename "$APP")

if [ "$(uname -s)" != "Darwin" ]; then
  info "macOS가 아니다($(uname -s)): DMG는 macOS에서만 만들 수 있다(hdiutil 필요)."
  info "  이 환경에서 할 수 있는 검사: tools/make-app.sh --check <app>"
  exit 3
fi
command -v hdiutil >/dev/null 2>&1 || die "hdiutil 이 없다"

# ---------- 메타 ----------
ver="${VERSION:-$(plist_value "$APP/Contents/Info.plist" CFBundleShortVersionString)}"
[ -n "$ver" ] || ver="0.0.0"
case "$(uname -m)" in
  arm64) arch=aarch64 ;;
  x86_64) arch=x86_64 ;;
  *) arch=$(uname -m) ;;
esac
if [ -z "$OUT" ]; then
  mkdir -p "$HERE/dist"
  OUT="$HERE/dist/jadecode-${ver}-macos-${arch}.dmg"
fi
mkdir -p "$(dirname "$OUT")"
rm -f "$OUT"

# ---------- 스테이징 ----------
STAGE=$(mktemp -d "${TMPDIR:-/tmp}/jadecode-dmg.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT INT TERM
cp -R "$APP" "$STAGE/$APP_NAME"
# 드래그 앤 드롭 설치용: /Applications 로 가는 바로가기
ln -s /Applications "$STAGE/Applications"
printf '%s\n' \
  "jadecode ${ver}" \
  "" \
  "1. jadecode.app 을 Applications 폴더로 끌어다 놓으세요." \
  "2. Applications 에서 jadecode.app 을 엽니다." \
  "3. 서버가 필요합니다: 터미널에서  jadecode server" \
  "" \
  "서명되지 않은 빌드라 첫 실행 시 경고가 뜰 수 있습니다." \
  "  Finder 에서 우클릭 → 열기, 또는" \
  "  xattr -dr com.apple.quarantine /Applications/jadecode.app" \
  > "$STAGE/설치 방법.txt"

# ---------- 생성 ----------
vol="$VOL_BASE $ver"
info "DMG 생성: $OUT (볼륨: $vol)"
# UDBZ(bzip2) — hdiutil 지원, UDZO보다 조금 작다.
if ! hdiutil create -volname "$vol" -srcfolder "$STAGE" -ov -format UDBZ "$OUT" >/dev/null; then
  info "UDBZ 실패, UDZO로 재시도"
  hdiutil create -volname "$vol" -srcfolder "$STAGE" -ov -format UDZO "$OUT" >/dev/null
fi

# ---------- 서명(자리) ----------
if [ -n "${CODESIGN_IDENTITY:-}" ] && command -v codesign >/dev/null 2>&1; then
  info "서명: $CODESIGN_IDENTITY"
  codesign --force --timestamp --sign "$CODESIGN_IDENTITY" "$OUT"
  codesign --verify --verbose=2 "$OUT" || die "DMG 서명 검증 실패"
  if [ -n "${NOTARY_PROFILE:-}" ] && command -v xcrun >/dev/null 2>&1; then
    info "공증: $NOTARY_PROFILE"
    xcrun notarytool submit "$OUT" --keychain-profile "$NOTARY_PROFILE" --wait
    xcrun stapler staple "$OUT"
  else
    info "공증: 건너뜀 (NOTARY_PROFILE 없음)"
  fi
else
  info "서명: 건너뜀 (CODESIGN_IDENTITY 없음) — 첫 실행에서 Gatekeeper 경고가 뜬다"
  info "공증: 건너뜀"
fi

# ---------- 검증 ----------
check_dmg "$OUT"

# DMG 안에 앱과 바로가기가 실제로 들어갔는지 확인한다.
MNT=$(mktemp -d "${TMPDIR:-/tmp}/jadecode-mnt.XXXXXX")
if hdiutil attach "$OUT" -mountpoint "$MNT" -nobrowse -readonly >/dev/null 2>&1; then
  [ -x "$MNT/$APP_NAME/Contents/MacOS/jadecode-client" ] \
    && info "ok   DMG 안 $APP_NAME 실행 파일" \
    || { hdiutil detach "$MNT" -quiet || true; rmdir "$MNT"; die "DMG 안 앱이 깨졌다"; }
  [ -L "$MNT/Applications" ] && info "ok   DMG 안 /Applications 바로가기" \
    || info "경고: /Applications 바로가기가 없다"
  hdiutil detach "$MNT" -quiet || true
else
  info "경고: DMG 마운트 실패(검사 생략)"
fi
rmdir "$MNT" 2>/dev/null || true

# 체크섬
shasum -a 256 "$OUT" > "$OUT.sha256"
info ""
info "완료: $OUT"
cat "$OUT.sha256"
