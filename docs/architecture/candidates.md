# 아키텍처 후보

> 상태: **1단계(후보 설계)**. 2026-11-08 작성. 이 문서는 갈래마다 후보 2~3개와 트레이드오프, 추천을 적는다. 추천은 스파이크(`spikes.md`)와 아키텍처 그릴링을 거쳐 ADR로 확정한다. 질문 분류는 `questions.md`에 있다.
> 근거: `SPEC.md`(`docs/spec-rewrite` 45cff00), 그릴링 기록 `work/jadecode/grilling.md`, 아래 「조사 근거」의 2026-11-08 조사.

## 읽는 법

- 갈래는 다섯이다: ① 에이전트 실행 계층 ② 서버 코어 ③ 봇·자기개선 장치 ④ 클라이언트 UI·프로토콜 ⑤ 원격 접속·배포.
- 후보마다 SPEC 요구 충족, 위험, 비용을 적는다. 비용은 첫 버전(Mac 서버 + Mac 프론트)까지의 대략적인 구현량이다(S: 며칠, M: 1~2주, L: 3주 이상).
- 모든 실행 계층 후보는 「오늘의 수동 운영에서 나온 요구」 다섯 가지(아래 R1~R5)를 어떻게 푸는지 짚는다. 마지막 절에서 추천 구성 전체로 다시 짚는다.

| 번호 | 오늘 겪은 일 → 해야 할 일 |
|---|---|
| R1 | 작업 폴더를 고를 수 없었다 → 인스턴스를 카드 폴더(cwd)에서 시작하고 레포별 워크트리를 함께 연다 |
| R2 | 보고 도구가 없어 대화 기록을 읽어 갔다 → 인스턴스가 보고·질문·제안·오류를 시스템에 직접 보낸다 |
| R3 | 권한 승인 창에서 몇 시간 멈췄고 아무도 몰랐다 → 권한은 봇 정의로 미리 정하고, 그래도 막히면 "막힘" 이벤트와 알림 |
| R4 | 이어 붙이다 사본이 생겨 id가 바뀌었다 → 인스턴스는 안정된 id를 갖고 끊겨도 같은 인스턴스로 이어진다 |
| R5 | 명단·상태·보고를 허브 문서에 손으로 적었다 → 인스턴스·카드·메시지 상태를 서버가 갖고 보드에 보인다 |

---

## ① 에이전트 실행 계층

에이전트 CLI(첫 버전은 Claude Code)를 띄우고, 말을 걸고, 상태를 읽고, 권한 요청을 받는 계층이다.

### A1. PTY 호스팅 + TUI 화면 판독 + 훅

서버가 의사 터미널(PTY)을 열고 에이전트의 대화형 TUI를 그대로 돌린다. herdr·rex가 쓰는 방식이다.

- 구성: `portable-pty` + 터미널 에뮬레이터(`alacritty_terminal` 또는 libghostty-vt). 상태는 Claude Code 훅(`UserPromptSubmit`=일하는 중, `Notification`의 `permission_prompt`·`idle_prompt`=막힘, `Stop`=한가함)과 화면 판독 규칙(herdr `src/detect/manifests/*.toml`을 옮겨 옴)으로 읽는다. 상태 보고 형식은 rex의 OSC 7501 Program Status Protocol을 그대로 써도 된다(간단해서 재구현 쉬움).
- 메시지 전달: 키 입력을 흘려 넣는다.
- 요구 충족
  - R1 ○ PTY를 cwd에서 연다.
  - R2 △ 에이전트에 jadecode CLI·MCP 도구를 주면 되지만, 오케스트레이터 → 인스턴스 방향은 키 입력 주입이라 바쁜 상태·붙여넣기·타이밍에 깨진다.
  - R3 △ 훅의 `PermissionRequest`로 코드에서 판정할 수 있다. 화면 판독 기반 막힘 감지는 문구가 바뀌면 깨진다(herdr는 매니페스트를 원격으로 자동 갱신해서 버틴다).
  - R4 ○ `--session-id <uuid>`로 시작, `--resume <id>`로 이음(`--fork-session`을 붙일 때만 새 id).
  - R5 ○ 서버가 PTY를 가지므로 상태를 안다.
  - 사람 붙기: 최상. 진짜 TUI를 그대로 보여 준다.
- 위험: 메시지 주입이 깨지기 쉽다. 화면 판독은 에이전트 버전마다 손봐야 한다. 서버가 PTY를 가지므로 서버 재시작 = 에이전트 프로세스 종료(herdr도 셸에 `claude --resume`을 다시 타이핑해 복구한다).
- 비용: M~L(PTY·에뮬레이터·판독 규칙·재시작 복구).

### A2. 구조화 드라이버 (추천)

에이전트를 화면 없이 구조화된 입출력으로 돌린다. Claude Code는 `claude -p --input-format stream-json --output-format stream-json`(공식 바이너리, 양방향 JSON 줄), Codex는 `codex app-server`(stdio JSON-RPC), Gemini·opencode는 ACP(`gemini --acp`, `opencode acp`)로 같은 모양을 얻는다.

- 구성
  - 서버 안의 **드라이버 인터페이스**는 ACP 개념에 맞춘다: `new_session(cwd, mcp_servers, bot_snapshot)`, `load(session)`, `prompt(text)`, `cancel()`, `update` 이벤트 흐름, `request_permission` 콜백. 첫 버전 드라이버는 Claude Code 하나다.
  - 인스턴스 id는 jadecode가 정하고(ULID), 에이전트의 세션 id(Claude `session_id`)를 따로 매핑해 둔다. 에이전트 쪽 id가 어떻게 바뀌어도 jadecode 인스턴스 id는 바뀌지 않는다.
  - 권한: 봇 정의의 권한 정책을 Claude Code `--settings` 파일(allow/deny 규칙)과 `--permission-mode`로 만들어 넣고, 규칙에 없는 요청은 `--permission-prompt-tool mcp__jadecode__permission`으로 서버에 RPC로 온다. v2.1.302부터 `-p`는 승인 수단이 없으면 거부하므로 "몰래 멈춤"이 구조적으로 없다.
  - 메시지: 인스턴스마다 jadecode MCP 서버(`--mcp-config`)를 붙여 `report`·`ask`·`propose`·`error` 도구와 카드 조작 도구를 준다. 오케스트레이터 → 인스턴스는 stdin의 사용자 턴으로 넣는다.
  - 사람 붙기: 1차는 이벤트 흐름을 그리는 대화 화면. 2차로 "터미널로 넘기기": 구조화 프로세스를 멈추고 PTY에서 `claude --resume <session_id>` TUI를 띄워 사람에게 넘기고, 끝나면 다시 구조화로 잇는다(스파이크 S4).
- 요구 충족
  - R1 ○ 프로세스를 카드 폴더에서 띄우고 `--add-dir`로 레포 워크트리를 더한다.
  - R2 ○ MCP 도구 호출이 곧 메시지다. 대화 기록을 읽어 갈 일이 없다.
  - R3 ○ 규칙 밖 요청은 반드시 RPC로 오거나 거부된다. 막힘 = "권한 RPC가 열려 있음"으로 정확히 안다.
  - R4 ○ jadecode 인스턴스 id ↔ 세션 id 매핑, `--session-id` 지정, 재시작 때 `--resume`.
  - R5 ○ 모든 상태가 이벤트로 서버에 온다.
- 위험
  - CLI의 비대화 모드 동작이 버전마다 바뀐다(예: v2.1.302의 기본 권한 변경). → CLI 버전을 봇 정의에 고정하고 드라이버 계약 테스트를 둔다.
  - 진짜 TUI를 보는 경험은 "터미널로 넘기기"에 기대야 한다.
  - 과금 정책: 구독 로그인은 "수정하지 않은 Claude Code 바이너리"를 본인이 쓰는 것을 허용한다. Agent SDK로 만든 앱에 구독을 쓰는 것은 회색 지대다(미확인). 그래서 SDK가 아니라 공식 `claude` 바이너리를 하위 프로세스로 구동한다. → `questions.md` X1
- 비용: M(드라이버 하나 + MCP 서버 + 대화 화면). 터미널 넘기기는 +S.

### A3. herdr를 실행 기층으로 (API 사용 또는 포크)

herdr 서버를 띄워 두고 jadecode 서버가 그 소켓 API(NDJSON)로 pane을 만들고 에이전트를 시작한다. 모자란 것은 포크해서 고친다.

- 장점: 24개 에이전트 판독 매니페스트, PTY·에뮬레이터(libghostty-vt), 실험적 live handoff(업그레이드 때 PTY fd를 넘겨 프로세스 생존), Linux·macOS·Windows 지원을 바로 얻는다.
- 요구 충족: R1 ○(`pane --cwd`). R2 △(A1과 같음). R3 △(Claude 상태는 v11부터 화면 판독, 승인 API 없음). R4 △(herdr pane id는 서버 수명, 재시작 때 셸에 resume 타이핑). R5 △(이벤트는 메모리 링 512개, 상태 변화 구독은 pane마다 100ms 폴링, 와일드카드·이력 없음, 뒤처지면 `events_lost`).
- 위험
  - 포크하면 약 29만 줄짜리 단일 크레이트(라이브러리 타깃 없음)를 1.0 전, 반년에 34번 릴리스하는 상류 옆에서 유지한다. 상류는 외부 PR을 자동으로 닫는다.
  - API는 호환 약속이 없다(약속은 endpoint 프로토콜 gen1뿐).
  - Android 빌드 불가(ghostty-vt). 서버만 쓰므로 당장 문제는 아니다.
- 비용: API 사용은 S(대신 R2~R5를 jadecode 쪽에서 메워야 해서 결국 M), 포크는 L + 상시 유지비.

### 검토 후 뺀 것: Claude Code 자체 백그라운드 세션

`claude --bg` / `claude agents --json` / `claude attach`는 상태(`working|blocked|done|failed|stopped`)와 생존(supervisor)을 준다. 지금 워크스페이스가 이 방식으로 돌고 있고, R3·R4가 바로 여기서 생긴 문제다. research preview이고, 셸에서 메시지를 보내는 공식 경로가 `--resume <uuid> --bg "msg"`뿐이며, 옵션을 붙이면 사본이 생긴다. 한 벤더에 묶인다. 그래서 뺀다.

### 추천: A2

R2·R3·R4를 구조로 푸는 것은 A2뿐이다. A1의 장점(진짜 TUI)은 A2의 "터미널로 넘기기"로 필요한 순간에만 얻는다. A3는 실행 기층보다 **차용 대상**으로 쓴다: 판독 매니페스트는 A2의 터미널 넘기기 동안 상태 보조 판독에, live handoff 발상은 서버 업그레이드 설계에 참고한다.

---

## ② 서버 코어 (상태·이벤트 이력·구독·메시지)

SPEC의 두 조건이 이 갈래를 정한다. "카드 폴더의 파일이 정본이고, 서버 상태는 파일에서 다시 만들 수 있거나 파일로 남아야 한다"(Q3), "메시지와 이벤트는 이력으로 남아 늦게 봐도 빠짐없이 본다"(Q22).

### B1. DB 정본 (SQLite에 모든 상태, 파일은 내보내기)

- 장점: 트랜잭션·질의가 쉽다. 구현이 가장 빠르다.
- 단점: "파일이 정본"을 어긴다. 에이전트는 파일(`task.md`)을 직접 고치므로 DB와 파일이 어긋나는 순간 어느 쪽이 맞는지 정해야 하고, 결국 파일을 따라가게 된다. 사람이 git·편집기로 들여다보는 지금 방식과도 멀다.
- 비용: S~M. **SPEC 위반이라 추천하지 않는다.**

### B2. 파일 정본 + 카드별 이벤트 일지 + 재생성 가능한 색인 (추천)

- 파일
  - 카드 폴더: `task.md`(앞머리 YAML에 상태 칸) + `events.jsonl`(그 카드의 이벤트·메시지, 덧붙이기만) + 레포별 워크트리 + `handoff/`·`transcripts/`.
  - 프로젝트 폴더: `project.toml` + 프로젝트 `events.jsonl`(오케스트레이터 대화, 카드 생성 등).
  - 워크스페이스: `events/global.jsonl`(봇 버전 반영, 정책 변경, 인스턴스 생애).
- 서버
  - 모든 이벤트에 워크스페이스 전역의 빈틈없는 순번(`seq`)을 매기고 해당 일지에 쓴 뒤 구독자에게 보낸다(쓰기 먼저, 보내기 나중).
  - SQLite(WAL)는 **색인**이다. 보드 투영, 인스턴스 명단, 지표, 구독용 seq 색인. 지워도 일지와 `task.md`를 훑어 다시 만든다.
  - 구독: 주제는 `project/<p>`, `project/<p>/card/<c>`, `instance/<i>`, `bot/<b>`처럼 경로이고 앞부분 일치로 거른다. 구독은 `from_seq`를 받아 그 뒤부터 색인을 읽어 보내므로, 느린 구독자도 끊기지 않고 따라잡는다(herdr·rex의 "뒤처지면 끊고 다시 스냅샷" 문제가 없다). 처음 붙는 클라이언트는 스냅샷 + 그 seq 이후 이벤트를 받는다(rex 방식).
  - `task.md` 쓰기 규칙: 본문은 누구나 고친다(서버는 파일 감시로 `card.file_changed` 이벤트를 낸다). 앞머리의 상태 칸(단계·경로·상태 배지)은 서버만 쓴다. 다른 쓰기가 상태 칸을 바꾸면 서버가 그것을 API 호출로 보고 종료 조건을 검사해 받아들이거나, 거절하고 되돌린 뒤 `error` 이벤트를 남긴다.
- 요구 충족: Q3 ○(색인은 버려도 됨), Q22 ○(일지가 이력), 늦게 붙은 구독자 ○, 카드 보관 때 일지가 카드와 함께 `arxiv/`로 간다 ○.
- 위험: 전역 seq와 카드별 파일을 함께 맞추는 쓰기 경로(서버가 유일한 쓰는 쪽이어야 함), 파일 감시의 플랫폼 차이(macOS FSEvents·Linux inotify), 재구성 시간. → 스파이크 S5.
- 비용: M.

### B3. 내장 메시지 브로커 (NATS JetStream 등) + 파일

- 장점: 커서가 있는 영구 스트림, 주제 와일드카드, 재전송을 공짜로 얻는다. 나중에 여러 머신으로 넓히기 쉽다.
- 단점: 프로세스·저장소가 하나 더 생기고, 브로커 저장소가 사실상 두 번째 정본이 된다. 사람 한 명·머신 한 대인 첫 버전에는 과하다. "파일에서 다시 만들기"를 따로 만들어야 하는 것은 B2와 같다.
- 비용: M + 운영 부담.

### 추천: B2

이벤트 일지 = 파일이라 SPEC의 두 조건을 한 장치로 채운다. 언어는 서버·클라이언트가 프로토콜 타입을 나눠 쓰도록 Rust를 추천한다(`questions.md` Q1에서 사용자에게 묻는다). 이 갈래의 구조는 언어와 무관하다.

#### 오케스트레이터 깨우기 (Q5와 연결)

오케스트레이터도 A2 드라이버로 도는 인스턴스다. 서버는 오케스트레이터가 구독한 주제에서 "깨울 이벤트"(질문·오류·막힘·종료 조건 충족·인스턴스 종료·사람 말)가 오면 모아서 **다이제스트 사용자 턴**으로 넣는다. 오케스트레이터가 일하는 중이면 그 턴이 끝날 때까지 쌓아 두었다가 한 번에 넣는다. 오케스트레이터는 다이제스트의 seq까지 읽었다고 기록한다. 그래서 "무엇을 받았는지"가 이력에 남고, 놓친 신호는 seq 차이로 드러난다. → 스파이크 S3

---

## ③ 봇·자기개선 장치

SPEC: 봇 정의 = 폴더(에이전트 종류·모델, 역할 지침, 스킬, 기억, 권한 정책). 인스턴스는 시작 시점 사본. 자기개선은 지침·스킬·기억만 바꾸고, 권한·사람 절차·예산은 바꾸지 못한다. 변경은 버전으로 남고 사람이 되돌릴 수 있다.

### C1. 봇 = git 폴더, 개선 = 카드 (추천)

- 저장
  - `bots/`는 git 레포 하나다. `bots/<bot>/bot.toml`(에이전트 종류·모델·CLI 버전), `instructions.md`, `skills/`, `memory/`.
  - 사람만 쓰는 값(권한 정책, 봇별 예산, 봇 수 상한, 예산 총량)은 `bots/` 밖의 `policy/`에 둔다. 개선 고리는 `policy/`에 쓸 권한이 없고, 서버는 개선 커밋의 변경 경로가 `instructions.md`·`skills/`·`memory/` 안인지 검사해 밖이면 거부한다. 경계가 파일 경로로 강제된다.
  - 버전 = 커밋, 되돌리기 = 그 봇 경로의 revert 커밋. 봇 화면의 "버전·지표·되돌리기"는 git 로그 + 지표 색인이다.
- 인스턴스 사본: 인스턴스는 시작할 때 봇 커밋 해시를 기록하고, 그 커밋에서 실행용 묶음을 만든다(`.jadecode/instances/<id>/`). Claude Code에는 `--append-system-prompt-file`(지침·기억), `--plugin-dir`(스킬을 플러그인 꼴로), `--settings`(권한 정책 → allow/deny), `--mcp-config`(jadecode 도구), `--model`로 넣는다(스파이크 S1에서 확인). 실행 중 봇이 바뀌어도 이미 뜬 인스턴스는 자기 해시를 쓴다.
- 고리
  - 서버는 "카드 K개 끝남"과 "한가함"을 세어 오케스트레이터에게 **개선 카드**를 발행하라고 신호한다. 개선 카드는 특수 프로젝트(`_improve`)의 일반 카드다. 보드·이력·예산·핸드오프를 그대로 쓴다.
  - 개선 카드의 일: 끝난 인스턴스 기록 흡수 → 변경안 커밋(가지) → 평가 세트 재실행(스파이크 S9) → 지표 비교 → 나아졌으면 서버 API `bot.publish`로 기본 가지에 반영. 사람 승인 없음.
  - 봇 복제·분화 = 새 봇 폴더를 만드는 커밋. 서버가 `policy/`의 봇 수 상한을 검사한다.
- 요구 충족: 버전·되돌리기 ○, 경계 강제 ○(경로 검사), 사본 ○(커밋 해시), 개선이 보드에 보임 ○, 오케스트레이터 자기개선도 같은 장치 ○.
- 위험: 평가 재실행의 비용과 판별력(같은 버전도 실행마다 결과가 다름). 이것이 자기개선 전체의 핵심 위험이다. → S9
- 비용: M(평가 재실행 장치 제외), 평가 재실행 +M.

### C2. 서버 내장 개선 엔진

- 서버가 스케줄러·평가 파이프라인을 직접 갖는다. 봇 버전은 내용 주소 스냅샷(해시 디렉터리), 평가는 서버가 고정 절차로 돌린다.
- 장점: 고리가 결정적이고 예측 가능하다. 오케스트레이터의 판단에 기대지 않는다.
- 단점: SPEC은 오케스트레이터가 고리를 돌리고 평가 세트도 고른다고 한다(Q17·Q26·Q28). 개선 절차 자체를 개선할 수 없다(서버 코드라서). 카드·보드와 별도인 두 번째 작업 장치가 생긴다.
- 비용: M~L.

### C3. 외부 프롬프트 최적화 프레임워크 (DSPy·GEPA 류)

- 장점: 지침 문장 최적화에 검증된 방법이 있다.
- 단점: 대상이 "프롬프트 + 고정 평가 함수"라서 스킬·기억 폴더나 코딩 카드처럼 길고 비싼 평가에 맞지 않는다. Python 의존이 생긴다.
- 쓰임: C1 개선 카드 안에서 개선 봇이 고를 수 있는 기법 하나로 둔다. 장치로는 채택하지 않는다.

### 추천: C1

개선을 카드로 만들면 새 장치 없이 SPEC의 "오케스트레이터가 돌린다·보드에 보인다·예산 안에서"가 따라온다. 경계는 폴더 분리 + 서버의 경로 검사로 강제한다.

---

## ④ 클라이언트 UI·프로토콜

조건: 네이티브 UI, 웹뷰 절대 금지(터미널·문서·diff 포함). 순서 Mac → (Linux 서버) → Android → iPad. Mac 첫 화면 다섯 가지: 칸반, 카드 상세(markdown `task.md`, 경로, 메시지 이력), 오케스트레이터 대화, 봇 목록, 인스턴스 붙기(대화 화면, 터미널).

어느 후보든 **클라이언트 코어**(프로토콜 타입, 연결·재연결, 스냅샷 + seq 캐시, 구독)는 UI와 무관한 Rust 크레이트로 둔다. 서버와 같은 타입을 쓰고, UI 수단을 바꿀 때 버리지 않는다.

### UI 후보

| | D1. GPUI + gpui-kit (추천: Mac 초안) | D2. nsh 하나로 4플랫폼 | D3. 플랫폼별 네이티브 + Rust 코어 |
|---|---|---|---|
| 구성 | Zed의 GPUI, longbridge/gpui-kit(markdown·diff·에디터) | longbridge/nsh: Rust 엔진 + 플랫폼 네이티브 뷰(AppKit/UIKit/Android View). 화면은 Rust 또는 TSX(QuickJS) | Mac·iPad SwiftUI + SwiftTerm, Android Compose + Termux terminal-view, 코어는 UniFFI |
| macOS | 정식 | AppKit | 정식 |
| Android | 커뮤니티 `gpui-mobile` 0.1뿐 | Android View | 정식 |
| iPad | 본가 `gpui_ios` 실험적(2026-10) | UIKit | 정식 |
| 웹뷰 | 없음(gpui-kit의 wry 크레이트는 넣지 않음) | 없음(`<WebView>` 요소를 쓰지 않으면) | 없음 |
| 터미널 | Zed `terminal_view` 이식 또는 커뮤니티 `gpui-terminal`(성숙도 미확인) | 내장 `Terminal`(vte, iOS·Android 동작, 에뮬레이터 교체 가능) | SwiftTerm, Termux view |
| markdown·diff | 있음·있음 | `TextView`·`DiffView` 내장 | 라이브러리 있음·직접 구현 |
| 성숙도 | 중간, 1.0 전. Zed·Longbridge Pro 출시 | 매우 초기: 첫 커밋 2026-10-14, v0.4.0(11-04) | 높음 |
| 언어 | Rust | Rust(+TSX 선택) | Swift + Kotlin + Rust |
| 비용(Mac 첫 화면) | M | M(+API 변화 대응) | M(Mac), 이후 Android가 사실상 두 번째 앱 |

- MyGo(egoist/mygo, Go)는 GPU 직접 그리기 모드가 있지만 모바일 백엔드가 없어 4플랫폼 경로에서 뺀다. Dioxus는 기본 데스크톱이 웹뷰라 뺀다. Flutter·Compose Multiplatform·Slint는 쓸 만한 터미널 위젯이 없거나 정체돼 뺀다.
- 한국어 입력(IME 조합)이 대화 화면·터미널에서 되는지가 실사용에서 큰 갈림길이다. 스파이크 통과 기준에 넣는다.

추천: **Mac 초안은 D1**. 첫 화면 다섯 가지에 필요한 부품(markdown·diff·에디터)이 가장 많이 검증돼 있다. 모바일은 Android 착수 직전에 D2(nsh)를 다시 평가하고, 그때도 미숙하면 D3로 간다. 클라이언트 코어를 분리해 두므로 갈아타는 비용은 화면 코드에 한정된다. D2를 지금 바로 고르는 선택지도 사용자에게 묻는다(`questions.md` Q2). 스파이크 S6(GPUI)·S7(nsh)을 함께 돌려 판단 근거를 만든다.

### 프로토콜 후보

| | P1. JSON-RPC 2.0 over WebSocket (추천) | P2. gRPC(protobuf) | P3. REST + SSE |
|---|---|---|---|
| 요청·응답 | `method.name` + JSON | 서비스 정의 | HTTP 동사 |
| 이벤트 | 같은 연결로 푸시(`events.subscribe{topics, from_seq}`) | 서버 스트리밍 | SSE(단방향) |
| 터미널 바이트 | 별도 WebSocket(일회용 티켓, rex 방식) | 양방향 스트림 | 어려움 |
| 에이전트·스크립트 친화 | 높음: JSON Schema 자기 서술(`api.schema`), CLI·MCP가 같은 메서드를 그대로 감쌈 | 중간: 도구가 필요 | 높음 |
| 모바일 | 쉬움 | 쉬움 | 쉬움 |
| 비용 | S | M | S(대신 양방향·터미널이 불편) |

- 전송: 로컬은 Unix 소켓(권한 0600)과 루프백, 원격은 같은 프로토콜을 Tailscale 쪽 리스너로(⑤).
- 에이전트의 서버 접근: 인스턴스마다 jadecode MCP 서버(stdio)를 붙인다. 인스턴스 토큰은 환경변수로 받고, 역할(오케스트레이터·워커)에 따라 도구 목록이 다르다. 사람·스크립트용 `jadecode` CLI도 같은 메서드를 감싼다. 에이전트 지침에는 "CLI 대신 MCP 도구"를 기본으로 적고, MCP를 못 쓰는 에이전트는 CLI를 쓴다.
- 스키마는 서버 Rust 타입에서 뽑고(`schemars`), 서버·클라이언트·MCP가 같은 타입을 쓴다. 판(revision)과 최소 판을 `server.version`으로 알린다(rex 방식).

추천: **P1**.

---

## ⑤ 원격 접속·배포

첫 버전은 한 컴퓨터에 서버와 프론트가 함께 있다. 원격은 Tailscale 같은 방식이 유력하다(Q37).

### E1. 로컬 우선 + 시스템 Tailscale 위 리스너 (추천)

- 첫 버전: Unix 소켓 + 루프백만 연다. 인증은 소켓 권한(같은 OS 사용자)과 기기 토큰.
- 원격(Android·iPad 단계): 서버가 tailnet 주소에 리스너를 더 열고, 붙는 쪽의 신원을 시스템 `tailscaled` LocalAPI `whois`로 확인한다. 기기는 처음 한 번 짝짓기(QR에 주소 + 일회용 토큰)로 등록한다. rex의 `tailnet://` 리스너와 같은 방식이다.
- 장점: 서버 코드에 VPN을 넣지 않는다. 사용자는 이미 Tailscale을 쓸 가능성이 높다.
- 위험: 서버 머신에 Tailscale 설치가 필요하다. 신원 확인 코드가 Tailscale API에 기댄다.
- 비용: S(첫 버전 0, 원격 단계 S).

### E2. tsnet 내장

- 서버가 스스로 tailnet 노드가 된다(rex `--tailscale`). 시스템 Tailscale이 없어도 된다.
- 단점: tsnet은 Go 라이브러리다. Rust 서버면 libtailscale(C) 바인딩이나 Go 사이드카가 필요하다. 첫 버전에 필요 없다.
- 비용: M.

### E3. SSH 터널 또는 자체 TLS + 토큰

- SSH는 데스크톱엔 쉽지만 Android·iPad 클라이언트에서 소켓 포워딩이 불편하다(rex iOS도 SSH 포워딩을 지원하지 않는다). 자체 TLS는 인증서·포트 노출 관리가 사람 몫이다.
- 쓰임: Tailscale을 못 쓰는 환경의 예비로만 둔다.

### 배포 (세 후보 공통)

- Mac: 클라이언트 `.app`, 서버는 같은 묶음 안의 바이너리를 LaunchAgent(launchd 사용자 서비스)로 띄운다. 앱을 닫아도 서버와 인스턴스는 돈다. 첫 배포는 직접 빌드·Homebrew cask, 자동 업데이트(Sparkle)는 나중.
- Linux: 단일 바이너리 + systemd 사용자 유닛.
- 데이터 형식 판을 파일에 적고, 서버는 자기보다 새 판을 만나면 덮어쓰지 않고 멈춘다(rex 방식).
- 서버 업그레이드 때 인스턴스: A2에서는 프로세스가 서버의 자식이라 업그레이드 = 인스턴스 재시작(`--resume`)이다. 일하는 중인 턴이 끊기므로 업그레이드는 모든 인스턴스가 한가할 때 하거나, 인스턴스 프로세스를 별도 감독 프로세스에 두는 방식을 그릴링에서 정한다(herdr live handoff 참고).

추천: **E1**. 에이전트는 서버 머신에서만 돈다(다른 머신 실행은 사용자 확인, `questions.md` Q25).

---

## 추천 구성 한눈에

```
 Mac 클라이언트 (GPUI)          Android·iPad (나중: nsh 또는 네이티브)
   └ 클라이언트 코어(Rust) ─┐    └ 클라이언트 코어(Rust)
                            │  JSON-RPC over WebSocket / Unix 소켓 (원격: tailnet 리스너)
 ┌──────────────────────────▼─────────────────────────────────────────┐
 │ jadecode 서버 (Rust, launchd/systemd 사용자 서비스)                     │
 │  API · 구독(seq, 주제 앞부분 일치) · 사람 절차 · 종료 조건 검사 · 예산     │
 │  이벤트 일지 쓰기 ──► work/<p>/<c>/events.jsonl, task.md 앞머리         │
 │  색인 SQLite (지워도 재생성)                                             │
 │  드라이버: Claude Code `claude -p` stream-json (ACP 모양 인터페이스)     │
 │  jadecode MCP 서버 ◄── 인스턴스의 report/ask/propose/error, 권한 RPC    │
 └───────┬───────────────────────────────┬────────────────────────────┘
         │ 하위 프로세스 (cwd = 카드 폴더)   │ 봇 사본 = bots/ 커밋 해시
   오케스트레이터 인스턴스, 워커 인스턴스      bots/ (git, 자기개선 대상) · policy/ (사람만)
```

### 오늘의 수동 운영 문제를 추천 구성이 푸는 방법

| | 푸는 장치 | 남는 위험 | 확인 스파이크 |
|---|---|---|---|
| R1 cwd | 서버가 카드 폴더를 cwd로 하위 프로세스를 띄우고 `--add-dir`로 레포 워크트리를 더한다. 워크트리는 카드 생성 API가 연다(지금 `new-work`가 하는 일) | 워크트리 밖 경로 접근이 권한 요청이 되는지 | S1, S2 |
| R2 보고 | MCP 도구 호출 = 메시지 이벤트. 받는 쪽은 구독·이력으로 읽는다 | 에이전트가 도구를 안 쓰고 끝내는 경우 → 턴 종료(`result`) 때 마지막 응답을 `report`로 자동 기록 | S3 |
| R3 막힘 | 권한 정책을 설정 파일로 미리 넣고, 규칙 밖 요청은 권한 RPC → 인스턴스 "막힘" 배지 + 오케스트레이터 깨우기 + 사람 알림. 시간 넘기면 거부 | 다른 git 루트 이동 같은 신뢰 확인 창이 비대화 모드에서 어떻게 나오는지 | S2 |
| R4 id | jadecode 인스턴스 id(ULID)는 서버가 정하고, 에이전트 세션 id는 매핑일 뿐. 재시작은 `--resume`(fork 없음) | 사람 붙기(터미널로 넘기기) 왕복 때 세션 id 유지 | S1, S4 |
| R5 명단 | 인스턴스·카드·메시지는 서버 상태 + 일지 파일. 허브 문서를 손으로 적을 일이 없다 | 색인 재구성 시간 | S5 |

## 조사 근거 (2026-11-08)

- herdr v0.9.5 소스(`/tmp/herdr-research/src`): 크레이트 2개(본체 약 29만 줄, lib 타깃 없음 / `ghostty-vt`), 이벤트 메모리 링 512개·연결당 100ms 폴링(`src/api/event_hub.rs`, `src/api/server.rs`), 상태 판독 TOML 매니페스트 24개 에이전트(`src/detect/manifests/`), Claude 통합 v11은 SessionStart 훅만, 재시작 때 셸에 resume 명령 타이핑(`src/agent_resume.rs`), 실험적 live handoff(`src/server/handoff.rs`), Android 미지원, 외부 PR 자동 닫음(`CONTRIBUTING.md`), 2026-05~10 릴리스 34회.
- rex 문서(`/tmp/rexdocs/md`): Unix 소켓·tailnet·tsnet 리스너, JSON Schema 자기 서술 API(`/llms.txt`), 빈틈없는 seq + 스냅샷(이력·재생 없음, 8MiB 뒤처지면 끊음), OSC 7501 Program Status Protocol rev 0.4, 승인 API 없음, 인증은 연결 방식 기준, 소스·라이선스 공개 없음, Android 클라이언트 없음.
- 에이전트 실행: Claude Code CLI 문서(headless, cli-reference, hooks, agent-sdk permissions·sessions, agent-view, legal-and-compliance), Codex app-server 문서, ACP 문서(agentclientprotocol.com). 세부는 `spikes.md`의 가설 칸에 옮겼다.
- 클라이언트 UI: zed-industries/zed(`crates/gpui_ios`), longbridge/gpui-kit v0.9.0, longbridge/nsh v0.4.0, egoist/mygo, SwiftTerm, Slint 1.12, DioxusLabs/blitz. 2026-11-08 GitHub 조회 기준이고 로컬 빌드는 하지 않았다.
