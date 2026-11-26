//! GPUI 화면.
//!
//! ```text
//! ┌──────────┬───────────────────────────────────────┬──────────────────┐
//! │ 프로젝트 │ 칸반: 할 일 · 진행 중 · 확인 필요 · 완료 │ 프로젝트 봇 대화 │
//! │ 목록     │ (메모 = md 파일)                       │ 또는 메모 작업    │
//! └──────────┴───────────────────────────────────────┴──────────────────┘
//! ```
//!
//! 상태와 동작은 `jc_core::Engine`이 갖는다. 화면은 120ms마다 `tick`을 불러 바뀐 것이 있으면 다시 그린다.

pub mod input;
pub mod theme;

use gpui::prelude::*;
use gpui::{
    actions, div, px, rgb, rgba, size, AnyElement, App, Application, Bounds, ClickEvent, Context,
    Div, ElementId, Entity, FocusHandle, Focusable, KeyBinding, Menu, MenuItem, OsAction,
    ScrollHandle, SharedString, Stateful, Subscription, SystemMenuType, TitlebarOptions, Window,
    WindowBounds, WindowOptions,
};
use input::{InputEvent, TextInput};
use jc_core::log::{Entry, Kind};
use jc_core::markdown::{markdown_blocks, strip_inline, MdBlock};
use jc_core::workspace::tilde;
use jc_core::{Column, Engine, Key, Memo, Workspace};
use std::time::{Duration, Instant};
use theme::*;

actions!(
    jadecode,
    [
        Quit,
        NewMemo,
        NewProject,
        CloseModal,
        Refresh,
        FocusComposer,
        CheckUpdate,
        About,
        OpenLogs,
        ReportIssue,
    ]
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modal {
    NewProject,
    NewMemo(Column),
}

/// 칸반 타일을 끌 때 함께 가는 값.
#[derive(Clone)]
struct DragMemo {
    stem: String,
    title: String,
    from: Column,
}

struct DragPreview(String);

impl Render for DragPreview {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_2()
            .w(px(220.))
            .rounded_md()
            .bg(rgb(RAISED))
            .border_1()
            .border_color(rgb(ACCENT))
            .text_sm()
            .text_color(rgb(TEXT))
            .truncate()
            .child(self.0.clone())
    }
}

/// "PR 만들기" 버튼이 에이전트에게 보내는 말.
const PR_PROMPT: &str =
    "지금까지 바꾼 내용을 확인하고 커밋한 뒤, 이 브랜치를 push해서 Draft PR을 열어 주세요. \
기본 브랜치에 직접 병합하지는 마세요. 끝나면 PR 링크를 알려 주세요.";

pub struct Main {
    engine: Engine,
    focus: FocusHandle,
    project: Option<String>,
    /// 오른쪽 칸에 연 메모. 없으면 봇 대화.
    memo: Option<String>,
    /// 메모 맥락(md)을 펼쳤는가.
    show_context: bool,
    modal: Option<Modal>,
    toast: Option<(String, bool, Instant)>,
    update_available: Option<String>,
    updating: bool,
    bot_input: Entity<TextInput>,
    memo_input: Entity<TextInput>,
    project_name: Entity<TextInput>,
    project_repo: Entity<TextInput>,
    memo_title: Entity<TextInput>,
    memo_body: Entity<TextInput>,
    chat_scroll: ScrollHandle,
    /// 마지막으로 그린 대화(열쇠, 길이). 길어지면 맨 아래로 민다.
    last_chat: Option<(Key, usize)>,
    last_title: String,
    seen_version: u64,
    _subs: Vec<Subscription>,
}

// ---------- 작은 부품 ----------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Btn {
    Primary,
    Ghost,
    Danger,
    Ok,
}

fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: Btn) -> Stateful<Div> {
    let (bg, fg, hover) = match kind {
        Btn::Primary => (ACCENT, WHITE, 0x74a0ff),
        Btn::Ghost => (RAISED, TEXT, HOVER),
        Btn::Danger => (0x3a1f22, RED, 0x4a2528),
        Btn::Ok => (0x1c3a2c, GREEN, 0x234a37),
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
        .pt_4()
        .pb_1()
        .text_xs()
        .text_color(rgb(FAINT))
        .child(t.into())
}

fn chip(label: impl Into<SharedString>, color: u32) -> Div {
    div()
        .flex_none()
        .px_2()
        .py(px(1.))
        .rounded_md()
        .border_1()
        .border_color(rgb(color))
        .text_xs()
        .text_color(rgb(color))
        .child(label.into())
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

/// 데스크톱 알림. 실패해도 조용히 넘어간다.
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

fn issue_url() -> String {
    let body = format!(
        "**무엇을 하다가**\n\n**무슨 일이 있었나**\n\n**기대한 것**\n\n---\n판: {}\n운영체제: {} {}",
        jc_core::version::VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH,
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
    format!("https://github.com/jadewisemann/jadecode/issues/new?body={enc}")
}

/// 마크다운 블록을 그린다.
fn markdown(md: &str) -> Div {
    let mut col = div().flex().flex_col().gap_2();
    for b in markdown_blocks(md) {
        col = col.child(match b {
            MdBlock::Heading(l, t) => {
                let d = div()
                    .text_color(rgb(TEXT))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .pt_1();
                match l {
                    1 => d.text_lg(),
                    _ => d.text_base(),
                }
                .child(strip_inline(&t))
            }
            MdBlock::Para(t) => div()
                .text_sm()
                .text_color(rgb(TEXT))
                .child(strip_inline(&t)),
            MdBlock::Bullet(ind, t) => div()
                .flex()
                .flex_row()
                .gap_2()
                .pl(px(12. * ind as f32))
                .text_sm()
                .text_color(rgb(TEXT))
                .child(div().text_color(rgb(FAINT)).child("•"))
                .child(div().flex_1().min_w(px(0.)).child(strip_inline(&t))),
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
                .child(div().flex_1().min_w(px(0.)).child(strip_inline(&t))),
            MdBlock::Code(lines) => {
                let mut c = div()
                    .flex()
                    .flex_col()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(INPUT))
                    .font_family(MONO)
                    .text_xs()
                    .text_color(rgb(TEXT));
                for l in lines {
                    c = c.child(div().child(if l.is_empty() { " ".to_string() } else { l }));
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
    col
}

/// 대화 한 줄.
fn entry_view(i: usize, e: &Entry) -> AnyElement {
    match e.kind {
        Kind::User => div()
            .flex()
            .flex_row()
            .child(div().flex_1())
            .child(
                div()
                    .max_w(px(340.))
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .bg(rgb(HUMAN_BUBBLE))
                    .text_sm()
                    .text_color(rgb(WHITE))
                    .child(e.text.clone()),
            )
            .into_any_element(),
        Kind::Assistant => div()
            .id(("say", i))
            .pr_4()
            .child(markdown(&e.text))
            .into_any_element(),
        Kind::Tool => div()
            .flex()
            .flex_row()
            .gap_2()
            .text_xs()
            .font_family(MONO)
            .text_color(rgb(FAINT))
            .child("›")
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .child(e.text.clone()),
            )
            .into_any_element(),
        Kind::Note => div()
            .px_3()
            .py_1()
            .rounded_md()
            .bg(rgb(0x221d33))
            .text_xs()
            .text_color(rgb(VIOLET))
            .child(e.text.clone())
            .into_any_element(),
        Kind::Error => div()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(0x3a1f22))
            .text_xs()
            .text_color(rgb(0xffd6d3))
            .child(e.text.clone())
            .into_any_element(),
        Kind::Turn => div()
            .text_xs()
            .text_color(rgb(FAINT))
            .child(e.text.clone())
            .into_any_element(),
    }
}

/// 실행 진입점.
pub fn run(workspace: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    jc_core::agent::augment_path();
    let ws = Workspace::resolve(workspace.as_deref())?;

    Application::new().run(move |cx: &mut App| {
        input::bind_keys(cx);
        cx.bind_keys([
            KeyBinding::new("secondary-q", Quit, None),
            KeyBinding::new("secondary-n", NewMemo, None),
            KeyBinding::new("secondary-shift-n", NewProject, None),
            KeyBinding::new("secondary-r", Refresh, None),
            KeyBinding::new("secondary-k", FocusComposer, None),
            KeyBinding::new("escape", CloseModal, Some("Main")),
        ]);
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.on_action(|_: &OpenLogs, _cx: &mut App| {
            let dir = crate::logging::log_dir();
            let _ = std::fs::create_dir_all(&dir);
            open_path(&dir.display().to_string());
        });
        cx.on_action(|_: &ReportIssue, _cx: &mut App| open_path(&issue_url()));
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
                    MenuItem::action("새 메모", NewMemo),
                    MenuItem::action("새 프로젝트", NewProject),
                    MenuItem::separator(),
                    MenuItem::action("새로고침", Refresh),
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
                name: "도움말".into(),
                items: vec![
                    MenuItem::action("문제 보고하기…", ReportIssue),
                    MenuItem::action("로그 폴더 열기", OpenLogs),
                ],
            },
        ]);

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
                    window_min_size: Some(size(px(900.), px(560.))),
                    app_id: Some("dev.jadewisemann.jadecode".into()),
                    ..Default::default()
                },
                move |window, cx| cx.new(|cx| Main::new(window, cx, Engine::new(ws))),
            )
            .expect("창을 열 수 없다");
        window
            .update(cx, |m, window, cx| {
                window.focus(&m.focus_handle(cx));
                // 화면 확인용: 화면을 차례로 띄우며 스크린샷을 찍고 끝난다(CI에서 쓴다).
                if let Some(dir) = std::env::var_os("JADECODE_SHOTS") {
                    m.start_shot_tour(std::path::PathBuf::from(dir), window, cx);
                }
            })
            .ok();
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
    Ok(())
}

impl Focusable for Main {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Main {
    fn new(window: &mut Window, cx: &mut Context<Self>, engine: Engine) -> Self {
        let bot_input = cx.new(|cx| {
            TextInput::new(cx, "봇에게 말하기 · Enter 보내기 · Shift-Enter 줄바꿈").multiline(8)
        });
        let memo_input = cx.new(|cx| {
            TextInput::new(cx, "에이전트에게 말하기 · 보내면 진행 중으로 옮깁니다").multiline(8)
        });
        let project_name = cx.new(|cx| TextInput::new(cx, "프로젝트 이름"));
        let project_repo = cx.new(|cx| TextInput::new(cx, "코드 레포 폴더 · 예: ~/code/myapp"));
        let memo_title = cx.new(|cx| TextInput::new(cx, "제목"));
        let memo_body = cx.new(|cx| {
            TextInput::new(cx, "맥락: 목표, 완료 조건, 관련 파일 · Shift-Enter 줄바꿈")
                .multiline(10)
        });

        let mut subs = vec![];
        subs.push(cx.subscribe_in(
            &bot_input,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Submitted(t) => this.send_bot(t.clone(), cx),
                InputEvent::Cancelled => window.focus(&this.focus),
                InputEvent::Changed => {}
            },
        ));
        subs.push(cx.subscribe_in(
            &memo_input,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Submitted(t) => this.send_memo(t.clone(), cx),
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
            &memo_title,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Submitted(_) => window.focus(&this.memo_body.focus_handle(cx)),
                InputEvent::Cancelled => this.close_modal(window, cx),
                InputEvent::Changed => {}
            },
        ));
        subs.push(cx.subscribe_in(
            &memo_body,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Submitted(_) => this.submit_memo(window, cx),
                InputEvent::Cancelled => this.close_modal(window, cx),
                InputEvent::Changed => {}
            },
        ));

        // 앱을 닫으면 돌던 에이전트도 끝낸다(따로 프로세스 그룹이라 저절로 끝나지 않는다).
        subs.push(cx.on_app_quit(|this, _cx| {
            this.engine.stop_all();
            async {}
        }));

        // 엔진 주기 작업.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            if this.update(cx, |this, cx| this.pump(cx)).is_err() {
                break;
            }
        })
        .detach();

        // 새 판 확인(개발 빌드는 건너뛴다).
        if !jc_core::version::is_dev_build() {
            Self::check_update(cx, false);
        }

        let project = engine.projects().first().map(|p| p.slug.clone());
        Self {
            engine,
            focus: cx.focus_handle(),
            project,
            memo: None,
            show_context: false,
            modal: None,
            toast: None,
            update_available: None,
            updating: false,
            bot_input,
            memo_input,
            project_name,
            project_repo,
            memo_title,
            memo_body,
            chat_scroll: ScrollHandle::new(),
            last_chat: None,
            last_title: String::new(),
            seen_version: 0,
            _subs: subs,
        }
    }

    fn pump(&mut self, cx: &mut Context<Self>) {
        let changed = self.engine.tick();
        for n in self.engine.take_notices() {
            notify_desktop(&n.title, &n.body);
        }
        // 고른 프로젝트·메모가 사라졌으면(파일을 지웠거나 옮겼으면) 고른 것을 정리한다.
        if let Some(p) = &self.project {
            if self.engine.project(p).is_none() {
                self.project = self.engine.projects().first().map(|p| p.slug.clone());
                self.memo = None;
            }
        } else if let Some(first) = self.engine.projects().first() {
            self.project = Some(first.slug.clone());
        }
        if let (Some(p), Some(m)) = (&self.project, &self.memo) {
            if self.engine.memo(p, m).is_none() {
                self.memo = None;
            }
        }
        let toast_expired = self
            .toast
            .as_ref()
            .is_some_and(|(_, err, t)| t.elapsed() > Duration::from_secs(if *err { 7 } else { 4 }));
        if toast_expired {
            self.toast = None;
        }
        if changed || toast_expired || self.engine.version != self.seen_version {
            self.seen_version = self.engine.version;
            cx.notify();
        }
    }

    fn toast(&mut self, msg: impl Into<String>, err: bool) {
        self.toast = Some((msg.into(), err, Instant::now()));
    }

    fn check_update(cx: &mut Context<Self>, manual: bool) {
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async { jc_core::update::check() })
                .await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(Some(tag)) => this.update_available = Some(tag),
                    Ok(None) if manual => this.toast("이미 최신 판입니다", false),
                    Err(e) if manual => {
                        this.toast(format!("업데이트를 확인하지 못했습니다: {e:#}"), true)
                    }
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn install_update(&mut self, cx: &mut Context<Self>) {
        let Some(tag) = self.update_available.clone() else {
            return;
        };
        self.updating = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let exe = std::env::current_exe()?;
                    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
                    let dir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                    jc_core::update::install(&tag, &dir, &|s| eprintln!("{s}"))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.updating = false;
                match res {
                    Ok(()) => {
                        this.update_available = None;
                        this.toast(
                            "업데이트했습니다. 앱을 다시 열면 새 판으로 실행됩니다.",
                            false,
                        );
                    }
                    Err(e) => this.toast(format!("업데이트 실패: {e:#}"), true),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---------- 동작 ----------

    fn select_project(&mut self, slug: String, cx: &mut Context<Self>) {
        self.project = Some(slug);
        self.memo = None;
        cx.notify();
    }

    fn open_memo(&mut self, stem: String, cx: &mut Context<Self>) {
        if let Some(p) = self.project.clone() {
            self.engine.refresh_diff(&Key::Memo(p, stem.clone()));
        }
        self.memo = Some(stem);
        self.show_context = false;
        cx.notify();
    }

    fn close_memo(&mut self, cx: &mut Context<Self>) {
        self.memo = None;
        cx.notify();
    }

    fn send_bot(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(p) = self.project.clone() else {
            return;
        };
        if let Err(e) = self.engine.send_bot(&p, &text) {
            self.toast(format!("{e:#}"), true);
            // 보내지 못했으면 입력을 되돌린다.
            self.bot_input.update(cx, |i, cx| i.set_text(text, cx));
        }
        self.chat_scroll.scroll_to_bottom();
        cx.notify();
    }

    fn send_memo(&mut self, text: String, cx: &mut Context<Self>) {
        let (Some(p), Some(m)) = (self.project.clone(), self.memo.clone()) else {
            return;
        };
        if let Err(e) = self.engine.send_memo(&p, &m, &text) {
            self.toast(format!("{e:#}"), true);
            self.memo_input.update(cx, |i, cx| i.set_text(text, cx));
        }
        self.chat_scroll.scroll_to_bottom();
        cx.notify();
    }

    fn move_memo(&mut self, stem: &str, to: Column, cx: &mut Context<Self>) {
        let Some(p) = self.project.clone() else {
            return;
        };
        if let Err(e) = self.engine.move_memo(&p, stem, to) {
            self.toast(format!("{e:#}"), true);
        }
        cx.notify();
    }

    fn open_modal(&mut self, m: Modal, window: &mut Window, cx: &mut Context<Self>) {
        let m = if matches!(m, Modal::NewMemo(_)) && self.project.is_none() {
            Modal::NewProject
        } else {
            m
        };
        self.modal = Some(m);
        let target = match m {
            Modal::NewProject => self.project_name.focus_handle(cx),
            Modal::NewMemo(_) => self.memo_title.focus_handle(cx),
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
        match self.engine.create_project(&name, &repo) {
            Ok(p) => {
                self.project = Some(p.slug.clone());
                self.memo = None;
                self.project_name.update(cx, |i, cx| i.clear(cx));
                self.project_repo.update(cx, |i, cx| i.clear(cx));
                self.toast(format!("{} 프로젝트를 만들었습니다", p.name()), false);
                self.modal = None;
                window.focus(&self.bot_input.focus_handle(cx));
            }
            Err(e) => self.toast(format!("{e:#}"), true),
        }
        cx.notify();
    }

    fn submit_memo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::NewMemo(col)) = self.modal else {
            return;
        };
        let Some(p) = self.project.clone() else {
            return;
        };
        let title = self.memo_title.read(cx).text();
        let body = self.memo_body.read(cx).text();
        match self.engine.create_memo(&p, &title, &body, col) {
            Ok(m) => {
                self.memo_title.update(cx, |i, cx| i.clear(cx));
                self.memo_body.update(cx, |i, cx| i.clear(cx));
                self.modal = None;
                window.focus(&self.focus);
                self.open_memo(m.stem, cx);
            }
            Err(e) => self.toast(format!("{e:#}"), true),
        }
        cx.notify();
    }

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
                this.project_repo
                    .update(cx, |i, cx| i.set_text(tilde(&p), cx));
                if this.project_name.read(cx).text().trim().is_empty() {
                    if let Some(n) = p.file_name() {
                        let n = n.to_string_lossy().to_string();
                        this.project_name.update(cx, |i, cx| i.set_text(n, cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---------- 그리기 ----------

    fn sidebar(&mut self, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut list = div().flex().flex_col().gap(px(2.)).px_2();
        let projects: Vec<(String, String)> = self
            .engine
            .projects()
            .iter()
            .map(|p| (p.slug.clone(), p.name().to_string()))
            .collect();
        for (slug, name) in projects {
            let selected = self.project.as_deref() == Some(&slug);
            let memos = self.engine.memos(&slug);
            let review = memos.iter().filter(|m| m.column == Column::Review).count();
            let doing = memos.iter().filter(|m| m.column == Column::Doing).count();
            let bot_busy = self.engine.is_running(&Key::Bot(slug.clone()));
            let s2 = slug.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("proj-{slug}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py(px(6.))
                    .rounded_md()
                    .cursor_pointer()
                    .bg(rgb(if selected { SELECTED } else { SIDEBAR }))
                    .hover(|s| s.bg(rgb(HOVER)))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_sm()
                            .text_color(rgb(if selected { TEXT } else { MUTED }))
                            .child(name),
                    )
                    .when(doing > 0 || bot_busy, |d| d.child(dot(ACCENT)))
                    .when(review > 0, |d| d.child(badge(review, AMBER)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.select_project(s2.clone(), cx)
                    })),
            );
        }
        div()
            .id("sidebar")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(200.))
            .h_full()
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .px_4()
                    .pt_4()
                    .pb_1()
                    .text_base()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(TEXT))
                    .child("jadecode"),
            )
            .child(section_label("프로젝트"))
            .child(list)
            .child(
                div()
                    .px_2()
                    .pt_1()
                    .child(
                        link_btn("new-project", "+ 새 프로젝트").on_click(cx.listener(
                            |this, _: &ClickEvent, w, cx| this.open_modal(Modal::NewProject, w, cx),
                        )),
                    ),
            )
            .child(div().flex_1())
            .children(self.update_available.clone().map(|tag| {
                div().p_3().child(
                    div()
                        .id("update")
                        .p_2()
                        .rounded_md()
                        .bg(rgb(0x1d2a47))
                        .text_xs()
                        .text_color(rgb(TEXT))
                        .cursor_pointer()
                        .child(if self.updating {
                            "업데이트하는 중…".to_string()
                        } else {
                            format!("새 판 {tag} · 눌러서 업데이트")
                        })
                        .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                            if !this.updating {
                                this.install_update(cx)
                            }
                        })),
                )
            }))
            .child(
                div()
                    .px_4()
                    .pb_3()
                    .text_xs()
                    .text_color(rgb(FAINT))
                    .truncate()
                    .child(tilde(&self.engine.ws.root)),
            )
    }

    fn header(&self, cx: &mut Context<Self>) -> Div {
        let Some(p) = self
            .project
            .as_deref()
            .and_then(|s| self.engine.project(s))
            .cloned()
        else {
            return div();
        };
        let repo = p.repo();
        let dir = p.dir.clone();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(TEXT))
                    .child(p.name().to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_xs()
                    .text_color(rgb(FAINT))
                    .child(match &repo {
                        Some(r) => tilde(r),
                        None => "코드 레포가 없습니다 · project.toml의 repo를 채우세요".into(),
                    }),
            )
            .children(repo.map(|r| {
                link_btn("open-repo", "레포 열기")
                    .on_click(move |_, _, _| open_path(&r.display().to_string()))
            }))
            .child(
                link_btn("open-board", "보드 폴더")
                    .on_click(move |_, _, _| open_path(&dir.display().to_string())),
            )
            .child(
                button("new-memo-top", "+ 메모", Btn::Primary).on_click(cx.listener(
                    |this, _: &ClickEvent, w, cx| {
                        this.open_modal(Modal::NewMemo(Column::Todo), w, cx)
                    },
                )),
            )
    }

    fn board(&mut self, cx: &mut Context<Self>) -> Stateful<Div> {
        let Some(slug) = self.project.clone() else {
            return div().id("board");
        };
        let memos: Vec<Memo> = self.engine.memos(&slug).to_vec();
        let mut cols = div()
            .id("board")
            .flex()
            .flex_row()
            .gap_3()
            .p_4()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .overflow_x_scroll();
        for c in Column::ALL {
            let items: Vec<&Memo> = memos.iter().filter(|m| m.column == c).collect();
            let mut col = div()
                .id(SharedString::from(format!("col-{}", c.dir_name())))
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(176.))
                .gap_2()
                .h_full()
                .p_2()
                .rounded_lg()
                .bg(rgb(PANEL))
                .border_1()
                .border_color(rgb(PANEL))
                .overflow_y_scroll()
                .drag_over::<DragMemo>(|st, _, _, _| st.border_color(rgb(ACCENT)))
                .on_drop(cx.listener(move |this, d: &DragMemo, _w, cx| {
                    if d.from != c {
                        this.move_memo(&d.stem, c, cx);
                    }
                }))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .px_1()
                        .pb_1()
                        .child(dot(column_color(c)))
                        .child(div().text_xs().text_color(rgb(MUTED)).child(c.label()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(FAINT))
                                .child(items.len().to_string()),
                        )
                        .child(div().flex_1())
                        .when(c == Column::Todo, |d| {
                            d.child(link_btn("add-todo", "+").on_click(cx.listener(
                                |this, _: &ClickEvent, w, cx| {
                                    this.open_modal(Modal::NewMemo(Column::Todo), w, cx)
                                },
                            )))
                        }),
                );
            if items.is_empty() {
                col = col.child(div().px_2().py_3().text_xs().text_color(rgb(FAINT)).child(
                    match c {
                        Column::Todo => "봇에게 말하거나 + 로 메모를 쓰세요",
                        Column::Doing => "여기로 옮기면 에이전트가 시작합니다",
                        Column::Review => "에이전트가 마친 일이 여기 옵니다",
                        Column::Done => "",
                    },
                ));
            }
            for m in items {
                col = col.child(self.tile(&slug, m, cx));
            }
            cols = cols.child(col);
        }
        cols
    }

    fn tile(&mut self, slug: &str, m: &Memo, cx: &mut Context<Self>) -> Stateful<Div> {
        let k = Key::Memo(slug.to_string(), m.stem.clone());
        let running = self.engine.is_running(&k);
        let selected = self.memo.as_deref() == Some(&m.stem);
        let sub = match m.column {
            Column::Doing | Column::Review => self
                .engine
                .last_reply(&k)
                .unwrap_or_else(|| m.preview.clone()),
            _ => m.preview.clone(),
        };
        let stem = m.stem.clone();
        let drag = DragMemo {
            stem: m.stem.clone(),
            title: m.title.clone(),
            from: m.column,
        };
        let foot = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .text_xs()
            .when(running, |d| {
                d.child(dot(ACCENT))
                    .child(div().text_color(rgb(ACCENT)).child("일하는 중"))
            })
            .when(!running && m.column == Column::Review, |d| {
                d.child(div().text_color(rgb(AMBER)).child("확인 필요"))
            })
            .child(div().flex_1())
            .child(
                div()
                    .text_color(rgb(FAINT))
                    .truncate()
                    .child(format!("{}.md", m.stem)),
            );
        div()
            .id(SharedString::from(format!("tile-{}", m.stem)))
            .on_drag(drag, |d, _offset, _w, cx| {
                let t = d.title.clone();
                cx.new(|_| DragPreview(t))
            })
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(10.))
            .rounded_md()
            .bg(rgb(if selected { SELECTED } else { RAISED }))
            .border_1()
            .border_color(rgb(if selected {
                ACCENT
            } else if m.column == Column::Review {
                0x5a4520
            } else {
                LINE
            }))
            .hover(|s| s.bg(rgb(HOVER)))
            .cursor_pointer()
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(TEXT))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(m.title.clone()),
            )
            .when(!sub.is_empty(), |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .line_clamp(2)
                        .child(sub),
                )
            })
            .child(foot)
            .on_click(
                cx.listener(move |this, _: &ClickEvent, _w, cx| this.open_memo(stem.clone(), cx)),
            )
    }

    /// 대화 기록 칸(봇·메모 공통).
    fn transcript(&mut self, k: &Key, empty: &'static str) -> Stateful<Div> {
        let entries: Vec<Entry> = self.engine.log(k).to_vec();
        let running = self.engine.is_running(k);
        let len = entries.len() + running as usize;
        if self.last_chat.as_ref() != Some(&(k.clone(), len)) {
            self.last_chat = Some((k.clone(), len));
            self.chat_scroll.scroll_to_bottom();
        }
        let mut col = div()
            .id("chat")
            .flex()
            .flex_col()
            .gap_3()
            .px_4()
            .py_3()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .track_scroll(&self.chat_scroll);
        if entries.is_empty() && !running {
            col = col.child(div().pt_8().text_sm().text_color(rgb(MUTED)).child(empty));
        }
        // 기록이 아주 길면 최근 것만 그린다(위쪽은 파일에 그대로 남아 있다).
        let skip = entries.len().saturating_sub(400);
        for (i, e) in entries.iter().enumerate().skip(skip) {
            col = col.child(entry_view(i, e));
        }
        if running {
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(ACCENT))
                    .child(dot(ACCENT))
                    .child("일하는 중…"),
            );
        }
        col
    }

    fn bot_pane(&mut self, cx: &mut Context<Self>) -> Div {
        let Some(slug) = self.project.clone() else {
            return div();
        };
        let k = Key::Bot(slug);
        let running = self.engine.is_running(&k);
        let k2 = k.clone();
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(div().text_sm().text_color(rgb(TEXT)).child("프로젝트 봇"))
                    .child(div().flex_1())
                    .when(running, |d| {
                        d.child(button("bot-stop", "멈추기", Btn::Danger).on_click(cx.listener(
                            move |this, _: &ClickEvent, _w, cx| {
                                this.engine.stop(&k2);
                                cx.notify();
                            },
                        )))
                    }),
            )
            .child(self.transcript(
                &k,
                "무엇을 하고 싶은지 말해 보세요. 봇이 일을 메모로 정리해 보드에 올리고, 진행 중으로 옮긴 메모는 코딩 에이전트가 맡습니다.",
            ))
            .child(div().p_3().child(self.bot_input.clone()))
    }

    fn memo_pane(&mut self, stem: &str, cx: &mut Context<Self>) -> Div {
        let Some(slug) = self.project.clone() else {
            return div();
        };
        let Some(m) = self.engine.memo(&slug, stem).cloned() else {
            return div();
        };
        let k = Key::Memo(slug.clone(), stem.to_string());
        let running = self.engine.is_running(&k);
        let branch = self.engine.branch(&slug, stem);
        let work_dir = self.engine.work_dir(&slug, stem).filter(|d| d.exists());
        let diff = self.engine.diff(&k);
        let mut actions = div().flex().flex_row().flex_wrap().gap_2();
        let s = stem.to_string();
        if running {
            let k2 = k.clone();
            actions = actions.child(
                button("m-stop", "멈추기", Btn::Danger).on_click(cx.listener(
                    move |this, _: &ClickEvent, _w, cx| {
                        this.engine.stop(&k2);
                        cx.notify();
                    },
                )),
            );
        } else {
            match m.column {
                Column::Todo => {
                    let s = s.clone();
                    actions = actions.child(button("m-start", "시작", Btn::Primary).on_click(
                        cx.listener(move |this, _: &ClickEvent, _w, cx| {
                            this.move_memo(&s, Column::Doing, cx)
                        }),
                    ));
                }
                Column::Review | Column::Doing => {
                    let s1 = s.clone();
                    actions = actions.child(button("m-done", "완료", Btn::Ok).on_click(
                        cx.listener(move |this, _: &ClickEvent, _w, cx| {
                            this.move_memo(&s1, Column::Done, cx)
                        }),
                    ));
                    if branch.is_some() {
                        actions = actions.child(button("m-pr", "PR 만들기", Btn::Ghost).on_click(
                            cx.listener(|this, _: &ClickEvent, _w, cx| {
                                this.send_memo(PR_PROMPT.to_string(), cx)
                            }),
                        ));
                    }
                }
                Column::Done => {
                    let s1 = s.clone();
                    actions = actions.child(button("m-reopen", "다시 열기", Btn::Ghost).on_click(
                        cx.listener(move |this, _: &ClickEvent, _w, cx| {
                            this.move_memo(&s1, Column::Review, cx)
                        }),
                    ));
                }
            }
        }
        let path = m.path.clone();
        actions = actions.child(
            button("m-edit", "메모 편집", Btn::Ghost)
                .on_click(move |_, _, _| open_path(&path.display().to_string())),
        );
        if let Some(d) = work_dir.clone() {
            actions = actions.child(
                button("m-folder", "작업 폴더", Btn::Ghost)
                    .on_click(move |_, _, _| open_path(&d.display().to_string())),
            );
        }
        let meta = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(rgb(FAINT))
            .child(chip(m.column.label(), column_color(m.column)))
            .children(branch.map(|b| div().font_family(MONO).child(b)))
            .children(diff.map(|(f, a, d)| {
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .child(format!("파일 {f}"))
                    .child(div().text_color(rgb(GREEN)).child(format!("+{a}")))
                    .child(div().text_color(rgb(RED)).child(format!("−{d}")))
            }));
        let context = div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .id("ctx-toggle")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(if self.show_context { "▾" } else { "▸" })
                    .child(format!("맥락 · {}/{}.md", m.column.dir_name(), m.stem))
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.show_context = !this.show_context;
                        cx.notify();
                    })),
            )
            .when(self.show_context, |d| {
                d.child(
                    div()
                        .id("ctx")
                        .max_h(px(300.))
                        .overflow_y_scroll()
                        .p_3()
                        .rounded_md()
                        .bg(rgb(PANEL))
                        .child(markdown(&m.text)),
                )
            });
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(link_btn("back", "← 봇").on_click(cx.listener(
                                |this, _: &ClickEvent, _w, cx| this.close_memo(cx),
                            )))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(TEXT))
                                    .child(m.title.clone()),
                            ),
                    )
                    .child(meta)
                    .child(actions),
            )
            .child(context)
            .child(self.transcript(
                &k,
                "아직 시작하지 않았습니다. 시작하면 이 메모의 내용을 맥락으로 코딩 에이전트가 전용 브랜치에서 일합니다.",
            ))
            .child(div().p_3().child(self.memo_input.clone()))
    }

    fn welcome(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .size_full()
            .child(div().text_xl().text_color(rgb(TEXT)).child("jadecode"))
            .child(
                div()
                    .max_w(px(520.))
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child("프로젝트마다 봇과 대화하고, 할 일은 칸반의 md 메모로 남깁니다. 진행 중으로 옮긴 메모는 Claude Code가 그 메모를 맥락으로 전용 브랜치에서 작업합니다."),
            )
            .child(
                button("welcome-create", "프로젝트 만들기", Btn::Primary).on_click(cx.listener(
                    |this, _: &ClickEvent, w, cx| this.open_modal(Modal::NewProject, w, cx),
                )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(FAINT))
                    .child(format!("워크스페이스: {}", tilde(&self.engine.ws.root))),
            )
    }

    fn modal_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.modal?;
        let mut dialog = div()
            .id("dialog")
            .flex()
            .flex_col()
            .gap_3()
            .w(px(560.))
            .p_5()
            .rounded_lg()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(LINE))
            .shadow_lg()
            .on_click(|_, _, cx| cx.stop_propagation());
        let foot =
            |ok_id: &'static str, ok: &'static str, cx: &mut Context<Self>| {
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(div().flex_1())
                    .child(button("dlg-cancel", "취소", Btn::Ghost).on_click(
                        cx.listener(|this, _: &ClickEvent, w, cx| this.close_modal(w, cx)),
                    ))
                    .child(button(ok_id, ok, Btn::Primary).on_click(cx.listener(
                        move |this, _: &ClickEvent, w, cx| match this.modal {
                            Some(Modal::NewProject) => this.submit_project(w, cx),
                            Some(Modal::NewMemo(_)) => this.submit_memo(w, cx),
                            None => {}
                        },
                    )))
            };
        match m {
            Modal::NewProject => {
                dialog = dialog
                    .child(div().text_lg().text_color(rgb(TEXT)).child("새 프로젝트"))
                    .child(self.project_name.clone())
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().child(self.project_repo.clone()))
                            .child(button("pick-repo", "폴더 고르기…", Btn::Ghost).on_click(
                                cx.listener(|this, _: &ClickEvent, _w, cx| this.pick_repo_folder(cx)),
                            )),
                    )
                    .child(div().text_xs().text_color(rgb(FAINT)).child(
                        "메모를 시작하면 이 레포에서 브랜치 jc/<메모 이름>과 전용 워크트리를 열어 에이전트가 거기서 일합니다. 원래 작업 폴더는 건드리지 않습니다.",
                    ))
                    .child(foot("np-ok", "만들기", cx));
            }
            Modal::NewMemo(col) => {
                dialog = dialog
                    .child(
                        div()
                            .text_lg()
                            .text_color(rgb(TEXT))
                            .child(format!("새 메모 · {}", col.label())),
                    )
                    .child(self.memo_title.clone())
                    .child(self.memo_body.clone())
                    .child(div().text_xs().text_color(rgb(FAINT)).child(
                        "메모는 todo/ 폴더의 md 파일로 저장되고, 그 내용이 곧 에이전트가 받는 맥락입니다.",
                    ))
                    .child(foot("nm-ok", "저장", cx));
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
                .pt(px(110.))
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
                .left(px(216.))
                .max_w(px(560.))
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

    // ---------- 스크린샷 ----------

    fn start_shot_tour(
        &mut self,
        dir: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = std::fs::create_dir_all(&dir);
        cx.spawn_in(window, async move |this, cx| {
            let exec = cx.background_executor().clone();
            exec.timer(Duration::from_millis(2500)).await;
            const STEPS: [&str; 6] = [
                "01-board-bot",
                "02-memo-review",
                "03-memo-context",
                "04-new-memo",
                "05-new-project",
                "06-narrow",
            ];
            for (i, name) in STEPS.iter().enumerate() {
                let ok = this
                    .update_in(cx, |this, window, cx| {
                        this.modal = None;
                        let first_review = this.project.clone().and_then(|p| {
                            this.engine
                                .memos(&p)
                                .iter()
                                .filter(|m| m.column == Column::Review)
                                .max_by_key(|m| m.stem == "safari-login")
                                .map(|m| m.stem.clone())
                        });
                        match i {
                            0 => this.memo = None,
                            1 => {
                                if let Some(s) = first_review {
                                    this.open_memo(s, cx);
                                }
                            }
                            2 => this.show_context = true,
                            3 => {
                                this.memo = None;
                                this.open_modal(Modal::NewMemo(Column::Todo), window, cx);
                                this.memo_title.update(cx, |t, cx| t.set_text("결제 영수증 메일 문구 수정", cx));
                                this.memo_body.update(cx, |t, cx| {
                                    t.set_text("목표: 영수증 메일의 환불 안내 문구를 새 정책에 맞춘다.\n완료 조건: 템플릿과 스냅숏 테스트가 새 문구를 쓴다.\n관련: templates/receipt.html", cx)
                                });
                            }
                            4 => this.open_modal(Modal::NewProject, window, cx),
                            _ => window.resize(size(px(1000.), px(660.))),
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !ok {
                    break;
                }
                exec.timer(Duration::from_millis(1600)).await;
                let path = dir.join(format!("{name}.png"));
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
}

impl Render for Main {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 창 제목에 확인이 필요한 메모 수를 보인다.
        let review: usize = self
            .engine
            .projects()
            .iter()
            .map(|p| {
                self.engine
                    .memos(&p.slug)
                    .iter()
                    .filter(|m| m.column == Column::Review)
                    .count()
            })
            .sum();
        let title = if review > 0 {
            format!("jadecode ({review})")
        } else {
            "jadecode".to_string()
        };
        if title != self.last_title {
            window.set_window_title(&title);
            self.last_title = title;
        }

        // 오른쪽 칸은 창 너비의 1/3쯤(좁은 화면에서 보드가 가려지지 않게).
        let pane_w = (window.viewport_size().width * 0.33).clamp(px(340.), px(460.));
        let body: AnyElement = if self.project.is_none() {
            self.welcome(cx).into_any_element()
        } else {
            let right = match self.memo.clone() {
                Some(stem) => self.memo_pane(&stem, cx),
                None => self.bot_pane(cx),
            };
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .child(self.header(cx))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_1()
                        .min_h(px(0.))
                        .child(self.board(cx))
                        .child(
                            div()
                                .flex_none()
                                .w(pane_w)
                                .h_full()
                                .border_l_1()
                                .border_color(rgb(LINE))
                                .bg(rgb(SIDEBAR))
                                .child(right),
                        ),
                )
                .into_any_element()
        };
        let sidebar = self.sidebar(cx);
        let modal = self.modal_view(cx);
        let toast = self.toast_view();
        div()
            .key_context("Main")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &NewMemo, w, cx| {
                this.open_modal(Modal::NewMemo(Column::Todo), w, cx)
            }))
            .on_action(
                cx.listener(|this, _: &NewProject, w, cx| {
                    this.open_modal(Modal::NewProject, w, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &CloseModal, w, cx| {
                if this.modal.is_some() {
                    this.close_modal(w, cx)
                } else if this.memo.is_some() {
                    this.close_memo(cx)
                }
            }))
            .on_action(cx.listener(|this, _: &Refresh, _w, cx| {
                this.engine.scan();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FocusComposer, w, cx| {
                let h = if this.memo.is_some() {
                    this.memo_input.focus_handle(cx)
                } else {
                    this.bot_input.focus_handle(cx)
                };
                w.focus(&h);
            }))
            .on_action(cx.listener(|_this, _: &CheckUpdate, _w, cx| {
                Self::check_update(cx, true);
            }))
            .on_action(cx.listener(|this, _: &About, _w, cx| {
                this.toast(
                    format!(
                        "jadecode {} · {}",
                        jc_core::version::VERSION,
                        tilde(&this.engine.ws.root)
                    ),
                    false,
                );
                cx.notify();
            }))
            .flex()
            .flex_row()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .child(sidebar)
            .child(body)
            .children(modal)
            .children(toast)
    }
}
