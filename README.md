# jadecode

오케스트레이터 봇이 카드를 발행·이동하고, 봇 인스턴스가 카드에서 일하는 task 기반 칸반.

- 무엇을 하는가: [`SPEC.md`](SPEC.md)
- 무엇으로 만드는가: [`docs/architecture/`](docs/architecture/) (후보 비교, 스파이크 결과, 질문 목록)
- 지금 어디까지: [`docs/implementation.md`](docs/implementation.md)

서버 · 네 에이전트 백엔드(Claude Code·OpenCode·omp·pi) · MCP 메시지 도구 · CLI · **GPUI 데스크톱 클라이언트**까지 들어 있다.

## 설치

### macOS: 경고를 매번 겪지 않는 방법 (권장)

| 방법 | Gatekeeper 경고 | 비고 |
|---|---|---|
| **Homebrew** `brew install --cask jadewisemann/jadecode/jadecode` | **없음** | quarantine 이 안 붙는다. 이후 `brew upgrade` |
| `curl \| sh` 설치 | **없음** | curl 로 받으면 quarantine 이 안 붙는다 |
| `jadecode update` | **없음** | 한 번 설치한 뒤부터는 자동 업데이트 |
| 브라우저로 DMG 다운로드 | 있음 | 브라우저가 quarantine 을 붙인다 |

브라우저 DMG 는 macOS 가 "인터넷에서 받은 파일"로 표시하기 때문에 매번 걸린다.
**Homebrew 나 `curl` 로 설치하면 그 표시가 붙지 않아 매번 겪지 않는다.**
Apple Developer 인증서(연 $99)로 서명·공증하면 브라우저 설치도 경고가 없어진다.

### macOS (Apple Silicon) — DMG

1. [Releases](../../releases) 에서 `jadecode-*-macos-aarch64.dmg` 를 내려받아 엽니다.
2. **jadecode.app** 을 **Applications** 폴더로 끌어다 놓습니다.
3. Applications 에서 jadecode.app 을 엽니다.

**서명이 ad-hoc(자체 서명)이라 Apple Developer 인증서 공증이 없습니다.** 그래서 macOS가
"jadecode.app이 손상되었습니다" 또는 "확인되지 않은 개발자"라고 막을 수 있습니다.

한 번에 고치기(권장):

```sh
curl -fsSL https://raw.githubusercontent.com/jadewisemann/jadecode/main/tools/fix-macos.sh | sh
```

이 스크립트가 격리 표시를 지우고 ad-hoc 서명을 다시 붙입니다.

수동으로 하려면:

```sh
xattr -dr com.apple.quarantine /Applications/jadecode.app
codesign --force --deep --sign - /Applications/jadecode.app
open /Applications/jadecode.app
```

Finder에서 우클릭 → 열기도 됩니다.

앱을 열기 전에 **서버**가 필요합니다:

```sh
jadecode init ~/jc/work --name Work   # 처음 한 번
cd ~/jc/work && jadecode server &
```

### 한 줄 (전 플랫폼)

```sh
curl -fsSL https://raw.githubusercontent.com/jadewisemann/jadecode/main/install.sh | sh
```

하는 일: 최신 릴리즈 내려받기 → **체크섬 검증** → `~/.local/bin`에 바이너리 →
macOS면 `/Applications/jadecode.app` 설치 + quarantine 해제 → `~/jc/work` 워크스페이스 생성.

옵션(환경변수):

```sh
JADECODE_VERSION=v0.1.1-alpha   # 특정 태그
JADECODE_PREFIX=$HOME/bin       # 바이너리 위치
JADECODE_WS=$HOME/jc/proj       # 워크스페이스 위치
JADECODE_NO_APP=1               # macOS 앱 설치 건너뛰기
JADECODE_NO_WS=1                # 워크스페이스 생성 건너뛰기
curl -fsSL .../install.sh | JADECODE_VERSION=v0.1.1-alpha sh
```

### 직접 내려받기

```sh
tar -xzf jadecode-*-<플랫폼>.tar.gz
cd jadecode-*-<플랫폼>
./install.sh            # 아카이브 안의 설치 스크립트
```

지원: `macos-aarch64`(Apple Silicon), `linux-x86_64`. 체크섬(`.sha256`)이 함께 올라간다.
Intel Mac·ARM 리눅스는 아직 빌드가 없다(소스 빌드만).

### macOS: Gatekeeper

이 빌드는 **코드서명·공증이 없다**(Apple Developer 인증서 필요). 설치 스크립트가 격리 표시를
지우지만, 직접 내려받았다면 다음을 한 번 실행한다:

```sh
xattr -dr com.apple.quarantine /Applications/jadecode.app
```

또는 Finder에서 우클릭 → 열기, 또는 시스템 설정 → 개인정보 보호 및 보안 → "그래도 열기".

## 처음 실행

```sh
export PATH="$HOME/.local/bin:$PATH"
cd ~/jc/work
jadecode server &                  # 서버(유닉스 소켓)
open /Applications/jadecode.app    # macOS 앱 (또는 jadecode-client)
```

클라이언트는 **서버가 떠 있어야** 한다. 에이전트는 각자 설치·로그인이 필요하다:

```sh
jadecode agents     # claude·opencode·omp·pi 설치·버전 확인
```

## 빠르게

```sh
cargo test                                  # 69개
cargo build

J=target/debug/jadecode
export JADECODE_MCP=$PWD/target/debug/jadecode-mcp   # 서버가 인스턴스에 붙일 MCP

$J init ~/jc/acme --name Acme        # 봇 4개(orchestrator/worker/reviewer/researcher)와 policy/가 생긴다
cd ~/jc/acme
$J server &                          # .jadecode/jadecode.sock (0600). 쓰기 프로세스는 하나만(flock)
                                     # 경로가 길면 자동으로 /tmp/jc-<이름>.sock 으로 물러난다(macOS 104자 한도)
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

cargo build -p jc-client --features ui    # 클라이언트는 GPUI 기능을 켜서 빌드
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
crates/jc-bench     성능 측정(일지·색인·구독·클라이언트 데이터 계층)
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

## 성능

`docs/performance.md`에 측정값과 병목을 적었다. 요약(카드 1,000·이벤트 10만, 이 컨테이너):

- 색인 재생성 1.0초, 전수 검사 0.5초, 재시작 RSS 89MB
- 덧붙이기 233/s(기본 fsync) → **507/s**(`--fsync batch-group`)
- 클라이언트 스냅숏 복제 869us → **0.025us**(`SnapshotHandle`)

```sh
cargo build --release -p jc-bench
jc-bench all /tmp/bench --projects 12 --cards 1000 --events 100000
```

**GPU 렌더 병목은 이 환경에서 못 잰다**(virtio GPU + llvmpipe, GPUI는 Vulkan 필요). Mac에서 재는 절차는
`docs/performance.md` 끝에 있다.

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
