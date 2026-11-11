# 구현 기록

명세(`SPEC.md`)와 아키텍처(`docs/architecture/`)를 코드로 옮긴 순서와, 지금 코드가 어디까지인지.

## 지금 있는 것 (2026-11-11)

`feat/server-core` 브랜치.

| 크레이트 | 내용 |
|---|---|
| `jc-core` | 일지(`events.jsonl`, 정본) + 색인(SQLite WAL, 투영) 쓰기·복구, `task.md` 앞머리·해시·비교 후 맞바꾸기 쓰기, 주제(마디 단위 일치), ULID, 워크스페이스 레이아웃 |
| `jc-server` | JSON-RPC 2.0 over WebSocket(Unix 소켓 0600 / 루프백 TCP), 단일 쓰기 스레드, 구독(구독자별 버퍼 없음), `task.md` 감시 |
| `jadecode-cli` | `init` `project add` `card add/list/show/status/next` `server` `rpc` `rpc-stream` `tail` `check` |

테스트 38개(`cargo test`): 저장소 12 · 코어 단위 15 · RPC 통합 10 · 계약 1.
실측 추가 확인: `tools/crash-test.sh`로 `kill -9` 12회 × 100건 = 303줄, 어긋남 0.

## 아키텍처 문서 → 코드 대응

| 결정 | 코드 | 비고 |
|---|---|---|
| ② B2 파일 정본 + 카드별 일지 + 재생성 색인 | `jc-core/src/store.rs`, `index.rs` | 쓰기 규칙 1~6 반영 |
| B2-1 쓰기 순서·승인 | `Store::append` | 응답 seq는 fsync·색인 커밋 뒤 |
| B2-2 복구 | `Store::open` | 찢긴 줄 자르기, 빈틈 뒤 줄 격리, 색인 재생성 |
| B2-3 빈 색인 채우기 | `Store::open`(`journal_mode=OFF`) | 100만 건 15초 → 3.6초(S5) |
| B2-4 맞바꾸기 쓰기 | `store::write_cas` | `renameat2(RENAME_EXCHANGE)`, macOS는 `renamex_np` 자리만 |
| B2-5 감시 | `jc-server::server::start_watcher` | 비재귀, 읽기 알림 무시, 해시로 자기 쓰기 거르기 |
| B2-6 오프라인 변경 | `Store::open`(투영), `check_card` | `card.reconciled` / `card.state_change_rejected` |
| ④ P1 JSON-RPC over WebSocket | `jc-server/src/rpc.rs`, `server.rs` | Unix 소켓 + 루프백 TCP |
| S5 구독(seq, 마디 단위 일치) | `topic::topic_matches`, `subscription` | 구독자별 버퍼 없음 |
| C1 봇 = git 폴더 | 없음 | 다음 work |

## 명세 대비 남은 것

- **A2 드라이버**(Claude Code stream-json, 격리 플래그, 권한 RPC 25분 상한) — 다음 work. 계약은 `docs/architecture/candidates.md`의 「A2 드라이버 계약」이 그대로 완료 조건이다.
- **MCP 서버**(`report`/`ask`/`propose`/`error`)와 오케스트레이터 깨우기(다이제스트, ack) — S3 규칙.
- **단계 종료 조건·사람 절차·경로 검사** — `card.advance`는 지금 경로만 따라간다. 종료 조건 검사와 `gate: human`은 서버가 아니라 정책이 정한다(다음 work).
- **워크트리 생성** — `card add`가 `--repo r=branch`를 앞머리에 적지만 `git worktree add`는 아직 부르지 않는다(레포 경로 설정 필요).
- **오케스트레이터·봇 정의·기억** — C1.
- **Mac 클라이언트(GPUI)** — berth 데스크톱 코드를 출발점으로. 별도 work.
- **원격(Tailscale)** — 첫 버전 범위 밖.

## 구현 중 확인 (Mac이 필요한 것)

아키텍처 문서가 「구현 중 확인」으로 남긴 항목 중 이 코드가 Mac에서 다시 봐야 하는 것:

- `renameat2` 대신 macOS `renamex_np(RENAME_SWAP)` — `store::exchange`가 `false`를 돌려 rename으로 물러난다(동시 편집 보호가 약해짐).
- FSEvents 지연·합침, APFS `fsync` vs `F_FULLFSYNC`.
- 카드 1,000개·이벤트 10만 건 재구성 시간(S5 Linux: 0.46~2.0초).
