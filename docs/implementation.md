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
| `jadecode-cli` | `init` `project add` `card add/list/show/status/next` `server` `rpc` `rpc-stream` `tail` `check` `bot list` `instance list/start/send/stop` `permission list/allow/deny` `policy` `top` |

테스트 69개(`cargo test`) + 실제 `claude`로 도는 live 계약 테스트 5개(`JADECODE_LIVE=1`, 기본 skip).

실측(2026-11-11, 이 컨테이너):

| 시험 | 결과 |
|---|---|
| `cargo test` | 69 통과 |
| `tools/crash-test.sh 12 100` | `kill -9` 12회 × 100건 = 303줄, `check` 어긋남 0 |
| `tools/e2e.sh haiku` | 워커가 `greet.sh` 생성+실행권한, `report(done)`, 카드 상태 `waiting_human`, 단계 `implement→verify`, `check` ok |
| `tools/wake.sh haiku` | 깨우기 다이제스트 1건, 오케스트레이터 `ack(upto=이벤트 seq)`, `ok: true` |
| `tools/full.sh haiku` | 사람 발주 → 오케스트레이터가 카드 생성·워커 발주 → 워커가 `hello.txt` 작성·`report(done)` → 오케스트레이터 깨우기·`ack(27)` → `check` ok. 이벤트 38건 |
| live 계약(드라이버) | 두 턴 같은 프로세스, 세션 id 고정, `--resume` 이력 유지, 턴별 사용량, 사용자 MCP 격리 통과 |

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

### 스파이크가 정한 규칙 중 코드가 지키는 것

- 인스턴스당 `claude` 프로세스 하나. 죽으면 같은 카드 폴더에서 `--resume`, 같은 세션 id.
- 시작 `init`의 `mcp_servers[]`에 `jadecode`가 `connected`가 아니면 즉시 다시 띄운다(S2 H2: 그대로 두면 첫 권한 요청에서 `result` 없이 죽는다).
- 권한 막힘의 정본은 서버의 `permission.requested`/`permission.resolved` 일지다. `permission_denials`에 기대지 않는다.
- 처리기 상한(기본 25분) < CLI 무응답 중단(1800초).
- `AskUserQuestion`은 권한 RPC에서 `msg.ask`로 바꾼다.
- 도구 없는 턴의 자동 `report(auto)`는 깨우지 않는다(헛 깨우기 7건 관측).
- `--setting-sources "" --strict-mcp-config` + 화이트리스트 환경으로 사용자 전역 지침·훅·커넥터 MCP를 뺀다.
- `mcp__jadecode*`와 `ToolSearch`는 기본 허용(안 하면 도구 호출마다 사람 승인이 필요해 턴이 멈춘다 — 구현 중 발견).

## 명세 대비 남은 것

- **단계 종료 조건·경로 검사·사람 절차 강제** — 지금 `card.advance`는 경로만 따라간다. 종료 조건(명령 검사·봇 판정·사람 승인)과 `policy`의 사람 절차 검사는 다음 단계.
- **워크트리 생성** — `card add --repo r=branch`는 앞머리에 적고 `--add-dir`로 이미 있는 폴더를 연다. `git worktree add`는 아직 안 부른다(레포 경로 설정 필요).
- **종료 조건 기반 단계 이동** — `card_advance`는 지금 경로만 따라간다. 명령 검사·리뷰 봇 판정·사람 승인을 붙이는 것이 다음.
- **봇 버전 관리(git 커밋)·자기개선(RSI)** — 사용자가 범위에서 뺐다. 봇은 폴더 + 기억(`memory/`)까지만.
- **Mac 클라이언트(GPUI)** — berth 데스크톱 코드를 출발점으로. 별도 work.
- **원격(Tailscale)** — 첫 버전 범위 밖.
- **터미널로 넘기기(사람 붙기)** — 대화 화면(이벤트 그리기)이 첫 버전, PTY 넘기기는 2차.

## 구현 중 확인 (Mac이 필요한 것)

- `renameat2` 대신 macOS `renamex_np(RENAME_SWAP)` — `store::exchange`가 `false`를 돌려 rename으로 물러난다(동시 편집 보호가 약해짐).
- FSEvents 지연·합침, APFS `fsync` vs `F_FULLFSYNC`.
- 카드 1,000개·이벤트 10만 건 재구성 시간(S5 Linux: 0.46~2.0초).

## 이 컨테이너에서 발견한 것

- `claude --session-id`는 진짜 UUID를 요구한다(ULID 문자열은 `Invalid session ID`). `jc_core::ulid::uuid4()`를 쓴다.
- 워크스페이스에 쓰기 프로세스는 하나여야 한다. 서버가 도는 동안 CLI가 직접 쓰면 서로 다른 `next_seq`로 색인에 써서 `UNIQUE constraint failed: events.seq`가 난다 → `flock` 잠금 + 서버가 살아 있으면 CLI가 RPC로 우회.
- MCP 도구 호출에서 런타임을 매번 새로 만들면 연결이 죽는다. 호출마다 연결을 새로 여는 단순 방식이 안전하다.
- 권한 대기를 관리자 루프 안에서 하면 깨우기·메시지가 멈춘다. 별도 태스크로 뺐다.
