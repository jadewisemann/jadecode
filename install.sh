#!/bin/sh
# jadecode 설치 스크립트 (macOS / Linux)
#
#   curl -fsSL https://raw.githubusercontent.com/jadewisemann/jadecode/main/install.sh | sh
#
# 하는 일
#   1. 플랫폼을 판별해 GitHub Release 아카이브를 내려받는다
#   2. 체크섬(.sha256)을 검증한다
#   3. ~/.local/bin 에 바이너리를, macOS면 /Applications 에 jadecode.app 을 둔다
#   4. macOS에서 서명이 없어 붙는 격리 표시(quarantine)를 지운다(본인용 빌드)
#   5. 워크스페이스가 없으면 만든다
#
# 환경변수
#   JADECODE_VERSION  설치할 태그 (기본: 최신)
#   JADECODE_PREFIX   바이너리 설치 위치 (기본: ~/.local/bin)
#   JADECODE_NO_APP   1이면 .app 설치를 건너뛴다 (macOS)
#   JADECODE_NO_WS    1이면 워크스페이스 생성을 건너뛴다
#   JADECODE_WS       워크스페이스 경로 (기본: ~/jc/work)
set -eu

REPO="jadewisemann/jadecode"
PREFIX="${JADECODE_PREFIX:-$HOME/.local/bin}"
WS="${JADECODE_WS:-$HOME/jc/work}"
TMP=$(mktemp -d 2>/dev/null || mktemp -d -t jadecode)
trap 'rm -rf "$TMP"' EXIT INT TERM

say() { printf '%s\n' "$*"; }
die() { printf '오류: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# ---------- 플랫폼 ----------
os=$(uname -s)
case "$os" in
  Darwin) os=macos ;;
  Linux) os=linux ;;
  *) die "지원하지 않는 OS: $os (macOS 또는 Linux)" ;;
esac

arch=$(uname -m)
case "$arch" in
  arm64|aarch64) arch=aarch64 ;;
  x86_64|amd64) arch=x86_64 ;;
  *) die "지원하지 않는 CPU: $arch" ;;
esac

# 이 저장소가 지금 배포하는 조합만 있다.
platform="${os}-${arch}"
case "$platform" in
  macos-aarch64|linux-x86_64) ;;
  macos-x86_64) die "Intel Mac 빌드는 아직 없다(Apple Silicon만 배포). 소스에서 빌드하려면 https://github.com/$REPO" ;;
  linux-aarch64) die "linux-aarch64 빌드는 아직 없다. 소스에서 빌드하려면 https://github.com/$REPO" ;;
  *) die "빌드가 없는 플랫폼: $platform" ;;
esac

have curl || die "curl 이 필요하다"
have tar || die "tar 가 필요하다"

# ---------- 버전 ----------
tag="${JADECODE_VERSION:-}"
if [ -z "$tag" ]; then
  say "최신 릴리즈 확인 중…"
  tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases" 2>/dev/null \
        | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)
  [ -n "$tag" ] || die "릴리즈를 찾을 수 없다. JADECODE_VERSION=v0.1.1-alpha 처럼 지정해 보라."
fi
say "설치 버전: $tag ($platform)"

# ---------- 내려받기 ----------
name="jadecode-${tag}-${platform}"
base="https://github.com/$REPO/releases/download/$tag"
say "내려받는 중… ${name}.tar.gz"
curl -fsSL -o "$TMP/a.tgz" "$base/${name}.tar.gz" || die "내려받기 실패: $base/${name}.tar.gz"
curl -fsSL -o "$TMP/a.sha" "$base/${name}.tar.gz.sha256" || die "체크섬 내려받기 실패"

# ---------- 체크섬 ----------
expected=$(awk '{print $1; exit}' "$TMP/a.sha")
if [ -z "$expected" ]; then die "체크섬 파일이 비었다"; fi
if have sha256sum; then
  actual=$(sha256sum "$TMP/a.tgz" | awk '{print $1}')
elif have shasum; then
  actual=$(shasum -a 256 "$TMP/a.tgz" | awk '{print $1}')
else
  actual=""
  say "경고: sha256sum/shasum 이 없어 체크섬 검증을 건너뛴다"
fi
if [ -n "$actual" ] && [ "$actual" != "$expected" ]; then
  die "체크섬 불일치(받은 파일이 손상됐거나 변조됐다)"
fi
[ -n "$actual" ] && say "체크섬 OK"

# ---------- 풀기 ----------
tar -xzf "$TMP/a.tgz" -C "$TMP"
src="$TMP/$name"
[ -d "$src" ] || die "아카이브 구조가 예상과 다르다: $name"

# ---------- 바이너리 ----------
mkdir -p "$PREFIX"
for b in jadecode jadecode-mcp jadecode-client jc-bench; do
  [ -f "$src/$b" ] || continue
  cp "$src/$b" "$PREFIX/$b"
  chmod +x "$PREFIX/$b"
done
say "바이너리 설치: $PREFIX"

# PATH 안내
case ":$PATH:" in
  *":$PREFIX:"*) ;;
  *) say "  참고: $PREFIX 가 PATH 에 없다. 셸 설정에 넣어라:"
     say "        export PATH=\"$PREFIX:\$PATH\"" ;;
esac

# ---------- macOS 앱 ----------
if [ "$os" = "macos" ] && [ "${JADECODE_NO_APP:-0}" != "1" ] && [ -d "$src/jadecode.app" ]; then
  dest="/Applications/jadecode.app"
  if [ -w /Applications ]; then
    rm -rf "$dest"
    cp -R "$src/jadecode.app" "$dest"
    say "앱 설치: $dest"
    # 서명이 없는 본인용 빌드라 격리 표시를 지운다. 지우지 않으면 첫 실행이 차단된다.
    if have xattr; then
      xattr -dr com.apple.quarantine "$dest" 2>/dev/null || true
    fi
  else
    say "  건너뜀: /Applications 에 쓸 수 없다. 직접 옮겨라: $src/jadecode.app"
    dest="$src/jadecode.app"
  fi
  if have xattr; then
    for b in jadecode jadecode-mcp jadecode-client jc-bench; do
      [ -f "$PREFIX/$b" ] && xattr -dr com.apple.quarantine "$PREFIX/$b" 2>/dev/null || true
    done
  fi
fi

# ---------- 워크스페이스 ----------
if [ "${JADECODE_NO_WS:-0}" != "1" ]; then
  if [ ! -f "$WS/jadecode.toml" ]; then
    mkdir -p "$(dirname "$WS")"
    if "$PREFIX/jadecode" init "$WS" --name "$(basename "$WS")" >/dev/null 2>&1; then
      say "워크스페이스 생성: $WS"
    else
      say "  워크스페이스 생성 실패. 나중에 직접: $PREFIX/jadecode init $WS"
    fi
  else
    say "워크스페이스 있음: $WS"
  fi
fi

# ---------- 다음 ----------
say ""
say "다음 단계:"
say "  export PATH=\"$PREFIX:\$PATH\""
say "  cd \"$WS\""
say "  jadecode server &          # 서버(유닉스 소켓)"
if [ "$os" = "macos" ] && [ -d "/Applications/jadecode.app" ]; then
  say "  open /Applications/jadecode.app"
else
  say "  jadecode-client            # GUI (서버가 떠 있어야 한다)"
fi
say ""
say "에이전트(claude·opencode·omp·pi)는 각자 설치·로그인이 필요하다: jadecode agents"
