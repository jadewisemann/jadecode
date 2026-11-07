# jadecode — task-driven multiplexer

> 상태: **구체화** (구현 전 확인 필요 항목은 맨 아래 「열린 질문」)

## 한 줄 요약

herdr 위에서 동작하는 설치형 데스크톱 앱. 사람이 **작업(task)** 을 추가하면, 코딩 에이전트(Claude Code 기본, 그 외 codex/opencode 등)가
작업 단위의 맥락을 참조하며 **단계(stage)** 를 하나씩 진행하고, 사람은 앱에서 작업의 대화·변경 기록·근거 문서를 본다.

## 목표 / 비목표

**목표**
- 작업 맥락을 **task 단위로** 관리한다. 에이전트 세션은 항상 자신이 속한 작업 문서를 참조한다.
- 기본 워크플로우 `시작 → 구체화 → 구현 → 검증 → PR → 보고 → 완료` 를 제공하고, 전역/작업별로 단계를 추가·생략·교체할 수 있다.
- 에이전트가 피드백이 필요하면 멈추고 질문을 남긴다. 사람이 답하면 이어서 진행한다.
- 여러 머신, 여러 종류의 에이전트를 지원한다. (herdr가 제공하는 능력을 그대로 활용)
- 누구나 설치해서 쓸 수 있는 배포 형태. (단일 바이너리, 설치 파일, 자동 업데이트)

**비목표 (MVP)**
- 다중 사용자 서버 / 로그인 / 권한.
- herdr 런타임 재구현. 터미널 지속성, 원격 머신, 에이전트 상태 감지는 herdr에 위임한다.

## 구성

```
┌─ jadecode 데스크톱 앱 (Go + MyGo) ──────────────────────────┐
│  네이티브 UI: 작업 보드 · 단계 진행 · 피드백 입력               │
│  웹뷰 삽입:  터미널(xterm.js) · task.md/근거 문서 · diff        │
├─ Task Engine ──────────────────────────────────────────────┤
│  워크스페이스 로드 · 워크플로우 해석 · 단계 전이 · 프롬프트 조립    │
├─ Agent Adapter ────────────────────────────────────────────┤
│  claude │ codex │ opencode │ …  시작 명령 · 스킬 매핑 · 대화 로그 파싱 │
├─ herdr (필수 런타임) ── CLI / socket API ───────────────────┤
│  패인 = 세션 · working/blocked/idle/done · --machine 원격     │
└────────────────────────────────────────────────────────────┘
```

- **스택**: Go + [MyGo](https://github.com/egoist/mygo). 네이티브 UI를 기본으로 하고, 터미널과 문서 렌더링만 웹뷰에 맡긴다.
- **런타임**: [herdr](https://github.com/herdrdev/herdr). jadecode는 herdr의 클라이언트로 동작한다. 세션 하나는 herdr 패인 하나다.
- **방법론**: [pstack](https://github.com/cursor/plugins/tree/main/pstack). claude 어댑터가 기본으로 사용한다(`pstack@cursor-plugins` 플러그인).

## 워크스페이스 구조

`jadecode init <path>` 로 경로 하나를 만들고, 모든 상태는 그 아래에 규칙대로 둔다. 앱과 에이전트는 둘 다 이 파일들을 읽고 쓴다. 이 파일들이 유일한 진실 원천이다.

```
<workspace>/                     # 예: ~/jadecode/acme
  jadecode.toml                  # 워크스페이스 설정: 기본 에이전트, 머신, 저장소 목록
  AGENTS.md                      # 워크스페이스 공통 에이전트 규칙 (claude 포함 모든 에이전트가 읽음)
  workflows/
    default.yaml                 # 기본 단계 정의
    stages/<stage-id>.md         # 단계별 지시 템플릿
  repositories/<repo>/           # 원본 git clone
  worktrees/<task-id>/<repo>/    # 작업별 git worktree (작업끼리 격리, 병렬 진행)
  knowledge/                     # 공용 지식: 아키텍처 메모, 결정 기록, 용어집
  tasks/<task-id>/
    task.md                      # 작업 문서 (아래 형식). 세션이 계속 참조
    workflow.yaml                # (선택) 이 작업만의 단계 변경
    status.json                  # 현재 단계 상태. 에이전트가 기록
    history.jsonl                # 상태 전이 기록. 앱이 기록
    evidence/                    # 근거 문서: 테스트 출력, 스크린샷, 검증 로그, PR 링크
  sessions/<session-id>/
    meta.json                    # task-id, stage, agent 종류, herdr 패인, 에이전트 세션 id
    transcript.*                 # 대화 로그 (에이전트별 형식, 어댑터가 정규화)
```

- `sessions/` 는 최상위에 둔다. 작업 없이 띄운 세션(일반 멀티플렉서 용도)도 담을 수 있고, 작업과의 연결은 `meta.json` 의 `task_id` 로 표현한다.
- 워크스페이스 자체를 git repo로 둘 수 있다(선택). `tasks/`, `knowledge/`, `workflows/` 는 커밋하고 `repositories/`, `worktrees/`, `sessions/*/transcript.*` 는 ignore 한다.

### task.md

```markdown
---
id: T-0001
title: 로그인 rate limit
repos: [api]
agent: claude
stage: refine
parent: T-0000            # (선택) 이 작업을 만든 메타 작업
depends_on:               # 선행 작업. 후행은 저장하지 않고 역으로 계산
  - T-0002                #   = T-0002 완료 후 시작
  - { task: T-0003, stage: pr, gates: implement }  # T-0003이 pr을 통과해야 내 implement 시작
---

## 목표
## 범위 / 비범위
## 수용 기준
## 열린 질문
## 피드백          ← 사람이 답한 내용이 여기에 누적
## 결정 기록
```

### status.json — 에이전트 → 앱 신호

에이전트는 단계를 끝내거나 사람이 필요할 때 이 파일을 쓴다. 파일 쓰기는 모든 에이전트가 할 수 있으므로 에이전트 종류와 무관하게 동작한다.

```json
{
  "stage": "refine",
  "state": "done | needs_feedback | failed",
  "summary": "한두 문장",
  "questions": [{ "id": "q1", "text": "..." }],
  "evidence": ["evidence/refine-notes.md"],
  "updated_at": "2026-11-03T05:30:00Z"
}
```

herdr의 `blocked` 상태는 보조 신호다. 에이전트가 권한 승인 UI에 걸린 경우를 감지해서 앱에 알림으로 띄운다.

## 워크플로우

```yaml
# workflows/default.yaml
stages:
  - id: start
  - id: refine        # 구체화
    skills: { claude: [pstack:in-your-own-words, pstack:architect] }
  - id: implement     # 구현
    skills: { claude: [pstack:poteto-mode] }
  - id: verify        # 검증
    skills: { claude: [pstack:interrogate] }
  - id: pr
    skills: { claude: [pstack:poteto-mode] }   # opening-a-pr playbook
  - id: report        # 보고
    skills: { claude: [pstack:ketchup] }
  - id: done
```

```yaml
# tasks/T-0001/workflow.yaml — 작업별 변경
extends: default
skip: [pr]
insert:
  - after: implement
    id: migration-review
    prompt: "DB 마이그레이션이 되돌릴 수 있는지 검토하고 근거를 evidence/ 에 남겨라."
```

- 전역 기본값은 `workflows/default.yaml` 을 수정해서 바꾼다(단계 추가, 순서 변경).
- 단계마다 `gate: human` 을 지정하면, 에이전트가 `done` 을 보고해도 사람이 승인해야 다음 단계로 간다.

## 작업 간 관계

### 선행 / 후행

- 관계는 **후행 작업의 `depends_on` 에만** 저장한다. 같은 사실을 양쪽 파일에 적으면 언젠가 어긋나기 때문이다. 후행 목록은 앱이 역으로 계산해서 보여준다.
- 작업을 추가할 때 두 방향 모두 지정할 수 있다.
  - `jadecode task add "..." --after T-0002`: 새 작업의 `depends_on` 에 T-0002를 추가한다.
  - `jadecode task add "..." --before T-0005`: T-0005의 `depends_on` 에 새 작업을 추가한다.
- 의존 단위는 다음과 같다.
  - 기본은 "선행 작업 **완료** 후 내 작업 **시작**"이다.
  - 단계 단위로 좁힐 수 있다: `{ task, stage, gates }`. 예를 들어 "선행 작업이 pr을 통과하면 내 implement를 시작한다"처럼 지정한다. 지정하지 않은 내 앞 단계(구체화 등)는 병렬로 먼저 진행한다.
- 스케줄러 규칙은 다음과 같다.
  - 모든 의존이 충족된 단계만 실행한다. 대기 중인 작업은 `blocked_by` 를 UI에 표시한다.
  - 작업을 추가하거나 수정할 때 **순환을 검사**하고, 순환이 생기면 거부한다.
  - 선행 작업이 `failed` 거나 취소되면 후행 작업은 자동으로 진행하지 않는다. 사람이 진행할지 결정한다.
- 맥락은 다음과 같이 이어진다. 후행 작업의 프롬프트에 선행 작업의 보고 문서(`tasks/<dep>/evidence/report.md`)와 PR 링크를 넣는다. 작업 맥락은 작업 단위로 분리돼 있지만, 의존 관계를 따라서는 결과가 전달된다.

### 메타 작업

다른 작업들을 만들고 순서를 정하는 작업이다. 하위 작업은 `parent` 로 연결된다.

```yaml
# workflows/meta.yaml
stages:
  - id: start
  - id: refine            # 큰 목표를 구체화
  - id: decompose         # 하위 작업 + 의존 그래프 제안 → tasks/<id>/plan.yaml
    gate: human           # 사람이 그래프를 승인/수정해야 진행
  - id: spawn             # 앱이 plan.yaml 대로 하위 작업 생성 (에이전트 아님)
  - id: track             # 하위 작업이 모두 끝날 때까지 대기, 실패 시 사람에게 알림
  - id: report            # 전체 결과 보고
  - id: done
```

```yaml
# tasks/T-0010/plan.yaml — decompose 단계 산출물
tasks:
  - key: schema
    title: rate limit 테이블 추가
  - key: middleware
    title: 미들웨어 구현
    depends_on: [schema]
  - key: docs
    title: 운영 문서
    depends_on: [{ task: middleware, stage: pr }]
```

- `plan.yaml` 의 `key` 는 생성 시점에 실제 작업 ID(`T-0011`, …)로 바뀐다.
- 사람도 메타 작업 없이 같은 형식의 `plan.yaml` 을 직접 써서 `jadecode task add --plan plan.yaml` 로 여러 작업을 한 번에 추가할 수 있다.
- 메타 작업은 하위 작업이 모두 `done` 이 되면 report 단계로 넘어간다.

## 단계 실행 흐름

1. 사람이 작업을 추가한다. 앱이 `tasks/<id>/task.md` 를 만든다.
2. 엔진이 다음 단계를 정한다. 필요하면 worktree를 만들고, herdr에 패인을 열어 에이전트를 시작한다(`herdr agent start`).
3. 엔진이 프롬프트를 조립해서 보낸다(`herdr agent prompt`). 프롬프트에는 다음이 들어간다.
   - 단계 템플릿
   - `task.md` 경로
   - `status.json` 규약
   - 어댑터의 스킬 매핑
4. 앱이 `status.json` 을 감시한다(파일 watch).
   - `done`: 단계가 끝났다. `history.jsonl` 에 기록하고 다음 단계로 간다(gate가 있으면 승인을 기다린다).
   - `needs_feedback`: 작업이 `대기` 상태가 되고 알림이 뜬다. 사람이 답하면 그 답을 `task.md#피드백` 에 추가하고, 같은 세션에 "피드백 반영 후 계속"을 보낸다.
   - `failed`: 사람에게 알린다. 재시도할지, 단계를 되돌릴지 사람이 고른다.
5. 사람은 언제든 앱에서 해당 패인에 붙어 대화형으로 개입할 수 있다(상세 보기).

## MVP 범위

1. `jadecode` CLI: `init`, `task add`, `task run`. 앱 없이 엔진을 검증하기 위한 용도다.
2. herdr 연동: 패인 열기, 에이전트 시작, 프롬프트 전송, 상태 조회. 로컬 머신만 지원한다.
3. claude 어댑터와 pstack 스킬 매핑, `status.json` 규약.
4. 기본 워크플로우와 작업별 `skip`/`insert`.
5. 작업 의존(`depends_on`, `--after`/`--before`, 순환 검사)과 메타 작업(`plan.yaml`, decompose → 승인 → spawn).
6. 데스크톱 앱: 작업 목록, 작업 상세(task.md, 단계 타임라인, 질문/답변, evidence), "패인 열기"(herdr에 attach).

이후 단계: 앱 내 터미널 임베딩(xterm.js 웹뷰), 원격 머신(`--machine`), codex/opencode 어댑터, diff 뷰.

## 열린 질문

1. 이름은 `jadecode` 로 확정했다(2026-11-06). 워크스페이스 기본 위치는 `~/jadecode/<name>` 으로 할까요?
2. worktree 위치: 최상위 `worktrees/<task-id>/` 와 작업 폴더 안 `tasks/<id>/work/` 중 무엇으로 할까요? (현재는 최상위 안)
3. 한 작업이 여러 repo를 건드릴 수 있게 할까요? (현재 `repos: [...]` 로 허용하는 안)
4. 후행 작업의 코드 기반: 선행 작업의 브랜치 위에 쌓을까요(stacked PR, 선행이 머지되기 전에도 진행 가능), 아니면 선행 작업이 main에 머지된 후 main에서 시작할까요? (현재 안: `stage: pr` 의존이면 stacked, 완료 의존이면 main)
5. decompose 단계에서 에이전트가 제안한 하위 작업을 사람이 꼭 승인해야 할까요? (현재 안: `gate: human` 기본)
