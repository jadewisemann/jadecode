# 구현 기록

명세(`SPEC.md`)와 아키텍처(`docs/architecture/`)를 코드로 옮긴 순서와, 지금 코드가 어디까지인지.

## 지금 있는 것 (2026-11-11)

`feat/server-core` 브랜치.

| 크레이트 | 내용 |
|---|---|
| `jc-core` | 일지(`events.jsonl`, 정본) + 색인(SQLite WAL, 투영) 쓰기·복구, `task.md` 앞머리·해시·비교 후 맞바꾸기 쓰기, 주제(마디 단위 일치), ULID/UUID, 워크스페이스 레이아웃, 기본 봇·정책 스캐폴드 |
| `jc-driver` | A2 드라이버: `claude -p` stream-json 상주, 격리 플래그, 화이트리스트 환경, interrupt, SIGTERM→SIGKILL(프로세스 그룹), 봇 spec·권한 정책 |
| `jc-mcp` | 인스턴스용 MCP 서버(stdio): `report`/`ask`/`propose`/`error`/`read_card`, 오케스트레이터용 `ack`/`send`/`reply`/`escalate`/`read_event`, `permission` RPC |
| `jc-server` | JSON-RPC 2.0 over WebSocket(Unix 0600 / 루프백 TCP), 단일 쓰기 스레드, 구독, `task.md` 감시, 인스턴스 관리자, 권한 게이트, 오케스트레이터 깨우기 |
| `jc-client` | **GPUI 데스크톱 클라이언트**: 보드(프로젝트별/전체 칸반), 카드 상세(경로 진행·메시지·task.md), 오케스트레이터 대화 + 권한 승인, 봇·인스턴스 화면. `--dump`로 창 없이 스냅숏 확인 |
| `jadecode-cli` | `init` `project add` `card add/list/show/status/next` `server` `rpc` `rpc-stream` `tail` `check` `bot list/add` `agents` `instance list/start/send/stop` `permission list/allow/deny` `policy` `top` |

테스트 95개(`cargo test`) + 실제 에이전트로 도는 시험 스크립트 5종.

실측(2026-11-11, 이 컨테이너):

| 시험 | 결과 |
|---|---|
| `cargo test` | 95 통과 |
| `tools/crash-test.sh 12 100` | `kill -9` 12회 × 100건 = 303줄, `check` 어긋남 0 |
| `tools/e2e.sh haiku` | 워커가 `greet.sh` 생성+실행권한, `report(done)`, 카드 상태 `waiting_human`, 단계 `implement→verify`, `check` ok |
| `tools/wake.sh haiku` | 깨우기 다이제스트 1건, 오케스트레이터 `ack(upto=이벤트 seq)`, `ok: true` |
| `tools/full.sh haiku` | 사람 발주 → 오케스트레이터가 카드 생성·워커 발주 → 워커가 `hello.txt` 작성·`report(done)` → 오케스트레이터 깨우기·`ack(27)` → `check` ok. 이벤트 38건 |
| live 계약(드라이버) | 두 턴 같은 프로세스, 세션 id 고정, `--resume` 이력 유지, 턴별 사용량, 사용자 MCP 격리 통과 |
| `tools/agents.sh` | **claude·opencode·omp·pi 네 백엔드 모두** 카드 하나를 파일 생성 + `report(done)`까지 완료(2026-11-11) |
| `tools/client.sh` | 스냅숏(프로젝트 1·카드 5·메시지 183·head 429) + 1280x820 창 생성 확인 |

## 아키텍처 문서 → 코드 대응

| 결정 | 코드 | 비고 |
|---|---|---|
| ② B2 파일 정본 + 카드별 일지 + 재생성 색인 | `jc-core/src/store.rs`, `index.rs` | 쓰기 규칙 1~6 반영 |
| B2-1 쓰기 순서·승인 | `Store::append` | 응답 seq는 fsync·색인 커밋 뒤 |
| B2-2 복구 | `Store::open` | 찢긴 줄 자르기, 빈틈 뒤 줄 격리, 색인 재생성 |
| B2-4 맞바꾸기 쓰기 | `store::write_cas` | `renameat2(RENAME_EXCHANGE)`, macOS는 `renamex_np` 자리만 |
| B2-5 감시 | `jc-server::server::start_watcher` | 비재귀, 읽기 알림 무시, 해시로 자기 쓰기 거르기 |
| B2-6 오프라인 변경 | `Store::open`(투영), `check_card` | `card.reconciled` / `card.state_change_rejected` |
| ④ P1 JSON-RPC over WebSocket | `jc-server/src/rpc.rs`, `server.rs` | Unix 소켓 + 루프백 TCP |
| S5 구독(seq, 마디 단위 일치) | `topic::topic_matches`, `subscription` | 구독자별 버퍼 없음 |
| A2 드라이버 계약 | `jc-driver/src/proc.rs`, `spec.rs` | 격리 플래그·화이트리스트 환경·세션 id 규칙·interrupt·종료 |
| S2 권한 무인 실행 | `crates/jc-server/src/instances.rs`, `permission.request` | 25분 상한, 장애 시 도구 오류, MCP 미연결이면 재시작 |
| S3 메시지·깨우기 | `jc-mcp/src/tools.rs`, `Manager::wake_loop` | 다이제스트 0.3초 모으기, ack, 자동 보고는 깨우지 않음 |
| C1 봇 = 폴더 | `jc-core/src/scaffold.rs`, `jc-driver/src/spec.rs` | 봇 4개 기본, 정책은 `policy/`(봇 밖) |
| 다중 에이전트(R2) | `jc-driver/src/agent.rs`, `acp.rs`, `pi.rs` | claude·opencode·omp·pi. ACP 2개, pi 확장 |
| ④ D1 Mac GPUI | `jc-client/src/{board,format,ui}.rs` | 명세 첫 화면 5가지 |

### 스파이크가 정한 규칙 중 코드가 지키는 것

- 인스턴스당 `claude` 프로세스 하나. 죽으면 같은 카드 폴더에서 `--resume`, 같은 세션 id.
- 시작 `init`의 `mcp_servers[]`에 `jadecode`가 `connected`가 아니면 즉시 다시 띄운다(S2 H2: 그대로 두면 첫 권한 요청에서 `result` 없이 죽는다).
- 권한 막힘의 정본은 서버의 `permission.requested`/`permission.resolved` 일지다. `permission_denials`에 기대지 않는다.
- 처리기 상한(기본 25분) < CLI 무응답 중단(1800초).
- `AskUserQuestion`은 권한 RPC에서 `msg.ask`로 바꾼다.
- 도구 없는 턴의 자동 `report(auto)`는 깨우지 않는다(헛 깨우기 7건 관측).
- `--setting-sources "" --strict-mcp-config` + 화이트리스트 환경으로 사용자 전역 지침·훅·커넥터 MCP를 뺀다.
- `mcp__jadecode*`와 `ToolSearch`는 기본 허용(안 하면 도구 호출마다 사람 승인이 필요해 턴이 멈춘다 — 구현 중 발견).

## 에이전트 백엔드 (실측 2026-11-11)

| 에이전트 | 구동 | 구조화 출력 | jadecode 도구 | 파일 쓰기 | 확인 |
|---|---|---|---|---|---|
| Claude Code 2.1.322 | `claude -p --input-format stream-json` | stream-json | MCP(stdio) | 에이전트 자기 도구 | 통과 |
| OpenCode 2.1.1 | `opencode acp` | ACP | ACP `mcpServers` | **클라이언트 fs** | 통과 |
| omp 18.12.2 | `omp acp` | ACP | ACP `mcpServers` | **클라이언트 fs** | 통과 |
| pi 0.73.1 | `pi --mode rpc` | 자체 JSONL | **없음 → 확장(extension)** | 에이전트 자기 도구 | 통과 |

- ACP는 에이전트가 파일을 직접 만지지 않는다. `fs/read_text_file`·`fs/write_text_file`을 클라이언트가 처리하므로 **카드 폴더·워크트리 밖 쓰기는 서버가 거부**한다(`acp::in_scope`).
- ACP 권한은 `session/request_permission`으로 온다. 사람/정책 답을 `optionId`(allow_once/reject_once)로 돌려준다.
- pi에는 MCP가 없다("does not include built-in MCP"). 그래서 `pi.registerTool()` 확장 파일을 생성해 `jadecode_report`/`ask`/`propose`/`read_card`를 주입하고, 도구 구현은 `jadecode rpc` CLI를 부른다(절대 경로 + `--socket`).
- 인증 환경변수(`ANTHROPIC_API_KEY`·`OPENAI_API_KEY`·`DEEPSEEK_API_KEY` 등)만 자식에 넘긴다. `CLAUDECODE*`는 계속 차단한다.
- 봇의 `agent` 칸이 백엔드를 고른다. `instance.start --agent`로 덮어쓸 수 있다. `jadecode agents`가 설치·버전·MCP 지원을 보여 준다.

## 클라이언트 (GPUI)

화면 구성(2026-11-24, PoC → 실사용 수준으로 다시 짰다):

| 자리 | 내용 |
|---|---|
| 사이드바 | 수집함 · 확인 요청(승인·질문 수) · 오케스트레이터 · 보드 · 에이전트 · 연결, 프로젝트 목록(+ 새 프로젝트), 새 판 알림, 연결 상태 |
| 가운데 | 카드 **목록**(확인 필요 → 진행 중 → 대기 → 끝남) 또는 **칸반**(수집 · 단계 열 · 회신 대기, 가로 스크롤, 카드를 끌어 경로 안의 다른 단계로 옮긴다). `⌘L`로 전환 |
| 오른쪽 | 카드 스레드: 단계 진행, 워커 시작·멈추기·에이전트 고르기, **대화**(에이전트 출력·도구 호출 묶음·권한 승인 버튼·사람 말) / **task.md**(마크다운) / **변경사항**(카드 폴더 git diff) |
| 입력 | 카드 스레드 입력: 워커가 있으면 그 워커에게, 없으면 워커를 띄워 첫 지시로 보낸다. 오케스트레이터 입력: 없으면 띄운다 |
| 첫 실행 | 프로젝트가 없으면 환영 화면에서 바로 만들고, 에이전트 설치 상태를 보여 준다 |

- **텍스트 입력**(`ui/input.rs`): GPUI 예제 입력기를 바탕으로 IME(한글 조합 밑줄·후보 창 위치), 단어 이동,
  복사·붙여넣기, 긴 글 가로 밀기, Enter/Esc 이벤트를 붙였다. 예제의 조합 선택 범위 계산 오류를 고쳤다.
  여러 줄 칸(`multiline(n)`)은 칸 너비에서 줄을 바꾸고 n줄까지 자란다(`⇧Enter` 줄바꿈, 위아래 화살표, 붙여넣기 줄 유지).
  비밀 칸(`secret()`)은 •로 가리고 복사를 막는다(Slack 토큰).
- **단축키·메뉴**: `⌘N` 새 카드, `⇧⌘N` 새 프로젝트, `⌘1~6` 화면(수집함·확인 요청·오케스트레이터·보드·에이전트·연결),
  `⌘L` 목록/칸반, `⌘K` 입력칸, `⌘R` 새로고침, `Esc` 닫기, `⌘Q` 종료.
  jadecode(정보·업데이트 확인)·파일·편집·보기·도움말(문제 보고·로그 폴더) 메뉴.
- **레포와 워크트리**(`jc-server/src/worktree.rs`): 프로젝트를 만들 때 코드 레포 폴더를 고르면(`project.add {repo_path}`)
  `project.toml`의 `[[repos]]`에 `path`가 남고, 새 카드는 그 레포를 갖는다. 워커를 시작하면
  `<카드>/<레포>`에 `git worktree add -b jc/<카드 폴더>`를 열고, 레포가 하나면 워커의 cwd가 그 워크트리다
  (카드 폴더는 함께 연다). 그래서 사람의 작업 폴더는 건드리지 않고, 변경사항 탭과 PR 버튼이 바로 동작한다.
- **로그·오류**: 터미널에서 띄우지 않으면 표준 오류를 `~/Library/Logs/jadecode/jadecode.log`(리눅스는
  `~/.local/state/jadecode/`)로 돌린다(5MB에서 한 번 돌림). 시작 실패와 화면 스레드 패닉은 시스템 대화상자로
  알린다. 쓰기 동작 하나가 패닉해도 동작 스레드는 살아남는다. 도움말 → 문제 보고는 판·운영체제를 채운 이슈 양식을 연다.
- **판 번호·업데이트**: 릴리즈 빌드는 `JADECODE_VERSION`을 넣어 빌드한다(`jc_core::version`). 앱은 시작할 때
  GitHub 최신 릴리즈와 비교해 새 판이 있으면 사이드바에 알리고, 누르면 함께 설치된 `jadecode update`로 갈아 끼운다.
- **데이터**: 화면이 그릴 값은 `view_model.rs`의 순수 함수가 만든다(묶기·받은편지함·스레드 합치기·마크다운·diff) — GPUI 없이 테스트한다.
  스냅숏의 `recent`(최근 이벤트 1,000건)와 `events.tail`로 받은 주제별 이력을 합쳐 스레드를 그린다.
- **연결**: 구독이 끊기면 2초마다 다시 붙는다. 사이드바 아래 점이 빨갛게 바뀐다. 쓰기 스레드도 끊기면 다시 연다.
- **서버 변경**: `instance.send`가 사람 말을 `msg.human` 이벤트로 남긴다(대화 앞뒤가 이력에 이어진다).
- 쓰기 동작은 별도 스레드·별도 연결로 실행한다(GPUI 실행기와 tokio를 섞지 않는다). 결과는 80ms 주기로 화면에 반영한다.
- **로컬 백엔드**(`jc-client::local`): 서버에 못 붙으면 앱 프로세스 안의 백그라운드 스레드에서 `jc_server::serve`를 돌리고 그 소켓에 붙는다. 워크스페이스가 없으면 `~/jc/work`를 만든다. macOS 앱은 PATH가 짧아 로그인 셸 PATH와 흔한 설치 위치(`/opt/homebrew/bin` 등)를 붙인다. `JADECODE_NO_EMBED=1`이면 띄우지 않는다.
- `jadecode-client --dump`는 창 없이 스냅숏을 JSON으로 낸다(CI·헤드리스 시험용).
- **이 컨테이너에서는 창이 검게 나온다**: Vulkan 드라이버가 없어(`/dev/dri` 없음) 렌더 결과가 화면에 오지 않는다. lavapipe(소프트웨어 Vulkan)+Xvfb에서도 창은 뜨지만 픽셀은 검다. 실제 픽셀 확인은 Mac(또는 GPU 있는 리눅스)에서 해야 한다.
- GPUI 0.2.2가 끌어오는 `zed-async-tar`가 `xattr 0.2`를 고정하는데 그 판이 최신 libc(aarch64)에서 `libc::ENOATTR`로 깨진다 → `vendor/xattr`에 상수만 고친 사본을 두고 `[patch.crates-io]`로 쓴다.

## 바깥 사이클: 수집함과 커넥터

안쪽 사이클(카드·워커·단계)과 별도로, 코드 밖에서 들어온 해야 할 일을 모아 일로 만드는 바깥 사이클이 있다.

- **수집함** — 토픽 `project/<p>/intake`. 이벤트 `intake.received`(외부 id로 중복 제거) → `intake.triaged`(카드로 만들거나 보관) → 카드가 끝나면 상태가 `reply_pending`(회신 대기) → `intake.replied`/`intake.closed`. 상태는 이벤트를 접어 계산한다(`jc-server/src/intake.rs`).
- **컨텍스트를 작게 옮긴다** — 카드로 만들 때 원문 전체 대신 짧은 brief + done_when + 원문 링크만 카드 본문에 넣는다. 워커는 원문이 필요하면 링크를 연다.
- **오케스트레이터** — `intake.received`가 깨우기 사유이고, MCP 도구 `intake_list`·`intake_get`·`intake_triage`로 분류할 수 있다.
- **칸반** — 왼쪽 "수집"(바깥·들어온 일), 가운데 단계 열(안쪽·코드), 오른쪽 "회신 대기"(바깥·돌려줄 일)의 세 구역으로 두 사이클을 한 화면에 보인다.

### 커넥터 (MCP)

출처마다 MCP 서버(stdio)를 하나씩 띄워 가져오기 도구를 부르고, 결과를 필드 경로로 수집 항목에 대응시킨다(`jc-server/src/connectors.rs`). 설정은 `.jadecode/connectors.toml`(권한 0600)에만 있고, RPC 응답에서는 비밀 값이 가려진다. 서버는 10초마다 확인해 연결마다 `interval_sec`(최소 15초) 간격으로 동기화한다(`JADECODE_NO_CONNECTORS=1`이면 끈다). 답장은 사람이 앱에서 승인해 보낼 때만 `reply` 도구로 나간다.

첫 프리셋은 Slack(`@modelcontextprotocol/server-slack`, Node.js 필요)이다. 다른 출처는 `custom`으로 직접 적는다.

```toml
[[connector]]
name = "mail"
project = "web"
preset = "custom"
command = "npx"
args = ["-y", "some-mail-mcp"]
interval_sec = 120
[connector.env]
MAIL_TOKEN = "…"
[connector.fetch]
tool = "list_unread"          # 가져오기 도구
args = { limit = 20 }
items = "messages"            # 결과 JSON에서 목록 경로
id = "id"                     # 항목별 필드 경로
title = "subject"
body = "snippet"
from = "from"
url = "https://mail.example/{id}"
skip_if = ""                  # 이 필드가 있으면 건너뜀(예: 봇 메시지)
[connector.reply]             # 선택: 없으면 답장 불가
tool = "send_reply"
args = { message_id = "{id}", text = "{text}" }
```

RPC: `intake.add|list|get|triage|close|reply|sync_github`, `connectors.list|presets|save|remove|sync|test`.

## 명세 대비 남은 것

- **단계 종료 조건·경로 검사·사람 절차 강제** — 지금 `card.advance`는 경로만 따라간다. 종료 조건(명령 검사·봇 판정·사람 승인)과 `policy`의 사람 절차 검사는 다음 단계.
- **종료 조건 기반 단계 이동** — `card_advance`는 지금 경로만 따라간다. 명령 검사·리뷰 봇 판정·사람 승인을 붙이는 것이 다음.
- **봇 버전 관리(git 커밋)·자기개선(RSI)** — 사용자가 범위에서 뺐다. 봇은 폴더 + 기억(`memory/`)까지만.
- **클라이언트 다음 단계** — 내장 터미널 패널(PTY). (커밋·PR은 워커에게 맡기는 버튼, 여러 줄 입력, 칸반 끌어 놓기, 데스크톱 알림은 들어갔다.)
- **GPU 프레임 병목** — 이 환경에서 못 잰다(`docs/performance.md` 참고). Mac에서 Metal System Trace로
  재야 한다. 클라이언트 데이터 계층은 이미 고쳤다(스냅숏 869us → 0.025us).
- **재시작 비용** — `Store::open`이 매번 `task.md` 1,000개를 읽어 해시를 비교한다(1.5초).
  mtime/size 선비교로 줄일 수 있다.
- **구독 팬아웃** — 구독자마다 같은 구간을 읽는다. 최근 512건 링버퍼를 공유하면 줄어든다.
- **원격(Tailscale)** — 첫 버전 범위 밖.
- **터미널로 넘기기(사람 붙기)** — 대화 화면(이벤트 그리기)이 첫 버전, PTY 넘기기는 2차.

## 구현 중 확인 (Mac이 필요한 것)

- `renameat2` 대신 macOS `renamex_np(RENAME_SWAP)` — `store::exchange`가 `false`를 돌려 rename으로 물러난다(동시 편집 보호가 약해짐).
- FSEvents 지연·합침, APFS `fsync` vs `F_FULLFSYNC`.
- 카드 1,000개·이벤트 10만 건 재구성 시간(S5 Linux: 0.46~2.0초).

## CI · 릴리즈

`.github/workflows/ci.yml`(push·PR): 리눅스·macOS에서 server(GPUI 없이)·client(GPUI 포함)
빌드+테스트, fmt+clippy. 다섯 잡 모두 통과(2026-11-11).

`.github/workflows/release.yml`(태그 `v*` 또는 수동): `linux-x86_64`·`macos-aarch64`
바이너리를 아카이브로 만들어 Draft Release에 붙인다(설치 스크립트·체크섬 포함).

- 첫 릴리즈: **v0.1.0-alpha** — `jadecode-v0.1.0-alpha-{linux-x86_64,macos-aarch64}.tar.gz`(12MB/7MB)
- 릴리즈 프로파일에서 debug 심볼을 빼고 strip+thin LTO → 로컬 609MB → 26MB
- GPUI는 `jc-client`의 `ui` feature 뒤에 있다. 기본 빌드는 데이터 계층만이라 X11/Vulkan 없이도 된다.
- CI에서 고친 것: macOS 유닉스 소켓 경로 한도, 테스트 임시 경로 충돌, `/bin/false` 이식성,
  `jc-bench`→`jc-client`(GPUI) 의존으로 인한 링크 실패, `macos-14` 라벨 미지원 → `macos-latest`.

## macOS 앱 번들 (.app)

`tools/make-app.sh`가 `jadecode-client` 바이너리로 `jadecode.app`을 만든다.
`release.yml`의 macOS 잡과 `tools/package.sh`(Darwin에서만)가 같은 스크립트를 부른다.

```
jadecode.app/Contents/
  Info.plist            CFBundleName/DisplayName=jadecode, id=dev.jadewisemann.jadecode,
                        CFBundleExecutable=jadecode-client, CFBundlePackageType=APPL,
                        CFBundleShortVersionString=태그의 숫자 부분, LSMinimumSystemVersion=14.0
  MacOS/jadecode-client 실행 권한 있는 바이너리
  Resources/            아이콘 자리. assets/jadecode.icns 가 있으면 자동으로 넣는다.
```

- **아카이브는 tar.gz 하나로 유지한다.** `.app`은 디렉터리지만 tar는 실행 권한·심볼릭 링크·
  번들 구조를 그대로 보존하고, 이름 규칙(`jadecode-<tag>-macos-aarch64.tar.gz`)도 안 깨진다.
  zip을 따로 내면 사용자가 두 파일을 받아야 하고 체크섬·설치 스크립트도 둘로 늘어난다.
  `install.sh`가 아카이브 안의 `.app`을 `/Applications`로 복사한다(쓸 수 없으면 안내만).
- **`--check` 모드**: 맥이 없어도 번들 구조·실행 권한·`Info.plist`(python3 `plistlib`)를 검사한다.
  CI의 macOS 클라이언트 잡이 빌드 직후 이 검사를 돌린다.
- **버전**: 태그 `v0.1.0-alpha` → `CFBundleShortVersionString` `0.1.0`. Apple은 숫자·점만 받으므로
  `-alpha` 같은 꼬리는 버린다(원래 값이 다르면 로그에 남긴다).
- 이 환경(리눅스 aarch64)에서 확인한 것: `bash -n` 두 스크립트, plist 파싱, 구조 검사,
  GPUI 바이너리가 없을 때 안내 후 종료(코드 3). 맥 실기 실행은 CI에서만 확인된다.

### 서명·공증 (자리만 있음)

지금은 서명하지 않으므로 Gatekeeper 경고가 뜬다(첫 실행: 우클릭 → 열기).
실제 배포에는 Apple Developer Program 인증서가 필요하다. 스크립트는 환경변수가 있을 때만 서명한다.

```sh
# 1. 인증서를 키체인에 넣고 이름 확인
security find-identity -v -p codesigning

# 2. 서명 (make-app.sh 안에서 codesign --deep --force --options runtime --timestamp)
CODESIGN_IDENTITY="Developer ID Application: 이름 (TEAMID)" tools/make-app.sh

# 3. 공증 자격을 한 번만 키체인에 저장 (앱 암호 = appleid.apple.com 에서 발급)
xcrun notarytool store-credentials jadecode \
  --apple-id <id> --team-id <TEAMID> --password <앱 암호>

# 4. 공증 + 스테이플 (스크립트가 ditto 로 zip 을 만들어 submit --wait, stapler staple)
NOTARY_PROFILE=jadecode CODESIGN_IDENTITY="Developer ID Application: 이름 (TEAMID)" tools/make-app.sh
```

CI에서는 `CODESIGN_IDENTITY`·`NOTARY_PROFILE` 저장소 비밀로 같은 일을 한다. 인증서(`.p12`)를
러너 키체인에 넣는 단계는 아직 없다. **남은 일: 인증서·앱 암호 발급, 릴리즈 워크플로에
키체인 가져오기 단계 추가, 실제 공증 통과 확인.**

## macOS에서 발견한 것 (CI 첫 실행)

- **유닉스 소켓 경로 길이**: macOS는 `sun_path[104]`라 경로가 104바이트를 넘으면 `bind`가 실패한다.
  GitHub Actions의 임시 폴더(`/var/folders/XX/…/T`)는 약 50자라 테스트 경로가 116자가 되어
  통합 테스트 8개가 전부 실패했다(리눅스에서는 108자라 통과).
  → `jc_core::short_temp_root()`(`/tmp/jc<번호>`)로 테스트 경로를 짧게 하고,
  서버는 경로가 한도를 넘으면 **원인이 담긴 오류**로 끝나게 했으며(`"유닉스 소켓 경로가 너무 길다(N자 > 103자)"`),
  CLI는 기본 소켓이 너무 길면 `/tmp/jc-<워크스페이스>.sock`으로 자동 물러난다.
  클라이언트는 `.jadecode/server.json`의 `socket` 값을 먼저 본다(서버가 실제로 연 곳).
- 실측: 121자 경로에서 `jadecode server`가 `/tmp/jc-ws.sock`으로 뜨고 RPC가 동작한다.

## 이 컨테이너에서 발견한 것

- `claude --session-id`는 진짜 UUID를 요구한다(ULID 문자열은 `Invalid session ID`). `jc_core::ulid::uuid4()`를 쓴다.
- 워크스페이스에 쓰기 프로세스는 하나여야 한다. 서버가 도는 동안 CLI가 직접 쓰면 서로 다른 `next_seq`로 색인에 써서 `UNIQUE constraint failed: events.seq`가 난다 → `flock` 잠금 + 서버가 살아 있으면 CLI가 RPC로 우회.
- MCP 도구 호출에서 런타임을 매번 새로 만들면 연결이 죽는다. 호출마다 연결을 새로 여는 단순 방식이 안전하다.
- 권한 대기를 관리자 루프 안에서 하면 깨우기·메시지가 멈춘다. 별도 태스크로 뺐다.
