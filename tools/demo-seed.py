#!/usr/bin/env python3
"""화면 확인용 데모 데이터를 넣는다(떠 있는 서버에 RPC로).

사용: tools/demo-seed.py <워크스페이스> <jadecode 바이너리>

- 데모 git 레포를 만들고 그 레포에 연결한 프로젝트를 만든다.
- 단계·상태가 섞인 카드 여러 장, 한 카드에는 대화·도구 호출·권한 요청·질문 이력과 워크트리 변경사항을 넣는다.
"""
import json
import os
import subprocess
import sys

ws, jc = sys.argv[1], sys.argv[2]
env = dict(os.environ, GIT_AUTHOR_NAME="demo", GIT_AUTHOR_EMAIL="demo@example.com",
           GIT_COMMITTER_NAME="demo", GIT_COMMITTER_EMAIL="demo@example.com")


def rpc(method, params):
    out = subprocess.run([jc, "rpc", method, json.dumps(params)], cwd=ws,
                         capture_output=True, text=True)
    if out.returncode != 0:
        print("rpc fail", method, out.stderr, file=sys.stderr)
        return {}
    try:
        v = json.loads(out.stdout)
    except Exception:
        print("bad json", out.stdout, file=sys.stderr)
        return {}
    return v.get("result", v)


def git(d, *a):
    subprocess.run(["git", "-C", d, *a], check=True, env=env, capture_output=True)


src = os.path.join(os.path.dirname(os.path.abspath(ws)), "demo-app")
os.makedirs(os.path.join(src, "src"), exist_ok=True)
git(src, "init", "-q", "-b", "main")
with open(os.path.join(src, "src", "login.ts"), "w") as f:
    f.write("export function canSubmit(form: HTMLFormElement) {\n  return form.checkValidity();\n}\n")
with open(os.path.join(src, "README.md"), "w") as f:
    f.write("# demo-app\n")
git(src, "add", ".")
git(src, "commit", "-qm", "init")

rpc("project.add", {"name": "webapp", "repo_path": src})
rpc("project.add", {"name": "docs-site"})

cards = [
    ("webapp", "Safari 로그인 버튼이 눌리지 않음", "working", 1),
    ("webapp", "결제 영수증 메일 문구 다듬기", "idle", 0),
    ("webapp", "API rate limit 429 응답 처리", "blocked", 2),
    ("webapp", "의존성 보안 업데이트", "done", 4),
    ("webapp", "다크 모드 대비 개선", "idle", 0),
    ("docs-site", "설치 가이드 스크린샷 교체", "working", 1),
    ("docs-site", "검색 결과 정렬 버그", "failed", 2),
]
first = None
for project, title, status, steps in cards:
    r = rpc("card.add", {"project": project, "title": title,
                         "body": f"## 목표\n\n{title}.\n\n## 완료 조건\n\n- [ ] 재현 확인\n- [ ] 수정과 테스트\n- [ ] PR\n"})
    rel = r.get("path")
    if not rel:
        continue
    for _ in range(steps):
        rpc("card.advance", {"card": rel})
    rpc("card.set_status", {"card": rel, "status": status})
    if first is None:
        first = rel

if first:
    parts = first.split("/")
    topic = f"project/{parts[1]}/card/{parts[2]}"
    # 워크트리와 변경사항
    wt = os.path.join(ws, first, "demo-app")
    git(src, "worktree", "add", "-q", "-b", "jc/" + parts[2], wt)
    with open(os.path.join(wt, "src", "login.ts"), "w") as f:
        f.write("export function canSubmit(form: HTMLFormElement) {\n"
                "  // Safari는 :invalid 계산이 늦다. 제출 직전에 다시 확인한다.\n"
                "  form.reportValidity();\n  return form.checkValidity();\n}\n")
    with open(os.path.join(wt, "src", "login.test.ts"), "w") as f:
        f.write("test('safari submit', () => {});\n")
    me = {"actor": {"instance": "w-01"}}
    for kind, body in [
        ("instance.started", {"bot": "worker", "agent": "claude", **me}),
        ("msg.human", {"text": "iOS 17 사파리에서 로그인 버튼이 안 눌린다는 문의가 왔어요. 재현부터 해 주세요."}),
        ("instance.output", {"text": "재현해 보겠습니다. 먼저 로그인 폼 코드를 읽습니다.", **me}),
        ("instance.tool_use", {"tool": "Read", "input": {"file_path": "src/login.ts"}, **me}),
        ("instance.tool_use", {"tool": "Grep", "input": {"pattern": "checkValidity"}, **me}),
        ("instance.output", {"text": "원인을 찾았습니다. Safari는 `:invalid` 상태를 늦게 계산해서 첫 제출 때 `checkValidity()`가 false를 돌려줍니다. 제출 직전에 `reportValidity()`를 먼저 부르도록 고치겠습니다.", **me}),
        ("instance.tool_use", {"tool": "Edit", "input": {"file_path": "src/login.ts"}, **me}),
        ("instance.file_written", {"path": "src/login.test.ts", **me}),
        ("instance.turn_end", {"turn": 1, "usage": {"input": 18234, "output": 1410, "cost_usd": 0.071}, **me}),
        ("permission.requested", {"tool": "Bash", "input": {"command": "npm test -- login"}, **me}),
        ("msg.ask", {"text": "Safari 15도 지원 대상인가요? 지원한다면 폴리필을 넣어야 합니다.", **me}),
    ]:
        rpc("events.append", {"topic": topic, "kind": kind, "body": body})

print("seeded", first)
