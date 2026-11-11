#!/usr/bin/env bash
# 릴리즈 아카이브를 만든다. GitHub Actions(release.yml)와 같은 일을 로컬에서 한다.
#
# 사용: tools/package.sh <tag>            (예: v0.1.0-alpha)
#       tools/package.sh <tag> --target x86_64-unknown-linux-gnu
# 결과: dist/jadecode-<tag>-<platform>.tar.gz (+ .sha256)
set -euo pipefail

TAG=${1:-}
[ -n "$TAG" ] || { echo "usage: tools/package.sh <tag> [--target <triple>]" >&2; exit 2; }
shift || true
TARGET=""
if [ "${1:-}" = "--target" ]; then TARGET=${2:-}; fi

HERE=$(cd "$(dirname "$0")/.." && pwd)
cd "$HERE"

os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
case "$arch" in
  x86_64|amd64) arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
esac
PLATFORM="${os}-${arch}"
if [ -n "$TARGET" ]; then
  case "$TARGET" in
    *x86_64*) arch=x86_64 ;;
    *aarch64*) arch=aarch64 ;;
  esac
  case "$TARGET" in
    *linux*) os=linux ;;
    *darwin*) os=macos ;;
  esac
  PLATFORM="${os}-${arch}"
fi

BINS="jadecode jadecode-mcp jadecode-client jc-bench"
for b in $BINS; do
  [ -x "target/release/$b" ] || { echo "target/release/$b 없음. cargo build --release --workspace --features jc-client/ui --bins 먼저" >&2; exit 3; }
done

DIR="jadecode-${TAG}-${PLATFORM}"
rm -rf "dist/$DIR"
mkdir -p "dist/$DIR"
for b in $BINS; do cp "target/release/$b" "dist/$DIR/"; done
cp README.md SPEC.md "dist/$DIR/"
cp -r docs "dist/$DIR/"

# macOS 에서 돌면 앱 번들도 넣는다(리눅스에서는 건너뛴다). release.yml 과 같은 경로.
if [ "$(uname -s)" = "Darwin" ]; then
  VERSION="${TAG#v}" ./tools/make-app.sh "target/release/jadecode-client" "dist/$DIR"
else
  echo "macOS 가 아니다: .app 번들은 만들지 않는다(리눅스 빌드에서는 건너뜀)"
fi

cat > "dist/$DIR/install.sh" <<'EOS'
#!/bin/sh
# jadecode 설치: 바이너리를 $JADECODE_PREFIX(기본 ~/.local/bin)에 둔다.
# macOS 아카이브에 jadecode.app 이 있으면 /Applications 에도 복사한다.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
DEST="${JADECODE_PREFIX:-$HOME/.local/bin}"
mkdir -p "$DEST"
for b in jadecode jadecode-mcp jadecode-client jc-bench; do
  cp "$HERE/$b" "$DEST/$b"
  chmod +x "$DEST/$b"
  echo "installed $DEST/$b"
done
if [ -d "$HERE/jadecode.app" ]; then
  if [ -w /Applications ]; then
    cp -R "$HERE/jadecode.app" /Applications/
    echo "installed /Applications/jadecode.app"
  else
    echo "건너뜀: /Applications 에 쓸 수 없다. $HERE/jadecode.app 을 직접 옮겨라."
  fi
fi
echo
echo "다음: jadecode init ~/jc/work && cd ~/jc/work && jadecode server"
EOS

chmod +x "dist/$DIR/install.sh"

# 실행에 필요한 공유 라이브러리(리눅스만).
if command -v ldd >/dev/null 2>&1; then
  { ldd "target/release/jadecode-client" 2>/dev/null | awk '{print $1}' | sort -u || true; } > "dist/$DIR/REQUIRES.txt"
fi

( cd dist && tar -czf "${DIR}.tar.gz" "$DIR" )
if command -v sha256sum >/dev/null 2>&1; then
  ( cd dist && sha256sum "${DIR}.tar.gz" > "${DIR}.tar.gz.sha256" )
else
  ( cd dist && shasum -a 256 "${DIR}.tar.gz" > "${DIR}.tar.gz.sha256" )
fi

echo "dist/${DIR}.tar.gz"
cat "dist/${DIR}.tar.gz.sha256"
