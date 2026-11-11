# 성능 측정

`crates/jc-bench`로 잰 값. **측정 환경**: 이 컨테이너(aarch64, 4코어, virtio 디스크, GPU 가속 없음), 2026-11-11.
숫자는 이 환경의 값이고 Mac·실제 SSD와는 다르다. 목적은 절대값보다 **병목 위치와 개선 전후 비교**다.

## 재는 것 / 못 재는 것

| 구분 | 항목 | 여기서 |
|---|---|---|
| 서버 쓰기 경로 | 일지 덧붙이기 처리량, fsync 비용 | ✅ |
| 서버 복구 | 색인 재생성(차가움/따뜻함), 전수 검사 | ✅ |
| 구독 | 색인 읽기 팬아웃 비용 | ✅ |
| 클라이언트 데이터 계층 | 스냅숏 복제, 이벤트 반영 | ✅ |
| **GPU 렌더** | 프레임 시간, FPS, 셰이더·스왑체인 병목 | ❌ |
| Mac 고유 | FSEvents 지연, APFS `F_FULLFSYNC`, `renamex_np` | ❌ |

GPU를 못 재는 이유: 이 컨테이너의 `/dev/dri`는 `Virtio 1.0 GPU`(3D 가속 없음)이고, Vulkan이 노출하는
장치는 llvmpipe(CPU 래스터라이저) 하나뿐이다. GPUI(blade→Vulkan)는 virtio에 `VK_ERROR_INCOMPATIBLE_DRIVER`로
실패해 아무것도 그리지 못한다. 캡처 자체는 정상(xclock 227색)이므로 측정 도구 문제가 아니다.
`ZED_HEADLESS`는 컴포지터만 바꾸고 GPU 렌더러를 CPU로 대체하지 못한다.

## 사용

```sh
cargo build --release -p jc-bench
B=target/release/jc-bench
$B gen /tmp/bench --projects 12 --cards 1000 --events 100000
$B rebuild /tmp/bench
$B append /tmp/bench --events 20000 --batch 100 --fsync batch
$B append /tmp/bench --events 20000 --batch 1  --fsync batch-group
$B subscribe /tmp/bench --subscribers 8 --events 500
$B client /tmp/bench --iterations 200
$B check /tmp/bench
```

## 측정값

카드 1,000개 · 프로젝트 12개 · 이벤트 10만 건.

| 항목 | 값 |
|---|---|
| 카드·폴더 생성 | 58ms (1,012 폴더+파일) |
| 일지 10만 줄 쓰기 | 5.3s (파일 1,013개) |
| 색인 재생성(차가움) | **1,044ms** (10만 건, 색인 73MB) |
| 재시작(따뜻함, 꼬리 없음) | **1,501ms** ← 병목 |
| 재시작 시 RSS | 89MB |
| 전수 검사(10만 건) | 535ms |

### 덧붙이기 처리량 (20,000건)

| 설정 | 처리량 | 이벤트당 |
|---|---|---|
| batch=1, fsync=batch (기본) | 233/s | 4.30ms |
| batch=10, fsync=batch | 480/s | 2.08ms |
| batch=100, fsync=batch | 560/s | 1.79ms |
| batch=1, fsync=**batch-group**(새) | **507/s** | **1.97ms** |
| batch=100, fsync=off | 6,568/s | 0.152ms |

### 클라이언트 (카드 1,000 · 메시지 500)

| 항목 | 값 | 프레임 예산(16ms) 대비 |
|---|---|---|
| 스냅숏 값 복제(예전) | 869us | **5.4%** |
| 스냅숏 핸들 복제(지금) | **0.025us** | 0.0002% |
| 이벤트 반영 | 0.75us | 무시 가능 |

## 찾은 병목과 고친 것

### 1. 클라이언트가 프레임마다 스냅숏 전체를 복제 (고침)

`App2::snap()`이 매 프레임 `BoardSnapshot::clone()`을 했다. 카드 1,000·메시지 500이면 **869us**로
16ms 예산의 5.4%를 순수 복제에 썼다. 카드가 더 많으면 프레임이 눈에 띄게 끊긴다.

→ `SnapshotHandle`(Arc)로 바꿔 참조 계수만 올린다. **869us → 0.025us(약 34,000배)**.
값 사본은 이벤트가 실제로 왔을 때만 만든다(구독 스레드에서 배치마다 한 번).

### 2. fsync가 이벤트 1건마다 (부분 고침 + 선택지 추가)

카드 1,000개 워크스페이스에서는 연속 이벤트가 서로 다른 일지로 가므로 `Batch`(= 묶음 끝 fsync)가
사실상 "이벤트 1건 = fsync 1회"가 된다. 233/s는 그 결과다.

→ `Fsync::BatchGroup`을 넣었다. 여러 append를 모아 20ms/256회마다 fsync한다.
**233/s → 507/s(2.2배)**. 승인(seq 응답)은 fsync 뒤에만 나가므로 순서·내구성 계약은 유지된다.
테스트: `batch_group_still_durable_after_flush`, `batch_group_keeps_seq_order`.

### 3. 재시작이 차가운 재생성보다 느림(따뜻한 1,501ms > 차가운 1,044ms)

원인은 `Store::open`이 색인 꼬리를 읽은 뒤 **`cards` 테이블 전체를 다시 훑어** `task.md` 1,000개를
읽고 해시를 비교하기 때문이다(안전하지만 매 시작마다 1,000파일 I/O).
`check` 전에 이 검사가 필요하다는 건 맞지만, 매 시작마다 전부 할 필요는 없다.
→ **남은 일**: `task.md`가 색인 이후 바뀌었는지만 보려면 mtime/size를 먼저 비교하고, 다를 때만 해시를
계산한다. 또는 `cards` 투영을 별도 파일에 두고 그 해시만 비교한다.

### 4. 구독 팬아웃

구독자 8명 × 500건 읽기는 지연이 선형이다(구독자별 버퍼가 없어 색인을 각자 읽는다).
느린 구독자는 TCP 역압으로 멈추므로 서버 RSS는 늘지 않는다(설계 의도). 다만 구독자가 아주 많으면
같은 구간을 N번 읽는다 → 공유 캐시(최근 512건 링버퍼)를 두면 줄일 수 있다(남은 일).

## 왜 이 숫자를 그대로 믿으면 안 되는가

- virtio 디스크라 fsync 지연이 실제 SSD와 다르다(이 환경이 더 느릴 가능성이 크다).
- 4코어·aarch64라 병렬 구간이 좁다.
- 클라이언트 수치는 데이터 계층만이다. **GPU 그리기 비용은 안 들어 있다.**
- 화면 프레임 병목을 찾으려면 GPU 있는 Mac에서 프레임 시간을 재야 한다(다음 절).

## GPU 병목을 재려면 (Mac에서)

1. `jadecode-client`를 `cargo build --release`로 빌드.
2. Instruments의 Metal System Trace 또는 `xcrun xctrace record --template 'Metal System Trace'`.
3. 카드 1,000개 워크스페이스를 만들고(`jc-bench gen`) 보드를 스크롤하며 프레임 시간을 본다.
4. 보는 값: 프레임당 CPU 시간, GPU 시간, 드로우 콜 수, 텍스트 아틀라스 재생성 횟수.
5. GPUI는 텍스트를 아틀라스에 굽는다. 카드가 많아지면 **글리프 아틀라스 재업로드**와
   **불필요한 레이아웃 재계산**이 첫 병목일 가능성이 크다. `docs/implementation.md`의 남은 일에 적어 두었다.
