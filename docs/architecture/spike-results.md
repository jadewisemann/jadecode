# 스파이크 결과

> 상태: **1차 스파이크 결과 통합**. 2026-11-09 작성. 스파이크 설계는 `spikes.md`, 반영한 곳은 `candidates.md`·`questions.md`다.
> 원본: 각 스파이크 work 폴더의 `result.md`(아래 경로). 실행 기록·시제품 코드는 그 폴더에만 있고 이 레포에는 없다.
> 공통 환경: Linux 6.17(aarch64) 서버 한 대, Claude Code CLI `2.1.319`, 사용자 구독 로그인(Max). API 키·Agent SDK는 쓰지 않았다. Mac에서는 아무것도 돌리지 않았다.

## 한눈에

| id | 제목 | 판정 | 후보에 주는 영향 | 수정 제안 | 원본 |
|---|---|---|---|---|---|
| S1 | Claude Code 구조화 실행 기본기 | 통과(H1~H5) | A2 유지 | 6개(+부가 1) | `work/jadecode/1108-architecture/1108-spike-s1-claude-driver/result.md` |
| S2 | 권한 정책 무인 실행 | 통과(H1~H4, H2는 조건 둘) | A2 유지, 보강 | 6개 | `work/jadecode/1108-architecture/1108-spike-s2-permission/result.md` |
| S3 | 메시지 도구와 오케스트레이터 깨우기 | 통과(H1~H3, H4는 정의에 따라 통과/부분) | A2·Q5·Q7 유지 | 4개 | `work/jadecode/1108-architecture/1108-spike-s3-messaging/result.md` |
| S4 | 사람 붙기 왕복(구조화 ↔ TUI) | 통과(H1·H2·H4, H3는 잠금 규칙 확정) | A2 + 터미널 넘기기 유지 | 4개 | `work/jadecode/1108-architecture/1108-spike-s4-attach/result.md` |
| S5 | 이벤트 일지·구독·재구성 | 통과(H1~H4, Linux) | B2 유지 | 쓰기 규칙 6개 | `work/jadecode/1108-architecture/1108-spike-s5-event-log/result.md` |
| S9 | 평가 재실행(리플레이) | H1 부분, H2 통과, **H3 실패**(판별력 없음), H4 측정 | **C1 수정** | 5개 | `work/jadecode/1108-architecture/1108-spike-s9-replay/result.md` |
| S6 | GPUI Mac 화면 시제품 | 안 돌림 | - | 구현 중 확인 | - |
| S7 | nsh 화면 시제품 | 안 돌림 | - | Android 단계 재평가 | - |

- S6·S7은 돌리지 않았다. 2026-11-09 사용자 결정에 따라 Mac이 필요한 것(S6·S7, S5의 Mac 수치)은 따로 기다리지 않고 구현하면서 확인한다. 클라이언트는 Mac GPUI로 시작한다(라운드 A2).
- 사용량(공시 단가 환산, 구독이라 실제 청구 아님): S1 약 \$0.3, S2 약 \$0.11, S3 약 \$1.5, S4 약 \$0.07(+TUI 쪽 캐시), S5 0(CLI 안 씀), S9 약 \$1.79. 합계 약 \$3.8. 다섯 개가 같은 계정에서 동시에 돌아 계정 5시간 이용률이 0.35에서 0.5로 올랐다(S1 관측).

## 여러 스파이크에 걸친 발견

설계 전반에 걸리므로 먼저 적는다. 각 스파이크 절에도 다시 나온다.

1. **인스턴스 격리가 설계 항목이다.** 아무 플래그 없이 띄우면 사용자 전역 지침(`~/.claude/CLAUDE.md`), 상위 폴더 `AGENTS.md`, 사용자 훅·플러그인, claude.ai 커넥터 MCP 9개가 인스턴스에 들어간다. 첫 턴 캐시 생성이 192,789토큰(격리 시 2,995토큰)으로 비용 차이도 크다(S1).
   - `--setting-sources "" --strict-mcp-config`면 위 다섯 가지가 모두 빠진다(S1 H4).
   - `--setting-sources project,local`만으로는 `~/.claude/CLAUDE.md`가 그대로 들어간다(S3·S4·S9). S9는 `claudeMdExcludes`로 뺐다.
   - 격리 뒤에도 남는 것: 내장 스킬 19개(실행은 deny 규칙으로 막음), Claude Code 기본 시스템 프롬프트와 계정 맥락, 사용자 셸 스냅숏(Bash 도구가 사용자 zsh 설정을 source함), 기본 커밋 attribution 문구(사용자 설정이 빠지면 기본값으로 돌아감 → 정책에 `attribution`을 명시해야 함)(S1).
   - 작업 폴더 밖 읽기를 막지 않으면 카드끼리 정보가 섞인다. haiku 워커가 형제 실행의 `config.toml`을 읽었다(S3).
   - `--bare`는 구독 OAuth를 읽지 않아 쓸 수 없다(S1·S3).
2. **자식 프로세스 환경은 화이트리스트로 넘긴다.** 부모가 Claude Code 세션이면 `CLAUDECODE`·`CLAUDE_CODE_SESSION_ID`·`CLAUDE_CODE_MESSAGING_SOCKET` 등을 물려받아, 자식 TUI가 부모 세션 이름을 달고 떴다(S1·S4).
3. **세션 id는 안정적이다.** `--session-id`로 정한 id가 resume·SIGKILL 뒤 resume·구조화 ↔ TUI 왕복 2회를 거쳐도 바뀌지 않았다. 새 id나 사본은 한 번도 생기지 않았다(S1 H3, S4 H1·H2). R4(오늘 겪은 "사본이 생겨 id가 바뀜")는 `--fork-session`·`--bg` 같은 옵션 조합에서 생긴 것으로 보고, 드라이버는 그 경로를 쓰지 않는다.
4. **조용한 멈춤은 0건이었다.** 권한 경로(S2), 메시지 경로(S3), 넘기기(S4) 모두에서 멈춘 채 아무도 모르는 경우가 없었다. 단 권한 서버 장애는 "거부"가 아니라 "도구 오류"나 "result 없는 종료"로 나온다(S2).
5. **CLI 동작은 판마다 바뀔 수 있다.** 쓰고 있는 것 중 `--append-system-prompt-file`·`--system-prompt-file`은 `--help`에 없는 숨은 플래그이고, `--permission-prompt-tool stdio`·`control_request`는 문서화 수준을 확인하지 못했다. 드라이버 계약 테스트의 씨앗은 S1 `harness/`·S2 `proto/cases.sh`다.
6. **사용량은 구독에서도 턴별로 나온다.** 단, 금액은 공시 단가 환산(`costBasis: list`)이고, 중단된 턴의 사용량은 어디에도 잡히지 않는다(S1 H5, S9 H4).

---

## S1. Claude Code 구조화 실행 기본기 — 통과

원본: `/home/ubuntu/code/work/jadecode/1108-architecture/1108-spike-s1-claude-driver/result.md`

**판정**

| 가설 | 판정 | 근거 요약 |
|---|---|---|
| H1 한 프로세스에 여러 턴 | 통과 | 3턴 같은 맥락. 15분 유휴 뒤에도 같은 프로세스가 다음 턴을 받음. 한가한 프로세스 RSS 약 212MB(+MCP 스텁 10MB). 턴마다 `system/init`이 다시 나옴 |
| H2 턴 중 메시지 | 통과(동작 확인) | 도구를 쓰는 턴이면 다음 도구 결과 경계에서 **같은 턴 안에 끼어들고**, 글만 쓰는 턴이면 **다음 턴으로 줄을 선다**. 버려지지 않음. 중단은 `control_request{subtype: interrupt}`로 현재 턴만 끊고, 줄 선 메시지는 살아남음 |
| H3 `--session-id`·`--resume` | 통과 | 도구 실행 중 SIGKILL 뒤 `--resume`도 같은 id·같은 기록. 이미 있는 id로 `--session-id`는 "already in use", 없는 id로 `--resume`은 "No conversation found" |
| H4 봇 정의 플래그 | 통과 | `--append-system-prompt-file`, `--plugin-dir`, `--settings`, `--mcp-config --strict-mcp-config`, `--model`, `--add-dir` 모두 동작. 전역 설정 격리 방법 있음(위 공통 발견 1) |
| H5 턴별 사용량 | 통과 | `result.usage`(이번 턴), `model_usage_since_last_result`(이번 턴, 모델별, 환산 금액), `total_cost_usd`(세션 누적, kill 뒤엔 끊김), `rate_limit_event`(계정 전체 5시간·7일 이용률, 가끔) |

**수정 제안 (A2 유지)**

1. 턴 경계와 메시지 큐는 서버가 쥔다. "메시지 하나 = result 하나"를 가정하지 않는다. 기본은 `result`(한가함)를 본 뒤 넣고, 급한 것만 즉시 넣는다(끼어들기).
2. `cancel()` = `control_request{subtype: interrupt}`. 이미 넣은 메시지는 취소되지 않으므로 서버 큐에서 아직 안 넣은 것만 취소할 수 있게 한다.
3. 재시작 규칙: 처음은 `--session-id <서버가 만든 uuid>`, 이후 모든 재시작은 **같은 카드 폴더에서** `--resume <같은 uuid>`. "already in use"와 "No conversation found"를 구별해 처리한다.
4. 봇 버전과 resume: 기본 `--system-prompt-snapshot on`이면 resume해도 시작 때의 지침이 그대로 쓰인다. 인스턴스를 시작 때 봇 커밋으로 고정하는 Q15와 맞으므로 기본값을 쓰고, 같은 인스턴스에 새 봇 버전을 적용할 때만 `off`로 resume한다.
5. 격리 표준 플래그: `--setting-sources "" --strict-mcp-config --settings <정책> --plugin-dir <봇> --append-system-prompt-file <봇 지침> --mcp-config <jadecode>`. 정책 파일에 내장 스킬 deny 목록과 `attribution`을 넣는다. 자식 환경변수는 화이트리스트.
6. 예산: 턴별 `model_usage_since_last_result`의 토큰을 서버가 직접 더한다(누적 필드는 resume·kill에 따라 끊겨서 쓰지 않음). 금액은 환산값으로 보인다. 중단된 턴이 빠지므로 실행 시간 상한을 보조 한도로 둔다. 계정 한도는 `rate_limit_event`로 따로 보인다(계정 전체라 봇별 배분에는 못 씀).
7. (부가) 종료는 SIGTERM → 몇 초 → SIGKILL. SIGKILL·프로세스 그룹 kill로는 도구 자식(자기 세션으로 뜸)이 고아로 남으므로, 비정상 종료 뒤 cgroup(Linux `systemd-run --user --scope` 등)이나 자손 추적으로 치운다.

**남은 의문 → 처리**: 몇 시간 유휴·잠자기 뒤 같은 프로세스 생존(실패하면 `--resume`으로 메움, 구현 중 확인), `still_queued` 필드 뜻(미확인, 쓰지 않음), 숨은 플래그가 빠질 가능성(계약 테스트, 대안 `--append-system-prompt`), CLI 판 고정 방법(구현 중 확인).

## S2. 권한 정책 무인 실행 — 통과(조건 둘)

원본: `/home/ubuntu/code/work/jadecode/1108-architecture/1108-spike-s2-permission/result.md`

**판정**

| 가설 | 판정 | 근거 요약 |
|---|---|---|
| H1 정한 범위 안은 승인 없이 | 통과 | allow 규칙·`acceptEdits`·`--add-dir` 안쪽은 RPC 0건. allow 목록에 없어도 읽기 전용으로 판정되는 Bash(`python3 --version`)는 묻지 않고 실행됨 |
| H2 규칙 밖은 권한 RPC, 답이 그대로 반영 | 통과(조건 둘) | allow → 실행, deny → `message`가 그대로 도구 결과. 하위 에이전트 요청도 같은 경로. 조건 1: 권한 서버가 시간 초과·죽음이면 "도구 오류"로 끝나고 `permission_denials`에 안 남음. 조건 2: 권한 서버가 아예 안 뜨면 첫 권한 요청에서 `result` 없이 rc=1 |
| H3 오늘 멈췄던 (a)~(e) | 통과 | 허용 밖 Bash·복합 명령·명령 치환, 카드 밖 쓰기·리다이렉트, 다른 git 루트 `cd`·`git -C`, WebFetch·WebSearch, AskUserQuestion 모두 RPC로 옴. settings `deny`는 RPC 없이 즉시 거부. `-p`에서는 "permission-root relocation" 창이 따로 뜨지 않음(그냥 규칙 밖 Bash, 셸 cwd는 카드 폴더로 돌아옴) |
| H4 무응답 | 통과 | CLI는 MCP 도구 무응답 **1800초**에 스스로 끊음(도구 오류). 서버 쪽 상한(시간 지나면 deny), stream-json `interrupt`, `MCP_TOOL_TIMEOUT` 셋 다 동작 |

- AskUserQuestion은 권한 RPC가 곧 질문 RPC다. 서버가 `updatedInput.answers`를 채워 allow하면 에이전트가 답을 받는다.
- 권한 경로는 둘 다 동작: `--permission-prompt-tool mcp__jadecode__permission`(MCP) 또는 `--permission-prompt-tool stdio`(stream-json `control_request{subtype: can_use_tool}`).

**수정 제안 (A2 보강)**

1. 막힘·거부의 정본은 서버의 권한 RPC 기록이다. RPC를 받는 순간 "막힘"을 일지에 쓰고, 답할 때 `resolved(allow|deny|timeout)`를 쓴다. `result.permission_denials`·`system/permission_denied`에 기대지 않는다.
2. 상한은 서버가 RPC 처리기에서 정하고, **처리기 상한 < CLI 쪽 상한(1800초)** 을 계약 테스트로 지킨다. Q19의 "30분 뒤 거부"는 25분으로 줄인다.
3. 시작 때 init의 `mcp_servers[]`에서 jadecode 서버가 `connected`가 아니면 그 인스턴스를 바로 `failed`로 두고 다시 띄운다. `result` 없이 끝나는 프로세스를 드라이버가 처리한다.
4. 권한 경로 후보 둘 중 첫 버전은 MCP(`mcp__jadecode__permission`). 인스턴스마다 어차피 MCP 서버를 붙이므로 추가 작업이 적다. stdio 경로는 순서가 확실하고 MCP 서버가 따로 죽을 일이 없어 예비로 둔다.
5. 질문도 같은 경로로 받는다. AskUserQuestion을 권한 RPC에서 가로채 jadecode `question` 메시지로 바꾸고, 답이 오면 `updatedInput.answers`로 돌려준다.
6. 정책 작성 규칙: 막을 명령은 allow 목록에서 빼는 것으로 부족하다(읽기 전용 판정 명령은 그냥 실행됨). 사람 절차 명령·네트워크 명령은 settings `deny`로 둔다. allow 규칙은 복합 명령·명령 치환·cwd 밖 리다이렉트를 통과시키지 않는다.

**남은 의문 → 처리**: MCP progress로 1800초를 늘릴 수 있는지(30분 넘게 사람을 기다리게 할 때만 필요, 미확인), `stdio` 경로의 공개 계약 여부(예비라서 보류), 하위 에이전트 구분(RPC 인자에 없음, `task_*` 이벤트와 맞춰야 함), TUI 동안의 권한 창(S4와 대조 → 아래 S4).

## S3. 메시지 도구와 오케스트레이터 깨우기 — 통과

원본: `/home/ubuntu/code/work/jadecode/1108-architecture/1108-spike-s3-messaging/result.md`

**판정**

| 가설 | 판정 | 근거 요약 |
|---|---|---|
| H1 워커가 도구를 알맞은 때에 부름 | 통과 | 완료 카드 26/26이 `report(done)`로 끝남(누락 0%). `ask` 5/5, `error` 4/4, `propose` sonnet 4/4. 메시지 도구 없이 끝난 턴 7/50(14%)은 모두 "기다리겠다" 턴. 자세한 지침과 최소 지침 차이 없음 |
| H2 다이제스트 → ack + 알맞은 도구 | 통과 | 다이제스트 58개 전부 정확한 seq로 ack. 시나리오 셋 모두 올바른 행동, 오케스트레이터가 사람 절차를 스스로 승인한 경우 0 |
| H3 일하는 중 이벤트는 쌓였다가 한 번에, 손실 없음 | 통과 | 깨우는 이벤트 130건, 빈틈 0·중복 0. 잡음 13건은 개수로만 붙음. 대기 중앙값 4.3초, 최대 15.4초 |
| H4 한가할 때 2초 안 | 통과(CLI 접수) / 부분(모델 첫 응답) | 서버·CLI 지연 0.3초 남짓(0.3초는 모으기 대기). 모델 첫 응답 중앙값 2.06초(16개 중 8개가 2초 넘음) — 서버 설계로 줄일 수 없음 |

- 추가 관찰: 오케스트레이터가 ack 전에 죽어도, 마지막 ack 뒤 이벤트를 새 인스턴스에 다시 넣고 "이전 인스턴스가 ack 이후 한 행동" 목록을 붙이면 중복 행동이 없었다(1회 시험). 같은 다이제스트 안의 사건끼리 엮어 판단하는 이득이 있었다.

**수정 제안 (A2·Q5·Q7 유지)**

1. 턴 종료 자동 기록(`report(auto)`)은 **깨우지 않는** 이벤트로 남긴다. 깨우는 것은 "워커가 `report(done)` 없이 한가함으로 넘어감·종료됨"일 때만. 직전에 오케스트레이터가 "기다려라"를 보낸 인스턴스의 무도구 턴은 깨우지 않는다(헛 깨우기 7건 → 0).
2. 다이제스트 재전달 규칙: 오케스트레이터 인스턴스가 바뀌면 마지막 ack 뒤의 이벤트를 다시 넣고, 앞에 "마지막 ack 이후 이 자리의 오케스트레이터가 한 행동" 목록을 붙인다(at-least-once + 중복 행동 방지).
3. 오케스트레이터 → 인스턴스 메시지는 모두 `reply_to`(선택)를 받는다. 사람 결정 전달은 원래 제안 seq를 잇는다(`send`가 실마리를 잃은 문제).
4. 인스턴스 맥락 격리를 설계 항목으로 둔다: 사용자 메모리를 인스턴스에 들일지, 작업 폴더 밖 읽기를 막을지(위 공통 발견 1).

**남은 의문 → 처리**: 오케스트레이터 모델 선택(opus 계열 미측정)과 몇 시간·수백 다이제스트 뒤 자동 압축이 ack 규칙을 지키는지(구현 중 확인), 모으기 대기 0.3초의 적정값(설정값), 도구 호출이 기록되기 전 죽는 경우의 중복(서버가 모름, 받아들임), `ask`를 막는 도구로 만들지(아래 설계: 막지 않음), 사람 직접 답 경로(S4 `entrypoint`로 구분 가능).

## S4. 사람 붙기 왕복 — 통과

원본: `/home/ubuntu/code/work/jadecode/1108-architecture/1108-spike-s4-attach/result.md`

**판정**

| 가설 | 판정 | 근거 요약 |
|---|---|---|
| H1 구조화 → TUI `--resume` | 통과 | 앞 턴·도구 호출이 화면에 그대로, `/status`의 Session ID 같음. 턴 도중 SIGKILL한 세션도 이어짐 |
| H2 TUI → 구조화 `--resume` | 통과 | 사람 턴까지 이어지고 id 그대로. 구조화 → TUI → 구조화 → TUI → TUI → 구조화를 거쳐도 파일 하나·id 하나·가지 0 |
| H3 동시 열기 | 확인 + 잠금 규칙 확정 | claude는 막지 않는다. 두 프로세스가 같은 전사에 번갈아 덧붙이며 대화가 **조용히 갈라지고**, 다시 열면 마지막 가지만 이어진다(다른 가지는 오류 없이 사라짐). claude 자체 잠금 없음. 실행 중 프로세스는 `~/.claude/sessions/<pid>.json`에 등록됨(비공식) |
| H4 TUI 동안 상태 보고 | 통과 | `--settings`의 http 훅으로 `UserPromptSubmit`·`PermissionRequest`(창이 뜨자마자, 약 2초)·`Notification`(permission_prompt 약 6초 뒤, idle_prompt 60초)·`Stop`(`last_assistant_message` 포함)·`SessionEnd`(끝난 방식 구별). **`SessionStart`만 http 훅으로 안 옴** → command 훅으로 받음. interrupt로 멈춘 턴은 `Stop`이 안 옴 |

- 넘기기 비용: 구조화 프로세스를 interrupt → result → stdin 닫기로 끝내는 데 약 1.1초. TUI로 넘어갈 때 프롬프트 캐시를 새로 만들어 넘기기 한 번에 약 1만~3만 토큰.
- 전사의 `entrypoint`가 TUI 턴은 `cli`, 구조화 턴은 `sdk-cli`라서 사람 턴을 구별해 `human_direct`로 옮길 수 있다.

**확정한 잠금 규칙**

- R-a 인스턴스 임대: jadecode 인스턴스 하나당 claude 프로세스는 최대 하나. 임대 상태 `structured | handoff | terminal | idle`를 서버가 들고, 다른 프로세스를 띄우기 전에 앞 프로세스의 종료(wait)를 확인한다.
- R-b 넘기기 순서: 구조화 → TUI는 interrupt(턴 중이면) → result → stdin 닫기 → 종료 확인 → PTY에서 `claude --resume <id>`. TUI → 구조화는 PTY 프로세스 종료(`SessionEnd reason=prompt_input_exit`) 확인 → `claude -p --resume <id>`. 5초 안에 안 끝나면 SIGTERM, 그다음 SIGKILL.
- R-c 외부 프로세스 감지(권고): 띄우기 직전 `~/.claude/sessions/*.json`을 읽어(쓰지 않음) 같은 `sessionId`이고 살아 있는 pid가 있으면 띄우지 않고 "다른 곳에서 열림" 이벤트. 비공식 형식이라 바뀌면 감지만 꺼진다.
- R-d 사후 감지: 세대가 바뀔 때마다 전사의 parentUuid 가지 수를 세어 0이 아니면 "갈라짐" 이벤트, 잃은 가지의 턴을 카드 이력에 보존.

**수정 제안 (A2 + 터미널 넘기기 유지)**

1. 인스턴스 상태 기계에 `handoff`·`terminal` 임대 상태와 R-a~R-d를 넣는다. 그 결과 **사람이 터미널로 붙어 있는 동안 그 인스턴스의 구조화 실행은 멈춘다**(라운드 B2).
2. 터미널 보기의 첫 화면이 폴더 신뢰 창일 수 있다. git 레포 cwd에서 TUI를 띄우면 "trust this folder?" 창이 뜬다(신뢰 확인이 git 루트에서 멈춤). 추천은 사람이 그 창에서 한 번 수락(워크트리 경로마다 한 번). 서버가 `~/.claude.json`에 써 넣거나 내부 변수로 건너뛰는 것은 하지 않는다.
3. 자식 환경 화이트리스트(공통 발견 2), 넘기기 때 `--permission-mode`를 카드 정책 값으로 명시(주지 않으면 TUI가 `auto`로 떴다).
4. 상태 훅은 http로 두되 `SessionStart`는 command 훅이나 서버 자체 기록(PTY 시작)으로 대신한다. interrupt는 서버가 따로 기록한다.

**남은 의문 → 처리**: git 워크트리가 원본 레포의 신뢰를 물려받는지(원본 레포를 신뢰시켜야 시험 가능 → 라운드 B4), TUI 안 `Esc` 중단 때 훅 흐름(구현 중 확인), 두 구조화 프로세스 동시 열기(TUI+구조화와 같다고 추정, R-a로 막음), GUI 렌더·한국어 IME(S6 몫 → 구현 중 확인).

## S5. 이벤트 일지·구독·재구성 — 통과(Linux)

원본: `/home/ubuntu/code/work/jadecode/1108-architecture/1108-spike-s5-event-log/result.md`
시제품: Rust 약 2,100줄(tokio, tokio-tungstenite, rusqlite, notify).

**판정**

| 가설 | 판정 | 근거 요약 |
|---|---|---|
| H1 `kill -9`에도 복구 가능 | 통과 | 무작위 `kill -9` 30회 + 죽는 지점 주입 7곳 × 5회 = 65회 전부 대조 통과. 일지 171,605줄 = 색인 = `last_seq`, 승인 168,809건 손실 0. 복구 최대 99ms. 손으로 만든 어긋남 7가지 중 6가지 복구, 바깥에서 일지를 잘라낸 1가지는 숨지 않고 드러남 |
| H2 재구성 5초 안(카드 1,000·이벤트 10만) | 통과(Linux) | 따뜻한 캐시 0.46초, 차가운 캐시 약 2.0초. 100만 건 3.6초(빈 색인 채울 때 WAL 끔). WAL 켠 채로 100만 건은 15초 |
| H3 앞부분 일치 구독, 느린 구독자 | 통과 | 구독자 6종(빠름·느림·20~40초 멈춤·재접속·마디 경계·카드 하나) × 부하 3가지에서 손실·중복·순서 어긋남·끊김 0. 멈춘 구독자가 있어도 서버 RSS 약 36MB 일정. 한가할 때 발행 → 수신 p50 3ms |
| H4 `task.md` 감시 | 통과(Linux inotify) | 편집 방식 5가지(덮어쓰기·rename·vim 식·덧붙이기·`sed -i`) × 20회 누락 0, 서버 자기 쓰기 오탐 0. 서버 상태 칸 쓰기와 에이전트 편집이 3초 동안 경합해도 에이전트 줄 손실 0 |

- 쓰기 처리량: 기본(묶음 fsync) 초당 1,200~1,600건, 병렬 fsync 4,400건. 에이전트 메시지·상태 이벤트(초당 수십 건)에는 충분하지만 토큰 단위 출력은 일지에 넣지 않는다.

**수정 제안 — B2 쓰기 규칙 6개 (B2 유지)**

1. 쓰기 순서와 승인: seq 부여 → 일지 `write` → fsync → 색인 커밋 → 그 뒤 알림·RPC 승인. 승인은 "일지에 fsync됐다"는 뜻이다.
2. 복구: 색인의 `journals(path, off)`부터 일지 꼬리를 읽어 넣는다. 찢긴 마지막 줄은 자르고, seq 빈틈 뒤 줄은 미승인으로 보고 `.jadecode/recovery/`로 옮긴다. 색인이 깨졌으면 묻지 않고 다시 만든다. 일지 자체가 줄어든 것(바깥 훼손)은 고치지 않고 드러낸다.
3. 빈 색인을 채울 때는 WAL을 끄고(`journal_mode=OFF`·`synchronous=OFF`) 넣고 끝나면 WAL로 돌린다.
4. 서버의 `task.md` 쓰기는 "다시 읽어 비교 + 맞바꾸기"(Linux `renameat2(RENAME_EXCHANGE)`, macOS `renamex_np(RENAME_SWAP)`). 그냥 임시 파일 + rename은 동시 편집에서 에이전트 편집을 잃는다(34/338).
5. 파일 감시: 카드 폴더를 비재귀로 보고(워크트리 안은 안 봄), 읽기 계열 알림은 무시하고(되먹임 고리 방지), 알림은 "다시 확인" 신호로만 쓴다. 자기 쓰기 구별은 시간 창이 아니라 서버가 마지막으로 쓴 내용 해시로 한다. 해시 비교와 서버 쓰기는 같은 쓰기 스레드에서 차례로. 작업 폴더 경로는 `canonicalize`한다(상대 경로면 전부 놓침).
6. 서버가 꺼진 동안의 `task.md` 변경(또는 rename 뒤 이벤트 전에 죽은 경우)은 시작할 때 파일을 정본으로 받아들이고 `card.reconciled`로 남긴다. 살아 있을 때 바깥에서 상태 칸을 바꾼 것만 검사·거절 대상이다.

**남은 의문 → 처리**

- Mac(재구성 시간, FSEvents 지연·합침·rename 모양, `renamex_np`, APFS `fsync` vs `F_FULLFSYNC`): 구현 중 확인.
- 실제 전원 손실: 시험 못 함(빈틈 경우로 흉내). 받아들임.
- 제자리(`O_TRUNC`)로 쓰는 에이전트가 맞바꾸기 뒤에 쓰면 밀려난 inode로 간다: Claude Code Write·Edit 도구의 쓰기 방식 확인 필요 → 구현 중 확인.
- 몰아치기 편집은 마지막 상태 하나로 합쳐짐: `task.md` 중간 이력은 남기지 않는다(설계자 결정, `questions.md` Q12).
- `task.md` 삭제·카드 폴더 이름 바꾸기 정책, 인스턴스 이벤트를 어느 일지에 둘지, 오프라인 상태 칸 변경에 종료 조건 검사를 할지: 설계자 결정(`questions.md` Q12·Q22).
- 색인이 줄 전문을 복사(크기 ≈ 일지), 재구성이 전부 메모리에 올림(100만 건 756MB): 첫 버전은 그대로, 커지면 흘려 넣기.

## S9. 평가 재실행 — C1 수정

원본: `/home/ubuntu/code/work/jadecode/1108-architecture/1108-spike-s9-replay/result.md`
설계: 합성 카드 2개(A-dur, B-tally) × 봇 버전 2개(「끝내기 전」 한 문단만 다름) × 2회 = 8회, 리뷰 봇 판정 2회씩. 모델 sonnet.

**판정**

| 가설 | 판정 | 근거 요약 |
|---|---|---|
| H1 시작 상태 되살림 | 부분 | 이 워크스페이스 기록으로는 **안 된다**: `arxiv/` 비어 있음, 워크스페이스 루트가 git이 아님, `new-work`가 기준을 브랜치 이름으로만 적음, 끝난 `1106-repo-bootstrap`은 시작 때 커밋이 없었음, `grilling.md` 같은 공유 입력이 이후에 바뀜. 대신 남길 기록 목록(아래)을 냈고, 그대로 남긴 스냅샷으로는 8/8 되살림 |
| H2 자동 채점 | 통과 | 수용 테스트 + `make check` + 규칙(변경 기록·관례 커밋·깨끗한 트리) + 리뷰 봇 점수로 8회 모두 사람 손 없이 채점. 리뷰 봇은 같은 결과물 두 번 판정에 평균 0.12점 차, 판정 불일치 0/8. `--json-schema`로 판정 JSON을 바로 받음 |
| H3 판별력 | **실패** | 카드 A는 버전 간 차이 0(천장), 카드 B는 같은 버전 안 차이 17.5 > 버전 간 8.75. 흔들림은 채점이 아니라 워커의 이진 행동(커밋을 했느냐) 때문. 결과 점수로 가르려면 버전당 약 17회(n=2 추정이라 매우 거침). 반면 **바꾼 문단이 겨냥한 행동을 세는 과정 지표("더한 테스트 수")는 두 카드 모두에서 v2 전부 > v1 전부** |
| H4 1회 시간·사용량 | 측정 | 워커 20~44초, 4~10턴, 출력 2.4k~6.8k 토큰, 환산 \$0.07~0.15. 리뷰 2회 약 \$0.085. 채점 포함 1회 약 \$0.20·약 1분. **장난감 카드 기준**이라 실제 카드는 수십 배 |

**카드 시작 때 남길 기록(시작 스냅샷)**

1. 레포마다 경로·원격 URL, **시작 커밋 해시**, 브랜치 이름, 카드 안 워크트리 경로. 시작 때 미커밋·미추적 변경이 있으면 그 패치·사본.
2. 시작 `task.md` 사본 + sha256.
3. 카드가 읽는 공유 입력 문서(`SPEC.md`, `grilling.md` 등)의 해시·사본.
4. 첫 사용자 턴(오케스트레이터 브리프) 원문.
5. 단계와 종료 조건(명령·cwd·기대 종료 코드, 판정 봇 이름과 봇 커밋).
6. 실행 봇의 커밋 해시, 모델·effort·CLI 판, 권한 정책(`policy/` 커밋), MCP 설정.
7. 선행 카드와 그 시점(라운드 A4 표기).
8. (끝날 때) 레포별 끝 커밋 해시. 원래 카드가 끝에 가진 테스트를 재실행의 수용 테스트로 쓴다.

**수정 제안 — C1 (장치는 유지, 평가 방식 변경)**

1. 시작 스냅샷 이벤트를 첫 버전부터 남긴다. 지금 워크스페이스 기록은 되살릴 수 없으므로, 남기기 시작한 뒤에 끝난 카드만 평가 세트 후보가 된다.
2. 평가의 주 신호를 "결과 점수 재실행 비교"에서 **"변경이 겨냥한 행동의 지표"** 로 옮긴다. 개선 카드는 변경안과 함께 무엇이 달라져야 하는지(테스트 추가 수, 커밋 누락률, 리뷰 루프 횟수 등)를 적고 그 지표로 견준다. 결과 점수는 "나빠지지 않았는가"의 회귀 검사로만 쓴다.
3. 재실행은 작고 싼 카드로, 회귀 검사용으로 둔다. 평가 세트는 바꾼 행동을 카드가 이미 요구하지 않는 것으로 고른다(천장 효과).
4. 대안을 함께 둔다: 반영 뒤 같은 봇의 실제 카드 K개 기록 지표를 이전 버전과 견주고, 나빠지면 서버가 자동으로 되돌린다(사람 승인 없음).
5. 리뷰 봇 판정은 재현성이 높아 단계 종료 조건과 평가 채점에 같이 쓴다. 정수 1~10 눈금이라 미세한 차이는 못 본다.

**실행 계층에 대한 덧붙은 사실**: `--add-dir`는 가변 인자라 맨 뒤에 두면 뒤의 프롬프트를 삼킨다(다른 플래그 앞에 둔다). 봇이 `cd repo; git …`나 셸 변수로 `git -C $R`을 쓰면 allow 규칙에 안 맞아 거부된다 → 봇 cwd를 레포로 두거나 정책·지침에서 형식을 맞춘다. heredoc 쓰기도 거부돼 Write 도구로 돌아갔다.

**남은 의문 → 처리**: 실제 크기 카드의 비용·흔들림(시작 스냅샷을 남긴 첫 카드들로 다시 잼, 구현 중 확인), 카드 수 vs 반복 수의 효율(미측정), **과정 지표를 누가 고르는가**(개선 봇이 고르면 자기에게 유리한 지표를 고를 위험 → 라운드 B1), 구독 한도가 병렬 재실행을 막는지(라운드 B6), 문서 카드 채점(리뷰 봇 판정만 남음).

---

## 구현 중 확인으로 넘긴 것

아래는 스파이크로 닫지 못했고 따로 기다리지 않는다. 첫 버전 구현 카드의 완료 조건이나 계약 테스트에 넣는다.

| 항목 | 출처 | 어디서 확인 |
|---|---|---|
| GPUI Mac 화면 다섯 가지, 터미널 보기, 한국어 IME | S6(안 돌림) | Mac 클라이언트 첫 카드 |
| nsh 4플랫폼 가능성 | S7(안 돌림) | Android 단계 직전 재평가(라운드 A2) |
| Mac 재구성 시간, FSEvents 지연·합침·rename 모양, `renamex_np(RENAME_SWAP)`, APFS `fsync`·`F_FULLFSYNC` | S5 | Mac 서버 일지·감시 카드 |
| Claude Code Write·Edit 도구가 제자리로 쓰는지 rename으로 쓰는지 | S5 | 드라이버 계약 테스트 |
| 몇 시간 유휴·잠자기 뒤 같은 프로세스 생존 | S1 | 드라이버 계약 테스트(실패 시 `--resume`) |
| 숨은 플래그(`--append-system-prompt-file`) 유지 여부, CLI 판 고정 방법 | S1 | 드라이버 계약 테스트 |
| TUI 안 `Esc` 중단의 훅 흐름 | S4 | 터미널 넘기기 카드(2차) |
| 오케스트레이터 장시간 실행·자동 압축 뒤 ack 규칙 | S3 | 오케스트레이터 카드 |
| 실제 크기 카드의 재실행 비용·흔들림 | S9 | 시작 스냅샷을 남긴 첫 카드들 |
