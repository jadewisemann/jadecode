#!/usr/bin/env bash
# jadecode-client 바이너리로 macOS 앱 번들(jadecode.app)을 만든다.
#
# 사용:
#   tools/make-app.sh                          # target/release/jadecode-client → dist/jadecode.app
#   tools/make-app.sh <바이너리> <출력 폴더>
#   tools/make-app.sh --check <jadecode.app>   # 만든 번들의 구조·plist만 검사(맥이 없어도 된다)
#
# 환경변수:
#   VERSION           CFBundleShortVersionString (기본: 루트 Cargo.toml 의 [workspace.package] version)
#   BUNDLE_ID         CFBundleIdentifier (기본: dev.jadewisemann.jadecode)
#   ICON              .icns 경로 (기본: assets/jadecode.icns, tools/jadecode.icns)
#   MIN_MACOS         LSMinimumSystemVersion (기본 14.0)
#   CODESIGN_IDENTITY "Developer ID Application: ..." — 없으면 서명을 건너뛴다
#   NOTARY_PROFILE    notarytool 키체인 프로파일 이름 — 없으면 공증을 건너뛴다
#
# 서명과 공증은 자리만 만들어 두었다. 실제로 하려면 Apple Developer 인증서와
# 앱 암호(키체인 프로파일)가 필요하다. docs/implementation.md 참고.
set -euo pipefail

HERE=$(cd "$(dirname "$0")/.." && pwd)
APP_NAME="jadecode.app"
EXECUTABLE="jadecode-client"
MIN_MACOS="${MIN_MACOS:-14.0}"   # README·릴리즈 노트의 "macOS 14 이상"과 맞춘다.

usage() {
  cat >&2 <<'EOS'
usage: tools/make-app.sh [바이너리] [출력 폴더]
       tools/make-app.sh --check <jadecode.app>

  바이너리   기본 target/release/jadecode-client
  출력 폴더  기본 dist   (그 안에 jadecode.app 을 만든다)
  --check    기존 번들의 필수 경로·실행 권한·Info.plist 를 검사한다.
EOS
}

# 루트 Cargo.toml 의 워크스페이스 버전.
workspace_version() {
  local toml="$HERE/Cargo.toml" v=""
  [ -f "$toml" ] || return 0
  v=$(awk '
    /^\[workspace\.package\]/ { inside = 1; next }
    /^\[/ { inside = 0 }
    inside && /^version[[:space:]]*=/ {
      gsub(/[",]/, "", $3); print $3; exit
    }
  ' "$toml")
  [ -n "$v" ] || v=$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$toml" | head -1)
  printf '%s' "$v"
}

write_plist() { # $1 = 목적지, $2 = 아이콘 파일 이름(없으면 빈 문자열)
  local dest=$1 icon=$2 version=$3
  local icon_key=""
  if [ -n "$icon" ]; then
    icon_key="  <key>CFBundleIconFile</key><string>${icon}</string>"
  fi
  cat > "$dest" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>jadecode</string>
  <key>CFBundleDisplayName</key><string>jadecode</string>
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
  <key>CFBundleExecutable</key><string>${EXECUTABLE}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
${icon_key}
  <key>LSMinimumSystemVersion</key><string>${MIN_MACOS}</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST
}

find_icon() {
  local c
  for c in "${ICON:-}" "$HERE/assets/jadecode.icns" "$HERE/tools/jadecode.icns"; do
    [ -n "$c" ] || continue
    [ -f "$c" ] && { printf '%s' "$c"; return 0; }
  done
  return 0
}

check_app() {
  local app=$1 rc=0 p
  app=${app%/}
  [ -d "$app" ] || { echo "FAIL 번들 없음: $app" >&2; return 1; }

  for p in Contents/Info.plist Contents/MacOS/$EXECUTABLE Contents/Resources; do
    if [ -e "$app/$p" ]; then
      echo "ok   $p"
    else
      echo "FAIL $p 없음"
      rc=1
    fi
  done

  if [ -f "$app/Contents/MacOS/$EXECUTABLE" ] && [ ! -x "$app/Contents/MacOS/$EXECUTABLE" ]; then
    echo "FAIL Contents/MacOS/$EXECUTABLE 실행 권한 없음"
    rc=1
  fi

  if [ -f "$app/Contents/Info.plist" ]; then
    if command -v python3 >/dev/null 2>&1; then
      python3 - "$app/Contents/Info.plist" "$EXECUTABLE" "$app/Contents/Resources" <<'PY' || rc=1
import os, plistlib, sys
path, exe, resources = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path, "rb") as f:
    info = plistlib.load(f)
required = {
    "CFBundleName": "jadecode",
    "CFBundleDisplayName": "jadecode",
    "CFBundleExecutable": exe,
    "CFBundlePackageType": "APPL",
    "LSMinimumSystemVersion": None,
    "CFBundleShortVersionString": None,
    "CFBundleIdentifier": None,
}
bad = []
for key, want in required.items():
    if key not in info:
        bad.append(f"{key} 없음")
    elif want is not None and info[key] != want:
        bad.append(f"{key}={info[key]!r} (기대 {want!r})")
if info.get("NSHighResolutionCapable") is not True:
    bad.append("NSHighResolutionCapable 가 true 가 아님")
icon = info.get("CFBundleIconFile")
if icon and not os.path.isfile(os.path.join(resources, icon)):
    bad.append(f"CFBundleIconFile={icon} 인데 Resources 에 파일이 없다")
if bad:
    for m in bad:
        print(f"FAIL plist: {m}")
    sys.exit(1)
print("ok   Info.plist", info["CFBundleIdentifier"], info["CFBundleShortVersionString"])
PY
    else
      echo "skip Info.plist 검사(python3 없음)"
    fi
  fi

  [ "$rc" -eq 0 ] && echo "검사 통과: $app" || echo "검사 실패: $app"
  return "$rc"
}

# ---------- --check ----------
if [ "${1:-}" = "--check" ]; then
  [ -n "${2:-}" ] || { usage; exit 2; }
  check_app "$2"
  exit $?
fi
if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  usage
  exit 0
fi

BIN=${1:-$HERE/target/release/$EXECUTABLE}
OUT=${2:-$HERE/dist}
case "$BIN" in
  /*) ;;
  *) if [ -d "$(dirname "$BIN")" ]; then
       BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"
     fi ;;
esac

if [ ! -f "$BIN" ]; then
  cat >&2 <<EOS
바이너리가 없다: $BIN
GPUI(화면)를 포함해 먼저 빌드하라:
  cargo build --release -p jc-client --features ui --bins
(--features ui 없이 빌드한 바이너리는 창을 열지 못한다. 데이터 계층만이라면 --dump)
EOS
  exit 3
fi
if [ ! -x "$BIN" ]; then
  echo "바이너리에 실행 권한이 없다: $BIN" >&2
  exit 3
fi

VERSION=${VERSION:-$(workspace_version)}
VERSION=${VERSION:-0.0.0}
# Apple 은 CFBundleShortVersionString 에 숫자·점만 받는다(0.1.0-rc1 은 거부된다).
# 앞의 숫자 부분만 쓰고, 원래 값이 다르면 알려 준다.
NUMERIC_VERSION=$(printf '%s' "$VERSION" | sed 's/^[vV]//' | grep -Eo '^[0-9]+(\.[0-9]+){0,2}' || true)
if [ -z "$NUMERIC_VERSION" ]; then
  echo "VERSION '$VERSION' 에서 숫자 버전을 못 뽑았다. 0.0.0 으로 둔다." >&2
  NUMERIC_VERSION=0.0.0
elif [ "$NUMERIC_VERSION" != "$VERSION" ]; then
  echo "버전: $VERSION → $NUMERIC_VERSION (CFBundleShortVersionString 은 숫자만)"
fi
VERSION=$NUMERIC_VERSION
BUNDLE_ID=${BUNDLE_ID:-dev.jadewisemann.jadecode}

APP="$OUT/$APP_NAME"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BIN" "$APP/Contents/MacOS/$EXECUTABLE"
chmod +x "$APP/Contents/MacOS/$EXECUTABLE"

# CLI(서버·MCP)도 번들에 넣는다. 앱 하나만 설치해도 서버를 띄울 수 있게 하려는 것이다.
# 바이너리는 GUI 옆에 두되 MacOS/ 안에 넣어야 codesign --deep 이 함께 서명한다.
for helper in jadecode jadecode-mcp; do
  # 실행 파일 옆에서 찾는다(릴리즈에서는 target/release 에 같이 있다).
  src=""
  for cand in "$(dirname "$BIN")/$helper" "$HERE/target/release/$helper" "$HERE/target/debug/$helper"; do
    [ -x "$cand" ] && { src="$cand"; break; }
  done
  if [ -n "$src" ]; then
    cp "$src" "$APP/Contents/MacOS/$helper"
    chmod +x "$APP/Contents/MacOS/$helper"
    echo "포함: $helper"
  else
    echo "경고: $helper 를 찾지 못했다(앱에서 서버를 띄울 수 없다)"
  fi
done

# 아이콘은 자리만 둔다. .icns 가 없으면 아무것도 넣지 않고 안내만 한다
# (없는 아이콘을 만들면 Dock 에 물음표가 뜬다).
ICON_FILE=$(find_icon)
ICON_NAME=""
if [ -n "$ICON_FILE" ]; then
  cp "$ICON_FILE" "$APP/Contents/Resources/jadecode.icns"
  ICON_NAME="jadecode.icns"
  echo "아이콘: $ICON_FILE"
else
  echo "아이콘: 없음 — assets/jadecode.icns 를 두면 자동으로 들어간다(지금은 생략)"
fi
write_plist "$APP/Contents/Info.plist" "$ICON_NAME" "$VERSION"

if [ "$(uname -s)" != "Darwin" ]; then
  echo "macOS가 아니다($(uname -s)): 번들 구조만 만들었다. 서명·공증은 macOS에서만 한다."
fi

# ---------- 서명(자리) ----------
# 실제 서명에는 "Developer ID Application" 인증서가 필요하다.
#   CODESIGN_IDENTITY="Developer ID Application: 이름 (TEAMID)" tools/make-app.sh
if [ -n "${CODESIGN_IDENTITY:-}" ] && command -v codesign >/dev/null 2>&1; then
  echo "서명: $CODESIGN_IDENTITY"
  codesign --deep --force --options runtime --timestamp \
    --sign "$CODESIGN_IDENTITY" "$APP"
  codesign --verify --deep --strict --verbose=2 "$APP"
elif command -v codesign >/dev/null 2>&1; then
  # 인증서가 없어도 **ad-hoc 서명**을 붙인다(codesign -s -).
  # 서명이 아예 없으면 macOS(특히 Apple Silicon)가 앱을 "손상됨"으로 판정해 실행을 막는다.
  # ad-hoc 서명은 공증이 아니라서 "확인되지 않은 개발자" 경고는 남지만, "손상됨"은 사라진다.
  echo "서명: ad-hoc (CODESIGN_IDENTITY 없음) — 손상됨 방지용"
  codesign --force --deep --timestamp=none --sign - "$APP" \
    && codesign --verify --deep --strict "$APP" \
    && echo "서명 검증 통과 (ad-hoc)"
else
  echo "서명: 건너뜀 (codesign 없음 — macOS가 아니다)"
fi

# ---------- 공증(자리) ----------
# 실제 공증에는 Apple Developer 계정·앱 암호·팀 id가 필요하다. 한 번만:
#   xcrun notarytool store-credentials jadecode \
#     --apple-id <id> --team-id <TEAMID> --password <앱 암호>
# 그다음:
#   NOTARY_PROFILE=jadecode CODESIGN_IDENTITY="Developer ID Application: ..." tools/make-app.sh
# notarytool 은 디렉터리를 못 받으므로 zip 으로 묶어 올린다.
if [ -n "${NOTARY_PROFILE:-}" ] && command -v xcrun >/dev/null 2>&1; then
  echo "공증: $NOTARY_PROFILE"
  TMPZIP=$(mktemp -d)/jadecode.zip
  ditto -c -k --keepParent "$APP" "$TMPZIP"
  xcrun notarytool submit "$TMPZIP" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$APP"
  rm -rf "$(dirname "$TMPZIP")"
elif [ -n "${NOTARY_PROFILE:-}" ]; then
  echo "공증: 건너뜀 (xcrun 없음 — macOS가 아니다)"
else
  echo "공증: 건너뜀 (NOTARY_PROFILE 없음) — 설치 파일(docs/implementation.md) 참고"
fi

echo
check_app "$APP"
echo "$APP"
