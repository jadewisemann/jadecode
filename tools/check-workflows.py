#!/usr/bin/env python3
"""GitHub Actions 워크플로를 GitHub가 보는 관점에 가깝게 검사한다.

`yaml.safe_load`만으로는 부족하다. YAML 1.1에서 `on:`이 불리언 True로 읽히고,
잘못된 들여쓰기도 그냥 통과한다. 그래서 구조를 직접 확인한다.

검사 항목
  1. 최상위 키가 name / on / permissions / env / defaults / concurrency / jobs 뿐인지
  2. `on` 트리거가 있는지(문자열 'on' 또는 True 키)
  3. 모든 job 에 runs-on 과 steps 가 있는지
  4. 모든 step 이 uses 또는 run 중 하나를 갖는지(둘 다/둘 다 없음은 오류)
  5. `with:`/`env:` 블록의 값이 문자열·숫자·불리언인지(중첩 dict는 오류)
  6. steps 안에서 최상위로 튀어나온 키가 없는지(들여쓰기 붕괴 탐지)
  7. 식 `${{ ... }}`의 중괄호 짝

사용: tools/check-workflows.py [워크플로 디렉터리]
"""
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    print("pyyaml 필요: pip install pyyaml", file=sys.stderr)
    sys.exit(2)

ALLOWED_TOP = {
    "name", "on", "permissions", "env", "defaults", "concurrency", "jobs", "run-name",
}
errors = []
warnings = []


def err(f, msg):
    errors.append(f"{f}: {msg}")


def warn(f, msg):
    warnings.append(f"{f}: {msg}")


def check_file(path: Path):
    name = path.name
    text = path.read_text()
    try:
        doc = yaml.safe_load(text)
    except yaml.YAMLError as e:
        err(name, f"YAML 파싱 실패: {e}")
        return
    if not isinstance(doc, dict):
        err(name, "최상위가 매핑이 아니다")
        return

    # 1) 최상위 키
    for k in doc:
        ks = "on" if k is True else k
        if ks not in ALLOWED_TOP:
            err(name, f"알 수 없는 최상위 키: {k!r} (들여쓰기가 깨졌을 가능성)")

    # 2) 트리거
    trig = doc.get("on", doc.get(True))
    if trig is None:
        err(name, "on 트리거가 없다")
    elif not isinstance(trig, (dict, list, str)):
        err(name, f"on 트리거 형식이 이상하다: {type(trig).__name__}")

    jobs = doc.get("jobs")
    if not isinstance(jobs, dict) or not jobs:
        err(name, "jobs 가 없거나 비었다")
        return

    for jname, job in jobs.items():
        if not isinstance(job, dict):
            err(name, f"job {jname} 가 매핑이 아니다")
            continue
        if "runs-on" not in job:
            err(name, f"job {jname}: runs-on 없음")
        steps = job.get("steps")
        if not isinstance(steps, list) or not steps:
            err(name, f"job {jname}: steps 없음")
            continue
        for i, st in enumerate(steps):
            if not isinstance(st, dict):
                err(name, f"job {jname} step {i}: 매핑이 아니다")
                continue
            has_uses = "uses" in st
            has_run = "run" in st
            if has_uses == has_run:
                err(name, f"job {jname} step {i}: uses 와 run 중 정확히 하나여야 한다")
            # 5) with/env 값 타입
            for block in ("with", "env"):
                b = st.get(block)
                if b is None:
                    continue
                if not isinstance(b, dict):
                    err(name, f"job {jname} step {i}: {block} 가 매핑이 아니다")
                    continue
                for k2, v2 in b.items():
                    if isinstance(v2, dict):
                        err(
                            name,
                            f"job {jname} step {i}: {block}.{k2} 가 매핑이다"
                            " (들여쓰기가 깨져 다른 키를 삼켰을 수 있다)",
                        )
            # 6) 알 수 없는 step 키
            allowed_step = {
                "name", "id", "uses", "run", "with", "env", "if", "shell", "working-directory",
                "timeout-minutes", "continue-on-error", "uses-comment",
            }
            for k2 in st:
                if k2 not in allowed_step:
                    err(name, f"job {jname} step {i}: 알 수 없는 step 키 {k2!r}")

    # 7) 식 중괄호 짝
    if text.count("${{") != text.count("}}") and "${{" in text:
        warn(name, "${{ }} 짝이 안 맞는 것처럼 보인다(문자열 안이라면 무시)")


def main():
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".github/workflows")
    files = sorted(root.glob("*.yml")) + sorted(root.glob("*.yaml"))
    if not files:
        print(f"워크플로 파일이 없다: {root}", file=sys.stderr)
        return 2
    for f in files:
        check_file(f)
        print(f"검사: {f}")
    for w in warnings:
        print(f"경고: {w}")
    for e in errors:
        print(f"오류: {e}", file=sys.stderr)
    if errors:
        print(f"\n{len(errors)}개 오류", file=sys.stderr)
        return 1
    print(f"\n통과: {len(files)}개 파일")
    return 0


if __name__ == "__main__":
    sys.exit(main())
