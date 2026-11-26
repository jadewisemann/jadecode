#!/usr/bin/env python3
"""화면 확인용 데모 워크스페이스를 만든다. 보드는 파일이라 파일만 쓰면 된다.

사용: tools/demo-seed.py <워크스페이스>
"""
import json
import subprocess
import sys
import time
from pathlib import Path

ws = Path(sys.argv[1])
repo = ws.parent / "demo-repo"
repo.mkdir(parents=True, exist_ok=True)
if not (repo / ".git").exists():
    env = {"GIT_AUTHOR_NAME": "demo", "GIT_AUTHOR_EMAIL": "demo@example.com",
           "GIT_COMMITTER_NAME": "demo", "GIT_COMMITTER_EMAIL": "demo@example.com", "PATH": "/usr/bin:/bin"}
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    (repo / "README.md").write_text("# shop\n")
    subprocess.run(["git", "add", "."], cwd=repo, check=True)
    subprocess.run(["git", "commit", "-qm", "init"], cwd=repo, check=True, env=env)

p = ws / "shop"
for c in ["todo", "doing", "review", "done"]:
    (p / c).mkdir(parents=True, exist_ok=True)
(p / "project.toml").write_text(f'name = "shop"\nrepo = "{repo}"\n')

memos = {
    "todo/receipt-copy.md": "# 결제 영수증 메일 문구 수정\n\n- 목표: 환불 안내 문구를 새 정책에 맞춘다.\n- 완료 조건: 템플릿과 스냅숏 테스트가 새 문구를 쓴다.\n- 관련: templates/receipt.html\n",
    "todo/deps-update.md": "# 배포 전 의존성 업데이트\n\n보안 공지 두 건(serde_yaml, h2). 잠금 파일만 올리고 테스트가 통과하는지 본다.\n",
    "doing/rate-limit.md": "# 로그인 시도 횟수 제한\n\n- 목표: 같은 IP에서 1분에 5번을 넘으면 429를 돌려준다.\n- 완료 조건: 통합 테스트 추가, 기존 테스트 통과.\n- 주의: 관리자 계정은 예외가 아니다.\n",
    "review/safari-login.md": "# 사파리 로그인 버튼 고치기\n\n고객 문의: iOS 17 사파리에서 로그인 버튼이 눌리지 않는다.\n\n## 완료 조건\n- [ ] iOS 17 사파리에서 버튼이 눌린다\n- [ ] 다른 브라우저 동작은 그대로다\n\n## 관련 파일\n- web/src/login/Button.tsx\n",
    "done/readme-install.md": "# README에 설치 방법 추가\n\nbrew와 curl 두 방법을 적는다.\n",
}
for rel, text in memos.items():
    (p / rel).write_text(text)

state = ws / ".jadecode" / "shop"
(state / "bot").mkdir(parents=True, exist_ok=True)
(state / "memos").mkdir(parents=True, exist_ok=True)
now = int(time.time() * 1000)


def log(path, rows):
    with open(path, "w") as f:
        for i, (kind, text) in enumerate(rows):
            f.write(json.dumps({"ts": now - (len(rows) - i) * 60000, "kind": kind, "text": text}, ensure_ascii=False) + "\n")


log(state / "bot/log.jsonl", [
    ("user", "고객 문의 들어온 거 정리해 줘. 사파리 로그인 버튼이 안 눌린대. 그리고 로그인 시도 제한도 이번 주에 넣자."),
    ("tool", "Grep · onClick"),
    ("tool", "Read · web/src/login/Button.tsx"),
    ("tool", "Write · doing/safari-login.md"),
    ("tool", "Write · doing/rate-limit.md"),
    ("assistant", "두 가지를 메모로 만들어 바로 시작했습니다.\n\n- **사파리 로그인 버튼**: `Button.tsx`가 `pointer-events`를 끄는 오버레이 아래에 있어 보입니다. 재현 조건과 완료 조건을 메모에 적었습니다.\n- **로그인 시도 제한**: 1분 5회, 429 응답으로 정했습니다. 관리자도 예외 없이 적용합니다."),
    ("turn", "턴 끝 · 21초 · $0.18"),
    ("note", "「사파리 로그인 버튼 고치기」(review/safari-login.md) 에이전트가 한 차례 마쳤습니다: 오버레이의 z-index를 고치고 회귀 테스트를 추가했습니다."),
])
log(state / "memos/safari-login.jsonl", [
    ("note", "메모를 맥락으로 브랜치 jc/safari-login에서 작업을 시작합니다."),
    ("tool", "Read · web/src/login/Button.tsx"),
    ("tool", "Bash · npm test -- login"),
    ("tool", "Edit · web/src/login/Overlay.css"),
    ("tool", "Bash · git commit -am 'fix: login overlay blocks taps on iOS Safari'"),
    ("assistant", "오버레이의 z-index를 고치고 회귀 테스트를 추가했습니다.\n\n- 원인: 로딩 오버레이가 사라진 뒤에도 `pointer-events: auto`로 남아 버튼을 덮고 있었습니다.\n- 확인할 점: iOS 17 실기에서 한 번 눌러 봐 주세요. 시뮬레이터에서는 통과했습니다."),
    ("turn", "턴 끝 · 2분 14초 · $0.62"),
])
# rate-limit은 세션이 없으므로 앱이 열리면 바로 시작한다(데모에서는 가짜 claude가 계속 일하는 척한다).
for s in ["safari-login", "readme-install"]:
    (state / f"memos/{s}.json").write_text(json.dumps({"session_id": "00000000-0000-4000-8000-000000000000"}))
(state / "bot/session.json").write_text(json.dumps({"session_id": "00000000-0000-4000-8000-000000000001"}))
print(f"데모 워크스페이스: {ws}")
