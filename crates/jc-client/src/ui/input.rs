//! 텍스트 입력. GPUI 0.2.2 예제의 입력기를 바탕으로 했다.
//!
//! - 운영체제 입력기(IME)를 쓴다(`EntityInputHandler`). 한글 조합 중인 글자에 밑줄을 긋고,
//!   후보 창 위치(`bounds_for_range`)를 알려 준다.
//! - 예제에 있던 조합 범위 계산 오류(`range.end`를 더하던 것)를 고쳤다.
//! - 한 줄 칸은 글이 칸보다 길면 커서가 보이도록 옆으로 민다.
//! - 여러 줄 칸(`multiline`)은 칸 너비에서 줄을 바꾸고, 최대 줄 수까지 높이가 자라며 그 뒤로는 위아래로 민다.
//!   Shift-Enter(또는 Alt-Enter)로 줄을 바꾸고, 붙여 넣은 글의 줄바꿈을 지킨다.
//! - 비밀 칸(`secret`)은 글자를 •로 가린다.
//! - Enter는 `Submitted`, Esc는 `Cancelled` 이벤트를 낸다.

use gpui::{
    actions, div, fill, point, prelude::*, px, relative, rgb, rgba, size, App, Bounds,
    ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    SharedString, Style, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine,
};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

use super::theme;

const BULLET: char = '•';

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        Submit,
        Cancel,
        WordLeft,
        WordRight,
        DeleteWordLeft,
        Newline,
        Up,
        Down,
        SelectUp,
        SelectDown,
    ]
);

/// 입력기 키 묶음. 앱 시작 때 한 번 건다.
pub fn bind_keys(cx: &mut App) {
    let ctx = Some("TextInput");
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("secondary-a", SelectAll, ctx),
        KeyBinding::new("secondary-v", Paste, ctx),
        KeyBinding::new("secondary-c", Copy, ctx),
        KeyBinding::new("secondary-x", Cut, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new("cmd-left", Home, ctx),
        KeyBinding::new("cmd-right", End, ctx),
        KeyBinding::new("alt-left", WordLeft, ctx),
        KeyBinding::new("alt-right", WordRight, ctx),
        KeyBinding::new("ctrl-left", WordLeft, ctx),
        KeyBinding::new("ctrl-right", WordRight, ctx),
        KeyBinding::new("alt-backspace", DeleteWordLeft, ctx),
        KeyBinding::new("ctrl-backspace", DeleteWordLeft, ctx),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, ctx),
        KeyBinding::new("enter", Submit, ctx),
        KeyBinding::new("shift-enter", Newline, ctx),
        KeyBinding::new("alt-enter", Newline, ctx),
        KeyBinding::new("up", Up, ctx),
        KeyBinding::new("down", Down, ctx),
        KeyBinding::new("shift-up", SelectUp, ctx),
        KeyBinding::new("shift-down", SelectDown, ctx),
        KeyBinding::new("escape", Cancel, ctx),
    ]);
}

/// 입력기가 내는 이벤트.
#[derive(Debug, Clone)]
pub enum InputEvent {
    Submitted(String),
    Cancelled,
    Changed,
}

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<Layout>,
    last_bounds: Option<Bounds<Pixels>>,
    /// 민 거리(한 줄 칸은 옆으로, 여러 줄 칸은 위아래로).
    last_scroll: Point<Pixels>,
    /// 여러 줄 칸이면 최대 줄 수.
    multiline: Option<usize>,
    /// 글자를 가린다(토큰 등).
    secret: bool,
    is_selecting: bool,
    /// Enter를 눌러도 비우지 않는다(검색칸 등).
    keep_on_submit: bool,
}

impl EventEmitter<InputEvent> for TextInput {}

impl TextInput {
    pub fn new(cx: &mut Context<Self>, placeholder: impl Into<SharedString>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            last_scroll: Point::default(),
            multiline: None,
            secret: false,
            is_selecting: false,
            keep_on_submit: false,
        }
    }

    /// 여러 줄 칸으로 만든다. 높이는 `max_lines`줄까지 자란다.
    pub fn multiline(mut self, max_lines: usize) -> Self {
        self.multiline = Some(max_lines.max(1));
        self
    }

    /// 글자를 •로 가린다.
    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }

    pub fn keep_on_submit(mut self) -> Self {
        self.keep_on_submit = true;
        self
    }

    pub fn text(&self) -> String {
        self.content.to_string()
    }

    pub fn set_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.content = text.into();
        let n = self.content.len();
        self.selected_range = n..n;
        self.marked_range = None;
        cx.notify();
    }

    pub fn set_placeholder(&mut self, p: impl Into<SharedString>, cx: &mut Context<Self>) {
        let p = p.into();
        if p != self.placeholder {
            self.placeholder = p;
            cx.notify();
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.content = "".into();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.last_scroll = Point::default();
        cx.notify();
    }

    fn submit(&mut self, _: &Submit, _w: &mut Window, cx: &mut Context<Self>) {
        // 한글 조합 중 Enter는 조합을 끝내는 키다. 입력기가 먼저 처리한다.
        if self.marked_range.is_some() {
            self.marked_range = None;
            cx.notify();
            return;
        }
        let text = self.content.to_string();
        // 빈 칸에서 Enter도 알린다(선택 칸을 비워 둔 채 제출할 수 있게). 받는 쪽이 거른다.
        if !self.keep_on_submit && !text.trim().is_empty() {
            self.clear(cx);
        }
        cx.emit(InputEvent::Submitted(text));
    }

    fn cancel(&mut self, _: &Cancel, _w: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Cancelled);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous_word(self.cursor_offset()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word(self.cursor_offset()), cx);
    }

    fn delete_word_left(
        &mut self,
        _: &DeleteWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_word(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    /// 커서가 있는 줄(문단)의 시작.
    fn line_start(&self, offset: usize) -> usize {
        self.content[..offset]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0)
    }

    /// 커서가 있는 줄(문단)의 끝.
    fn line_end(&self, offset: usize) -> usize {
        self.content[offset..]
            .find('\n')
            .map(|i| offset + i)
            .unwrap_or(self.content.len())
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_start(self.cursor_offset()), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_end(self.cursor_offset()), cx);
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        if self.multiline.is_some() {
            self.replace_text_in_range(None, "\n", window, cx);
        }
    }

    /// 화면에서 한 줄 위(-1)나 아래(+1)의 같은 x 위치.
    fn vertical_target(&self, dir: i32) -> Option<usize> {
        let layout = self.last_layout.as_ref()?;
        if self.multiline.is_none() || self.content.is_empty() {
            return None;
        }
        let pos = layout.position(self.content_to_disp(self.cursor_offset()));
        let y = pos.y + layout.line_height * (dir as f32) + layout.line_height / 2.;
        if y < px(0.) {
            return Some(0);
        }
        if y >= layout.height {
            return Some(self.content.len());
        }
        Some(self.disp_to_content(layout.index_at(point(pos.x, y))))
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.vertical_target(-1) {
            self.move_to(ix, cx);
        }
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.vertical_target(1) {
            self.move_to(ix, cx);
        }
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.vertical_target(-1) {
            self.select_to(ix, cx);
        }
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.vertical_target(1) {
            self.select_to(ix, cx);
        }
    }

    /// 내용 오프셋 → 화면 글 오프셋(비밀 칸은 글자마다 •).
    fn content_to_disp(&self, offset: usize) -> usize {
        if self.secret {
            self.content[..offset.min(self.content.len())]
                .chars()
                .count()
                * BULLET.len_utf8()
        } else {
            offset
        }
    }

    /// 화면 글 오프셋 → 내용 오프셋.
    fn disp_to_content(&self, offset: usize) -> usize {
        if self.secret {
            let n = offset / BULLET.len_utf8();
            self.content
                .char_indices()
                .nth(n)
                .map(|(i, _)| i)
                .unwrap_or(self.content.len())
        } else {
            offset.min(self.content.len())
        }
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = text.replace("\r\n", "\n");
            let text = if self.multiline.is_some() {
                text
            } else {
                text.replace('\n', " ")
            };
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.secret {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.secret {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(layout)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        let local = point(
            position.x - bounds.left() + self.last_scroll.x,
            position.y - bounds.top() + self.last_scroll.y,
        );
        self.disp_to_content(layout.index_at(local))
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }

    fn previous_word(&self, offset: usize) -> usize {
        self.content
            .split_word_bound_indices()
            .rev()
            .find_map(|(idx, w)| (idx < offset && !w.trim().is_empty()).then_some(idx))
            .unwrap_or(0)
    }

    fn next_word(&self, offset: usize) -> usize {
        self.content
            .split_word_bound_indices()
            .find_map(|(idx, w)| {
                let end = idx + w.len();
                (end > offset && !w.trim().is_empty()).then_some(end)
            })
            .unwrap_or(self.content.len())
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range.take();
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        // 새 선택 범위는 조합 글자 안에서의 위치(UTF-16)다. 조합 글자 시작점을 더해야 한다.
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|r| {
                let marked = &self.content[range.start..range.start + new_text.len()];
                let to8 = |u: usize| {
                    let mut c16 = 0;
                    let mut c8 = 0;
                    for ch in marked.chars() {
                        if c16 >= u {
                            break;
                        }
                        c16 += ch.len_utf16();
                        c8 += ch.len_utf8();
                    }
                    c8
                };
                range.start + to8(r.start)..range.start + to8(r.end)
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let origin = bounds.origin - self.last_scroll;
        let a = layout.position(self.content_to_disp(range.start));
        let b = layout.position(self.content_to_disp(range.end));
        // 조합 글자가 줄을 넘으면 끝 줄 기준으로 알린다(후보 창 위치용).
        let a = if a.y == b.y { a } else { point(px(0.), b.y) };
        Some(Bounds::from_corners(
            origin + a,
            origin + point(b.x, b.y + layout.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let local = self.last_bounds?.localize(&point)?;
        let layout = self.last_layout.as_ref()?;
        let ix = layout.index_at(local + self.last_scroll);
        Some(self.offset_to_utf16(self.disp_to_content(ix)))
    }
}

/// 모양을 잡은 글(문단마다 한 줄, 문단 안에서 줄바꿈). 오프셋은 화면 글 기준이다.
struct Layout {
    lines: Vec<WrappedLine>,
    /// 문단마다 시작 오프셋.
    starts: Vec<usize>,
    /// 문단마다 위쪽 y.
    tops: Vec<Pixels>,
    line_height: Pixels,
    height: Pixels,
    width: Pixels,
}

impl Layout {
    fn new(lines: Vec<WrappedLine>, line_height: Pixels) -> Self {
        let mut starts = Vec::with_capacity(lines.len());
        let mut tops = Vec::with_capacity(lines.len());
        let (mut off, mut y, mut w) = (0usize, px(0.), px(0.));
        for l in &lines {
            starts.push(off);
            tops.push(y);
            off += l.len() + 1;
            let sz = l.size(line_height);
            y += sz.height;
            w = w.max(sz.width);
        }
        Self {
            lines,
            starts,
            tops,
            line_height,
            height: y.max(line_height),
            width: w,
        }
    }

    /// 오프셋의 위치(글 원점 기준, 그 줄의 위쪽).
    fn position(&self, ix: usize) -> Point<Pixels> {
        for i in (0..self.lines.len()).rev() {
            if ix >= self.starts[i] {
                let line = &self.lines[i];
                let local = (ix - self.starts[i]).min(line.len());
                let p = line
                    .position_for_index(local, self.line_height)
                    .unwrap_or_else(|| {
                        let h = line.size(self.line_height).height;
                        point(line.width(), h - self.line_height)
                    });
                return point(p.x, p.y + self.tops[i]);
            }
        }
        point(px(0.), px(0.))
    }

    /// 위치에 가장 가까운 오프셋.
    fn index_at(&self, pt: Point<Pixels>) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        let mut i = 0;
        for (k, top) in self.tops.iter().enumerate() {
            if pt.y >= *top {
                i = k;
            }
        }
        let line = &self.lines[i];
        let h = line.size(self.line_height).height;
        let y = (pt.y - self.tops[i]).max(px(0.)).min(h - px(1.));
        let x = pt.x.max(px(0.));
        let local = match line.closest_index_for_position(point(x, y), self.line_height) {
            Ok(v) | Err(v) => v,
        };
        self.starts[i] + local.min(line.len())
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    layout: Option<Layout>,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
    scroll: Point<Pixels>,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

/// 화면에 보일 글과 색 묶음.
fn display_runs(input: &TextInput, window: &Window) -> (SharedString, Vec<TextRun>) {
    let style = window.text_style();
    let content = &input.content;
    let (display_text, text_color): (SharedString, gpui::Hsla) = if content.is_empty() {
        (
            input.placeholder.clone(),
            gpui::Hsla::from(rgb(theme::FAINT)),
        )
    } else if input.secret {
        (
            BULLET.to_string().repeat(content.chars().count()).into(),
            style.color,
        )
    } else {
        (content.clone(), style.color)
    };
    let run = TextRun {
        len: display_text.len(),
        font: style.font(),
        color: text_color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let runs = match input.marked_range.as_ref() {
        Some(marked) if !content.is_empty() => {
            let (ms, me) = (
                input.content_to_disp(marked.start),
                input.content_to_disp(marked.end),
            );
            vec![
                TextRun {
                    len: ms,
                    ..run.clone()
                },
                TextRun {
                    len: me - ms,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - me,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        }
        _ => vec![run],
    };
    (display_text, runs)
}

fn shape(
    window: &Window,
    text: SharedString,
    runs: &[TextRun],
    wrap: Option<Pixels>,
) -> Vec<WrappedLine> {
    let font_size = window.text_style().font_size.to_pixels(window.rem_size());
    window
        .text_system()
        .shape_text(text, font_size, runs, wrap, None)
        .map(|v| v.into_vec())
        .unwrap_or_default()
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let input = self.input.read(cx);
        let lh = window.line_height();
        // 여러 줄 칸은 지난 프레임의 너비로 줄 수를 세어 높이를 정한다(너비가 바뀌면 다시 그린다).
        let rows = match (input.multiline, input.last_bounds) {
            (Some(max), Some(b)) => {
                let (text, runs) = display_runs(input, window);
                let lines = shape(window, text, &runs, Some(b.size.width));
                let h = lines.iter().fold(px(0.), |a, l| a + l.size(lh).height);
                ((h / lh).round() as usize).clamp(1, max)
            }
            _ => 1,
        };
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = (lh * rows as f32).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let multiline = input.multiline.is_some();
        let empty = input.content.is_empty();
        let selected = input.content_to_disp(input.selected_range.start)
            ..input.content_to_disp(input.selected_range.end);
        let cursor_ix = if empty {
            0
        } else {
            input.content_to_disp(input.cursor_offset())
        };
        let prev = input.last_scroll;
        let lh = window.line_height();

        let (text, runs) = display_runs(input, window);
        let wrap = multiline.then_some(bounds.size.width);
        let layout = Layout::new(shape(window, text, &runs, wrap), lh);

        let cpos = layout.position(cursor_ix);
        let mut scroll = prev;
        if multiline {
            scroll.x = px(0.);
            let h = bounds.size.height;
            if cpos.y + lh - scroll.y > h {
                scroll.y = cpos.y + lh - h;
            }
            if cpos.y < scroll.y {
                scroll.y = cpos.y;
            }
            if layout.height <= h {
                scroll.y = px(0.);
            }
        } else {
            scroll.y = px(0.);
            let width = bounds.size.width;
            if cpos.x - scroll.x > width - px(4.) {
                scroll.x = cpos.x - width + px(4.);
            }
            if cpos.x < scroll.x {
                scroll.x = cpos.x;
            }
            if layout.width <= width {
                scroll.x = px(0.);
            }
        }
        let origin = bounds.origin - scroll;

        let mut selection = vec![];
        let cursor = if selected.is_empty() || empty {
            Some(fill(
                Bounds::new(origin + cpos, size(px(1.5), lh)),
                rgb(theme::ACCENT),
            ))
        } else {
            let a = layout.position(selected.start);
            let b = layout.position(selected.end);
            let color = rgba(0x5b8cff44);
            let right = layout.width.max(bounds.size.width);
            if a.y == b.y {
                selection.push(fill(
                    Bounds::from_corners(origin + a, origin + point(b.x, b.y + lh)),
                    color,
                ));
            } else {
                selection.push(fill(
                    Bounds::from_corners(origin + a, origin + point(right, a.y + lh)),
                    color,
                ));
                if b.y > a.y + lh {
                    selection.push(fill(
                        Bounds::from_corners(
                            origin + point(px(0.), a.y + lh),
                            origin + point(right, b.y),
                        ),
                        color,
                    ));
                }
                selection.push(fill(
                    Bounds::from_corners(
                        origin + point(px(0.), b.y),
                        origin + point(b.x, b.y + lh),
                    ),
                    color,
                ));
            }
            None
        };
        PrepaintState {
            layout: Some(layout),
            cursor,
            selection,
            scroll,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let scroll = prepaint.scroll;
        let lh = window.line_height();
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for q in prepaint.selection.drain(..) {
                window.paint_quad(q)
            }
            let layout = prepaint.layout.take().unwrap();
            let origin = bounds.origin - scroll;
            for (line, top) in layout.lines.iter().zip(&layout.tops) {
                let _ = line.paint(
                    origin + point(px(0.), *top),
                    lh,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            if focus_handle.is_focused(window) {
                if let Some(cursor) = prepaint.cursor.take() {
                    window.paint_quad(cursor);
                }
            }
            self.input.update(cx, |input, cx| {
                let width_changed = input.multiline.is_some()
                    && input
                        .last_bounds
                        .map(|b| b.size.width != bounds.size.width)
                        .unwrap_or(true);
                input.last_layout = Some(layout);
                input.last_bounds = Some(bounds);
                input.last_scroll = scroll;
                if width_changed {
                    cx.notify();
                }
            });
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        div()
            .flex()
            .flex_1()
            .min_w(px(0.))
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::cancel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(theme::INPUT))
            .border_1()
            .border_color(rgb(if focused { theme::ACCENT } else { theme::LINE }))
            .text_sm()
            .text_color(rgb(theme::TEXT))
            .overflow_hidden()
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
