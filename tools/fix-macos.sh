#!/usr/bin/env bash
# macOS에서 "jadecode.app이 손상되었습니다" 또는 "확인되지 않은 개발자" 가 뜰 때 고친다.
#
#   curl -fsSL https://raw.githubusercontent.com/jadewisemann/jadecode/main/tools/fix-macos.sh | sh
#
# 하는 일
#   1. 격리 표시(com.apple.quarantine)를 지운다
#   2. **ad-hoc 서명**으로 다시 서명한다 (codesign -s -)
#      — Apple 인증서 없이도 서명이 존재하게 되어 "손상됨"이 사라진다.
#      — 공증(notarization)은 안 되므로 "확인되지 않은 개발자"는 남을 수 있다(우클릭 열기).
#   3. 실행 파일 권한을 확인한다
#   4. 상태를 출력한다
#
# 환경변수
#   JADECODE_APP   앱 경로 (기본: /Applications/jadecode.app)
set -eu

APP="${JADECODE_APP:-/Applications/jadecode.app}"
say() { printf '%s\n' "$*"; }
die() { printf '오류: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || die "macOS 전용 스크립트다"
[ -d "$APP" ] || die "앱을 찾을 수 없다: $APP
  Applications 로 옮겼는지 확인하거나 JADECODE_APP 으로 경로를 지정하라."

EXE="$APP/Contents/MacOS/jadecode-client"
[ -f "$EXE" ] || die "앱 안에 실행 파일이 없다: $EXE"

say "대상: $APP"

# 1) 격리 표시 제거 (앱 전체 + 설치된 CLI 바이너리)
say "[1/4] 격리 표시 제거"
xattr -dr com.apple.quarantine "$APP" 2>/dev/null || true
for b in jadecode jadecode-mcp jadecode-client jc-bench; do
  p="$HOME/.local/bin/$b"
  [ -f "$p" ] && xattr -dr com.apple.quarantine "$p" 2>/dev/null || true
done
say "      완료 (남은 속성: $(xattr -p com.apple.quarantine "$APP" 2>/dev/null || echo '없음'))"

# 2) 실행 권한
say "[2/4] 실행 권한"
chmod +x "$EXE"
say "      $(ls -l "$EXE" | awk '{print $1}') $(basename "$EXE")"

# 3) ad-hoc 서명 — "손상됨"의 가장 흔한 원인(서명 없음)을 없앤다.
say "[3/4] ad-hoc 서명 (codesign -s -)"
if command -v codesign >/dev/null 2>&1; then
  # 중첩된 바이너리부터 서명하고 마지막에 번들 전체를 서명한다.
  codesign --force --timestamp=none --sign - "$EXE" 2>&1 | sed 's/^/      /' || true
  if codesign --force --deep --timestamp=none --sign - "$APP" 2>&1 | sed 's/^/      /'; then
    if codesign --verify --deep --strict "$APP" 2>/dev/null; then
      say "      서명 검증 통과"
    else
      say "      경고: 서명 검증 실패(그래도 실행은 될 수 있다)"
    fi
  else
    say "      경고: 서명 실패. 다음을 직접 실행해 원인을 보라:"
    say "        codesign --force --deep --sign - \"$APP\""
  fi
else
  say "      codesign 없음 — 건너뜀"
fi

# 4) 상태
say "[4/4] 상태"
if codesign -dv "$APP" 2>&1 | grep -q "Signature=adhoc"; then
  say "      서명 종류: ad-hoc (인증서 없음)"
elif codesign -dv "$APP" >/dev/null 2>&1; then
  say "      서명 종류: $(codesign -dv "$APP" 2>&1 | sed -n 's/^Authority=//p' | head -1)"
else
  say "      서명 종류: 없음"
fi
say "      격리 표시: $(xattr -p com.apple.quarantine "$APP" 2>/dev/null || echo '없음')"

say ""
say "이제 실행해 보라:"
say "  open \"$APP\""
say ""
say "그래도 막히면 다음을 차례로 시도하라:"
say "  1. Finder 에서 앱을 우클릭 → 열기 (한 번만 하면 이후엔 더블클릭으로 열린다)"
say "  2. 시스템 설정 → 개인정보 보호 및 보안 → 아래쪽 '그래도 열기'"
say "  3. 터미널에서 직접 실행해 실제 오류를 본다:"
say "       \"$EXE\""
say "  4. 그래도 '손상됨'이면 격리 속성이 다시 붙은 것이다. 다시: xattr -dr com.apple.quarantine \"$APP\""
