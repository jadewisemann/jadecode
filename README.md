# jadecode

오케스트레이터 봇이 카드를 발행·이동하고, 봇 인스턴스가 카드에서 일하는 task 기반 칸반.

- 무엇을 하는가: [`SPEC.md`](SPEC.md)
- 무엇으로 만드는가: [`docs/architecture/`](docs/architecture/) (후보 비교, 스파이크 결과, 질문 목록)
- 지금 어디까지: [`docs/implementation.md`](docs/implementation.md)

서버 · 네 에이전트 백엔드(Claude Code·OpenCode·omp·pi) · MCP 메시지 도구 · CLI · **GPUI 데스크톱 클라이언트**까지 들어 있다.

## 빠르게

```sh
cargo test                                  # 69개
cargo build

J=target/debug/jadecode
export JADECODE_MCP=$PWD/target/debug/jadecode-mcp   # 서버가 인스턴스에 붙일 MCP

$J init ~/jc/acme --name Acme        # 봇 4개(orchestrator/worker/reviewer/researcher)와 policy/가 생긴다
cd ~/jc/acme
$J server &                          # .jadecode/jadecode.sock (0600). 쓰기 프로세스는 하나만(flock)
$J project add api
$J card add "로그인 throttle" --project api --path implement,verify
$J card list
$J instance start worker --project api --card <카드 폴더 이름>
$J instance list
$J top                               # 단계별 보드
$J tail --limit 10
$J permission list                   # 사람 답을 기다리는 권한 요청
$J permission allow 42 "merge ok"    # 그 요청 승인
$J check                             # 일지·색인 전수 대조

jadecode-client                      # GPUI 창(서버가 떠 있어야 한다)
jadecode-client --dump               # 창 없이 보드 스냅숏 JSON
```

실제 Claude Code로 끝까지 돌려 보려면(구독 로그인 필요, 과금):

```sh
./tools/e2e.sh haiku        # 카드 생성 → 워커 작업 → report(done) → 단계 이동
./tools/wake.sh haiku       # 워커 이벤트 → 오케스트레이터 다이제스트 → ack
./tools/full.sh haiku       # 사람 발주 → 오케스트레이터 → 카드·워커 → 보고 → 깨우기(전체)
./tools/agents.sh           # 네 백엔드로 카드 하나씩(각 에이전트 로그인 필요)
./tools/client.sh           # 클라이언트 스냅숏 + 창 생성 시험
./tools/crash-test.sh 12 100  # kill -9 반복 복구
JADECODE_LIVE=1 cargo test -p jc-driver --test live -- --test-threads=1   # 드라이버 계약
```

## 구조

```
crates/jc-core      일지(정본)·색인(재생성 가능)·task.md·레이아웃·봇 스캐폴드
crates/jc-driver    에이전트 백엔드: claude stream-json · ACP(opencode·omp) · pi RPC
crates/jc-mcp       인스턴스용 MCP 서버(report/ask/propose/error, ack/send/reply, permission RPC)
crates/jc-server    JSON-RPC over WebSocket, 쓰기 스레드, 구독, 감시, 인스턴스·권한·깨우기
crates/jc-client    GPUI 데스크톱 클라이언트(보드·카드·대화·봇)
crates/jadecode-cli `jadecode` 바이너리
vendor/xattr        GPUI 빌드용 xattr 0.2 사본(최신 libc 호환 패치)
tools/              e2e.sh · wake.sh · full.sh · agents.sh · client.sh · crash-test.sh
```

## 에이전트 백엔드

봇 정의(`bots/<name>/bot.toml`)의 `agent` 칸이 고른다.

| agent | 구동 | jadecode 도구 |
|---|---|---|
| `claude` | `claude -p` stream-json | MCP |
| `opencode` | `opencode acp` | ACP MCP |
| `omp` | `omp acp` | ACP MCP |
| `pi` | `pi --mode rpc` | **확장(extension)** — pi는 MCP를 지원하지 않는다 |

```sh
jadecode agents                                  # 설치·버전·MCP 지원 확인
jadecode bot add fast --agent omp --model deepseek/deepseek-v4-flash
jadecode instance start fast --project api --card <카드> --agent omp
```

워크스페이스 모양(파일이 정본이다):

```
<workspace>/
  jadecode.toml
  AGENTS.md
  bots/<bot>/          bot.toml · instructions.md · memory/   (봇 정의)
  policy/              policy.toml · permissions/<bot>.json   (사람만 정하는 값)
  events/global.jsonl  봇·정책·전역
  work/<project>/
    project.toml
    events.jsonl       프로젝트 일지(오케스트레이터 대화·카드 생성)
    <card>/
      task.md          앞머리 `jadecode:` 칸은 서버 소유, 본문은 누구나
      events.jsonl     그 카드의 이벤트·메시지(덧붙이기만)
  .jadecode/           색인·소켓·인스턴스 설정·복구 메모(정본 아님, 지워도 된다)
```

## 지키는 규칙

스파이크(S1~S5, S9)가 실측으로 정한 것 중 코드가 지는 부분이다. 근거는 [`docs/architecture/candidates.md`](docs/architecture/candidates.md), [`docs/architecture/spike-results.md`](docs/architecture/spike-results.md).

- **쓰기 순서**: 단일 쓰기 스레드가 seq 부여 → 일지 덧붙이기 → fsync → 색인 커밋 → 그 뒤 응답·알림. 응답받은 seq는 이미 fsync됐다.
- **쓰기 프로세스는 하나**: 워크스페이스마다 `flock`. 서버가 돌면 CLI는 RPC로 우회한다.
- **색인은 투영**: 지우거나 깨져도 일지와 `task.md`에서 다시 만든다.
- **복구**: 찢긴 마지막 줄은 자르고 `.jadecode/recovery/torn.log`에 남긴다. seq 빈틈 뒤 줄은 미승인으로 `gap.log`로 옮긴다.
- **`task.md` 동시 편집**: 서버 쓰기는 비교 후 맞바꾸기(`renameat2(RENAME_EXCHANGE)`). 바깥 편집이 앞머리 `jadecode:` 칸을 바꾸면 되돌리고 `card.state_change_rejected`. 본문 변경은 받아들이고 `card.file_changed`.
- **구독**: 구독자별 버퍼가 없다. `from_seq` 뒤를 색인에서 읽어 보낸다. 주제는 마디 단위 앞부분 일치(`project/p1` ≠ `project/p10`).
- **인스턴스**: 하나당 `claude` 프로세스 하나, 세션 id는 서버가 만든 UUID. 죽으면 같은 폴더에서 `--resume`. 시작 `init`에 MCP가 `connected`가 아니면 즉시 재시작.
- **권한**: 정책은 `--settings`로 넣고, 규칙 밖은 `mcp__jadecode__permission` RPC로 온다. 사람 답 상한 25분(< CLI 1800초). 막힘은 `permission.*` 일지가 정본.
- **깨우기**: 오케스트레이터에게 0.3초 모아 다이제스트 사용자 턴으로 넣는다. 도구 없는 턴의 자동 보고는 깨우지 않는다.

## RPC

전송은 WebSocket(첫 버전은 유닉스 소켓 0600, `--port`로 루프백 TCP). `server.info`가 메서드 목록을 자기 서술한다.

| 메서드 | 하는 일 |
|---|---|
| `server.info` / `server.stats` | 이름·메서드 목록 / 쓰기 묶음·seq·인스턴스·대기 권한 |
| `projects.list` / `project.add` | 프로젝트 목록 / 만들기 |
| `cards.list` `{project?, stage?}` | 카드 투영 목록 |
| `card.add` `{project,title,path?,repos?}` | 카드 만들기(색인 등록까지) |
| `card.get` `{card}` | `task.md` 원문 + 앞머리 + 색인 상태 |
| `card.set_status` / `card.advance` | 상태 칸 / 다음 단계(끝이면 `4000 path_end`) |
| `card.append_event` `{card,kind,body}` | 카드 일지에 메시지 |
| `events.append` / `events.subscribe` / `events.head` / `events.tail` | 일지 쓰기·구독·머리·최근 |
| `instances.list` / `instance.start` / `instance.send` / `instance.stop` | 봇 인스턴스 |
| `permission.request` / `permission.resolve` | 권한 RPC(Claude Code가 부름) / 사람 답 |
| `bots.list` / `policy.get` | 봇 정의 / 사람이 정한 값 |

`card` 인자는 카드 폴더 이름, 상대 경로(`work/p/c`), ULID(앞부분) 중 아무거나 된다.
