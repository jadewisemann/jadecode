//! GPUI 화면.
//!
//! 구성(왼쪽부터):
//! 1. 사이드바: 받은편지함(승인·질문) · 오케스트레이터 · 보드 · 에이전트, 프로젝트 목록
//! 2. 가운데: 카드 목록(급한 순) 또는 칸반(단계 열)
//! 3. 오른쪽: 카드 스레드(에이전트 대화·도구 호출·권한 승인 + 입력), task.md, 변경사항(diff)
//!
//! 데이터는 `board::Board`가 갖는다. 쓰기 동작은 **별도 연결**로 백그라운드 스레드에서
//! 실행한다(GPUI 실행기와 tokio를 섞지 않는다). 결과는 `Outcome`과 이벤트 구독으로 돌아온다.

pub mod input;
pub mod theme;

use crate::board::{
    AgentInfo, Board, BoardSnapshot, BotInfo, Card as CardData, CardDetail, EventRow,
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
        GoInbox,
        GoOrchestrator,
        GoBoard,
        GoAgents,
        ToggleBoardMode,
        CloseModal,
        Refresh,
        FocusComposer,
    ]
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Inbox,
    Orchestrator,
    Board,
    Agents,
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
}

/// 백그라운드 동작의 결과.
enum Outcome {
    Info(String),
    Error(String),
    Card(CardDetail),
    History { key: String, rows: Vec<EventRow> },
    Diff { card: String, text: String },
    ProjectAdded(String),
    CardAdded(String),
    Agents(Vec<AgentInfo>, Vec<BotInfo>),
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
            KeyBinding::new("secondary-1", GoInbox, None),
            KeyBinding::new("secondary-2", GoOrchestrator, None),
            KeyBinding::new("secondary-3", GoBoard, None),
            KeyBinding::new("secondary-4", GoAgents, None),
            KeyBinding::new("secondary-l", ToggleBoardMode, None),
            KeyBinding::new("secondary-r", Refresh, None),
            KeyBinding::new("secondary-k", FocusComposer, None),
            KeyBinding::new("escape", CloseModal, Some("Workspace")),
        ]);
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.set_menus(vec![
            Menu {
                name: "jadecode".into(),
                items: vec![
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
                    MenuItem::action("받은편지함", GoInbox),
                    MenuItem::action("오케스트레이터", GoOrchestrator),
                    MenuItem::action("보드", GoBoard),
                    MenuItem::action("에이전트", GoAgents),
                    MenuItem::separator(),
                    MenuItem::action("목록/칸반 전환", ToggleBoardMode),
                    MenuItem::action("새로고침", Refresh),
                ],
            },
        ]);

        let bounds = Bounds::centered(None, size(px(1360.), px(860.)), cx);
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
                            let _ = out.send(Outcome::Error(format!("백엔드에 붙지 못했다: {e}")));
                            continue;
                        }
                    };
                }
                let b = board.as_mut().unwrap();
                if ws_root.is_none() {
                    ws_root = rt.block_on(b.workspace()).ok();
                }
                let r = run_action(&rt, b, ws_root.as_deref(), a);
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
                Outcome::Info("워커를 띄웠다".into())
            }
            Err(e) => fail("워커 시작 실패", e),
        },
        Action::StopInstance(id) => match rt.block_on(b.stop_instance(&id)) {
            Ok(()) => Outcome::Info("인스턴스를 멈췄다".into()),
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
                Ok(()) => Outcome::Info("오케스트레이터에게 보냈다".into()),
                Err(e) => fail("보내기 실패", e),
            }
        }
        Action::SendInstance { instance, text } => {
            match rt.block_on(b.send_to_instance(&instance, &text)) {
                Ok(()) => Outcome::Info("보냈다".into()),
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
                return Outcome::Error("워크스페이스 위치를 몰라 변경사항을 읽지 못했다".into());
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
        Action::OpenFolder { card, terminal } => {
            let Some(ws) = ws else {
                return Outcome::Error("워크스페이스 위치를 모른다".into());
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
                Ok(_) => Outcome::Info(format!("열었다: {}", tilde(&dir.display().to_string()))),
                Err(e) => Outcome::Error(format!("열기 실패: {e}")),
            }
        }
    }
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
        let composer = cx.new(|cx| TextInput::new(cx, "워커에게 말하기"));
        let orch_input = cx.new(|cx| TextInput::new(cx, "오케스트레이터에게 할 일을 말하세요"));
        let project_name = cx.new(|cx| TextInput::new(cx, "프로젝트 이름 (영문·숫자)"));
        let project_repo = cx.new(|cx| TextInput::new(cx, "레포 이름 (선택)"));
        let card_title = cx.new(|cx| TextInput::new(cx, "무엇을 할까요? (카드 제목)"));
        let card_body = cx.new(|cx| TextInput::new(cx, "자세한 설명 (선택)"));

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
                Outcome::Info(m) => self.toast(m, false),
                Outcome::Error(m) => self.toast(m, true),
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
                    self.toast(format!("프로젝트 {slug}를 만들었다"), false);
                    self.project = Some(slug);
                    self.nav = Nav::Board;
                }
                Outcome::CardAdded(path) => {
                    self.toast("카드를 만들었다", false);
                    self.nav = Nav::Board;
                    self.open_card(path, cx);
                }
                Outcome::Agents(a, b) => {
                    self.agents = a;
                    self.bots = b;
                }
            }
        }
        let v = self.live.version.load(Ordering::Relaxed);
        if v != self.seen_version {
            self.seen_version = v;
            dirty = true;
            self.notify_new_inbox();
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
                self.toast("워커를 띄우고 말을 전한다", false);
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
        col = col
            .child(self.nav_item(
                "nav-inbox",
                "받은편지함",
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
        let mut cols = div()
            .id("kanban")
            .flex()
            .flex_row()
            .gap_3()
            .p_4()
            .size_full()
            .overflow_x_scroll();
        for (si, stage) in stages.iter().enumerate() {
            let cards: Vec<CardData> = snap
                .stage_cards(self.project.as_deref(), stage)
                .into_iter()
                .cloned()
                .collect();
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
                        .child(format::stage_label(stage))
                        .child(cards.len().to_string()),
                );
            for c in cards {
                let selected = self.selected_card.as_deref() == Some(&c.path);
                let insts = snap.instances_of(&c.path);
                let perms = vm::pending_permissions_of(&snap, &c.path);
                let path = c.path.clone();
                let key: SharedString = format!("tile-{}", c.path).into();
                col = col.child(
                    div()
                        .id(ElementId::Name(key))
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
        cols.into_any_element()
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
                        "일은 카드 단위로 진행됩니다. 카드를 만들면 에이전트가 그 카드 폴더에서 일하고, 승인이나 답이 필요할 때만 받은편지함으로 돌아옵니다.",
                    ))
                    .child(div().text_sm().text_color(rgb(TEXT)).pt_2().child("1. 프로젝트 이름"))
                    .child(self.project_name.clone())
                    .child(div().text_sm().text_color(rgb(TEXT)).child("2. 레포 이름 (선택)"))
                    .child(self.project_repo.clone())
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
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(dot(status_color(&card.status)))
                    .child(format::status_label(&card.status))
                    .child("·")
                    .child(card.project.clone())
                    .child("·")
                    .child(card.id.chars().take(10).collect::<String>()),
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
            }))
            .child(
                button("open-folder", "폴더", Btn::Subtle).on_click(cx.listener(
                    move |this, _: &ClickEvent, _w, cx| {
                        this.send(Action::OpenFolder {
                            card: Some(c4.clone()),
                            terminal: false,
                        });
                        cx.notify();
                    },
                )),
            )
            .child(
                button("open-term", "터미널", Btn::Subtle).on_click(cx.listener(
                    move |this, _: &ClickEvent, _w, cx| {
                        this.send(Action::OpenFolder {
                            card: Some(c5.clone()),
                            terminal: true,
                        });
                        cx.notify();
                    },
                )),
            )
            .child(
                button("make-pr", "PR 만들기", Btn::Ghost).on_click(cx.listener(
                    move |this, _: &ClickEvent, _w, cx| {
                        this.card_tab = CardTab::Thread;
                        this.send_from_composer(PR_PROMPT.to_string(), cx);
                    },
                )),
            );
        head = head.child(actions_row);

        // 탭
        let tab = self.card_tab;
        let mut tabs = div()
            .flex()
            .flex_row()
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
            .items_center()
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
                    .child("아직 대화가 없습니다."),
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
            .child(div().text_lg().text_color(rgb(TEXT)).child("받은편지함"))
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
                                "설치되지 않음".to_string()
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
        col = col.child(section_label(format!("도는 인스턴스 {}", snap.instances.len())).px_0());
        if snap.instances.is_empty() {
            col = col.child(
                div()
                    .text_sm()
                    .text_color(rgb(FAINT))
                    .child("지금 도는 인스턴스가 없습니다."),
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
                    .child(self.project_repo.clone())
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(FAINT))
                            .child("Enter로 다음 칸 · Esc로 닫기"),
                    )
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
                this.toast("새로 고쳤다", false);
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
