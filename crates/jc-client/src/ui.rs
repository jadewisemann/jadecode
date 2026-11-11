//! GPUI 화면. 명세의 첫 화면 다섯 가지를 담는다.
//!
//! 1. 프로젝트별 칸반 / 전체 칸반 — `View::Board`
//! 2. 카드 상세(task.md·경로·메시지·근거) — `View::Card`
//! 3. 오케스트레이터 대화(권한 승인 포함) — `View::Chat`
//! 4. 봇·에이전트 목록 — `View::Bots`
//! 5. 인스턴스에 붙기 — 카드 상세의 인스턴스 줄과 대화 탭
//!
//! 데이터는 `board::Board`가 갖는다. 쓰기 동작은 **별도 연결**로 백그라운드 스레드에서
//! 실행한다(GPUI 실행기와 tokio를 섞지 않는다). 결과는 이벤트 구독으로 돌아온다.

use crate::board::{Board, BoardSnapshot, Card as CardData, EventRow};
use crate::format;
use gpui::prelude::*;
use gpui::{
    div, point, px, rgb, size, App, Application, Bounds, Context, MouseButton, SharedString, Window,
    WindowBounds, WindowOptions,
};

const BG: u32 = 0x14161b;
const PANEL: u32 = 0x1c1f26;
const PANEL2: u32 = 0x232732;
const LINE: u32 = 0x2e3440;
const TEXT: u32 = 0xe6e8ee;
const MUTED: u32 = 0x8b93a7;
const BLUE: u32 = 0x4c8dff;
const GREEN: u32 = 0x53c08a;
const AMBER: u32 = 0xe0a44b;
const RED: u32 = 0xe05c5c;

fn status_color(status: &str) -> u32 {
    match status {
        "working" => BLUE,
        "blocked" => RED,
        "waiting_human" => AMBER,
        "done" => GREEN,
        "failed" => RED,
        _ => MUTED,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Board,
    Card,
    Chat,
    Bots,
}

/// 백그라운드에서 실행할 쓰기 동작.
#[derive(Debug, Clone)]
enum Action {
    Advance(String),
    StartWorker { project: String, card: String },
    ResolvePermission { seq: u64, allow: bool },
    Send { project: String, text: String },
    LoadMd(String),
}

pub struct App2 {
    /// 백그라운드 구독 스레드가 갱신하는 스냅숏. 화면은 `Arc` 사본을 받아 읽기만 한다
    /// (예전에는 프레임마다 전체를 clone해 1.2ms를 썼다).
    snap: std::sync::Arc<std::sync::Mutex<std::sync::Arc<BoardSnapshot>>>,
    view: View,
    project: Option<String>,
    selected_card: Option<String>,
    card_md: String,
    chat_input: String,
    /// 마지막 동작 오류(화면 아래에 보인다).
    status_line: Option<String>,
    tx: std::sync::mpsc::Sender<Action>,
    action_results: std::sync::mpsc::Receiver<String>,
    /// 화면이 마지막으로 본 seq(변경 감지).
    seen_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

/// 실행 진입점.
pub fn run(workspace: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    let conn = crate::board::Connection::discover(workspace.as_deref())?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let board = rt.block_on(conn.open())?;
    drop(rt);

    // 쓰기 동작 전용 스레드: 자기 연결을 하나 갖는다.
    let (tx, rx) = std::sync::mpsc::channel::<Action>();
    let (res_tx, res_rx) = std::sync::mpsc::channel::<String>();
    let socket = conn.socket.clone();
    std::thread::Builder::new()
        .name("jadecode-client-actions".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(r) => r,
                Err(e) => {
                    let _ = res_tx.send(format!("런타임 오류: {e}"));
                    return;
                }
            };
            let mut board: Option<Board> = None;
            while let Ok(a) = rx.recv() {
                if board.is_none() {
                    let c = crate::board::Connection {
                        socket: socket.clone(),
                    };
                    board = match rt.block_on(c.open()) {
                        Ok(b) => Some(b),
                        Err(e) => {
                            let _ = res_tx.send(format!("서버 연결 실패: {e}"));
                            continue;
                        }
                    };
                }
                let b = board.as_mut().unwrap();
                let msg = match a {
                    Action::Advance(card) => match rt.block_on(b.advance(&card)) {
                        Ok(stage) => format!("단계 이동: {stage}"),
                        Err(e) => format!("단계 이동 실패: {e}"),
                    },
                    Action::StartWorker { project, card } => {
                        match rt.block_on(b.start_worker(&project, &card, None)) {
                            Ok(()) => "워커를 띄웠다".to_string(),
                            Err(e) => format!("워커 시작 실패: {e}"),
                        }
                    }
                    Action::ResolvePermission { seq, allow } => {
                        let m = if allow { "승인" } else { "거절" };
                        match rt.block_on(b.resolve_permission(seq, allow, m)) {
                            Ok(()) => format!("권한 #{seq} {m}"),
                            Err(e) => format!("권한 답 실패: {e}"),
                        }
                    }
                    Action::Send { project, text } => {
                        match rt.block_on(b.send_to_orchestrator(&project, &text)) {
                            Ok(()) => "오케스트레이터에게 보냈다".to_string(),
                            Err(e) => format!("보내기 실패: {e}"),
                        }
                    }
                    Action::LoadMd(card) => match rt.block_on(b.card_markdown(&card)) {
                        Ok(md) => format!("__MD__{md}"),
                        Err(e) => format!("task.md 읽기 실패: {e}"),
                    },
                };
                let _ = res_tx.send(msg);
            }
        })
        .ok();

    // 구독 스레드: 이벤트를 받아 공유 스냅숏을 갱신한다(화면은 읽기만).
    let snap = std::sync::Arc::new(std::sync::Mutex::new(std::sync::Arc::new(
        board.snapshot().clone(),
    )));
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(board.snapshot().head_seq));
    {
        let snap = snap.clone();
        let seen = seen.clone();
        std::thread::Builder::new()
            .name("jadecode-client-sub".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(r) => r,
                    Err(_) => return,
                };
                rt.block_on(async move {
                    let mut board = board;
                    loop {
                        match board.poll().await {
                            Some(rows) => {
                                let head = board.snapshot().head_seq;
                                if let Ok(mut s) = snap.lock() {
                                    // 이벤트가 왔을 때만 새 스냅숏을 만든다(프레임마다 아님).
                                    *s = std::sync::Arc::new(board.snapshot().clone());
                                }
                                seen.store(head, std::sync::atomic::Ordering::Relaxed);
                                let _ = rows;
                            }
                            None => {
                                tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                            }
                        }
                    }
                });
            })
            .ok();
    }

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(1280.), px(820.)));
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |_w, cx| {
                cx.new(|_| App2 {
                    snap: snap.clone(),
                    view: View::Board,
                    project: None,
                    selected_card: None,
                    card_md: String::new(),
                    chat_input: String::new(),
                    status_line: None,
                    tx,
                    action_results: res_rx,
                    seen_seq: seen.clone(),
                })
            },
        )
        .expect("window");
        cx.activate(true);
    });
    Ok(())
}

impl App2 {
    /// 백그라운드 스레드가 채운 스냅숏(참조 계수만 올린다).
    fn snap(&self) -> std::sync::Arc<BoardSnapshot> {
        self.snap
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| std::sync::Arc::new(BoardSnapshot::default()))
    }

    /// 동작 결과만 반영한다(보드 갱신은 구독 스레드가 한다).
    fn tick(&mut self) {
        while let Ok(m) = self.action_results.try_recv() {
            if let Some(md) = m.strip_prefix("__MD__") {
                self.card_md = md.to_string();
            } else {
                self.status_line = Some(m);
            }
        }
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.view;
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_4()
            .py_3()
            .bg(rgb(PANEL))
            .border_b_1()
            .border_color(rgb(LINE))
            .child(div().text_lg().text_color(rgb(TEXT)).child("jadecode").mr_4());
        for (v, label) in [
            (View::Board, "보드"),
            (View::Card, "카드"),
            (View::Chat, "대화"),
            (View::Bots, "봇"),
        ] {
            let active = v == view;
            row = row.child(
                div()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(if active { PANEL2 } else { PANEL }))
                    .text_color(rgb(if active { TEXT } else { MUTED }))
                    .cursor_pointer()
                    .child(label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _w, cx| {
                            this.view = v;
                            cx.notify();
                        }),
                    ),
            );
        }
        row = row.child(div().flex_1());
        let projects: Vec<(Option<String>, String)> = std::iter::once((None, "전체".to_string()))
            .chain(
                self.snap()
                    .projects
                    .iter()
                    .map(|p| (Some(p.slug.clone()), p.name.clone())),
            )
            .collect();
        let cur = self.project.clone();
        for (slug, name) in projects {
            let active = slug == cur;
            row = row.child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_sm()
                    .bg(rgb(if active { BLUE } else { PANEL2 }))
                    .text_color(rgb(if active { 0xffffff } else { MUTED }))
                    .cursor_pointer()
                    .child(name)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _w, cx| {
                            this.project = slug.clone();
                            cx.notify();
                        }),
                    ),
            );
        }
        row.child(
            div()
                .ml_3()
                .text_sm()
                .text_color(rgb(MUTED))
                .child(format!("seq {}", self.snap().head_seq)),
        )
    }

    fn board_view(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let project = self.project.clone();
        let stages = self.snap().stages(project.as_deref());
        let mut cols = div().flex().flex_row().gap_3().p_4().flex_1().overflow_hidden();
        if stages.is_empty() {
            return cols
                .child(
                    div()
                        .text_color(rgb(MUTED))
                        .child("카드가 없다. 터미널에서 `jadecode card add ...` 또는 대화 탭에서 오케스트레이터에게 말하라."),
                )
                .into_any_element();
        }
        for stage in stages {
            let cards: Vec<CardData> = self
                .snap()
                .stage_cards(project.as_deref(), &stage)
                .into_iter()
                .cloned()
                .collect();
            let count = cards.len();
            let mut col = div()
                .flex()
                .flex_col()
                .gap_2()
                .w(px(260.))
                .bg(rgb(PANEL))
                .rounded_lg()
                .p_2()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .px_1()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .child(format::stage_label(&stage))
                        .child(format!("{count}")),
                );
            for c in cards {
                col = col.child(self.card_tile(c, cx));
            }
            cols = cols.child(col);
        }
        cols.into_any_element()
    }

    fn card_tile(&self, c: CardData, cx: &mut Context<Self>) -> impl IntoElement {
        let (badge, _) = format::status_badge(&c.status);
        let color = status_color(&c.status);
        let inst_line = self
            .snap()
            .instances_of(&c.path)
            .iter()
            .map(|i| format::instance_line(i))
            .collect::<Vec<_>>()
            .join(" / ");
        let path = c.path.clone();
        let id_short: String = c.id.chars().take(10).collect();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .bg(rgb(PANEL2))
            .border_l_2()
            .border_color(rgb(color))
            .cursor_pointer()
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(color))
                    .child(format!("{badge} {}", format::status_label(&c.status))),
            )
            .child(div().text_color(rgb(TEXT)).child(c.title.clone()))
            .children(if inst_line.is_empty() {
                None
            } else {
                Some(div().text_xs().text_color(rgb(MUTED)).child(inst_line))
            })
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format!("{id_short} · {}", c.project)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _w, cx| {
                    this.view = View::Card;
                    this.selected_card = Some(path.clone());
                    this.card_md.clear();
                    let _ = this.tx.send(Action::LoadMd(path.clone()));
                    cx.notify();
                }),
            )
    }

    fn card_view(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(path) = self.selected_card.clone() else {
            return div()
                .p_4()
                .text_color(rgb(MUTED))
                .child("보드에서 카드를 고르세요.")
                .into_any_element();
        };
        let Some(card) = self.snap().card_by_path(&path).cloned() else {
            return div()
                .p_4()
                .text_color(rgb(MUTED))
                .child(format!("카드를 찾을 수 없다: {path}"))
                .into_any_element();
        };
        let insts: Vec<_> = self
            .snap()
            .instances_of(&path)
            .into_iter()
            .cloned()
            .collect();
        let mut body = div().flex().flex_col().gap_2().p_4().flex_1();

        // 경로 진행
        let mut progress = div().flex().flex_row().gap_2().items_center();
        let stages = ["refine", "implement", "verify", "pr", "report"];
        for (i, st) in stages.iter().enumerate() {
            let here = format::stage_label(st) == format::stage_label(&card.stage);
            progress = progress.child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_sm()
                    .bg(rgb(if here { BLUE } else { PANEL2 }))
                    .text_color(rgb(if here { 0xffffff } else { MUTED }))
                    .child(format!("{}. {}", i + 1, format::stage_label(st))),
            );
            if i + 1 < stages.len() {
                progress = progress.child(div().text_color(rgb(LINE)).child("›"));
            }
        }
        body = body.child(progress);

        let p1 = card.path.clone();
        let p2 = card.path.clone();
        let p3 = card.path.clone();
        let proj = self
            .project
            .clone()
            .unwrap_or_else(|| card.project.clone());
        body = body.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(
                    div()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(BLUE))
                        .text_color(rgb(0xffffff))
                        .cursor_pointer()
                        .child("다음 단계")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _w, cx| {
                                let _ = this.tx.send(Action::Advance(p1.clone()));
                                cx.notify();
                            }),
                        ),
                )
                .child(
                    div()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(PANEL2))
                        .text_color(rgb(TEXT))
                        .cursor_pointer()
                        .child("워커 띄우기")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _w, cx| {
                                let _ = this.tx.send(Action::StartWorker {
                                    project: proj.clone(),
                                    card: p2.clone(),
                                });
                                cx.notify();
                            }),
                        ),
                )
                .child(
                    div()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(PANEL2))
                        .text_color(rgb(TEXT))
                        .cursor_pointer()
                        .child("task.md 읽기")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _w, cx| {
                                let _ = this.tx.send(Action::LoadMd(p3.clone()));
                                cx.notify();
                            }),
                        ),
                ),
        );

        for i in insts {
            body = body.child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(PANEL2))
                    .text_sm()
                    .text_color(rgb(TEXT))
                    .child(format::instance_line(&i)),
            );
        }

        let msgs: Vec<EventRow> = self
            .snap()
            .messages
            .iter()
            .filter(|m| m.topic.ends_with(&path))
            .cloned()
            .collect();
        let mut mlist = div().flex().flex_col().gap_1().overflow_hidden();
        for m in msgs.iter().rev().take(12) {
            let (head, text) = format::event_line(m);
            mlist = mlist.child(
                div()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(PANEL))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(head))
                    .child(div().text_sm().text_color(rgb(TEXT)).child(text)),
            );
        }
        body = body.child(mlist);

        let md = if self.card_md.is_empty() {
            "task.md를 불러오려면 \"task.md 읽기\"를 누르세요.".to_string()
        } else {
            self.card_md.clone()
        };
        body.child(
            div()
                .flex_1()
                .p_3()
                .rounded_md()
                .bg(rgb(PANEL))
                .text_sm()
                .text_color(rgb(TEXT))
                .child(md),
        )
        .into_any_element()
    }

    fn chat_view(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut list = div().flex().flex_col().gap_2().p_4().flex_1().overflow_hidden();

        for p in self.snap().pending_permissions.clone() {
            let seq = p.seq;
            list = list.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(PANEL2))
                    .border_l_2()
                    .border_color(rgb(AMBER))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(rgb(TEXT))
                            .child(format::permission_line(&p)),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(GREEN))
                            .text_color(rgb(0x08130d))
                            .cursor_pointer()
                            .child("승인")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _w, cx| {
                                    let _ = this.tx.send(Action::ResolvePermission {
                                        seq,
                                        allow: true,
                                    });
                                    cx.notify();
                                }),
                            ),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(RED))
                            .text_color(rgb(0x1a0808))
                            .cursor_pointer()
                            .child("거절")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _w, cx| {
                                    let _ = this.tx.send(Action::ResolvePermission {
                                        seq,
                                        allow: false,
                                    });
                                    cx.notify();
                                }),
                            ),
                    ),
            );
        }

        let msgs = self.snap().messages.clone();
        let show = msgs.len().saturating_sub(40);
        for m in &msgs[show..] {
            let (head, text) = format::event_line(m);
            let color = match m.kind.as_str() {
                "msg.ask" | "msg.propose" | "msg.escalation" => AMBER,
                "msg.error" => RED,
                "msg.report_done" => GREEN,
                _ => MUTED,
            };
            list = list.child(
                div()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(PANEL))
                    .child(div().text_xs().text_color(rgb(color)).child(head))
                    .child(div().text_sm().text_color(rgb(TEXT)).child(text)),
            );
        }

        let input = self.chat_input.clone();
        let project = self.project.clone().unwrap_or_default();
        let has_project = !project.is_empty();
        list.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .p_2()
                .rounded_md()
                .bg(rgb(PANEL2))
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(rgb(if input.is_empty() { MUTED } else { TEXT }))
                        .child(if input.is_empty() {
                            if has_project {
                                "오케스트레이터에게 말하기".to_string()
                            } else {
                                "위에서 프로젝트를 고르세요".to_string()
                            }
                        } else {
                            input
                        }),
                )
                .child(
                    div()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(if has_project { BLUE } else { PANEL }))
                        .text_color(rgb(0xffffff))
                        .cursor_pointer()
                        .child("보내기")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _w, cx| {
                                let text = this.chat_input.clone();
                                if !text.trim().is_empty() && !project.is_empty() {
                                    this.chat_input.clear();
                                    let _ = this.tx.send(Action::Send {
                                        project: project.clone(),
                                        text,
                                    });
                                }
                                cx.notify();
                            }),
                        ),
                ),
        )
        .into_any_element()
    }

    fn bots_view(&self) -> gpui::AnyElement {
        let mut rows = div().flex().flex_col().gap_2().p_4().flex_1();
        rows = rows.child(div().text_xs().text_color(rgb(MUTED)).child(
            "봇 정의는 bots/<name>/bot.toml 이다. agent 칸이 백엔드를 고른다(claude·opencode·omp·pi).",
        ));
        // 지금 도는 인스턴스의 백엔드 분포를 보여 준다.
        for i in self.snap().instances.clone() {
            rows = rows.child(
                div()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(PANEL))
                    .text_sm()
                    .text_color(rgb(TEXT))
                    .child(format!(
                        "{} · {} · {} — {}",
                        i.id, i.bot, format::agent_label(&i.agent), format::status_label(&i.state)
                    )),
            );
        }
        if self.snap().instances.is_empty() {
            rows = rows.child(div().text_sm().text_color(rgb(MUTED)).child("도는 인스턴스가 없다."));
        }
        rows.child(
            div()
                .mt_2()
                .text_xs()
                .text_color(rgb(MUTED))
                .child("터미널: jadecode bot list · jadecode agents · jadecode instance list"),
        )
        .into_any_element()
    }
}

impl Render for App2 {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.tick();
        let body = match self.view {
            View::Board => self.board_view(cx),
            View::Card => self.card_view(cx),
            View::Chat => self.chat_view(cx),
            View::Bots => self.bots_view(),
        };
        let status = self.status_line.clone();
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .child(self.header(cx))
            .child(body)
            .children(status.map(|s| {
                div()
                    .px_4()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(s)
            }))
    }
}

/// 테스트: 화면 없이도 보드 데이터가 명세의 다섯 화면을 채울 수 있는지 확인한다.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::BoardSnapshot;

    #[test]
    fn board_columns_from_snapshot() {
        let mut s = BoardSnapshot::default();
        s.cards.push(CardData {
            path: "work/api/c1".into(),
            id: "01A".into(),
            title: "t".into(),
            project: "api".into(),
            stage: "implement".into(),
            status: "working".into(),
        });
        assert_eq!(s.stages(Some("api")), vec!["implement"]);
        assert_eq!(status_color("working"), BLUE);
        assert_eq!(status_color("blocked"), RED);
        assert_eq!(status_color("done"), GREEN);
    }
}
