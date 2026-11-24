//! GPUI 화면.
//!
//! 구성(왼쪽부터):
//! 1. 사이드바: 수집함(바깥 사이클) · 확인 요청(승인·질문) · 오케스트레이터 · 보드 · 에이전트 · 연결, 프로젝트 목록
//! 2. 가운데: 카드 목록(급한 순) 또는 칸반(수집 · 단계 열 · 회신 대기)
//! 3. 오른쪽: 카드 스레드(에이전트 대화·도구 호출·권한 승인 + 입력), task.md, 변경사항(diff)
//!
//! 데이터는 `board::Board`가 갖는다. 쓰기 동작은 **별도 연결**로 백그라운드 스레드에서
//! 실행한다(GPUI 실행기와 tokio를 섞지 않는다). 결과는 `Outcome`과 이벤트 구독으로 돌아온다.

pub mod input;
pub mod theme;

use crate::board::{
    AgentInfo, Board, BoardSnapshot, BotInfo, Card as CardData, CardDetail, ConnectorInfo,
    EventRow, IntakeItem,
};
use crate::format;
use crate::view_model::{self as vm, Attention, Bubble, DiffKind, InboxItem, MdBlock};
use gpui::prelude::*;
use gpui::{
    actions, div, px, relative, rgb, rgba, size, AnyElement, App, Application, Bounds, ClickEvent,
    Context, Div, ElementId, Entity, FocusHandle, Focusable, KeyBinding, Menu, MenuItem, OsAction,
    ScrollHandle, SharedString, Stateful, Subscription, SystemMenuType, TitlebarOptions, Window,
    WindowBounds, WindowOptions,
};
use input::{InputEvent, TextInput};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use theme::*;

actions!(
    jadecode,
    [
        Quit,
        NewCard,
        NewProject,
        GoIntake,
        GoInbox,
        GoOrchestrator,
        GoBoard,
        GoAgents,
        GoConnectors,
        OpenLogs,
        ReportIssue,
        CheckUpdate,
        About,
        ToggleBoardMode,
        CloseModal,
        Refresh,
        FocusComposer,
    ]
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Intake,
    Inbox,
    Orchestrator,
    Board,
    Agents,
    Connectors,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardMode {
    List,
    Kanban,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardTab {
    Thread,
    Task,
    Changes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modal {
    NewProject,
    NewCard,
}

/// 백그라운드에서 실행할 쓰기·읽기 동작.
#[derive(Debug, Clone)]
enum Action {
    Advance(String),
    SetStatus {
        card: String,
        status: String,
    },
    StartWorker {
        project: String,
        card: String,
        agent: Option<String>,
        then_send: Option<String>,
    },
    StopInstance(String),
    ResolvePermission {
        seq: u64,
        allow: bool,
    },
    SendOrchestrator {
        project: String,
        text: String,
    },
    SendInstance {
        instance: String,
        text: String,
    },
    LoadCard(String),
    LoadHistory {
        key: String,
        topics: Vec<String>,
        limit: u64,
    },
    LoadDiff(String),
    AddProject {
        name: String,
        repo: String,
    },
    AddCard {
        project: String,
        title: String,
        body: String,
        start: bool,
        agent: Option<String>,
    },
    LoadAgents,
    OpenFolder {
        card: Option<String>,
        terminal: bool,
    },
    LoadIntake,
    AddMemo {
        project: String,
        text: String,
    },
    TriageToCard {
        id: String,
        title: String,
        brief: String,
        start: bool,
        project: String,
        agent: Option<String>,
    },
    Archive(String),
    CloseIntake(String),
    Reply {
        id: String,
        text: String,
    },
    AskOrchestratorTriage {
        project: String,
        id: String,
    },
    LoadConnectors,
    SaveConnector {
        name: String,
        project: String,
        preset: String,
        fields: serde_json::Value,
    },
    RemoveConnector(String),
    SyncConnectors(Option<String>),
    TestConnector(String),
    /// 칸반에서 끌어 놓아 단계를 옮긴다.
    MoveStage {
        card: String,
        stage: String,
    },
    /// 새 판 확인. `true`면 사람이 메뉴에서 부른 것(결과를 늘 알린다).
    CheckUpdate(bool),
    /// 함께 설치된 `jadecode update`로 갈아 끼운다.
    RunUpdate,
}

/// 백그라운드 동작의 결과.
enum Outcome {
    Info(String),
    Error(String),
    Card(CardDetail),
    History {
        key: String,
        rows: Vec<EventRow>,
    },
    Diff {
        card: String,
        text: String,
    },
    ProjectAdded(String),
    CardAdded(String),
    Agents(Vec<AgentInfo>, Vec<BotInfo>),
    Intake(Vec<IntakeItem>),
    Connectors(Vec<ConnectorInfo>),
    /// 새 판(태그). 없으면 None.
    Update {
        latest: Option<String>,
        manual: bool,
    },
}

/// 구독 스레드와 화면이 나누는 상태.
struct Live {
    snap: Mutex<Arc<BoardSnapshot>>,
    /// 스냅숏이 바뀔 때마다 오른다(화면이 다시 그릴지 정한다).
    version: AtomicU64,
    connected: AtomicBool,
}

pub struct Workspace {
    live: Arc<Live>,
    seen_version: u64,
    focus: FocusHandle,
    nav: Nav,
    board_mode: BoardMode,
    card_tab: CardTab,
    project: Option<String>,
    selected_card: Option<String>,
    details: HashMap<String, CardDetail>,
    histories: HashMap<String, Vec<EventRow>>,
    diffs: HashMap<String, String>,
    agents: Vec<AgentInfo>,
    bots: Vec<BotInfo>,
    intake: Vec<IntakeItem>,
    connectors: Vec<ConnectorInfo>,
    reply_target: Option<String>,
    last_intake_seq: u64,
    conn_project: Option<String>,
    /// 새 판이 있으면 그 태그.
    update_available: Option<String>,
    updating: bool,
    memo_input: Entity<TextInput>,
    reply_input: Entity<TextInput>,
    conn_token: Entity<TextInput>,
    conn_team: Entity<TextInput>,
    conn_channel: Entity<TextInput>,
    agent_choice: Option<String>,
    modal: Option<Modal>,
    new_card_start: bool,
    toast: Option<(String, bool, Instant)>,
    backend_label: String,
    backend_embedded: bool,
    tx: mpsc::Sender<Action>,
    results: mpsc::Receiver<Outcome>,
    // 입력칸
    composer: Entity<TextInput>,
    orch_input: Entity<TextInput>,
    project_name: Entity<TextInput>,
    project_repo: Entity<TextInput>,
    card_title: Entity<TextInput>,
    card_body: Entity<TextInput>,
    thread_scroll: ScrollHandle,
    last_thread_len: usize,
    last_title: String,
    /// 이미 알린 받은편지함 항목(seq). 처음 그릴 때 있던 것은 알리지 않는다.
    notified: Option<std::collections::HashSet<u64>>,
    _subs: Vec<Subscription>,
}

/// 실행 진입점.
pub fn run(workspace: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    // 떠 있는 서버가 있으면 붙고, 없으면 이 프로세스 안에서 로컬 백엔드를 띄운다.
    let backend = crate::local::ensure_backend(workspace.as_deref())?;
    let backend_embedded = matches!(backend.kind, crate::local::BackendKind::Embedded);
    let backend_label = format!(
        "{} · {}",
        backend.label(),
        backend
            .workspace
            .as_ref()
            .map(|w| tilde(&w.display().to_string()))
            .unwrap_or_else(|| backend.conn.socket.display().to_string())
    );
    let conn = backend.conn.clone();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let board = rt.block_on(conn.open())?;
    drop(rt);

    let (tx, rx) = mpsc::channel::<Action>();
    let (res_tx, res_rx) = mpsc::channel::<Outcome>();
    spawn_action_thread(conn.socket.clone(), rx, res_tx);

    let live = Arc::new(Live {
        snap: Mutex::new(Arc::new(board.snapshot().clone())),
        version: AtomicU64::new(1),
        connected: AtomicBool::new(true),
    });
    spawn_subscription_thread(board, conn.clone(), live.clone());

    Application::new().run(move |cx: &mut App| {
        input::bind_keys(cx);
        cx.bind_keys([
            KeyBinding::new("secondary-q", Quit, None),
            KeyBinding::new("secondary-n", NewCard, None),
            KeyBinding::new("secondary-shift-n", NewProject, None),
            KeyBinding::new("secondary-1", GoIntake, None),
            KeyBinding::new("secondary-2", GoInbox, None),
            KeyBinding::new("secondary-3", GoOrchestrator, None),
            KeyBinding::new("secondary-4", GoBoard, None),
            KeyBinding::new("secondary-5", GoAgents, None),
            KeyBinding::new("secondary-6", GoConnectors, None),
            KeyBinding::new("secondary-l", ToggleBoardMode, None),
            KeyBinding::new("secondary-r", Refresh, None),
            KeyBinding::new("secondary-k", FocusComposer, None),
            KeyBinding::new("escape", CloseModal, Some("Workspace")),
        ]);
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.on_action(|_: &OpenLogs, _cx: &mut App| {
            let dir = crate::logging::log_dir();
            let _ = std::fs::create_dir_all(&dir);
            open_path(&dir.display().to_string());
        });
        cx.on_action(|_: &ReportIssue, _cx: &mut App| {
            open_path(&issue_url());
        });
        cx.set_menus(vec![
            Menu {
                name: "jadecode".into(),
                items: vec![
                    MenuItem::action("jadecode 정보", About),
                    MenuItem::action("업데이트 확인…", CheckUpdate),
                    MenuItem::separator(),
                    MenuItem::os_submenu("Services", SystemMenuType::Services),
                    MenuItem::separator(),
                    MenuItem::action("jadecode 종료", Quit),
                ],
            },
            Menu {
                name: "파일".into(),
                items: vec![
                    MenuItem::action("새 카드", NewCard),
                    MenuItem::action("새 프로젝트", NewProject),
                ],
            },
            Menu {
                name: "편집".into(),
                items: vec![
                    MenuItem::os_action("잘라내기", input::Cut, OsAction::Cut),
                    MenuItem::os_action("복사", input::Copy, OsAction::Copy),
                    MenuItem::os_action("붙여넣기", input::Paste, OsAction::Paste),
                    MenuItem::os_action("모두 선택", input::SelectAll, OsAction::SelectAll),
                ],
            },
            Menu {
                name: "보기".into(),
                items: vec![
                    MenuItem::action("수집함", GoIntake),
                    MenuItem::action("확인 요청", GoInbox),
                    MenuItem::action("오케스트레이터", GoOrchestrator),
                    MenuItem::action("보드", GoBoard),
                    MenuItem::action("에이전트", GoAgents),
                    MenuItem::action("연결", GoConnectors),
                    MenuItem::separator(),
                    MenuItem::action("목록/칸반 전환", ToggleBoardMode),
                    MenuItem::action("새로고침", Refresh),
                ],
            },
            Menu {
                name: "도움말".into(),
                items: vec![
                    MenuItem::action("문제 보고하기…", ReportIssue),
                    MenuItem::action("로그 폴더 열기", OpenLogs),
                ],
            },
        ]);

        // 화면이 작으면(노트북·CI 1024x768) 창이 화면 밖으로 나가지 않게 줄인다.
        let (mut w, mut h) = (px(1360.), px(860.));
        if let Some(d) = cx.primary_display() {
            let ds = d.bounds().size;
            w = w.min(ds.width * 0.94).max(px(960.));
            h = h.min(ds.height * 0.80).max(px(600.));
        }
        let bounds = Bounds::centered(None, size(w, h), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("jadecode".into()),
                        ..Default::default()
                    }),
                    window_min_size: Some(size(px(960.), px(600.))),
                    app_id: Some("dev.jadewisemann.jadecode".into()),
                    ..Default::default()
                },
                {
                    let live = live.clone();
                    let backend_label = backend_label.clone();
                    move |window, cx| {
                        cx.new(|cx| {
                            Workspace::new(
                                window,
                                cx,
                                live,
                                tx,
                                res_rx,
                                backend_label,
                                backend_embedded,
                            )
                        })
                    }
                },
            )
            .expect("창을 열 수 없다");
        window
            .update(cx, |ws, window, cx| {
                window.focus(&ws.focus_handle(cx));
                // 화면 확인용: 화면을 차례로 띄우며 스크린샷을 찍고 끝난다(CI에서 쓴다).
                if let Some(dir) = std::env::var_os("JADECODE_SHOTS") {
                    ws.start_shot_tour(std::path::PathBuf::from(dir), window, cx);
                }
            })
            .ok();
        cx.activate(true);
    });
    Ok(())
}

/// "PR 만들기" 버튼이 워커에게 보내는 지시.
const PR_PROMPT: &str =
    "지금까지 바꾼 내용을 확인하고 커밋한 뒤 브랜치를 푸시해서 Draft PR을 열어 주세요. \
보호 브랜치에 직접 병합하지는 마세요. 끝나면 PR 링크를 report로 알려 주세요.";

/// 데스크톱 알림(받은편지함에 새 일이 들어왔을 때). 실패해도 조용히 넘어간다.
fn notify_desktop(title: &str, body: &str) {
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        if cfg!(target_os = "macos") {
            let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
            let script = format!(
                "display notification \"{}\" with title \"{}\"",
                esc(&body),
                esc(&title)
            );
            let _ = std::process::Command::new("osascript")
                .args(["-e", &script])
                .output();
        } else {
            let _ = std::process::Command::new("notify-send")
                .args([&title, &body])
                .output();
        }
    });
}

/// 파일·폴더·URL을 기본 앱으로 연다.
fn open_path(target: &str) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(cmd).arg(target).spawn();
}

/// 문제 보고 주소. 판·운영체제를 미리 채운다(개인 정보는 넣지 않는다).
fn issue_url() -> String {
    let body = format!(
        "**무엇을 하다가**\n\n**무슨 일이 있었나**\n\n**기대한 것**\n\n---\n판: {}\n운영체제: {} {}\n로그: {}",
        jc_core::version::VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH,
        crate::logging::log_path().display()
    );
    let enc: String = body
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    format!("https://github.com/jadewisemann/jadecode/issues/new?labels=alpha&body={enc}")
}

fn tilde(p: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && p.starts_with(&h) => format!("~{}", &p[h.len()..]),
        _ => p.to_string(),
    }
}

fn spawn_action_thread(
    socket: std::path::PathBuf,
    rx: mpsc::Receiver<Action>,
    out: mpsc::Sender<Outcome>,
) {
    std::thread::Builder::new()
        .name("jadecode-client-actions".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(r) => r,
                Err(e) => {
                    let _ = out.send(Outcome::Error(format!("런타임 오류: {e}")));
                    return;
                }
            };
            let mut board: Option<Board> = None;
            let mut ws_root: Option<std::path::PathBuf> = None;
            while let Ok(a) = rx.recv() {
                // 연결이 끊겼으면 다시 연다.
                if board.as_ref().map(|b| !b.is_connected()).unwrap_or(true) {
                    let c = crate::board::Connection {
                        socket: socket.clone(),
                    };
                    board = match rt.block_on(c.open()) {
                        Ok(b) => Some(b),
                        Err(e) => {
                            let _ = out
                                .send(Outcome::Error(format!("백엔드에 연결하지 못했습니다: {e}")));
                            continue;
                        }
                    };
                }
                let b = board.as_mut().unwrap();
                if ws_root.is_none() {
                    ws_root = rt.block_on(b.workspace()).ok();
                }
                // 한 동작이 패닉해도 동작 스레드는 살아남아 다음 동작을 받는다.
                // 변종 이름만 남긴다(입력값에 토큰이 들어 있을 수 있다).
                let what: String = format!("{a:?}")
                    .split(|c: char| !c.is_alphanumeric())
                    .next()
                    .unwrap_or("")
                    .to_string();
                let r = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_action(&rt, b, ws_root.as_deref(), a)
                })) {
                    Ok(r) => r,
                    Err(_) => {
                        eprintln!("동작 중 패닉: {what}");
                        board = None;
                        Outcome::Error(
                            "내부 오류로 동작을 끝내지 못했습니다 (도움말 → 로그 폴더 열기)".into(),
                        )
                    }
                };
                // RPC가 실패했으면 다음 동작 전에 다시 붙도록 연결을 버린다.
                if let Outcome::Error(e) = &r {
                    if e.contains("연결") || e.contains("closed") {
                        board = None;
                    }
                }
                let _ = out.send(r);
            }
        })
        .ok();
}

fn run_action(
    rt: &tokio::runtime::Runtime,
    b: &mut Board,
    ws: Option<&std::path::Path>,
    a: Action,
) -> Outcome {
    let fail = |what: &str, e: anyhow::Error| Outcome::Error(format!("{what}: {e:#}"));
    match a {
        Action::Advance(card) => match rt.block_on(b.advance(&card)) {
            Ok(stage) => Outcome::Info(format!("다음 단계로: {}", format::stage_label(&stage))),
            Err(e) => fail("단계 이동 실패", e),
        },
        Action::SetStatus { card, status } => match rt.block_on(b.set_status(&card, &status)) {
            Ok(()) => Outcome::Info(format!("상태: {}", format::status_label(&status))),
            Err(e) => fail("상태 바꾸기 실패", e),
        },
        Action::StartWorker {
            project,
            card,
            agent,
            then_send,
        } => match rt.block_on(b.start_worker(&project, &card, agent.as_deref())) {
            Ok(id) => {
                if let Some(text) = then_send {
                    if let Err(e) = rt.block_on(b.send_to_instance(&id, &text)) {
                        return fail("워커에게 보내기 실패", e);
                    }
                }
                Outcome::Info("워커를 시작했습니다".into())
            }
            Err(e) => fail("워커 시작 실패", e),
        },
        Action::StopInstance(id) => match rt.block_on(b.stop_instance(&id)) {
            Ok(()) => Outcome::Info("워커를 멈췄습니다".into()),
            Err(e) => fail("멈추기 실패", e),
        },
        Action::ResolvePermission { seq, allow } => {
            let m = if allow { "승인" } else { "거절" };
            match rt.block_on(b.resolve_permission(seq, allow, m)) {
                Ok(()) => Outcome::Info(format!("권한 #{seq} {m}")),
                Err(e) => fail("권한 답 실패", e),
            }
        }
        Action::SendOrchestrator { project, text } => {
            match rt.block_on(b.send_to_orchestrator(&project, &text)) {
                Ok(()) => Outcome::Info("오케스트레이터에게 보냈습니다".into()),
                Err(e) => fail("보내기 실패", e),
            }
        }
        Action::SendInstance { instance, text } => {
            match rt.block_on(b.send_to_instance(&instance, &text)) {
                Ok(()) => Outcome::Info("보냈습니다".into()),
                Err(e) => fail("보내기 실패", e),
            }
        }
        Action::LoadCard(card) => match rt.block_on(b.card_detail(&card)) {
            Ok(d) => Outcome::Card(d),
            Err(e) => fail("카드 읽기 실패", e),
        },
        Action::LoadHistory { key, topics, limit } => {
            match rt.block_on(b.history(&topics, limit)) {
                Ok(rows) => Outcome::History { key, rows },
                Err(e) => fail("이력 읽기 실패", e),
            }
        }
        Action::LoadDiff(card) => {
            let Some(ws) = ws else {
                return Outcome::Error(
                    "워크스페이스 위치를 몰라 변경사항을 읽지 못했습니다".into(),
                );
            };
            let text = vm::collect_diff(&ws.join(&card), 4000);
            Outcome::Diff { card, text }
        }
        Action::AddProject { name, repo } => match rt.block_on(b.add_project(&name, Some(&repo))) {
            Ok(slug) => Outcome::ProjectAdded(slug),
            Err(e) => fail("프로젝트 만들기 실패", e),
        },
        Action::AddCard {
            project,
            title,
            body,
            start,
            agent,
        } => match rt.block_on(b.add_card(&project, &title, Some(&body))) {
            Ok(path) => {
                if start {
                    if let Err(e) = rt.block_on(b.start_worker(&project, &path, agent.as_deref())) {
                        return fail("카드는 만들었지만 워커 시작 실패", e);
                    }
                }
                Outcome::CardAdded(path)
            }
            Err(e) => fail("카드 만들기 실패", e),
        },
        Action::LoadAgents => {
            let agents = rt.block_on(b.agents()).unwrap_or_default();
            let bots = rt.block_on(b.bots()).unwrap_or_default();
            Outcome::Agents(agents, bots)
        }
        Action::LoadIntake => match rt.block_on(b.intake_list(None)) {
            Ok(v) => Outcome::Intake(v),
            Err(e) => fail("수집함 읽기 실패", e),
        },
        Action::AddMemo { project, text } => {
            let (title, body) = match text.split_once(" - ") {
                Some((t, b)) => (t.trim().to_string(), b.trim().to_string()),
                None => (text.trim().to_string(), String::new()),
            };
            match rt.block_on(b.intake_add(&project, &title, &body)) {
                Ok(_) => Outcome::Info("수집함에 넣었습니다".into()),
                Err(e) => fail("메모 넣기 실패", e),
            }
        }
        Action::TriageToCard {
            id,
            title,
            brief,
            start,
            project,
            agent,
        } => match rt.block_on(b.intake_triage(&id, "card", Some(&title), Some(&brief))) {
            Ok(Some(card)) => {
                if start {
                    if let Err(e) = rt.block_on(b.start_worker(&project, &card, agent.as_deref())) {
                        return fail("카드는 만들었지만 워커 시작 실패", e);
                    }
                }
                Outcome::CardAdded(card)
            }
            Ok(None) => Outcome::Error("카드를 만들지 못했습니다".into()),
            Err(e) => fail("카드 만들기 실패", e),
        },
        Action::Archive(id) => match rt.block_on(b.intake_triage(&id, "archive", None, None)) {
            Ok(_) => Outcome::Info("보관했습니다".into()),
            Err(e) => fail("보관 실패", e),
        },
        Action::CloseIntake(id) => match rt.block_on(b.intake_close(&id)) {
            Ok(()) => Outcome::Info("닫았습니다".into()),
            Err(e) => fail("닫기 실패", e),
        },
        Action::Reply { id, text } => match rt.block_on(b.intake_reply(&id, &text)) {
            Ok(()) => Outcome::Info("답장을 보냈습니다".into()),
            Err(e) => fail("답장 보내기 실패", e),
        },
        Action::AskOrchestratorTriage { project, id } => {
            let text = format!(
                "수집 항목 {id}를 `intake_get`으로 읽고 `intake_triage`로 분류해 주세요. 코드 작업이면 원문 없이도 일할 수 있게 짧은 brief와 done_when을 써서 카드로 만들고 워커를 붙여 주세요."
            );
            match rt.block_on(b.send_to_orchestrator(&project, &text)) {
                Ok(()) => Outcome::Info("오케스트레이터에게 분류를 맡겼습니다".into()),
                Err(e) => fail("오케스트레이터에게 보내기 실패", e),
            }
        }
        Action::LoadConnectors => match rt.block_on(b.connectors()) {
            Ok(v) => Outcome::Connectors(v),
            Err(e) => fail("커넥터 읽기 실패", e),
        },
        Action::SaveConnector {
            name,
            project,
            preset,
            fields,
        } => match rt.block_on(b.connector_save(&name, &project, &preset, fields)) {
            Ok(()) => match rt.block_on(b.connector_sync(Some(&name))) {
                Ok((n, errs)) if errs.is_empty() => {
                    Outcome::Info(format!("{name} 연결됨 · 새 항목 {n}개"))
                }
                Ok((_, errs)) => {
                    Outcome::Error(format!("저장했지만 동기화 실패: {}", errs.join("; ")))
                }
                Err(e) => fail("저장했지만 동기화 실패", e),
            },
            Err(e) => fail("커넥터 저장 실패", e),
        },
        Action::RemoveConnector(name) => match rt.block_on(b.connector_remove(&name)) {
            Ok(()) => Outcome::Info(format!("{name} 연결을 지웠습니다")),
            Err(e) => fail("지우기 실패", e),
        },
        Action::SyncConnectors(name) => match rt.block_on(b.connector_sync(name.as_deref())) {
            Ok((n, errs)) if errs.is_empty() => Outcome::Info(format!("동기화 · 새 항목 {n}개")),
            Ok((_, errs)) => Outcome::Error(format!("동기화 실패: {}", errs.join("; "))),
            Err(e) => fail("동기화 실패", e),
        },
        Action::TestConnector(name) => match rt.block_on(b.connector_test(&name)) {
            Ok(tools) => Outcome::Info(format!("{name} 정상 · 도구 {}개", tools.len())),
            Err(e) => fail("연결 시험 실패", e),
        },
        Action::MoveStage { card, stage } => match rt.block_on(b.move_to_stage(&card, &stage)) {
            Ok(stage) => {
                Outcome::Info(format!("{} 단계로 옮겼습니다", format::stage_label(&stage)))
            }
            Err(e) => fail("단계를 옮기지 못했습니다", e),
        },
        Action::CheckUpdate(manual) => match latest_release_tag() {
            Ok(tag) => Outcome::Update {
                latest: jc_core::version::is_newer(&tag, jc_core::version::VERSION).then_some(tag),
                manual,
            },
            Err(e) if manual => fail("업데이트를 확인하지 못했습니다", e),
            Err(_) => Outcome::Update {
                latest: None,
                manual,
            },
        },
        Action::RunUpdate => {
            let exe = std::env::current_exe().ok();
            let cli = exe
                .as_ref()
                .and_then(|e| e.parent())
                .map(|d| d.join("jadecode"))
                .filter(|p| p.exists())
                .unwrap_or_else(|| std::path::PathBuf::from("jadecode"));
            let mut cmd = std::process::Command::new(&cli);
            cmd.arg("update");
            // 지금 실행 중인 앱 번들을 갈아 끼우게 한다(/Applications 밖에 둔 경우 포함).
            if let Some(app) = exe.as_ref().and_then(|e| {
                e.ancestors()
                    .find(|a| a.extension().is_some_and(|x| x == "app"))
            }) {
                cmd.env("JADECODE_APP", app);
            }
            match cmd.output() {
                Ok(o) if o.status.success() => {
                    Outcome::Info("업데이트를 설치했습니다. 앱을 다시 시작하면 적용됩니다".into())
                }
                Ok(o) => Outcome::Error(format!(
                    "업데이트 실패: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                )),
                Err(e) => Outcome::Error(format!("업데이트 실패: {e}")),
            }
        }
        Action::OpenFolder { card, terminal } => {
            let Some(ws) = ws else {
                return Outcome::Error("워크스페이스 위치를 알 수 없습니다".into());
            };
            let dir = match &card {
                Some(c) => ws.join(c),
                None => ws.to_path_buf(),
            };
            let r = if cfg!(target_os = "macos") {
                let mut c = std::process::Command::new("open");
                if terminal {
                    c.args(["-a", "Terminal"]);
                }
                c.arg(&dir).spawn()
            } else {
                std::process::Command::new("xdg-open").arg(&dir).spawn()
            };
            match r {
                Ok(_) => {
                    Outcome::Info(format!("열었습니다: {}", tilde(&dir.display().to_string())))
                }
                Err(e) => Outcome::Error(format!("열기 실패: {e}")),
            }
        }
    }
}

/// GitHub에서 초안이 아닌 최신 릴리즈 태그를 읽는다(curl 사용, 10초 제한).
fn latest_release_tag() -> anyhow::Result<String> {
    let out = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "10",
            "-H",
            "User-Agent: jadecode-app",
            "https://api.github.com/repos/jadewisemann/jadecode/releases?per_page=5",
        ])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    v.as_array()
        .into_iter()
        .flatten()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false))
        .find_map(|r| r["tag_name"].as_str().map(String::from))
        .ok_or_else(|| anyhow::anyhow!("릴리즈를 찾지 못했습니다"))
}

/// 이벤트를 받아 공유 스냅숏을 갱신한다. 연결이 끊기면 2초마다 다시 붙는다.
fn spawn_subscription_thread(board: Board, conn: crate::board::Connection, live: Arc<Live>) {
    std::thread::Builder::new()
        .name("jadecode-client-sub".into())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(async move {
                let mut board = board;
                loop {
                    if !board.is_connected() {
                        live.connected.store(false, Ordering::Relaxed);
                        live.version.fetch_add(1, Ordering::Relaxed);
                        loop {
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            if let Ok(b) = conn.open().await {
                                board = b;
                                break;
                            }
                        }
                        live.connected.store(true, Ordering::Relaxed);
                        if let Ok(mut s) = live.snap.lock() {
                            *s = Arc::new(board.snapshot().clone());
                        }
                        live.version.fetch_add(1, Ordering::Relaxed);
                    }
                    if board.poll().await.is_some() {
                        if let Ok(mut s) = live.snap.lock() {
                            // 이벤트가 왔을 때만 새 스냅숏을 만든다(프레임마다 아님).
                            *s = Arc::new(board.snapshot().clone());
                        }
                        live.version.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        })
        .ok();
}

// ---------- 끌어 놓기 ----------

/// 칸반에서 끄는 카드.
#[derive(Clone)]
struct DragCard {
    path: String,
    title: String,
    stage: String,
}

/// 끄는 동안 커서를 따라다니는 작은 타일.
struct DragPreview(String);

impl Render for DragPreview {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_2()
            .w(px(240.))
            .rounded_md()
            .bg(rgb(RAISED))
            .border_1()
            .border_color(rgb(ACCENT))
            .shadow_lg()
            .text_sm()
            .text_color(rgb(TEXT))
            .truncate()
            .child(self.0.clone())
    }
}

// ---------- 작은 부품 ----------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Btn {
    Primary,
    Ghost,
    Danger,
    Ok,
    Subtle,
}

fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: Btn) -> Stateful<Div> {
    let (bg, fg, hover) = match kind {
        Btn::Primary => (ACCENT, WHITE, 0x74a0ff),
        Btn::Ghost => (RAISED, TEXT, HOVER),
        Btn::Danger => (0x3a1f22, RED, 0x4a2528),
        Btn::Ok => (0x1c3a2c, GREEN, 0x234a37),
        Btn::Subtle => (PANEL, MUTED, HOVER),
    };
    div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .px_3()
        .py_1()
        .rounded_md()
        .text_sm()
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(hover)))
        .child(label.into())
}

/// 탭 줄에 두는 작은 텍스트 버튼.
fn link_btn(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id.into())
        .px_2()
        .py_1()
        .rounded_md()
        .text_xs()
        .text_color(rgb(MUTED))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(HOVER)).text_color(rgb(TEXT)))
        .child(label.into())
}

fn chip(id: impl Into<ElementId>, label: impl Into<SharedString>, on: bool) -> Stateful<Div> {
    div()
        .id(id.into())
        .px_2()
        .py(px(2.))
        .rounded_md()
        .text_xs()
        .border_1()
        .border_color(rgb(if on { ACCENT } else { LINE }))
        .bg(rgb(if on { 0x1d2a47 } else { PANEL }))
        .text_color(rgb(if on { TEXT } else { MUTED }))
        .cursor_pointer()
        .child(label.into())
}

fn dot(color: u32) -> Div {
    div().flex_none().size(px(8.)).rounded_full().bg(rgb(color))
}

fn badge(n: usize, color: u32) -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .rounded_full()
        .text_xs()
        .bg(rgb(color))
        .text_color(rgb(0x0b0d10))
        .child(n.to_string())
}

fn section_label(t: impl Into<SharedString>) -> Div {
    div()
        .px_3()
        .pt_3()
        .pb_1()
        .text_xs()
        .text_color(rgb(FAINT))
        .child(t.into())
}

fn empty_state(title: &str, body: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .p_8()
        .size_full()
        .child(div().text_color(rgb(TEXT)).child(title.to_string()))
        .child(
            div()
                .text_sm()
                .text_color(rgb(MUTED))
                .max_w(px(460.))
                .child(body.to_string()),
        )
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Workspace {
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        live: Arc<Live>,
        tx: mpsc::Sender<Action>,
        results: mpsc::Receiver<Outcome>,
        backend_label: String,
        backend_embedded: bool,
    ) -> Self {
        let composer = cx.new(|cx| {
            TextInput::new(cx, "워커에게 말하기 · Enter 보내기 · Shift-Enter 줄바꿈").multiline(8)
        });
        let orch_input = cx.new(|cx| {
            TextInput::new(
                cx,
                "오케스트레이터에게 할 일을 말하세요 · Shift-Enter 줄바꿈",
            )
            .multiline(8)
        });
        let project_name = cx.new(|cx| TextInput::new(cx, "프로젝트 이름 (영문·숫자)"));
        let project_repo =
            cx.new(|cx| TextInput::new(cx, "코드 레포 폴더 (선택) · 예: ~/code/myapp"));
        let card_title = cx.new(|cx| TextInput::new(cx, "무엇을 할까요? (카드 제목)"));
        let card_body =
            cx.new(|cx| TextInput::new(cx, "자세한 설명 (선택) · Shift-Enter 줄바꿈").multiline(6));
        let memo_input =
            cx.new(|cx| TextInput::new(cx, "직접 메모 넣기 · \"제목 - 내용\" · Enter"));
        let reply_input = cx.new(|cx| {
            TextInput::new(cx, "답장 내용 · Enter 보내기 · Shift-Enter 줄바꿈").multiline(6)
        });
        let conn_token = cx.new(|cx| TextInput::new(cx, "봇 토큰 (xoxb-…)").secret());
        let conn_team = cx.new(|cx| TextInput::new(cx, "워크스페이스 ID (T…)"));
        let conn_channel = cx.new(|cx| TextInput::new(cx, "채널 ID (C…)"));

        let mut subs = vec![];
        subs.push(cx.subscribe_in(
            &composer,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Submitted(text) if !text.trim().is_empty() => {
                    this.send_from_composer(text.clone(), cx)
                }
                InputEvent::Submitted(_) => {}
                InputEvent::Cancelled => window.focus(&this.focus),
                InputEvent::Changed => {}
            },
        ));
        subs.push(cx.subscribe_in(
            &orch_input,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Submitted(text) if !text.trim().is_empty() => {
                    this.send_to_orchestrator(text.clone(), cx)
                }
                InputEvent::Submitted(_) => {}
                InputEvent::Cancelled => window.focus(&this.focus),
                InputEvent::Changed => {}
            },
        ));
        subs.push(cx.subscribe_in(
            &project_name,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Submitted(_) => window.focus(&this.project_repo.focus_handle(cx)),
                InputEvent::Cancelled => this.close_modal(window, cx),
                InputEvent::Changed => {}
            },
        ));
        subs.push(cx.subscribe_in(
            &project_repo,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Submitted(_) => this.submit_project(window, cx),
                InputEvent::Cancelled => this.close_modal(window, cx),
                InputEvent::Changed => {}
            },
        ));
        subs.push(cx.subscribe_in(
            &card_title,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Submitted(_) => window.focus(&this.card_body.focus_handle(cx)),
                InputEvent::Cancelled => this.close_modal(window, cx),
                InputEvent::Changed => {}
            },
        ));
        subs.push(cx.subscribe_in(
            &card_body,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Submitted(_) => this.submit_card(window, cx),
                InputEvent::Cancelled => this.close_modal(window, cx),
                InputEvent::Changed => {}
            },
        ));

        subs.push(cx.subscribe_in(
            &memo_input,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Submitted(text) if !text.trim().is_empty() => {
                    this.add_memo(text.clone(), cx)
                }
                InputEvent::Submitted(_) => {}
                InputEvent::Cancelled => window.focus(&this.focus),
                InputEvent::Changed => {}
            },
        ));
        subs.push(cx.subscribe_in(
            &reply_input,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Submitted(text) if !text.trim().is_empty() => {
                    this.send_reply(text.clone(), cx)
                }
                InputEvent::Submitted(_) => {}
                InputEvent::Cancelled => {
                    this.reply_target = None;
                    window.focus(&this.focus);
                    cx.notify();
                }
                InputEvent::Changed => {}
            },
        ));
        for (from, to) in [
            (conn_token.clone(), Some(conn_team.clone())),
            (conn_team.clone(), Some(conn_channel.clone())),
            (conn_channel.clone(), None),
        ] {
            subs.push(cx.subscribe_in(
                &from,
                window,
                move |this, _, ev: &InputEvent, window, cx| {
                    if let InputEvent::Submitted(_) = ev {
                        match &to {
                            Some(next) => window.focus(&next.focus_handle(cx)),
                            None => this.save_slack(cx),
                        }
                    }
                },
            ));
        }

        // 결과·스냅숏 변화를 화면에 반영하는 주기 작업.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(80))
                .await;
            if this.update(cx, |this, cx| this.pump(cx)).is_err() {
                break;
            }
        })
        .detach();

        let _ = tx.send(Action::LoadAgents);
        let _ = tx.send(Action::LoadIntake);
        let _ = tx.send(Action::LoadConnectors);
        if !jc_core::version::is_dev_build() {
            let _ = tx.send(Action::CheckUpdate(false));
        }
        Self {
            live,
            seen_version: 0,
            focus: cx.focus_handle(),
            nav: Nav::Board,
            board_mode: BoardMode::List,
            card_tab: CardTab::Thread,
            project: None,
            selected_card: None,
            details: HashMap::new(),
            histories: HashMap::new(),
            diffs: HashMap::new(),
            agents: vec![],
            bots: vec![],
            agent_choice: None,
            modal: None,
            new_card_start: true,
            toast: None,
            backend_label,
            backend_embedded,
            tx,
            results,
            composer,
            orch_input,
            project_name,
            project_repo,
            card_title,
            card_body,
            intake: vec![],
            connectors: vec![],
            reply_target: None,
            last_intake_seq: 0,
            conn_project: None,
            update_available: None,
            updating: false,
            memo_input,
            reply_input,
            conn_token,
            conn_team,
            conn_channel,
            thread_scroll: ScrollHandle::new(),
            last_thread_len: 0,
            last_title: String::new(),
            notified: None,
            _subs: subs,
        }
    }

    fn snap(&self) -> Arc<BoardSnapshot> {
        self.live
            .snap
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| Arc::new(BoardSnapshot::default()))
    }

    fn send(&self, a: Action) {
        let _ = self.tx.send(a);
    }

    fn toast(&mut self, msg: impl Into<String>, error: bool) {
        self.toast = Some((msg.into(), error, Instant::now()));
    }

    /// 백그라운드 결과를 반영하고, 바뀐 것이 있으면 다시 그린다.
    fn pump(&mut self, cx: &mut Context<Self>) {
        let mut dirty = false;
        while let Ok(o) = self.results.try_recv() {
            dirty = true;
            match o {
                Outcome::Info(m) => {
                    if self.updating && m.starts_with("업데이트") {
                        self.updating = false;
                        self.update_available = None;
                    }
                    self.toast(m, false)
                }
                Outcome::Error(m) => {
                    if m.starts_with("업데이트") {
                        self.updating = false;
                    }
                    self.toast(m, true)
                }
                Outcome::Card(d) => {
                    self.details.insert(d.path.clone(), d);
                }
                Outcome::History { key, rows } => {
                    self.histories.insert(key, rows);
                }
                Outcome::Diff { card, text } => {
                    self.diffs.insert(card, text);
                }
                Outcome::ProjectAdded(slug) => {
                    self.toast(format!("프로젝트 {slug}를 만들었습니다"), false);
                    self.project = Some(slug);
                    self.nav = Nav::Board;
                }
                Outcome::CardAdded(path) => {
                    self.toast("카드를 만들었습니다", false);
                    self.nav = Nav::Board;
                    self.open_card(path, cx);
                }
                Outcome::Agents(a, b) => {
                    self.agents = a;
                    self.bots = b;
                }
                Outcome::Intake(v) => self.intake = v,
                Outcome::Connectors(v) => self.connectors = v,
                Outcome::Update { latest, manual } => {
                    if manual {
                        match &latest {
                            Some(t) => self.toast(format!("새 판 {t}이 있습니다"), false),
                            None => self.toast(
                                format!("최신 판입니다 ({})", jc_core::version::VERSION),
                                false,
                            ),
                        }
                    }
                    self.update_available = latest;
                }
            }
        }
        let v = self.live.version.load(Ordering::Relaxed);
        if v != self.seen_version {
            self.seen_version = v;
            dirty = true;
            self.notify_new_inbox();
            // 수집함·카드 상태가 바뀌었으면 수집함을 다시 읽는다.
            let snap = self.snap();
            let newest = snap
                .recent
                .iter()
                .filter(|e| e.kind.starts_with("intake.") || e.kind == "card.status_changed")
                .map(|e| e.seq)
                .max()
                .unwrap_or(0);
            if newest > self.last_intake_seq {
                if self.last_intake_seq > 0 {
                    let fresh = snap
                        .recent
                        .iter()
                        .filter(|e| e.seq > self.last_intake_seq && e.kind == "intake.received")
                        .count();
                    if fresh > 0 {
                        notify_desktop("jadecode · 수집함", &format!("새로 들어온 일 {fresh}건"));
                    }
                }
                self.last_intake_seq = newest;
                self.send(Action::LoadIntake);
            }
        }
        if let Some((_, _, at)) = &self.toast {
            if at.elapsed() > Duration::from_secs(6) {
                self.toast = None;
                dirty = true;
            }
        }
        if dirty {
            cx.notify();
        }
    }

    /// `JADECODE_SHOTS=<폴더>`: 화면을 하나씩 띄우고 `screencapture`로 찍은 뒤 끝낸다.
    /// GPU가 있는 macOS CI에서 실제 픽셀을 확인하려고 쓴다.
    fn start_shot_tour(
        &mut self,
        dir: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = std::fs::create_dir_all(&dir);
        cx.spawn_in(window, async move |this, cx| {
            let exec = cx.background_executor().clone();
            let wait = |ms: u64| exec.timer(Duration::from_millis(ms));
            wait(2500).await;
            const STEPS: [&str; 15] = [
                "01-board-list",
                "02-board-kanban",
                "03-card-thread",
                "04-card-task",
                "05-card-changes",
                "06-intake",
                "07-kanban-card",
                "08-inbox",
                "09-orchestrator",
                "10-agents",
                "11-connectors",
                "12-new-card",
                "13-new-project",
                "14-composer-multiline",
                "15-narrow",
            ];
            // 수집함에 데모 항목을 넣는다.
            let _ = this.update(cx, |this, cx| {
                if let Some(project) = this.current_or_first_project() {
                    for t in [
                        "로그인 페이지가 사파리에서 깨져요 - 고객 문의: iOS 17 사파리에서 로그인 버튼이 눌리지 않는다고 합니다.",
                        "다음 주 배포 전에 의존성 업데이트 - 보안 공지가 두 건 올라왔습니다.",
                        "결제 영수증 메일 문구 수정",
                    ] {
                        this.send(Action::AddMemo {
                            project: project.clone(),
                            text: t.into(),
                        });
                    }
                }
                this.send(Action::LoadIntake);
                cx.notify();
            });
            wait(1500).await;
            for (i, name) in STEPS.iter().enumerate() {
                let ok = this
                    .update_in(cx, |this, window, cx| {
                        this.modal = None;
                        let snap = this.snap();
                        let first = snap
                            .cards
                            .iter()
                            .find(|c| c.path.contains("safari"))
                            .or(snap.cards.first())
                            .map(|c| c.path.clone());
                        match i {
                            0 => {
                                this.nav = Nav::Board;
                                this.board_mode = BoardMode::List;
                                this.selected_card = None;
                            }
                            1 => this.board_mode = BoardMode::Kanban,
                            2 => {
                                this.board_mode = BoardMode::List;
                                if let Some(p) = first {
                                    this.card_tab = CardTab::Thread;
                                    this.open_card(p, cx);
                                }
                            }
                            3 => this.card_tab = CardTab::Task,
                            4 => {
                                this.card_tab = CardTab::Changes;
                                if let Some(p) = first {
                                    this.send(Action::LoadDiff(p));
                                }
                            }
                            5 => {
                                this.selected_card = None;
                                this.nav = Nav::Intake;
                            }
                            6 => {
                                this.nav = Nav::Board;
                                this.board_mode = BoardMode::Kanban;
                                if let Some(p) = first {
                                    this.card_tab = CardTab::Thread;
                                    this.open_card(p, cx);
                                }
                            }
                            7 => {
                                this.selected_card = None;
                                this.board_mode = BoardMode::List;
                                this.nav = Nav::Inbox;
                            }
                            8 => this.nav = Nav::Orchestrator,
                            9 => this.nav = Nav::Agents,
                            10 => this.nav = Nav::Connectors,
                            11 => {
                                this.nav = Nav::Board;
                                this.open_modal(Modal::NewCard, window, cx);
                            }
                            12 => this.open_modal(Modal::NewProject, window, cx),
                            13 => {
                                this.nav = Nav::Board;
                                if let Some(p) = first {
                                    this.card_tab = CardTab::Thread;
                                    this.open_card(p, cx);
                                }
                                this.composer.update(cx, |c, cx| {
                                    c.set_text(
                                        "첫 줄입니다.\n두 번째 줄은 조금 더 길게 써서 칸 너비를 넘기면 자동으로 줄이 바뀌는지 확인합니다. 한글과 English가 섞여도 괜찮은지 봅니다.\n세 번째 줄",
                                        cx,
                                    )
                                });
                                window.focus(&this.composer.focus_handle(cx));
                            }
                            _ => {
                                window.resize(size(px(980.), px(640.)));
                            }
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !ok {
                    break;
                }
                wait(1800).await;
                let path = dir.join(format!("{name}.png"));
                // 창 영역만 찍는다(화면 좌표, 포인트).
                let rect = this
                    .update_in(cx, |_, window, _| {
                        let b = window.bounds();
                        format!(
                            "{},{},{},{}",
                            f32::from(b.origin.x) as i32,
                            f32::from(b.origin.y) as i32,
                            f32::from(b.size.width) as i32,
                            f32::from(b.size.height) as i32
                        )
                    })
                    .unwrap_or_default();
                let mut cmd = std::process::Command::new("screencapture");
                cmd.args(["-x", "-o"]);
                if !rect.is_empty() {
                    cmd.arg(format!("-R{rect}"));
                }
                let _ = cmd.arg(&path).status();
                eprintln!("shot {}", path.display());
            }
            let _ = this.update(cx, |_, cx| cx.quit());
        })
        .detach();
    }

    fn current_or_first_project(&self) -> Option<String> {
        self.project
            .clone()
            .or_else(|| self.snap().projects.first().map(|p| p.slug.clone()))
    }

    fn add_memo(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(project) = self.current_or_first_project() else {
            self.toast("먼저 프로젝트를 만드세요", true);
            cx.notify();
            return;
        };
        self.send(Action::AddMemo { project, text });
        cx.notify();
    }

    fn send_reply(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(id) = self.reply_target.clone() else {
            return;
        };
        self.send(Action::Reply { id, text });
        self.reply_target = None;
        self.reply_input.update(cx, |i, cx| i.clear(cx));
        cx.notify();
    }

    fn save_slack(&mut self, cx: &mut Context<Self>) {
        let token = self.conn_token.read(cx).text();
        let team = self.conn_team.read(cx).text();
        let channel = self.conn_channel.read(cx).text();
        let Some(project) = self
            .conn_project
            .clone()
            .or_else(|| self.current_or_first_project())
        else {
            self.toast("먼저 프로젝트를 만드세요", true);
            cx.notify();
            return;
        };
        if token.trim().is_empty() || channel.trim().is_empty() {
            self.toast("봇 토큰과 채널 ID를 넣으세요", true);
            cx.notify();
            return;
        }
        let name = format!("slack-{}", channel.trim().to_lowercase());
        self.send(Action::SaveConnector {
            name,
            project,
            preset: "slack".into(),
            fields: serde_json::json!({"token": token.trim(), "team_id": team.trim(), "channel_id": channel.trim()}),
        });
        self.send(Action::LoadConnectors);
        self.conn_token.update(cx, |i, cx| i.clear(cx));
        self.toast(
            "연결을 저장하고 처음 동기화하는 중… (npx가 처음 받는 데 시간이 걸릴 수 있습니다)",
            false,
        );
        cx.notify();
    }

    /// 받은편지함에 새로 들어온 것을 데스크톱 알림으로 알린다.
    fn notify_new_inbox(&mut self) {
        let snap = self.snap();
        let items = vm::inbox(&snap);
        let Some(seen) = self.notified.as_mut() else {
            self.notified = Some(items.iter().map(|i| i.event().seq).collect());
            return;
        };
        for it in &items {
            let e = it.event();
            if !seen.insert(e.seq) {
                continue;
            }
            let card = vm::card_of_topic(&e.topic)
                .and_then(|p| snap.card_by_path(&p).map(|c| c.title.clone()))
                .unwrap_or_else(|| e.topic.clone());
            let (title, body) = match it {
                InboxItem::Permission(_) => ("권한 승인이 필요합니다", format::permission_line(e)),
                InboxItem::Question(_) => ("에이전트가 기다립니다", vm::thread_item(e).text),
            };
            notify_desktop(&format!("jadecode · {title}"), &format!("{card}: {body}"));
        }
    }

    fn open_card(&mut self, path: String, cx: &mut Context<Self>) {
        let topic = vm::card_topic(&path);
        self.send(Action::LoadCard(path.clone()));
        self.send(Action::LoadHistory {
            key: topic.clone(),
            topics: vec![topic],
            limit: 600,
        });
        if self.card_tab == CardTab::Changes {
            self.send(Action::LoadDiff(path.clone()));
        }
        self.selected_card = Some(path);
        self.last_thread_len = 0;
        self.thread_scroll.scroll_to_bottom();
        cx.notify();
    }

    fn set_project(&mut self, p: Option<String>, cx: &mut Context<Self>) {
        self.project = p.clone();
        if let Some(p) = p {
            let topic = format!("project/{p}");
            self.send(Action::LoadHistory {
                key: topic.clone(),
                topics: vec![topic],
                limit: 1000,
            });
        }
        cx.notify();
    }

    fn live_worker(&self, card: &str) -> Option<String> {
        self.snap()
            .instances_of(card)
            .iter()
            .find(|i| i.role != "orchestrator")
            .map(|i| i.id.clone())
    }

    fn send_from_composer(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(card) = self.selected_card.clone() else {
            return;
        };
        let snap = self.snap();
        let Some(c) = snap.card_by_path(&card) else {
            return;
        };
        match self.live_worker(&card) {
            Some(inst) => self.send(Action::SendInstance {
                instance: inst,
                text,
            }),
            None => {
                // 일하는 워커가 없으면 띄우고 그 말을 첫 지시로 보낸다.
                self.send(Action::StartWorker {
                    project: c.project.clone(),
                    card: card.clone(),
                    agent: self.agent_choice.clone(),
                    then_send: Some(text),
                });
                self.toast("워커를 시작하고 말을 전합니다", false);
            }
        }
        self.thread_scroll.scroll_to_bottom();
        cx.notify();
    }

    fn send_to_orchestrator(&mut self, text: String, cx: &mut Context<Self>) {
        let project = self
            .project
            .clone()
            .or_else(|| self.snap().projects.first().map(|p| p.slug.clone()));
        let Some(project) = project else {
            self.toast("먼저 프로젝트를 만드세요", true);
            cx.notify();
            return;
        };
        if self.project.is_none() {
            self.set_project(Some(project.clone()), cx);
        }
        self.send(Action::SendOrchestrator { project, text });
        self.thread_scroll.scroll_to_bottom();
        cx.notify();
    }

    fn open_modal(&mut self, m: Modal, window: &mut Window, cx: &mut Context<Self>) {
        if m == Modal::NewCard && self.snap().projects.is_empty() {
            self.modal = Some(Modal::NewProject);
        } else {
            self.modal = Some(m);
        }
        let target = match self.modal {
            Some(Modal::NewProject) => self.project_name.focus_handle(cx),
            _ => self.card_title.focus_handle(cx),
        };
        window.focus(&target);
        cx.notify();
    }

    fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = None;
        window.focus(&self.focus);
        cx.notify();
    }

    /// 레포 폴더 칸 + "폴더 고르기" 버튼.
    fn repo_row(&self, id: &'static str, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(self.project_repo.clone())
            .child(
                button(id, "폴더 고르기…", Btn::Ghost).on_click(
                    cx.listener(|this, _: &ClickEvent, _w, cx| this.pick_repo_folder(cx)),
                ),
            )
    }

    /// 운영체제 폴더 선택 창으로 레포 폴더를 고른다. 이름 칸이 비어 있으면 폴더 이름을 넣는다.
    fn pick_repo_folder(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("이 레포 사용".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(p) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let shown = tilde(&p.display().to_string());
                this.project_repo.update(cx, |i, cx| i.set_text(shown, cx));
                if this.project_name.read(cx).text().trim().is_empty() {
                    let leaf = p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    this.project_name.update(cx, |i, cx| i.set_text(leaf, cx));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn submit_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.project_name.read(cx).text();
        let repo = self.project_repo.read(cx).text();
        if name.trim().is_empty() {
            window.focus(&self.project_name.focus_handle(cx));
            return;
        }
        if !name.chars().any(|c| c.is_ascii_alphanumeric()) {
            self.toast(
                "프로젝트 이름에는 영문이나 숫자가 하나 이상 있어야 합니다",
                true,
            );
            cx.notify();
            return;
        }
        self.send(Action::AddProject {
            name: name.trim().to_string(),
            repo: repo.trim().to_string(),
        });
        self.project_name.update(cx, |i, cx| i.clear(cx));
        self.project_repo.update(cx, |i, cx| i.clear(cx));
        self.close_modal(window, cx);
    }

    fn submit_card(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.card_title.read(cx).text();
        let body = self.card_body.read(cx).text();
        if title.trim().is_empty() {
            window.focus(&self.card_title.focus_handle(cx));
            return;
        }
        let project = self
            .project
            .clone()
            .or_else(|| self.snap().projects.first().map(|p| p.slug.clone()));
        let Some(project) = project else {
            self.open_modal(Modal::NewProject, window, cx);
            return;
        };
        self.send(Action::AddCard {
            project,
            title: title.trim().to_string(),
            body: body.trim().to_string(),
            start: self.new_card_start,
            agent: self.agent_choice.clone(),
        });
        self.card_title.update(cx, |i, cx| i.clear(cx));
        self.card_body.update(cx, |i, cx| i.clear(cx));
        self.close_modal(window, cx);
    }

    // ---------- 사이드바 ----------

    fn nav_item(
        &self,
        id: &'static str,
        label: &str,
        active: bool,
        count: Option<(usize, u32)>,
        cx: &mut Context<Self>,
        on: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .mx_2()
            .px_2()
            .py(px(5.))
            .rounded_md()
            .text_sm()
            .bg(rgb(if active { SELECTED } else { SIDEBAR }))
            .text_color(rgb(if active { TEXT } else { MUTED }))
            .hover(|s| s.bg(rgb(HOVER)))
            .cursor_pointer()
            .child(div().flex_1().truncate().child(label.to_string()))
            .children(count.filter(|(n, _)| *n > 0).map(|(n, c)| badge(n, c)))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                on(this, window, cx);
                cx.notify();
            }))
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let snap = self.snap();
        let inbox_n = vm::inbox(&snap).len();
        let att = vm::attention_map(&snap);
        let att_of = |c: &CardData| att.get(&c.path).copied().unwrap_or(Attention::Idle);
        let connected = self.live.connected.load(Ordering::Relaxed);
        let mut col = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(236.))
            .h_full()
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .pt_4()
                    .pb_2()
                    .child(
                        div()
                            .text_color(rgb(TEXT))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("jadecode"),
                    )
                    .child(div().flex_1())
                    .child(button("new-card-top", "+ 새 카드", Btn::Primary).on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.open_modal(Modal::NewCard, window, cx)
                        }),
                    )),
            );
        let intake_n = vm::intake_attention(&self.intake);
        col = col
            .child(self.nav_item(
                "nav-intake",
                "수집함",
                self.nav == Nav::Intake,
                Some((intake_n, VIOLET)),
                cx,
                |t, window, cx| {
                    t.nav = Nav::Intake;
                    let _ = t.tx.send(Action::LoadIntake);
                    window.focus(&t.memo_input.focus_handle(cx));
                },
            ))
            .child(self.nav_item(
                "nav-inbox",
                "확인 요청",
                self.nav == Nav::Inbox,
                Some((inbox_n, AMBER)),
                cx,
                |t, _, _| t.nav = Nav::Inbox,
            ))
            .child(self.nav_item(
                "nav-orch",
                "오케스트레이터",
                self.nav == Nav::Orchestrator,
                None,
                cx,
                |t, window, cx| {
                    t.nav = Nav::Orchestrator;
                    if t.project.is_none() {
                        let first = t.snap().projects.first().map(|p| p.slug.clone());
                        t.set_project(first, cx);
                    } else {
                        t.set_project(t.project.clone(), cx);
                    }
                    window.focus(&t.orch_input.focus_handle(cx));
                },
            ))
            .child(
                self.nav_item(
                    "nav-board",
                    "보드",
                    self.nav == Nav::Board,
                    Some((
                        snap.cards
                            .iter()
                            .filter(|c| att_of(c) == Attention::Running)
                            .count(),
                        ACCENT,
                    )),
                    cx,
                    |t, _, _| t.nav = Nav::Board,
                ),
            )
            .child(self.nav_item(
                "nav-agents",
                "에이전트",
                self.nav == Nav::Agents,
                None,
                cx,
                |t, _, _| {
                    t.nav = Nav::Agents;
                    let _ = t.tx.send(Action::LoadAgents);
                },
            ))
            .child(self.nav_item(
                "nav-connectors",
                "연결",
                self.nav == Nav::Connectors,
                None,
                cx,
                |t, _, _| {
                    t.nav = Nav::Connectors;
                    let _ = t.tx.send(Action::LoadConnectors);
                },
            ));

        col = col.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .pr_3()
                .child(section_label("프로젝트"))
                .child(div().flex_1())
                .child(
                    div()
                        .id("new-project")
                        .px_2()
                        .rounded_md()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .hover(|s| s.bg(rgb(HOVER)).text_color(rgb(TEXT)))
                        .cursor_pointer()
                        .child("+")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.open_modal(Modal::NewProject, window, cx)
                        })),
                ),
        );
        let mut plist = div()
            .id("projects")
            .flex()
            .flex_col()
            .gap(px(1.))
            .overflow_y_scroll()
            .flex_1();
        let all_active = self.project.is_none();
        plist = plist.child(
            self.nav_item(
                "proj-all",
                "모든 프로젝트",
                all_active && self.nav == Nav::Board,
                Some((snap.cards.len(), RAISED)),
                cx,
                |t, _, cx| {
                    t.set_project(None, cx);
                    if t.nav != Nav::Orchestrator {
                        t.nav = Nav::Board;
                    }
                },
            )
            .text_color(rgb(if all_active { TEXT } else { MUTED })),
        );
        for (i, p) in snap.projects.iter().enumerate() {
            let slug = p.slug.clone();
            let active = self.project.as_deref() == Some(&p.slug);
            let n = snap.cards.iter().filter(|c| c.project == p.slug).count();
            let needs = snap
                .cards
                .iter()
                .filter(|c| c.project == p.slug && att_of(c) == Attention::NeedsYou)
                .count();
            let label = if p.name.is_empty() {
                p.slug.clone()
            } else {
                p.name.clone()
            };
            plist = plist.child(
                div()
                    .id(("proj", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .mx_2()
                    .px_2()
                    .py(px(5.))
                    .rounded_md()
                    .text_sm()
                    .bg(rgb(if active { SELECTED } else { SIDEBAR }))
                    .text_color(rgb(if active { TEXT } else { MUTED }))
                    .hover(|s| s.bg(rgb(HOVER)))
                    .cursor_pointer()
                    .child(div().flex_none().text_color(rgb(FAINT)).child("#"))
                    .child(div().flex_1().truncate().child(label))
                    .children((needs > 0).then(|| badge(needs, AMBER)))
                    .child(div().text_xs().text_color(rgb(FAINT)).child(n.to_string()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.set_project(Some(slug.clone()), cx);
                        if this.nav != Nav::Orchestrator {
                            this.nav = Nav::Board;
                        }
                        cx.notify();
                    })),
            );
        }
        if snap.projects.is_empty() {
            plist = plist.child(
                div()
                    .px_4()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(FAINT))
                    .child("아직 프로젝트가 없습니다"),
            );
        }
        col = col.child(plist);

        // 새 판 알림
        if let Some(tag) = self.update_available.clone() {
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .mx_3()
                    .mb_2()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(0x1d2a47))
                    .border_1()
                    .border_color(rgb(ACCENT))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(TEXT))
                            .child(format!("새 판 {tag}")),
                    )
                    .child(
                        button(
                            "do-update",
                            if self.updating {
                                "설치하는 중…"
                            } else {
                                "업데이트 설치"
                            },
                            Btn::Primary,
                        )
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, _w, cx| {
                                if !this.updating {
                                    this.updating = true;
                                    this.send(Action::RunUpdate);
                                }
                                cx.notify();
                            },
                        )),
                    ),
            );
        }
        // 아래: 연결 상태
        col.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .px_4()
                .py_3()
                .border_t_1()
                .border_color(rgb(LINE))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(dot(if !connected {
                            RED
                        } else if self.backend_embedded {
                            VIOLET
                        } else {
                            GREEN
                        }))
                        .child(if connected {
                            "연결됨".to_string()
                        } else {
                            "다시 연결하는 중…".to_string()
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(FAINT))
                        .truncate()
                        .child(self.backend_label.clone()),
                )
                .child(
                    div()
                        .id("open-ws")
                        .text_xs()
                        .text_color(rgb(FAINT))
                        .hover(|s| s.text_color(rgb(TEXT)))
                        .cursor_pointer()
                        .child("워크스페이스 폴더 열기")
                        .on_click(cx.listener(|this, _: &ClickEvent, _w, _cx| {
                            this.send(Action::OpenFolder {
                                card: None,
                                terminal: false,
                            })
                        })),
                ),
        )
    }

    // ---------- 보드 ----------

    fn board_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let snap = self.snap();
        let title = match &self.project {
            Some(p) => snap
                .projects
                .iter()
                .find(|x| &x.slug == p)
                .map(|x| {
                    if x.name.is_empty() {
                        x.slug.clone()
                    } else {
                        x.name.clone()
                    }
                })
                .unwrap_or_else(|| p.clone()),
            None => "모든 프로젝트".to_string(),
        };
        let mode = self.board_mode;
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_4()
            .h(px(48.))
            .flex_none()
            .border_b_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .text_color(rgb(TEXT))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .p(px(2.))
                    .gap(px(2.))
                    .rounded_md()
                    .bg(rgb(PANEL))
                    .child(chip("mode-list", "목록", mode == BoardMode::List).on_click(
                        cx.listener(|this, _: &ClickEvent, _w, cx| {
                            this.board_mode = BoardMode::List;
                            cx.notify();
                        }),
                    ))
                    .child(
                        chip("mode-kanban", "칸반", mode == BoardMode::Kanban).on_click(
                            cx.listener(|this, _: &ClickEvent, _w, cx| {
                                this.board_mode = BoardMode::Kanban;
                                cx.notify();
                            }),
                        ),
                    ),
            )
    }

    fn card_row(
        &self,
        snap: &BoardSnapshot,
        c: &CardData,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selected = self.selected_card.as_deref() == Some(&c.path);
        let insts = snap.instances_of(&c.path);
        let perms = vm::pending_permissions_of(snap, &c.path);
        let inst_text = insts
            .first()
            .map(|i| {
                format!(
                    "{} · {}",
                    format::agent_label(&i.agent),
                    format::status_label(&i.state)
                )
            })
            .unwrap_or_default();
        let path = c.path.clone();
        let id_key: SharedString = format!("card-{}", c.path).into();
        div()
            .id(ElementId::Name(id_key))
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_4()
            .py_2()
            .bg(rgb(if selected { SELECTED } else { BG }))
            .hover(|s| s.bg(rgb(HOVER)))
            .cursor_pointer()
            .child(dot(status_color(&c.status)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(TEXT))
                            .truncate()
                            .child(c.title.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(FAINT))
                            .truncate()
                            .child(format!(
                                "{} · {} · {}{}",
                                c.project,
                                format::stage_label(&c.stage),
                                format::status_label(&c.status),
                                if inst_text.is_empty() {
                                    String::new()
                                } else {
                                    format!(" · {inst_text}")
                                }
                            )),
                    ),
            )
            .children((perms > 0).then(|| badge(perms, AMBER)))
            .children((!insts.is_empty()).then(|| dot(instance_color(&insts[0].state))))
            .on_click(
                cx.listener(move |this, _: &ClickEvent, _w, cx| this.open_card(path.clone(), cx)),
            )
    }

    fn list_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let snap = self.snap();
        let groups = vm::group_cards(&snap, self.project.as_deref());
        let mut list = div()
            .id("card-list")
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .size_full()
            .pb_4();
        for (att, cards) in groups {
            list = list.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .px_4()
                    .pt_4()
                    .pb_1()
                    .text_xs()
                    .text_color(rgb(match att {
                        Attention::NeedsYou => AMBER,
                        Attention::Running => ACCENT,
                        _ => FAINT,
                    }))
                    .child(att.label())
                    .child(div().text_color(rgb(FAINT)).child(cards.len().to_string())),
            );
            for c in &cards {
                list = list.child(self.card_row(&snap, c, cx));
            }
        }
        list.into_any_element()
    }

    fn kanban_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let snap = self.snap();
        let stages = vm::ordered_stages(&snap, self.project.as_deref());
        let mut cols = div().flex().flex_row().gap_3().flex_1().min_h(px(0.));
        for (si, stage) in stages.iter().enumerate() {
            let cards: Vec<CardData> = snap
                .stage_cards(self.project.as_deref(), stage)
                .into_iter()
                .cloned()
                .collect();
            let target = stage.clone();
            let mut col = div()
                .id(("col", si))
                .flex()
                .flex_col()
                .flex_none()
                .gap_2()
                .w(px(272.))
                .h_full()
                .p_2()
                .rounded_lg()
                .bg(rgb(PANEL))
                .border_1()
                .border_color(rgb(PANEL))
                .overflow_y_scroll()
                .drag_over::<DragCard>(|st, _, _, _| st.border_color(rgb(ACCENT)).bg(rgb(0x18203a)))
                .on_drop(cx.listener(move |this, d: &DragCard, _w, cx| {
                    if d.stage != target {
                        this.send(Action::MoveStage {
                            card: d.path.clone(),
                            stage: target.clone(),
                        });
                    }
                    cx.notify();
                }))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .px_1()
                        .pb_1()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(format::stage_label(stage))
                        .child(cards.len().to_string()),
                );
            for c in cards {
                let selected = self.selected_card.as_deref() == Some(&c.path);
                let insts = snap.instances_of(&c.path);
                let perms = vm::pending_permissions_of(&snap, &c.path);
                let path = c.path.clone();
                let key: SharedString = format!("tile-{}", c.path).into();
                let drag = DragCard {
                    path: c.path.clone(),
                    title: c.title.clone(),
                    stage: c.stage.clone(),
                };
                col = col.child(
                    div()
                        .id(ElementId::Name(key))
                        .on_drag(drag, |d, _offset, _w, cx| {
                            let t = d.title.clone();
                            cx.new(|_| DragPreview(t))
                        })
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_2()
                        .rounded_md()
                        .bg(rgb(if selected { SELECTED } else { RAISED }))
                        .border_1()
                        .border_color(rgb(if selected { ACCENT } else { LINE }))
                        .hover(|s| s.bg(rgb(HOVER)))
                        .cursor_pointer()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .text_color(rgb(status_color(&c.status)))
                                .child(dot(status_color(&c.status)))
                                .child(format::status_label(&c.status))
                                .child(div().flex_1())
                                .children((perms > 0).then(|| badge(perms, AMBER))),
                        )
                        .child(div().text_sm().text_color(rgb(TEXT)).child(c.title.clone()))
                        .child(
                            div().text_xs().text_color(rgb(FAINT)).child(
                                insts
                                    .first()
                                    .map(|i| {
                                        format!(
                                            "{} · {}",
                                            format::agent_label(&i.agent),
                                            format::status_label(&i.state)
                                        )
                                    })
                                    .unwrap_or_else(|| c.project.clone()),
                            ),
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                            this.open_card(path.clone(), cx)
                        })),
                );
            }
            cols = cols.child(col);
        }
        let project = self.project.as_deref();
        let pick = |state: &str| -> Vec<IntakeItem> {
            self.intake
                .iter()
                .filter(|i| i.state == state && project.is_none_or(|p| i.project == p))
                .cloned()
                .collect()
        };
        let zone = |id: &'static str, label: &'static str, color: u32, body: AnyElement| {
            div()
                .id(id)
                .flex()
                .flex_col()
                .flex_none()
                .gap_2()
                .h_full()
                .child(div().px_1().text_xs().text_color(rgb(color)).child(label))
                .child(body)
        };
        div()
            .id("kanban")
            .flex()
            .flex_row()
            .gap_4()
            .p_4()
            .size_full()
            .overflow_x_scroll()
            .child(zone(
                "zone-in",
                "바깥 사이클 · 들어온 일",
                VIOLET,
                self.intake_column("kc-new", "수집", VIOLET, pick("new"), cx),
            ))
            .child(zone(
                "zone-code",
                "안쪽 사이클 · 코드",
                ACCENT,
                cols.into_any_element(),
            ))
            .child(zone(
                "zone-out",
                "바깥 사이클 · 돌려줄 일",
                AMBER,
                self.intake_column("kc-reply", "회신 대기", AMBER, pick("reply_pending"), cx),
            ))
            .into_any_element()
    }

    /// 칸반 바깥 사이클 열: 수집함 항목 타일. 누르면 수집함으로 간다.
    fn intake_column(
        &self,
        id: &'static str,
        label: &'static str,
        color: u32,
        items: Vec<IntakeItem>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut col = div()
            .id(id)
            .flex()
            .flex_col()
            .flex_1()
            .gap_2()
            .w(px(232.))
            .p_2()
            .rounded_lg()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(LINE))
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .px_1()
                    .pb_1()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(label)
                    .child(items.len().to_string()),
            );
        if items.is_empty() {
            col = col.child(
                div()
                    .px_1()
                    .text_xs()
                    .text_color(rgb(FAINT))
                    .child("비어 있음"),
            );
        }
        for (i, it) in items.iter().enumerate() {
            let src = if it.connector.is_empty() {
                it.source.clone()
            } else {
                it.connector.clone()
            };
            let card = it.card.clone();
            col = col.child(
                div()
                    .id((id, i))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(RAISED))
                    .border_l_2()
                    .border_color(rgb(color))
                    .hover(|s| s.bg(rgb(HOVER)))
                    .cursor_pointer()
                    .child(div().text_xs().text_color(rgb(FAINT)).truncate().child(
                        if it.from.is_empty() {
                            src
                        } else {
                            format!("{src} · {}", it.from)
                        },
                    ))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(TEXT))
                            .child(it.title.clone()),
                    )
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _w, cx| match &card {
                            // 회신 대기 항목은 끝난 카드를 먼저 보여 준다.
                            Some(c) => this.open_card(c.clone(), cx),
                            None => {
                                this.nav = Nav::Intake;
                                cx.notify();
                            }
                        }),
                    ),
            );
        }
        col.into_any_element()
    }

    fn welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        let claude = self.agents.iter().find(|a| a.name == "claude");
        let any_agent = self.agents.iter().any(|a| a.installed);
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .w(px(520.))
                    .p_6()
                    .rounded_lg()
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgb(LINE))
                    .child(div().text_lg().text_color(rgb(TEXT)).child("jadecode에 오신 것을 환영합니다"))
                    .child(div().text_sm().text_color(rgb(MUTED)).child(
                        "일은 카드 단위로 진행됩니다. 카드를 만들면 에이전트가 그 카드 전용 작업 공간에서 일하고, 승인이나 답이 필요할 때만 \"확인 요청\"으로 돌아옵니다. 메신저·메모로 들어온 일은 \"수집함\"에 모입니다.",
                    ))
                    .child(div().text_sm().text_color(rgb(TEXT)).pt_2().child("1. 프로젝트 이름"))
                    .child(self.project_name.clone())
                    .child(div().text_sm().text_color(rgb(TEXT)).child("2. 코드 레포 폴더 (선택)"))
                    .child(self.repo_row("welcome-pick", cx))
                    .child(div().text_xs().text_color(rgb(FAINT)).child(
                        "카드마다 이 레포의 git 워크트리를 따로 열어 워커가 그 안에서 일합니다. 원본 작업 폴더는 건드리지 않습니다.",
                    ))
                    .child(
                        div().flex().flex_row().gap_2().pt_1().child(
                            button("welcome-create", "프로젝트 만들기", Btn::Primary).on_click(cx.listener(
                                |this, _: &ClickEvent, window, cx| this.submit_project(window, cx),
                            )),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .pt_3()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(dot(if any_agent { GREEN } else { AMBER }))
                            .child(match (claude, any_agent) {
                                (Some(c), _) if c.installed => format!("Claude Code 확인됨 {}", c.version),
                                (_, true) => "에이전트 확인됨".to_string(),
                                _ if self.agents.is_empty() => "에이전트를 확인하는 중…".to_string(),
                                _ => "설치된 에이전트가 없습니다. Claude Code를 설치하고 로그인하세요 (npm i -g @anthropic-ai/claude-code).".to_string(),
                            }),
                    ),
            )
            .into_any_element()
    }

    fn board_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let snap = self.snap();
        if snap.projects.is_empty() && snap.cards.is_empty() {
            return self.welcome(cx);
        }
        let has_cards = snap.cards.iter().any(|c| {
            self.project
                .as_deref()
                .map(|p| c.project == p)
                .unwrap_or(true)
        });
        let body: AnyElement = if !has_cards {
            div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .size_full()
                .child(div().text_color(rgb(TEXT)).child("카드가 없습니다"))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .child("새 카드를 만들거나 오케스트레이터에게 할 일을 말하세요."),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(button("empty-new-card", "새 카드", Btn::Primary).on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.open_modal(Modal::NewCard, window, cx)
                            }),
                        ))
                        .child(
                            button("empty-orch", "오케스트레이터에게 말하기", Btn::Ghost).on_click(
                                cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.nav = Nav::Orchestrator;
                                    window.focus(&this.orch_input.focus_handle(cx));
                                    cx.notify();
                                }),
                            ),
                        ),
                )
                .into_any_element()
        } else {
            match self.board_mode {
                BoardMode::List => self.list_view(cx),
                BoardMode::Kanban => self.kanban_view(cx),
            }
        };
        let show_card = self
            .selected_card
            .as_ref()
            .map(|p| snap.card_by_path(p).is_some())
            .unwrap_or(false);
        let left_w = if self.board_mode == BoardMode::List {
            px(380.)
        } else {
            px(0.)
        };
        let mut row = div().flex().flex_row().flex_1().min_h(px(0.));
        if show_card {
            if self.board_mode == BoardMode::List {
                row = row
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .w(left_w)
                            .h_full()
                            .border_r_1()
                            .border_color(rgb(LINE))
                            .child(self.board_toolbar(cx))
                            .child(div().flex_1().min_h(px(0.)).child(body)),
                    )
                    .child(self.card_pane(cx));
            } else {
                row = row
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .h_full()
                            .child(self.board_toolbar(cx))
                            .child(div().flex_1().min_h(px(0.)).child(body)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(560.))
                            .h_full()
                            .border_l_1()
                            .border_color(rgb(LINE))
                            .child(self.card_pane(cx)),
                    );
            }
        } else {
            row = row.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .h_full()
                    .child(self.board_toolbar(cx))
                    .child(div().flex_1().min_h(px(0.)).child(body)),
            );
        }
        row.into_any_element()
    }

    // ---------- 카드 스레드 ----------

    fn card_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        let snap = self.snap();
        let Some(path) = self.selected_card.clone() else {
            return div().into_any_element();
        };
        let Some(card) = snap.card_by_path(&path).cloned() else {
            return empty_state("카드를 찾을 수 없습니다", &path).into_any_element();
        };
        let detail = self.details.get(&path);
        let insts: Vec<_> = snap.instances_of(&path).into_iter().cloned().collect();
        let stages: Vec<String> = detail
            .map(|d| d.stages.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| vm::DEFAULT_STAGES.iter().map(|s| s.to_string()).collect());
        let cur = stages.iter().position(|s| s == &card.stage);

        // 머리
        let mut head = div()
            .flex()
            .flex_col()
            .gap_2()
            .px_5()
            .pt_4()
            .pb_3()
            .border_b_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_lg()
                            .text_color(rgb(TEXT))
                            .child(card.title.clone()),
                    )
                    .child(
                        div()
                            .id("close-card")
                            .px_2()
                            .rounded_md()
                            .text_color(rgb(FAINT))
                            .hover(|s| s.bg(rgb(HOVER)).text_color(rgb(TEXT)))
                            .cursor_pointer()
                            .child("✕")
                            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                                this.selected_card = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(dot(status_color(&card.status)))
                    .child(format::status_label(&card.status))
                    .child("·")
                    .child(card.project.clone())
                    .children(
                        detail
                            .map(|d| d.repos.clone())
                            .unwrap_or_default()
                            .into_iter()
                            .flat_map(|r| {
                                [
                                    div().child("·"),
                                    div().font_family(MONO).text_color(rgb(TEXT)).child(r),
                                ]
                            }),
                    ),
            );

        // 단계 진행
        let mut prog = div().flex().flex_row().flex_wrap().items_center().gap_1();
        for (i, st) in stages.iter().enumerate() {
            let (bg, fg) = match cur {
                Some(c) if i < c => (0x1c3a2c, GREEN),
                Some(c) if i == c => (ACCENT, WHITE),
                _ => (PANEL, FAINT),
            };
            prog = prog.child(
                div()
                    .px_2()
                    .py(px(2.))
                    .rounded_md()
                    .text_xs()
                    .bg(rgb(bg))
                    .text_color(rgb(fg))
                    .child(format::stage_label(st)),
            );
            if i + 1 < stages.len() {
                prog = prog.child(div().text_xs().text_color(rgb(LINE)).child("›"));
            }
        }
        head = head.child(prog);

        // 동작
        let project = card.project.clone();
        let mut actions_row = div().flex().flex_row().flex_wrap().items_center().gap_2();
        if insts.iter().all(|i| i.role == "orchestrator") {
            let (p1, c1) = (project.clone(), path.clone());
            actions_row =
                actions_row.child(button("start-worker", "워커 시작", Btn::Primary).on_click(
                    cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.send(Action::StartWorker {
                            project: p1.clone(),
                            card: c1.clone(),
                            agent: this.agent_choice.clone(),
                            then_send: None,
                        });
                        cx.notify();
                    }),
                ));
            // 에이전트 고르기
            for (ai, a) in self.agents.iter().filter(|a| a.installed).enumerate() {
                let name = a.name.clone();
                let on = self.agent_choice.as_deref() == Some(&a.name);
                actions_row = actions_row.child(
                    chip(("agent-pick", ai), format::agent_label(&a.name), on).on_click(
                        cx.listener(move |this, _: &ClickEvent, _w, cx| {
                            this.agent_choice = if this.agent_choice.as_deref() == Some(&name) {
                                None
                            } else {
                                Some(name.clone())
                            };
                            cx.notify();
                        }),
                    ),
                );
            }
        } else {
            for (ii, i) in insts
                .iter()
                .filter(|i| i.role != "orchestrator")
                .enumerate()
            {
                let id = i.id.clone();
                actions_row = actions_row
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(PANEL))
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(dot(instance_color(&i.state)))
                            .child(format!(
                                "{} · {} · {}턴",
                                format::agent_label(&i.agent),
                                format::status_label(&i.state),
                                i.turns
                            )),
                    )
                    .child(
                        button(("stop", ii), "멈추기", Btn::Danger).on_click(cx.listener(
                            move |this, _: &ClickEvent, _w, cx| {
                                this.send(Action::StopInstance(id.clone()));
                                cx.notify();
                            },
                        )),
                    );
            }
        }
        let c2 = path.clone();
        let c3 = path.clone();
        let c4 = path.clone();
        let c5 = path.clone();
        actions_row = actions_row
            .child(div().flex_1())
            .child(
                button("advance", "다음 단계", Btn::Ghost).on_click(cx.listener(
                    move |this, _: &ClickEvent, _w, cx| {
                        this.send(Action::Advance(c2.clone()));
                        cx.notify();
                    },
                )),
            )
            .children((card.status != "done").then(|| {
                button("mark-done", "끝내기", Btn::Ok).on_click(cx.listener(
                    move |this, _: &ClickEvent, _w, cx| {
                        this.send(Action::SetStatus {
                            card: c3.clone(),
                            status: "done".into(),
                        });
                        cx.notify();
                    },
                ))
            }));
        head = head.child(actions_row);

        // 탭
        let tab = self.card_tab;
        let mut tabs = div()
            .flex()
            .flex_row()
            .items_end()
            .gap_1()
            .px_5()
            .pt_2()
            .border_b_1()
            .border_color(rgb(LINE));
        for (t, label, key) in [
            (CardTab::Thread, "대화", "tab-thread"),
            (CardTab::Task, "task.md", "tab-task"),
            (CardTab::Changes, "변경사항", "tab-changes"),
        ] {
            let active = tab == t;
            let p = path.clone();
            tabs = tabs.child(
                div()
                    .id(key)
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(rgb(if active { TEXT } else { MUTED }))
                    .border_b_2()
                    .border_color(rgb(if active { ACCENT } else { BG }))
                    .cursor_pointer()
                    .hover(|s| s.text_color(rgb(TEXT)))
                    .child(label)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.card_tab = t;
                        match t {
                            CardTab::Changes => this.send(Action::LoadDiff(p.clone())),
                            CardTab::Task => this.send(Action::LoadCard(p.clone())),
                            CardTab::Thread => this.thread_scroll.scroll_to_bottom(),
                        }
                        cx.notify();
                    })),
            );
        }

        // 보조 동작: 탭 줄 오른쪽에 작게 둔다.
        let tools = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .pb_1()
            .child(link_btn("open-folder", "폴더").on_click(cx.listener(
                move |this, _: &ClickEvent, _w, cx| {
                    this.send(Action::OpenFolder {
                        card: Some(c4.clone()),
                        terminal: false,
                    });
                    cx.notify();
                },
            )))
            .child(link_btn("open-term", "터미널").on_click(cx.listener(
                move |this, _: &ClickEvent, _w, cx| {
                    this.send(Action::OpenFolder {
                        card: Some(c5.clone()),
                        terminal: true,
                    });
                    cx.notify();
                },
            )))
            .child(link_btn("make-pr", "PR 만들기").on_click(cx.listener(
                move |this, _: &ClickEvent, _w, cx| {
                    this.card_tab = CardTab::Thread;
                    this.send_from_composer(PR_PROMPT.to_string(), cx);
                },
            )));
        tabs = tabs.child(div().flex_1()).child(tools);

        let body: AnyElement = match tab {
            CardTab::Thread => {
                let topic = vm::card_topic(&path);
                let hist = self.histories.get(&topic).cloned().unwrap_or_default();
                let events = vm::thread(&hist, &snap.recent, &topic, false);
                self.thread_body(&snap, events, "thread", cx)
            }
            CardTab::Task => self.markdown_body(detail.map(|d| d.markdown.as_str())),
            CardTab::Changes => self.diff_body(&path, cx),
        };

        let composer: Option<AnyElement> = (tab == CardTab::Thread).then(|| {
            let has_worker = insts.iter().any(|i| i.role != "orchestrator");
            let placeholder = if has_worker {
                "워커에게 말하기 · Enter로 보내기"
            } else {
                "말을 보내면 워커가 떠서 이 카드에서 일합니다 · Enter"
            };
            self.composer
                .update(cx, |i, cx| i.set_placeholder(placeholder, cx));
            self.composer_row(self.composer.clone(), "send-card", cx, |this, cx| {
                let text = this.composer.read(cx).text();
                if !text.trim().is_empty() {
                    this.composer.update(cx, |i, cx| i.clear(cx));
                    this.send_from_composer(text, cx);
                }
            })
        });

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .bg(rgb(BG))
            .child(head)
            .child(tabs)
            .child(div().flex_1().min_h(px(0.)).child(body))
            .children(composer)
            .into_any_element()
    }

    fn composer_row(
        &self,
        input: Entity<TextInput>,
        id: &'static str,
        cx: &mut Context<Self>,
        on_send: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        div()
            .flex()
            .flex_row()
            .items_end()
            .gap_2()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(rgb(LINE))
            .child(input)
            .child(button(id, "보내기", Btn::Primary).on_click(cx.listener(
                move |this, _: &ClickEvent, _w, cx| {
                    on_send(this, cx);
                    cx.notify();
                },
            )))
            .into_any_element()
    }

    fn thread_body(
        &self,
        snap: &BoardSnapshot,
        events: Vec<EventRow>,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pending: std::collections::HashSet<u64> =
            snap.pending_permissions.iter().map(|p| p.seq).collect();
        let now = now_ms();
        let mut col = div()
            .id(id)
            .flex()
            .flex_col()
            .gap_2()
            .px_5()
            .py_4()
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.thread_scroll);
        if events.is_empty() {
            col = col.child(
                div()
                    .text_sm()
                    .text_color(rgb(FAINT))
                    .child(if id == "thread" {
                        "아직 대화가 없습니다. 아래 칸에 할 일을 적어 보내면 워커가 이 카드의 작업 공간에서 일을 시작합니다."
                    } else {
                        "아직 대화가 없습니다. 예: \"로그인 버그 카드 만들고 워커 붙여줘\", \"지금 막힌 카드 정리해줘\"처럼 말해 보세요."
                    }),
            );
        }
        // 도구 호출이 길게 이어지면 묶어서 보인다.
        let mut tool_run: Vec<vm::ThreadItem> = vec![];
        let flush_tools = |col: Stateful<Div>, run: &mut Vec<vm::ThreadItem>| -> Stateful<Div> {
            if run.is_empty() {
                return col;
            }
            let mut box_ = div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .pl_3()
                .border_l_2()
                .border_color(rgb(LINE));
            let n = run.len();
            let skip = n.saturating_sub(6);
            if skip > 0 {
                box_ = box_.child(
                    div()
                        .text_xs()
                        .text_color(rgb(FAINT))
                        .child(format!("도구 호출 {skip}개 더…")),
                );
            }
            for t in run.drain(..).skip(skip) {
                box_ = box_.child(
                    div()
                        .text_xs()
                        .font_family(MONO)
                        .text_color(rgb(MUTED))
                        .truncate()
                        .child(format!("› {}", t.text)),
                );
            }
            col.child(box_)
        };
        for e in &events {
            let it = vm::thread_item(e);
            if it.bubble == Bubble::Tool {
                tool_run.push(it);
                continue;
            }
            col = flush_tools(col, &mut tool_run);
            let when = format::ago(now, e.ts);
            col = match it.bubble {
                Bubble::Human => col.child(
                    div().flex().flex_row().child(div().flex_1()).child(
                        div()
                            .max_w(relative(0.8))
                            .px_3()
                            .py_2()
                            .rounded_lg()
                            .bg(rgb(0x22304d))
                            .text_sm()
                            .text_color(rgb(TEXT))
                            .child(it.text),
                    ),
                ),
                Bubble::Agent => col.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .text_xs()
                                .text_color(rgb(FAINT))
                                .child(if it.head.is_empty() {
                                    "에이전트".to_string()
                                } else {
                                    it.head.clone()
                                })
                                .child(when),
                        )
                        .child(div().text_sm().text_color(rgb(TEXT)).child(it.text)),
                ),
                Bubble::System => col.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .text_xs()
                        .text_color(rgb(tone_color(it.tone)))
                        .child(div().text_color(rgb(FAINT)).child("—"))
                        .child(it.text),
                ),
                Bubble::Notice | Bubble::Tool => {
                    let is_pending = e.kind == "permission.requested" && pending.contains(&e.seq);
                    let seq = e.seq;
                    let mut card = div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(PANEL))
                        .border_l_2()
                        .border_color(rgb(tone_color(it.tone)))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .text_xs()
                                .text_color(rgb(tone_color(it.tone)))
                                .child(it.head.clone())
                                .child(div().text_color(rgb(FAINT)).child(when)),
                        )
                        .child(div().text_sm().text_color(rgb(TEXT)).child(it.text));
                    if is_pending {
                        card = card.child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .pt_1()
                                .child(button(("allow", seq as usize), "승인", Btn::Ok).on_click(
                                    cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                        this.send(Action::ResolvePermission { seq, allow: true });
                                        cx.notify();
                                    }),
                                ))
                                .child(
                                    button(("deny", seq as usize), "거절", Btn::Danger).on_click(
                                        cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                            this.send(Action::ResolvePermission {
                                                seq,
                                                allow: false,
                                            });
                                            cx.notify();
                                        }),
                                    ),
                                ),
                        );
                    }
                    col.child(card)
                }
            };
        }
        col = flush_tools(col, &mut tool_run);
        col.into_any_element()
    }

    fn markdown_body(&self, md: Option<&str>) -> AnyElement {
        let Some(md) = md else {
            return div()
                .p_5()
                .text_sm()
                .text_color(rgb(FAINT))
                .child("불러오는 중…")
                .into_any_element();
        };
        let mut col = div()
            .id("md")
            .flex()
            .flex_col()
            .gap_2()
            .px_6()
            .py_4()
            .size_full()
            .overflow_y_scroll();
        for b in vm::markdown_blocks(md) {
            col = col.child(match b {
                MdBlock::Heading(l, t) => {
                    let d = div()
                        .text_color(rgb(TEXT))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .pt_2();
                    match l {
                        1 => d.text_xl(),
                        2 => d.text_lg(),
                        _ => d.text_base(),
                    }
                    .child(vm::strip_inline(&t))
                }
                MdBlock::Para(t) => div()
                    .text_sm()
                    .text_color(rgb(TEXT))
                    .child(vm::strip_inline(&t)),
                MdBlock::Bullet(ind, t) => div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .pl(px(12. * ind as f32))
                    .text_sm()
                    .text_color(rgb(TEXT))
                    .child(div().text_color(rgb(FAINT)).child("•"))
                    .child(div().flex_1().child(vm::strip_inline(&t))),
                MdBlock::Check(done, t) => div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .text_sm()
                    .text_color(rgb(if done { MUTED } else { TEXT }))
                    .child(
                        div()
                            .text_color(rgb(if done { GREEN } else { FAINT }))
                            .child(if done { "☑" } else { "☐" }),
                    )
                    .child(div().flex_1().child(vm::strip_inline(&t))),
                MdBlock::Code(lines) => {
                    let mut c = div()
                        .flex()
                        .flex_col()
                        .p_3()
                        .rounded_md()
                        .bg(rgb(PANEL))
                        .font_family(MONO)
                        .text_xs()
                        .text_color(rgb(TEXT));
                    for l in lines {
                        c = c.child(div().whitespace_nowrap().child(if l.is_empty() {
                            " ".to_string()
                        } else {
                            l
                        }));
                    }
                    c
                }
                MdBlock::Quote(t) => div()
                    .pl_3()
                    .border_l_2()
                    .border_color(rgb(LINE))
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child(t),
                MdBlock::Rule => div().h(px(1.)).bg(rgb(LINE)),
            });
        }
        col.into_any_element()
    }

    fn diff_body(&self, card: &str, cx: &mut Context<Self>) -> AnyElement {
        let Some(text) = self.diffs.get(card) else {
            return div()
                .p_5()
                .text_sm()
                .text_color(rgb(FAINT))
                .child("변경사항을 읽는 중…")
                .into_any_element();
        };
        let lines = vm::diff_lines(text);
        let (files, add, del) = vm::diff_stat(&lines);
        let c = card.to_string();
        let c2 = card.to_string();
        let mut col = div().flex().flex_col().size_full().child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .px_5()
                .py_2()
                .border_b_1()
                .border_color(rgb(LINE))
                .text_xs()
                .text_color(rgb(MUTED))
                .child(format!("파일 {files}개"))
                .child(div().text_color(rgb(GREEN)).child(format!("+{add}")))
                .child(div().text_color(rgb(RED)).child(format!("−{del}")))
                .child(div().flex_1())
                .child(
                    button("diff-term", "터미널", Btn::Subtle).on_click(cx.listener(
                        move |this, _: &ClickEvent, _w, _cx| {
                            this.send(Action::OpenFolder {
                                card: Some(c2.clone()),
                                terminal: true,
                            })
                        },
                    )),
                )
                .child(
                    button("diff-refresh", "새로고침", Btn::Subtle).on_click(cx.listener(
                        move |this, _: &ClickEvent, _w, _cx| this.send(Action::LoadDiff(c.clone())),
                    )),
                ),
        );
        if lines.is_empty() {
            return col
                .child(empty_state(
                    "바뀐 것이 없습니다",
                    "카드 폴더 안의 git 워크트리에서 바뀐 파일이 여기에 보입니다.",
                ))
                .into_any_element();
        }
        let mut body = div()
            .id("diff")
            .flex()
            .flex_col()
            .px_2()
            .py_2()
            .flex_1()
            .overflow_y_scroll()
            .font_family(MONO)
            .text_xs();
        for (k, l) in lines {
            let (fg, bg) = match k {
                DiffKind::File => (TEXT, RAISED),
                DiffKind::Hunk => (VIOLET, BG),
                DiffKind::Add => (GREEN, 0x13251c),
                DiffKind::Del => (RED, 0x2a1517),
                DiffKind::Meta => (FAINT, BG),
                DiffKind::Context => (MUTED, BG),
            };
            body = body.child(
                div()
                    .px_2()
                    .bg(rgb(bg))
                    .text_color(rgb(fg))
                    .whitespace_nowrap()
                    .when(k == DiffKind::File, |d| d.mt_2().py_1())
                    .child(if l.is_empty() { " ".to_string() } else { l }),
            );
        }
        col = col.child(body);
        col.into_any_element()
    }

    // ---------- 받은편지함 ----------

    fn inbox_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let snap = self.snap();
        let items = vm::inbox(&snap);
        let mut col = div()
            .id("inbox")
            .flex()
            .flex_col()
            .gap_2()
            .p_5()
            .size_full()
            .overflow_y_scroll()
            .child(div().text_lg().text_color(rgb(TEXT)).child("확인 요청"))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .pb_2()
                    .child("에이전트가 승인이나 답을 기다리는 일만 모입니다."),
            );
        if items.is_empty() {
            col = col.child(empty_state(
                "모두 처리했습니다",
                "에이전트가 막히거나 질문하면 여기에 나타납니다.",
            ));
        }
        for (n, it) in items.iter().enumerate() {
            let e = it.event().clone();
            let card_path = vm::card_of_topic(&e.topic);
            let card_title = card_path
                .as_ref()
                .and_then(|p| snap.card_by_path(p))
                .map(|c| c.title.clone())
                .unwrap_or_else(|| e.topic.clone());
            let (label, color, text) = match it {
                InboxItem::Permission(_) => ("권한 요청", AMBER, format::permission_line(&e)),
                InboxItem::Question(_) => {
                    let ti = vm::thread_item(&e);
                    (
                        if e.kind == "msg.error" {
                            "오류"
                        } else {
                            "질문"
                        },
                        tone_color(ti.tone),
                        ti.text,
                    )
                }
            };
            let mut row = div()
                .flex()
                .flex_col()
                .gap_1()
                .p_3()
                .rounded_md()
                .bg(rgb(PANEL))
                .border_l_2()
                .border_color(rgb(color))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .text_xs()
                        .child(div().text_color(rgb(color)).child(label))
                        .child(div().text_color(rgb(MUTED)).truncate().child(card_title))
                        .child(div().flex_1())
                        .child(
                            div()
                                .text_color(rgb(FAINT))
                                .child(format::ago(now_ms(), e.ts)),
                        ),
                )
                .child(div().text_sm().text_color(rgb(TEXT)).child(text));
            let mut btns = div().flex().flex_row().gap_2().pt_1();
            if let InboxItem::Permission(p) = it {
                let seq = p.seq;
                btns = btns
                    .child(
                        button(("ibx-allow", n), "승인", Btn::Ok).on_click(cx.listener(
                            move |this, _: &ClickEvent, _w, cx| {
                                this.send(Action::ResolvePermission { seq, allow: true });
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        button(("ibx-deny", n), "거절", Btn::Danger).on_click(cx.listener(
                            move |this, _: &ClickEvent, _w, cx| {
                                this.send(Action::ResolvePermission { seq, allow: false });
                                cx.notify();
                            },
                        )),
                    );
            }
            if let Some(p) = card_path {
                btns = btns.child(button(("ibx-open", n), "카드 열기", Btn::Ghost).on_click(
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.nav = Nav::Board;
                        this.card_tab = CardTab::Thread;
                        this.open_card(p.clone(), cx);
                        window.focus(&this.composer.focus_handle(cx));
                    }),
                ));
            } else {
                btns = btns.child(
                    button(("ibx-orch", n), "오케스트레이터 열기", Btn::Ghost).on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.nav = Nav::Orchestrator;
                            window.focus(&this.orch_input.focus_handle(cx));
                            cx.notify();
                        }),
                    ),
                );
            }
            row = row.child(btns);
            col = col.child(row);
        }
        col.into_any_element()
    }

    // ---------- 오케스트레이터 ----------

    fn orchestrator_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let snap = self.snap();
        let Some(project) = self
            .project
            .clone()
            .or_else(|| snap.projects.first().map(|p| p.slug.clone()))
        else {
            return empty_state(
                "프로젝트가 먼저 필요합니다",
                "왼쪽의 + 를 눌러 프로젝트를 만들면 그 프로젝트의 오케스트레이터와 대화할 수 있습니다.",
            )
            .into_any_element();
        };
        let orch = snap
            .instances
            .iter()
            .find(|i| i.role == "orchestrator" && i.project == project)
            .cloned();
        let topic = format!("project/{project}");
        let hist = self.histories.get(&topic).cloned().unwrap_or_default();
        let events = vm::thread(&hist, &snap.recent, &topic, true);
        let head = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_5()
            .h(px(48.))
            .flex_none()
            .border_b_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .text_color(rgb(TEXT))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(format!("오케스트레이터 · {project}")),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(dot(orch
                        .as_ref()
                        .map(|o| instance_color(&o.state))
                        .unwrap_or(FAINT)))
                    .child(match &orch {
                        Some(o) => format!(
                            "{} · {}",
                            format::agent_label(&o.agent),
                            format::status_label(&o.state)
                        ),
                        None => "쉬는 중 · 말을 보내면 깨어납니다".to_string(),
                    }),
            )
            .children(orch.map(|o| {
                let id = o.id.clone();
                button("orch-stop", "멈추기", Btn::Danger).on_click(cx.listener(
                    move |this, _: &ClickEvent, _w, _cx| {
                        this.send(Action::StopInstance(id.clone()))
                    },
                ))
            }));
        let composer = self.composer_row(self.orch_input.clone(), "send-orch", cx, |this, cx| {
            let text = this.orch_input.read(cx).text();
            if !text.trim().is_empty() {
                this.orch_input.update(cx, |i, cx| i.clear(cx));
                this.send_to_orchestrator(text, cx);
            }
        });
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(head)
            .child(div().flex_1().min_h(px(0.)).child(self.thread_body(
                &snap,
                events,
                "orch-thread",
                cx,
            )))
            .child(composer)
            .into_any_element()
    }

    // ---------- 에이전트 ----------

    fn agents_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let snap = self.snap();
        let mut col = div()
            .id("agents")
            .flex()
            .flex_col()
            .gap_2()
            .p_5()
            .size_full()
            .overflow_y_scroll()
            .child(div().text_lg().text_color(rgb(TEXT)).child("에이전트"))
            .child(section_label("백엔드").px_0());
        if self.agents.is_empty() {
            col = col.child(div().text_sm().text_color(rgb(FAINT)).child("확인하는 중…"));
        }
        for a in &self.agents {
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(PANEL))
                    .child(dot(if a.installed { GREEN } else { FAINT }))
                    .child(
                        div()
                            .w(px(120.))
                            .text_sm()
                            .text_color(rgb(TEXT))
                            .child(format::agent_label(&a.name)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .truncate()
                            .child(if a.installed {
                                format!("{} {}", a.version, a.detail)
                            } else {
                                match a.name.as_str() {
                                    "claude" => {
                                        "설치되지 않음 · npm i -g @anthropic-ai/claude-code"
                                            .to_string()
                                    }
                                    "opencode" => {
                                        "설치되지 않음 · npm i -g opencode-ai".to_string()
                                    }
                                    _ => "설치되지 않음".to_string(),
                                }
                            }),
                    ),
            );
        }
        col = col.child(section_label("봇").px_0());
        for b in &self.bots {
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(PANEL))
                    .text_sm()
                    .child(
                        div()
                            .w(px(120.))
                            .text_color(rgb(TEXT))
                            .child(b.name.clone()),
                    )
                    .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                        "{}{}",
                        format::agent_label(&b.agent),
                        if b.model.is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", b.model)
                        }
                    ))),
            );
        }
        col =
            col.child(section_label(format!("실행 중인 인스턴스 {}", snap.instances.len())).px_0());
        if snap.instances.is_empty() {
            col = col.child(
                div()
                    .text_sm()
                    .text_color(rgb(FAINT))
                    .child("지금 실행 중인 인스턴스가 없습니다."),
            );
        }
        for (n, i) in snap.instances.iter().enumerate() {
            let id = i.id.clone();
            let card = i.card.clone();
            let title = card
                .as_ref()
                .and_then(|c| snap.card_by_path(c))
                .map(|c| c.title.clone())
                .unwrap_or_else(|| format!("{} 오케스트레이터", i.project));
            col =
                col.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(PANEL))
                        .child(dot(instance_color(&i.state)))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w(px(0.))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(TEXT))
                                        .truncate()
                                        .child(title),
                                )
                                .child(div().text_xs().text_color(rgb(FAINT)).child(format!(
                                    "{} · {} · {} · {}턴",
                                    i.bot,
                                    format::agent_label(&i.agent),
                                    format::status_label(&i.state),
                                    i.turns
                                ))),
                        )
                        .children(card.map(|c| {
                            button(("inst-open", n), "열기", Btn::Ghost).on_click(cx.listener(
                                move |this, _: &ClickEvent, _w, cx| {
                                    this.nav = Nav::Board;
                                    this.open_card(c.clone(), cx);
                                },
                            ))
                        }))
                        .child(button(("inst-stop", n), "멈추기", Btn::Danger).on_click(
                            cx.listener(move |this, _: &ClickEvent, _w, _cx| {
                                this.send(Action::StopInstance(id.clone()))
                            }),
                        )),
                );
        }
        col.into_any_element()
    }

    // ---------- 모달 ----------

    fn modal_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.modal?;
        let snap = self.snap();
        let mut dialog = div()
            .id("dialog")
            .flex()
            .flex_col()
            .gap_3()
            .w(px(540.))
            .p_5()
            .rounded_lg()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(LINE))
            .shadow_lg()
            // 대화상자 안을 눌러도 닫히지 않게 한다.
            .on_click(|_, _, cx| cx.stop_propagation());
        match m {
            Modal::NewProject => {
                dialog = dialog
                    .child(div().text_lg().text_color(rgb(TEXT)).child("새 프로젝트"))
                    .child(self.project_name.clone())
                    .child(self.repo_row("np-pick", cx))
                    .child(div().text_xs().text_color(rgb(FAINT)).child(
                        "레포를 고르면 카드마다 그 레포의 git 워크트리(브랜치 jc/<카드>)를 열어 워커가 거기서 일합니다. Enter로 다음 칸 · Esc로 닫기",
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(div().flex_1())
                            .child(button("np-cancel", "취소", Btn::Ghost).on_click(
                                cx.listener(|this, _: &ClickEvent, w, cx| this.close_modal(w, cx)),
                            ))
                            .child(
                                button("np-ok", "만들기", Btn::Primary).on_click(cx.listener(
                                    |this, _: &ClickEvent, w, cx| this.submit_project(w, cx),
                                )),
                            ),
                    );
            }
            Modal::NewCard => {
                let target = self
                    .project
                    .clone()
                    .or_else(|| snap.projects.first().map(|p| p.slug.clone()))
                    .unwrap_or_default();
                let mut projects = div().flex().flex_row().flex_wrap().gap_1();
                for (i, p) in snap.projects.iter().enumerate() {
                    let slug = p.slug.clone();
                    projects = projects.child(
                        chip(("nc-proj", i), p.slug.clone(), p.slug == target).on_click(
                            cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                this.project = Some(slug.clone());
                                cx.notify();
                            }),
                        ),
                    );
                }
                let mut agents = div().flex().flex_row().flex_wrap().gap_1().items_center();
                agents = agents.child(
                    chip(
                        "nc-start",
                        if self.new_card_start {
                            "✓ 바로 워커 시작"
                        } else {
                            "워커 시작 안 함"
                        },
                        self.new_card_start,
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.new_card_start = !this.new_card_start;
                        cx.notify();
                    })),
                );
                if self.new_card_start {
                    for (i, a) in self.agents.iter().filter(|a| a.installed).enumerate() {
                        let name = a.name.clone();
                        let on = self.agent_choice.as_deref() == Some(&a.name);
                        agents = agents.child(
                            chip(("nc-agent", i), format::agent_label(&a.name), on).on_click(
                                cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                    this.agent_choice =
                                        if this.agent_choice.as_deref() == Some(&name) {
                                            None
                                        } else {
                                            Some(name.clone())
                                        };
                                    cx.notify();
                                }),
                            ),
                        );
                    }
                }
                dialog = dialog
                    .child(div().text_lg().text_color(rgb(TEXT)).child("새 카드"))
                    .child(projects)
                    .child(self.card_title.clone())
                    .child(self.card_body.clone())
                    .child(agents)
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(FAINT))
                            .child("Enter로 다음 칸 · 마지막 칸에서 Enter로 만들기 · Esc로 닫기"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(div().flex_1())
                            .child(button("nc-cancel", "취소", Btn::Ghost).on_click(
                                cx.listener(|this, _: &ClickEvent, w, cx| this.close_modal(w, cx)),
                            ))
                            .child(button("nc-ok", "만들기", Btn::Primary).on_click(
                                cx.listener(|this, _: &ClickEvent, w, cx| this.submit_card(w, cx)),
                            )),
                    );
            }
        }
        Some(
            div()
                .id("overlay")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(120.))
                .bg(rgba(0x0000008c))
                .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.close_modal(w, cx)))
                .child(dialog)
                .into_any_element(),
        )
    }

    // ---------- 수집함(바깥 사이클) ----------

    fn intake_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let groups = vm::group_intake(&self.intake, self.project.as_deref());
        let has_conn = !self.connectors.is_empty();
        let mut col = div()
            .id("intake")
            .flex()
            .flex_col()
            .gap_2()
            .p_5()
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(div().text_lg().text_color(rgb(TEXT)).child("수집함"))
                    .child(div().flex_1())
                    .child(
                        button("ik-sync", "지금 동기화", Btn::Ghost).on_click(cx.listener(
                            |this, _: &ClickEvent, _w, cx| {
                                this.send(Action::SyncConnectors(None));
                                this.send(Action::LoadIntake);
                                this.send(Action::LoadConnectors);
                                this.toast("연결된 출처를 동기화하는 중…", false);
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        button("ik-conn", "연결 관리", Btn::Subtle).on_click(cx.listener(
                            |this, _: &ClickEvent, _w, cx| {
                                this.nav = Nav::Connectors;
                                this.send(Action::LoadConnectors);
                                cx.notify();
                            },
                        )),
                    ),
            )
            .child(div().text_sm().text_color(rgb(MUTED)).child(
                "메신저·GitHub·메모처럼 코드 밖에서 들어온 해야 할 일입니다. 카드로 만들면 \
                 원문 대신 짧은 요약만 워커에게 넘어가고, 카드가 끝나면 회신 대기로 돌아옵니다.",
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .py_2()
                    .child(self.memo_input.clone()),
            );
        if groups.is_empty() {
            col = col.child(empty_state(
                "수집함이 비어 있습니다",
                if has_conn {
                    "연결된 출처에서 새 메시지가 오면 여기에 모입니다. 위 칸에 직접 메모를 넣을 수도 있습니다."
                } else {
                    "왼쪽 '연결'에서 Slack 같은 출처를 연결하거나, 위 칸에 직접 메모를 넣어 보세요."
                },
            ));
        }
        let mut n = 0usize;
        for (label, items) in groups {
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .pt_3()
                    .text_xs()
                    .text_color(rgb(match label {
                        "new" => VIOLET,
                        "reply_pending" => AMBER,
                        _ => FAINT,
                    }))
                    .child(vm::intake_state_label(label))
                    .child(div().text_color(rgb(FAINT)).child(items.len().to_string())),
            );
            for it in items {
                n += 1;
                col = col.child(self.intake_row(n, &it, cx));
            }
        }
        col.into_any_element()
    }

    fn intake_row(&self, n: usize, it: &IntakeItem, cx: &mut Context<Self>) -> Div {
        let color = match it.state.as_str() {
            "new" => VIOLET,
            "reply_pending" => AMBER,
            "carded" => ACCENT,
            _ => LINE,
        };
        let preview: String = it.body.chars().take(320).collect();
        let source = if it.connector.is_empty() {
            it.source.clone()
        } else {
            it.connector.clone()
        };
        let mut row = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .rounded_md()
            .bg(rgb(PANEL))
            .border_l_2()
            .border_color(rgb(color))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .text_xs()
                    .child(div().text_color(rgb(color)).child(source))
                    .children(
                        (!it.from.is_empty())
                            .then(|| div().text_color(rgb(MUTED)).child(it.from.clone())),
                    )
                    .child(div().text_color(rgb(FAINT)).child(it.project.clone()))
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_color(rgb(FAINT))
                            .child(format::ago(now_ms(), it.ts)),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(TEXT))
                    .child(it.title.clone()),
            )
            .children(
                (!preview.is_empty() && preview != it.title)
                    .then(|| div().text_xs().text_color(rgb(MUTED)).child(preview)),
            )
            .children((!it.url.is_empty()).then(|| {
                div()
                    .text_xs()
                    .text_color(rgb(FAINT))
                    .truncate()
                    .child(it.url.clone())
            }));
        let id = it.id.clone();
        let project = it.project.clone();
        let mut btns = div().flex().flex_row().flex_wrap().gap_2().pt_1();
        match it.state.as_str() {
            "new" => {
                let (i1, p1, t1) = (id.clone(), project.clone(), it.title.clone());
                let brief = vm::default_brief(&it.body, &it.title);
                let (i2, p2) = (id.clone(), project.clone());
                let i3 = id.clone();
                btns = btns
                    .child(
                        button(("ik-card", n), "카드로 만들고 시작", Btn::Primary).on_click(
                            cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                this.send(Action::TriageToCard {
                                    id: i1.clone(),
                                    title: t1.clone(),
                                    brief: brief.clone(),
                                    start: true,
                                    project: p1.clone(),
                                    agent: this.agent_choice.clone(),
                                });
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        button(("ik-orch", n), "오케스트레이터에게 맡기기", Btn::Ghost).on_click(
                            cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                this.send(Action::AskOrchestratorTriage {
                                    project: p2.clone(),
                                    id: i2.clone(),
                                });
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        button(("ik-arch", n), "보관", Btn::Subtle).on_click(cx.listener(
                            move |this, _: &ClickEvent, _w, cx| {
                                this.send(Action::Archive(i3.clone()));
                                cx.notify();
                            },
                        )),
                    );
            }
            "reply_pending" => {
                let can_reply = self
                    .connectors
                    .iter()
                    .any(|c| c.name == it.connector && c.can_reply);
                let editing = self.reply_target.as_deref() == Some(&it.id);
                if editing {
                    row = row.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .pt_1()
                            .child(self.reply_input.clone())
                            .child(button(("ik-send", n), "보내기", Btn::Primary).on_click(
                                cx.listener(|this, _: &ClickEvent, _w, cx| {
                                    let text = this.reply_input.read(cx).text();
                                    if !text.trim().is_empty() {
                                        this.send_reply(text, cx);
                                    }
                                }),
                            )),
                    );
                } else if can_reply {
                    let i1 = id.clone();
                    btns = btns.child(button(("ik-reply", n), "답장 쓰기", Btn::Primary).on_click(
                        cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.reply_target = Some(i1.clone());
                            window.focus(&this.reply_input.focus_handle(cx));
                            cx.notify();
                        }),
                    ));
                }
                let i2 = id.clone();
                btns = btns.child(
                    button(("ik-close", n), "답장 없이 닫기", Btn::Subtle).on_click(cx.listener(
                        move |this, _: &ClickEvent, _w, cx| {
                            if this.reply_target.as_deref() == Some(&i2) {
                                this.reply_target = None;
                            }
                            this.send(Action::CloseIntake(i2.clone()));
                            cx.notify();
                        },
                    )),
                );
            }
            _ => {}
        }
        if let Some(card) = it.card.clone() {
            btns = btns.child(button(("ik-open", n), "카드 열기", Btn::Ghost).on_click(
                cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    this.nav = Nav::Board;
                    this.card_tab = CardTab::Thread;
                    this.open_card(card.clone(), cx);
                }),
            ));
        }
        row.child(btns)
    }

    // ---------- 연결(커넥터) ----------

    fn connectors_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let snap = self.snap();
        let mut col = div()
            .id("connectors")
            .flex()
            .flex_col()
            .gap_2()
            .p_5()
            .size_full()
            .overflow_y_scroll()
            .child(div().text_lg().text_color(rgb(TEXT)).child("연결"))
            .child(div().text_sm().text_color(rgb(MUTED)).child(
                "MCP 서버로 바깥 출처를 연결합니다. 연결마다 주기적으로 새 메시지를 읽어 \
                 수집함에 넣고, 사람이 승인한 답장만 다시 보냅니다. 토큰은 이 컴퓨터의 \
                 .jadecode/connectors.toml(권한 0600)에만 저장됩니다.",
            ));

        col = col.child(section_label("연결된 출처"));
        if self.connectors.is_empty() {
            col = col.child(
                div()
                    .px_3()
                    .text_sm()
                    .text_color(rgb(FAINT))
                    .child("아직 연결이 없습니다."),
            );
        }
        for (n, c) in self.connectors.iter().enumerate() {
            let (scol, stext) = match &c.status {
                Some((true, s)) => (GREEN, s.clone()),
                Some((false, s)) => (RED, s.clone()),
                None => (FAINT, "아직 동기화하지 않음".to_string()),
            };
            let (n1, n2, n3) = (c.name.clone(), c.name.clone(), c.name.clone());
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(PANEL))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(dot(if c.enabled { scol } else { FAINT }))
                            .child(div().text_sm().text_color(rgb(TEXT)).child(c.name.clone()))
                            .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                                "{} · {}{}{}",
                                c.preset,
                                c.project,
                                if c.can_reply { " · 답장 가능" } else { "" },
                                if c.enabled { "" } else { " · 꺼짐" }
                            )))
                            .child(div().flex_1())
                            .child(button(("cn-sync", n), "동기화", Btn::Ghost).on_click(
                                cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                    this.send(Action::SyncConnectors(Some(n1.clone())));
                                    this.send(Action::LoadConnectors);
                                    cx.notify();
                                }),
                            ))
                            .child(button(("cn-test", n), "시험", Btn::Subtle).on_click(
                                cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                    this.send(Action::TestConnector(n2.clone()));
                                    this.send(Action::LoadConnectors);
                                    cx.notify();
                                }),
                            ))
                            .child(button(("cn-del", n), "삭제", Btn::Danger).on_click(
                                cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                    this.send(Action::RemoveConnector(n3.clone()));
                                    this.send(Action::LoadConnectors);
                                    cx.notify();
                                }),
                            )),
                    )
                    .child(div().text_xs().text_color(rgb(scol)).child(stext)),
            );
        }

        // Slack 추가 양식
        let target = self
            .conn_project
            .clone()
            .or_else(|| self.current_or_first_project())
            .unwrap_or_default();
        let mut projects = div().flex().flex_row().flex_wrap().gap_1().items_center();
        projects = projects.child(div().text_xs().text_color(rgb(MUTED)).child("프로젝트"));
        for (i, p) in snap.projects.iter().enumerate() {
            let slug = p.slug.clone();
            projects = projects.child(
                chip(("cn-proj", i), p.slug.clone(), p.slug == target).on_click(cx.listener(
                    move |this, _: &ClickEvent, _w, cx| {
                        this.conn_project = Some(slug.clone());
                        cx.notify();
                    },
                )),
            );
        }
        col = col.child(section_label("Slack 채널 연결")).child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded_md()
                .bg(rgb(PANEL))
                .child(div().text_xs().text_color(rgb(MUTED)).child(
                    "Slack 앱을 만들고 봇 권한 channels:history, chat:write, users:read를 준 뒤 \
                     채널에 봇을 초대하세요. Node.js(npx)가 설치되어 있어야 합니다.",
                ))
                .child(projects)
                .child(self.conn_token.clone())
                .child(self.conn_team.clone())
                .child(self.conn_channel.clone())
                .child(
                    div().flex().flex_row().child(
                        button("cn-save", "저장하고 연결", Btn::Primary).on_click(
                            cx.listener(|this, _: &ClickEvent, _w, cx| this.save_slack(cx)),
                        ),
                    ),
                ),
        );
        col = col.child(section_label("다른 MCP 서버")).child(
            div().px_3().text_xs().text_color(rgb(MUTED)).child(
                "이메일·Linear·Notion 같은 다른 출처는 .jadecode/connectors.toml에 \
                     preset = \"custom\"으로 command·args·fetch(도구 이름과 필드 경로)를 적으면 \
                     같은 방식으로 수집됩니다. 형식은 docs/implementation.md에 있습니다.",
            ),
        );
        col.into_any_element()
    }

    fn toast_view(&self) -> Option<AnyElement> {
        let (msg, err, _) = self.toast.clone()?;
        Some(
            div()
                .absolute()
                .bottom(px(16.))
                .right(px(16.))
                .max_w(px(520.))
                .px_4()
                .py_2()
                .rounded_md()
                .bg(rgb(if err { 0x3a1f22 } else { RAISED }))
                .border_1()
                .border_color(rgb(if err { RED } else { LINE }))
                .text_sm()
                .text_color(rgb(if err { 0xffd6d3 } else { TEXT }))
                .shadow_lg()
                .child(msg)
                .into_any_element(),
        )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 창 제목에 확인이 필요한 일 수를 보인다.
        let n = vm::inbox(&self.snap()).len();
        let title = if n > 0 {
            format!("jadecode ({n})")
        } else {
            "jadecode".to_string()
        };
        if title != self.last_title {
            window.set_window_title(&title);
            self.last_title = title;
        }
        // 스레드가 길어지면 맨 아래로.
        if let Some(card) = &self.selected_card {
            let topic = vm::card_topic(card);
            let len = self.histories.get(&topic).map(|h| h.len()).unwrap_or(0)
                + self
                    .snap()
                    .recent
                    .iter()
                    .filter(|e| e.topic == topic)
                    .count();
            if len != self.last_thread_len {
                self.last_thread_len = len;
                self.thread_scroll.scroll_to_bottom();
            }
        }

        let page = match self.nav {
            Nav::Board => self.board_page(cx),
            Nav::Inbox => self.inbox_page(cx),
            Nav::Orchestrator => self.orchestrator_page(cx),
            Nav::Agents => self.agents_page(cx),
            Nav::Intake => self.intake_page(cx),
            Nav::Connectors => self.connectors_page(cx),
        };
        let modal = self.modal_view(cx);
        let toast = self.toast_view();
        div()
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|this, _: &NewCard, w, cx| this.open_modal(Modal::NewCard, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &NewProject, w, cx| {
                    this.open_modal(Modal::NewProject, w, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &CloseModal, w, cx| {
                if this.modal.is_some() {
                    this.close_modal(w, cx)
                } else if this.selected_card.is_some() {
                    this.selected_card = None;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &GoIntake, w, cx| {
                this.nav = Nav::Intake;
                this.send(Action::LoadIntake);
                w.focus(&this.memo_input.focus_handle(cx));
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &GoConnectors, _w, cx| {
                this.nav = Nav::Connectors;
                this.send(Action::LoadConnectors);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &CheckUpdate, _w, cx| {
                this.send(Action::CheckUpdate(true));
                this.toast("업데이트를 확인하는 중…", false);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &About, _w, cx| {
                this.toast(
                    format!(
                        "jadecode {} · {}",
                        jc_core::version::VERSION,
                        this.backend_label
                    ),
                    false,
                );
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &GoInbox, _w, cx| {
                this.nav = Nav::Inbox;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &GoOrchestrator, w, cx| {
                this.nav = Nav::Orchestrator;
                this.set_project(
                    this.project
                        .clone()
                        .or_else(|| this.snap().projects.first().map(|p| p.slug.clone())),
                    cx,
                );
                w.focus(&this.orch_input.focus_handle(cx));
            }))
            .on_action(cx.listener(|this, _: &GoBoard, _w, cx| {
                this.nav = Nav::Board;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &GoAgents, _w, cx| {
                this.nav = Nav::Agents;
                this.send(Action::LoadAgents);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleBoardMode, _w, cx| {
                this.board_mode = match this.board_mode {
                    BoardMode::List => BoardMode::Kanban,
                    BoardMode::Kanban => BoardMode::List,
                };
                this.nav = Nav::Board;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Refresh, _w, cx| {
                this.send(Action::LoadAgents);
                if let Some(c) = this.selected_card.clone() {
                    this.open_card(c, cx);
                }
                if let Some(p) = this.project.clone() {
                    this.set_project(Some(p), cx);
                }
                this.toast("새로 고쳤습니다", false);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FocusComposer, w, cx| {
                if this.nav == Nav::Orchestrator {
                    w.focus(&this.orch_input.focus_handle(cx));
                } else if this.selected_card.is_some() {
                    this.card_tab = CardTab::Thread;
                    w.focus(&this.composer.focus_handle(cx));
                }
                cx.notify();
            }))
            .relative()
            .flex()
            .flex_row()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_family(".SystemUIFont")
            .child(self.sidebar(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .child(page),
            )
            .children(modal)
            .children(toast)
    }
}

/// 테스트: 화면 없이도 색 규칙이 지켜지는지 확인한다.
#[cfg(test)]
mod tests {
    use super::theme::*;

    #[test]
    fn status_colors() {
        assert_eq!(status_color("working"), ACCENT);
        assert_eq!(status_color("blocked"), RED);
        assert_eq!(status_color("done"), GREEN);
        assert_eq!(tone_color("warn"), AMBER);
    }
}
