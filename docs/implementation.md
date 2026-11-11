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

- `jc-client`: 데이터 계층(`Board`)과 화면(`ui`)을 나눴다. 데이터 계층은 WebSocket JSON-RPC로 붙고, 이벤트 구독으로 스냅숏을 갱신한다.
- 화면: 보드(프로젝트 선택 + 단계 열), 카드(경로 진행·인스턴스·메시지·task.md), 대화(권한 승인 버튼 포함), 봇/인스턴스.
- 쓰기 동작은 별도 스레드·별도 연결로 실행한다(GPUI 실행기와 tokio를 섞지 않는다).
- `jadecode-client --dump`는 창 없이 스냅숏을 JSON으로 낸다(CI·헤드리스 시험용).
- **이 컨테이너에서는 창이 검게 나온다**: Vulkan 드라이버가 없어(`/dev/dri` 없음) 렌더 결과가 화면에 오지 않는다. GPUI 최소 예제도 같다. 창 생성·데이터 바인딩은 확인했고, 실제 픽셀 확인은 Mac(또는 GPU 있는 리눅스)에서 해야 한다.
- GPUI 0.2.2가 끌어오는 `zed-async-tar`가 `xattr 0.2`를 고정하는데 그 판이 최신 libc(aarch64)에서 `libc::ENOATTR`로 깨진다 → `vendor/xattr`에 상수만 고친 사본을 두고 `[patch.crates-io]`로 쓴다.

## 명세 대비 남은 것

- **단계 종료 조건·경로 검사·사람 절차 강제** — 지금 `card.advance`는 경로만 따라간다. 종료 조건(명령 검사·봇 판정·사람 승인)과 `policy`의 사람 절차 검사는 다음 단계.
- **워크트리 생성** — `card add --repo r=branch`는 앞머리에 적고 `--add-dir`로 이미 있는 폴더를 연다. `git worktree add`는 아직 안 부른다(레포 경로 설정 필요).
- **종료 조건 기반 단계 이동** — `card_advance`는 지금 경로만 따라간다. 명령 검사·리뷰 봇 판정·사람 승인을 붙이는 것이 다음.
- **봇 버전 관리(git 커밋)·자기개선(RSI)** — 사용자가 범위에서 뺐다. 봇은 폴더 + 기억(`memory/`)까지만.
- **클라이언트 마감** — 한국어 IME 입력(지금은 표시만), 마크다운 렌더, diff 뷰, 터미널 붙기. berth 데스크톱 코드를 참고.
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
