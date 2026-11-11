# jadecode

오케스트레이터 봇이 카드를 발행·이동하고, 봇 인스턴스가 카드에서 일하는 task 기반 칸반.

- 무엇을 하는가: [`SPEC.md`](SPEC.md)
- 무엇으로 만드는가: [`docs/architecture/`](docs/architecture/) (후보 비교, 스파이크 결과, 질문 목록)

이 저장소에는 지금 **서버 코어**가 들어 있다. Mac 클라이언트와 에이전트 드라이버는 다음 단계다([`docs/implementation.md`](docs/implementation.md)).

## 빠르게

```sh
cargo test                      # 38개 테스트

cargo build
J=target/debug/jadecode
$J init ~/jc/acme --name Acme
cd ~/jc/acme
$J project add api
$J card add "로그인 throttle" --project api --path refine,implement,verify --repo api=feat/throttle
$J card list
$J server &                     # .jadecode/jadecode.sock (0600)
$J rpc cards.list
$J rpc card.advance '{"card":"<카드 폴더 이름 또는 ULID>}'
$J rpc events.tail '{"limit":3}'
$J check                        # 일지·색인 전수 대조
```

`$J tail --topics project/api --limit 20`, `$J card status <card> working`, `$J card next <card>`도 있다.

## 구조

```
crates/jc-core      일지(정본)·색인(SQLite, 재생성 가능)·task.md·워크스페이스 레이아웃
crates/jc-server    JSON-RPC 2.0 over WebSocket, 쓰기 스레드, 구독, task.md 감시
crates/jadecode-cli `jadecode` 바이너리
tools/crash-test.sh kill -9 반복 시험
```

워크스페이스 모양(파일이 정본이다):

```
<workspace>/
  jadecode.toml
  AGENTS.md
  events/global.jsonl        봇·정책·전역
  work/<project>/
    project.toml
    events.jsonl             프로젝트 일지(오케스트레이터 대화·카드 생성)
    <card>/
      task.md                앞머리 `jadecode:` 칸은 서버 소유, 본문은 누구나
      events.jsonl           그 카드의 이벤트·메시지(덧붙이기만)
  .jadecode/                 색인·소켓·복구 메모(정본 아님, 지워도 된다)
```

## 지키는 규칙

스파이크(S1~S5, S9)가 실측으로 정한 것 중 서버가 지는 부분이다. 근거는 [`docs/architecture/candidates.md`](docs/architecture/candidates.md), [`docs/architecture/spike-results.md`](docs/architecture/spike-results.md).

- **쓰기 순서**: 단일 쓰기 스레드가 seq 부여 → 일지 덧붙이기 → fsync → 색인 커밋 → 그 뒤 응답·알림. 응답받은 seq는 이미 fsync됐다.
- **색인은 투영**: 지우거나 깨져도 일지와 `task.md`에서 다시 만든다. 시작할 때 색인 꼬리부터 일지를 읽어 이어 붙인다.
- **복구**: 찢긴 마지막 줄은 자르고 `.jadecode/recovery/torn.log`에 남긴다. seq 빈틈 뒤 줄은 미승인으로 보고 `gap.log`로 옮긴다. 일지가 줄었으면(바깥 훼손) 고치지 않고 처음부터 다시 색인한다.
- **`task.md` 동시 편집**: 서버 쓰기는 비교 후 맞바꾸기(`renameat2(RENAME_EXCHANGE)`)다. 그냥 tmp+rename은 에이전트 편집을 잃었다(S5: 34/338 → 0). 바깥 편집이 앞머리 `jadecode:` 칸을 바꾸면 되돌리고 `card.state_change_rejected`를 남긴다. 본문 변경은 받아들이고 `card.file_changed`만 남긴다(서버가 꺼져 있던 변경은 `card.reconciled`).
- **구독**: 구독자별 버퍼가 없다. `from_seq` 뒤를 색인에서 읽어 보낸다. 주제는 경로이고 마디 단위 앞부분 일치다(`project/p1` ≠ `project/p10`). 느린 구독자는 TCP 역압으로 멈추고 서버 메모리는 자라지 않는다.
- **감시**: 카드·프로젝트 폴더만 비재귀로 본다(워크트리 안은 안 봄). 읽기 알림은 무시하고, 자기 쓰기는 내용 해시로 거른다.

## RPC

전송은 WebSocket(첫 버전은 유닉스 소켓 0600, `--port`로 루프백 TCP). 메서드는 `server.info`가 자기 서술한다.

| 메서드 | 하는 일 |
|---|---|
| `server.info` / `server.stats` | 이름·버전·메서드 목록 / 쓰기 묶음·seq·구독 알림 수 |
| `projects.list` | 프로젝트와 카드 수 |
| `cards.list` `{project?, stage?}` | 카드 투영 목록 |
| `card.get` `{card}` | `task.md` 원문 + 앞머리 + 색인 상태 |
| `card.set_status` `{card, status}` | 상태 칸(idle/working/blocked/waiting_human/done/failed) |
| `card.advance` `{card}` | 경로의 다음 단계로. 끝이면 `4000 path_end`로 거절 |
| `card.append_event` `{card, kind, body}` | 카드 일지에 메시지 추가(`card.`/`msg.`/`instance.` 접두사) |
| `events.append` `{topic, kind, body}` | 임의 주제에 이벤트 |
| `events.subscribe` `{topics, from_seq}` | `events.event` 알림 흐름 |
| `events.head` / `events.tail` `{topics?, limit?, before_seq?}` | 마지막 seq / 최근 이벤트 |

`card` 인자는 카드 폴더 이름, 상대 경로(`work/p/c`), ULID(앞부분) 중 아무거나 된다.
