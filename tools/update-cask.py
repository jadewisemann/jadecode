#!/usr/bin/env python3
"""릴리즈가 만들어지면 Homebrew cask 파일을 갱신해 tap 저장소에 커밋한다.

Homebrew 로 설치하면 macOS 가 quarantine 표시를 붙이지 않는다.
그래서 "손상됨"·"확인되지 않은 개발자" 를 매번 겪지 않는다(사용자 요구).

사용:
  tools/update-cask.py --tag v0.1.2-alpha [--tap <tap 디렉터리>] [--dry-run]

--tap 을 주면 그 디렉터리(예: ~/code/repository/homebrew-jadecode)의
Casks/jadecode.rb 를 갱신하고 커밋한다. 없으면 stdout 으로만 낸다.

SHA 는 릴리즈의 .dmg.sha256 자산에서 읽는다(파일을 다시 받지 않는다).
"""
import argparse
import json
import subprocess
import sys
import urllib.request
from pathlib import Path

REPO = "jadewisemann/jadecode"


def gh_api(url):
    req = urllib.request.Request(url, headers={"User-Agent": "jadecode-cask"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


def fetch_text(url):
    req = urllib.request.Request(url, headers={"User-Agent": "jadecode-cask"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read().decode()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag", required=True, help="릴리즈 태그 (예: v0.1.2-alpha)")
    ap.add_argument("--tap", help="tap 저장소 경로 (있으면 파일을 쓰고 커밋)")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    tag = args.tag
    ver = tag.lstrip("v")
    # Homebrew 의 version 은 숫자만 쓰는 편이 안전하다(-alpha 는 버전 비교에 방해).
    num = ver.split("-")[0]
    # DMG 는 태그의 v 를 뺀 이름으로 올라간다(make-dmg.sh 가 버전만 쓴다).
    asset = f"jadecode-{ver}-macos-aarch64.dmg"

    rel = gh_api(f"https://api.github.com/repos/{REPO}/releases/tags/{tag}")
    names = {a["name"]: a["browser_download_url"] for a in rel.get("assets", [])}
    if asset not in names:
        print(f"오류: 자산이 없다: {asset}", file=sys.stderr)
        print("있는 자산: " + ", ".join(sorted(names)), file=sys.stderr)
        return 1

    sha_name = asset + ".sha256"
    if sha_name not in names:
        print(f"오류: 체크섬 자산이 없다: {sha_name}", file=sys.stderr)
        return 1
    sha_text = fetch_text(names[sha_name])
    sha = sha_text.split()[0]
    if len(sha) != 64:
        print(f"오류: 체크섬 형식이 이상하다: {sha_text[:80]}", file=sys.stderr)
        return 1

    url = names[asset]
    cask = f'''cask "jadecode" do
  version "{num}"
  sha256 "{sha}"

  url "{url}",
      verified: "github.com/{REPO}/"
  name "jadecode"
  desc "Task-driven kanban where orchestrator bots move cards and worker bots do the work"
  homepage "https://github.com/{REPO}"

  depends_on macos: ">= :sonoma"
  depends_on arch: :arm64

  app "jadecode.app"

  # 서버·CLI 는 앱 번들 안에 있지만, 터미널에서 쓰려면 심볼릭 링크를 두는 편이 낫다.
  binary "#{{appdir}}/jadecode.app/Contents/MacOS/jadecode"
  binary "#{{appdir}}/jadecode.app/Contents/MacOS/jadecode-mcp"

  caveats <<~EOS
    앱을 열기 전에 서버가 필요합니다(앱이 없으면 자동으로 띄웁니다):

      jadecode run --open

    워크스페이스 기본 위치: ~/jc/work
    에이전트(claude·opencode·omp·pi)는 각자 설치·로그인이 필요합니다:

      jadecode agents
  EOS
end
'''

    if args.dry_run or not args.tap:
        print(cask)
        return 0

    tap = Path(args.tap).expanduser()
    out = tap / "Casks" / "jadecode.rb"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(cask)
    print(f"썼다: {out}", file=sys.stderr)

    # 커밋(작성자는 저장소 설정을 따른다)
    try:
        subprocess.run(["git", "-C", str(tap), "add", "Casks/jadecode.rb"], check=True)
        msg = f"jadecode {num}"
        r = subprocess.run(["git", "-C", str(tap), "diff", "--cached", "--quiet"])
        if r.returncode == 0:
            print("변경 없음", file=sys.stderr)
        else:
            subprocess.run(["git", "-C", str(tap), "commit", "-q", "-m", msg], check=True)
            print("커밋 완료", file=sys.stderr)
    except subprocess.CalledProcessError as e:
        print(f"커밋 실패: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
