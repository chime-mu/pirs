//! Interactive mode: a terminal UI with an inline viewport (editor, status
//! line, extension widgets, streaming tail) while finished content scrolls
//! into the terminal's normal scrollback, like pi's TUI.

use crate::agent_session::{AgentSession, UiBackend, UiEvent};
use async_trait::async_trait;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use pi_agent::{AgentEvent, AgentMessage, ToolResult};
use pi_ai::{Content, ThinkingLevel};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Size};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use ratatui::{Terminal, TerminalOptions, Viewport};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use unicode_width::UnicodeWidthStr;

// ---------------------------------------------------------------------------
// Backend (called from the session / extension host threads)
// ---------------------------------------------------------------------------

pub enum DialogKind {
    Select { options: Vec<String> },
    Confirm { message: String },
    Input { placeholder: String },
}

pub struct DialogRequest {
    pub title: String,
    pub kind: DialogKind,
    pub timeout_ms: Option<u64>,
    pub reply: oneshot::Sender<Option<String>>,
}

pub struct TuiBackend {
    tx: mpsc::UnboundedSender<UiEvent>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<UiEvent>>>,
    dialog_tx: mpsc::UnboundedSender<DialogRequest>,
    dialog_rx: Mutex<Option<mpsc::UnboundedReceiver<DialogRequest>>>,
    editor_text: Arc<Mutex<String>>,
}

impl Default for TuiBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl TuiBackend {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (dialog_tx, dialog_rx) = mpsc::unbounded_channel();
        TuiBackend { tx, rx: Mutex::new(Some(rx)), dialog_tx, dialog_rx: Mutex::new(Some(dialog_rx)), editor_text: Arc::new(Mutex::new(String::new())) }
    }

    async fn dialog(&self, title: String, kind: DialogKind, opts: &Value) -> Option<String> {
        let (reply, rx) = oneshot::channel();
        let timeout_ms = opts["timeout"].as_u64();
        if self.dialog_tx.send(DialogRequest { title, kind, timeout_ms, reply }).is_err() {
            return None;
        }
        rx.await.ok().flatten()
    }
}

#[async_trait]
impl UiBackend for TuiBackend {
    fn mode(&self) -> &'static str {
        "tui"
    }
    fn has_ui(&self) -> bool {
        true
    }
    fn emit(&self, event: UiEvent) {
        let _ = self.tx.send(event);
    }
    async fn select(&self, title: String, options: Vec<String>, opts: Value) -> Option<String> {
        self.dialog(title, DialogKind::Select { options }, &opts).await
    }
    async fn confirm(&self, title: String, message: String, opts: Value) -> bool {
        self.dialog(title, DialogKind::Confirm { message }, &opts).await.map(|v| v == "yes").unwrap_or(false)
    }
    async fn input(&self, title: String, placeholder: String, opts: Value) -> Option<String> {
        self.dialog(title, DialogKind::Input { placeholder }, &opts).await
    }
    fn get_editor_text(&self) -> String {
        self.editor_text.lock().unwrap().clone()
    }
}

// ---------------------------------------------------------------------------
// Backend wrapper: never query the terminal for the cursor position.
//
// crossterm answers `cursor::position()` by reading the tty, which blocks for
// up to two seconds while the async `EventStream` thread owns the input
// reader. ratatui asks for the position on `clear()`, so we track it instead.
// ---------------------------------------------------------------------------

struct TrackedBackend {
    inner: CrosstermBackend<std::io::Stdout>,
    cursor: Position,
}

impl Backend for TrackedBackend {
    type Error = std::io::Error;
    fn draw<'a, I>(&mut self, content: I) -> std::io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }
    fn append_lines(&mut self, n: u16) -> std::io::Result<()> {
        self.inner.append_lines(n)
    }
    fn hide_cursor(&mut self) -> std::io::Result<()> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> std::io::Result<()> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> std::io::Result<Position> {
        Ok(self.cursor)
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> std::io::Result<()> {
        let p = position.into();
        self.cursor = p;
        self.inner.set_cursor_position(p)
    }
    fn clear(&mut self) -> std::io::Result<()> {
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> std::io::Result<()> {
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> std::io::Result<Size> {
        self.inner.size()
    }
    fn window_size(&mut self) -> std::io::Result<WindowSize> {
        self.inner.window_size()
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
    fn scroll_region_up(&mut self, region: std::ops::Range<u16>, amount: u16) -> std::io::Result<()> {
        self.inner.scroll_region_up(region, amount)
    }
    fn scroll_region_down(&mut self, region: std::ops::Range<u16>, amount: u16) -> std::io::Result<()> {
        self.inner.scroll_region_down(region, amount)
    }
}

// ---------------------------------------------------------------------------
// UI state
// ---------------------------------------------------------------------------

struct ActiveDialog {
    request: DialogRequest,
    selected: usize,
    input: String,
    deadline: Option<std::time::Instant>,
}

#[derive(Default)]
struct Ui {
    input: String,
    cursor: usize, // byte offset
    history: Vec<String>,
    history_index: Option<usize>,
    running: bool,
    spinner: usize,
    working_message: Option<String>,
    statuses: BTreeMap<String, String>,
    widgets_above: BTreeMap<String, Vec<String>>,
    widgets_below: BTreeMap<String, Vec<String>>,
    /// Streaming assistant text not yet committed to scrollback.
    live_text: String,
    live_thinking: String,
    dialog: Option<ActiveDialog>,
    last_usage: Option<(u64, u64, f64)>,
    total_cost: f64,
    /// Pending scrollback lines to flush via insert_before.
    pending: Vec<Line<'static>>,
    ctrl_c_armed: bool,
    quit: bool,
    tool_args: BTreeMap<String, (String, Value)>,
}

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn wrap_line(s: &str, width: usize) -> Vec<String> {
    let width = width.max(4);
    let mut out = Vec::new();
    for raw in s.split('\n') {
        let mut cur = String::new();
        let mut cur_w = 0;
        for ch in raw.chars() {
            let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
            if cur_w + w > width {
                out.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            cur.push(ch);
            cur_w += w;
        }
        out.push(cur);
    }
    out
}

fn styled_lines(text: &str, width: usize, style: Style, prefix: &str) -> Vec<Line<'static>> {
    let pw = prefix.width();
    wrap_line(text, width.saturating_sub(pw))
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            let p = if i == 0 { prefix.to_string() } else { " ".repeat(pw) };
            Line::from(vec![Span::styled(p, style), Span::styled(l, style)])
        })
        .collect()
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn summarize_args(name: &str, args: &Value) -> String {
    match name {
        "bash" => args["command"].as_str().unwrap_or("").to_string(),
        "read" | "write" | "edit" => args["path"].as_str().unwrap_or("").to_string(),
        "grep" => format!("{} {}", args["pattern"].as_str().unwrap_or(""), args["path"].as_str().unwrap_or("")),
        "find" => format!("{} {}", args["pattern"].as_str().unwrap_or(""), args["path"].as_str().unwrap_or("")),
        "ls" => args["path"].as_str().unwrap_or(".").to_string(),
        _ => {
            let s = args.to_string();
            if s.len() > 120 { format!("{}…", &s[..120]) } else { s }
        }
    }
}

fn result_preview(result: &ToolResult, is_error: bool, max_lines: usize) -> Vec<String> {
    let text: String = result.content.iter().filter_map(|c| c.as_text()).collect::<Vec<_>>().join("\n");
    let images = result.content.iter().filter(|c| matches!(c, Content::Image { .. })).count();
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    let total = lines.len();
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        lines.push(format!("… ({} more lines)", total - max_lines));
    }
    if images > 0 {
        lines.push(format!("[{images} image(s)]"));
    }
    if lines.is_empty() {
        lines.push(if is_error { "(error)".into() } else { "(no output)".into() });
    }
    lines
}

impl Ui {
    fn push_lines(&mut self, lines: Vec<Line<'static>>) {
        self.pending.extend(lines);
    }
    fn push_text(&mut self, text: &str, width: usize, style: Style, prefix: &str) {
        let lines = styled_lines(text, width, style, prefix);
        self.push_lines(lines);
    }
    fn blank(&mut self) {
        self.pending.push(Line::from(""));
    }

    fn insert_str(&mut self, s: &str) {
        self.input.insert_str(self.cursor, s);
        self.cursor += s.len();
    }
    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.input[..self.cursor].chars().next_back().map(|c| c.len_utf8()).unwrap_or(1);
        self.input.drain(self.cursor - prev..self.cursor);
        self.cursor -= prev;
    }
    fn delete(&mut self) {
        if self.cursor >= self.input.len() {
            return;
        }
        let next = self.input[self.cursor..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        self.input.drain(self.cursor..self.cursor + next);
    }
    fn left(&mut self) {
        if self.cursor > 0 {
            let prev = self.input[..self.cursor].chars().next_back().map(|c| c.len_utf8()).unwrap_or(1);
            self.cursor -= prev;
        }
    }
    fn right(&mut self) {
        if self.cursor < self.input.len() {
            let next = self.input[self.cursor..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            self.cursor += next;
        }
    }
    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let idx = match self.history_index {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_index = Some(idx);
        self.input = self.history[idx].clone();
        self.cursor = self.input.len();
    }
    fn history_next(&mut self) {
        let Some(i) = self.history_index else { return };
        if i + 1 >= self.history.len() {
            self.history_index = None;
            self.input.clear();
            self.cursor = 0;
        } else {
            self.history_index = Some(i + 1);
            self.input = self.history[i + 1].clone();
            self.cursor = self.input.len();
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

const VIEWPORT_HEIGHT: u16 = 14;

fn render(ui: &Ui, model_label: &str, area: Rect, buf: &mut Buffer) -> Option<(u16, u16)> {
    let width = area.width as usize;
    let dim = Style::default().fg(Color::DarkGray);
    let accent = Style::default().fg(Color::Cyan);
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut cursor: Option<(u16, u16)> = None;

    if let Some(d) = &ui.dialog {
        rows.push(Line::from(Span::styled(format!(" {} ", d.request.title.lines().next().unwrap_or("")), Style::default().fg(Color::Black).bg(Color::Yellow))));
        for extra in d.request.title.lines().skip(1) {
            rows.push(Line::from(Span::styled(format!(" {extra}"), Style::default().fg(Color::Yellow))));
        }
        match &d.request.kind {
            DialogKind::Select { options } => {
                let max = (VIEWPORT_HEIGHT as usize).saturating_sub(rows.len() + 2).max(1);
                let start = d.selected.saturating_sub(max.saturating_sub(1));
                for (i, o) in options.iter().enumerate().skip(start).take(max) {
                    let marker = if i == d.selected { "❯ " } else { "  " };
                    let style = if i == d.selected { accent.add_modifier(Modifier::BOLD) } else { Style::default() };
                    rows.push(Line::from(Span::styled(format!("{marker}{o}"), style)));
                }
                rows.push(Line::from(Span::styled(" ↑/↓ move · enter select · esc cancel", dim)));
            }
            DialogKind::Confirm { message } => {
                for l in wrap_line(message, width.saturating_sub(2)) {
                    rows.push(Line::from(format!(" {l}")));
                }
                let (y, n) = if d.selected == 0 { (accent.add_modifier(Modifier::BOLD), Style::default()) } else { (Style::default(), accent.add_modifier(Modifier::BOLD)) };
                rows.push(Line::from(vec![Span::raw(" "), Span::styled("[ Yes ]", y), Span::raw("  "), Span::styled("[ No ]", n), Span::styled("   ←/→ · y/n · enter · esc", dim)]));
            }
            DialogKind::Input { placeholder } => {
                let shown = if d.input.is_empty() { Span::styled(placeholder.clone(), dim) } else { Span::raw(d.input.clone()) };
                rows.push(Line::from(vec![Span::styled("> ", accent), shown]));
                cursor = Some((2 + d.input.width() as u16, rows.len() as u16 - 1));
                rows.push(Line::from(Span::styled(" enter submit · esc cancel", dim)));
            }
        }
        if let Some(deadline) = d.deadline {
            let left = deadline.saturating_duration_since(std::time::Instant::now()).as_secs();
            rows.push(Line::from(Span::styled(format!(" auto-dismiss in {left}s"), dim)));
        }
    } else {
        // Live streaming tail.
        let tail_budget = (VIEWPORT_HEIGHT as usize).saturating_sub(5 + ui.widgets_above.values().map(|w| w.len()).sum::<usize>() + ui.widgets_below.values().map(|w| w.len()).sum::<usize>());
        if !ui.live_thinking.is_empty() && ui.live_text.is_empty() {
            let mut lines = wrap_line(&ui.live_thinking, width.saturating_sub(2));
            let keep = tail_budget.min(lines.len());
            lines = lines.split_off(lines.len() - keep);
            for l in lines {
                rows.push(Line::from(Span::styled(format!("  {l}"), Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC))));
            }
        } else if !ui.live_text.is_empty() {
            let mut lines = wrap_line(&ui.live_text, width);
            let keep = tail_budget.min(lines.len());
            lines = lines.split_off(lines.len() - keep);
            for l in lines {
                rows.push(Line::from(l));
            }
        }
        for lines in ui.widgets_above.values() {
            for l in lines {
                rows.push(Line::from(Span::styled(strip_ansi(l), dim)));
            }
        }
        // Editor.
        let prompt = "> ";
        let editor_width = width.saturating_sub(prompt.len()).max(4);
        let before = &ui.input[..ui.cursor];
        let cursor_line = before.matches('\n').count();
        let cursor_col = before.rsplit('\n').next().unwrap_or("").width();
        let editor_start = rows.len();
        for (i, l) in ui.input.split('\n').enumerate() {
            let p = if i == 0 { prompt.to_string() } else { "  ".to_string() };
            rows.push(Line::from(vec![Span::styled(p, accent.add_modifier(Modifier::BOLD)), Span::raw(l.to_string())]));
        }
        cursor = Some(((prompt.len() + cursor_col.min(editor_width)) as u16, (editor_start + cursor_line) as u16));
        for lines in ui.widgets_below.values() {
            for l in lines {
                rows.push(Line::from(Span::styled(strip_ansi(l), dim)));
            }
        }
        // Status line.
        let mut status: Vec<Span<'static>> = Vec::new();
        if ui.running {
            status.push(Span::styled(format!("{} ", SPINNER[ui.spinner % SPINNER.len()]), accent));
            status.push(Span::styled(ui.working_message.clone().unwrap_or_else(|| "working… (esc to abort)".into()), dim));
        } else {
            status.push(Span::styled(model_label.to_string(), dim));
        }
        if let Some((input, output, cost)) = ui.last_usage {
            status.push(Span::styled(format!("  ↑{input} ↓{output} ${cost:.4}"), dim));
        }
        if ui.total_cost > 0.0 {
            status.push(Span::styled(format!(" total ${:.4}", ui.total_cost), dim));
        }
        for (k, v) in &ui.statuses {
            status.push(Span::styled(format!("  {}", strip_ansi(v)), Style::default().fg(Color::Magenta)));
            let _ = k;
        }
        rows.push(Line::from(status));
    }

    // Bottom-align into the viewport.
    let h = area.height as usize;
    let skip = rows.len().saturating_sub(h);
    let offset = h.saturating_sub(rows.len());
    let visible: Vec<Line<'static>> = rows.into_iter().skip(skip).collect();
    let para = Paragraph::new(visible);
    para.render(Rect { x: area.x, y: area.y + offset as u16, width: area.width, height: area.height.saturating_sub(offset as u16) }, buf);
    cursor.map(|(x, y)| (area.x + x, area.y + (offset as u16 + y).saturating_sub(skip as u16)))
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

fn help_text() -> String {
    [
        "Commands:",
        "  /help                 show this help",
        "  /model [spec]         show or switch model (provider/id, id, or substring)",
        "  /thinking [level]     show or set thinking level (off, minimal, low, medium, high, xhigh, max)",
        "  /tools                list active tools",
        "  /extensions           list loaded extensions, commands and errors",
        "  /session              show the session file",
        "  /new                  start a new session",
        "  /reload               reload extensions and context files (AGENTS.md etc.)",
        "  /clear                clear the screen",
        "  /exit, /quit          exit",
        "  !cmd  !!cmd           run a shell command (!! keeps it out of the model context)",
        "Keys: enter send · alt+enter newline · esc abort · ctrl+c clear/exit · ctrl+d exit · ↑/↓ history",
        "Typing while the agent works queues a steering message (delivered after the current tool calls).",
    ]
    .join("\n")
}

pub async fn run(session: AgentSession, tui: Arc<TuiBackend>, initial: Option<String>, loaded: &[pi_ext::LoadedExtension], failures: &[(std::path::PathBuf, String)]) -> i32 {
    let mut rx = tui.rx.lock().unwrap().take().expect("ui receiver");
    let mut dialog_rx = tui.dialog_rx.lock().unwrap().take().expect("dialog receiver");

    if crossterm::terminal::enable_raw_mode().is_err() {
        eprintln!("error: interactive mode needs a terminal; use -p for print mode");
        return 1;
    }
    let mut stdout = std::io::stdout();
    let _ = crossterm::execute!(stdout, crossterm::event::EnableBracketedPaste);
    // Query the cursor once, before the input stream thread exists.
    let start = crossterm::cursor::position().map(|(x, y)| Position { x, y }).unwrap_or(Position { x: 0, y: 0 });
    let backend = TrackedBackend { inner: CrosstermBackend::new(stdout), cursor: start };
    let mut terminal = match Terminal::with_options(backend, TerminalOptions { viewport: Viewport::Inline(VIEWPORT_HEIGHT) }) {
        Ok(t) => t,
        Err(e) => {
            let _ = crossterm::terminal::disable_raw_mode();
            eprintln!("error: {e}");
            return 1;
        }
    };
    let mut ui = Ui::default();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(90));
    let width = terminal.size().map(|s| s.width as usize).unwrap_or(80);

    // Header.
    let model = session.current_model().map(|m| m.key()).unwrap_or_default();
    ui.push_text(&format!("pirs · {model} · {}", session.0.cwd.display()), width, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD), "");
    if let Some(f) = session.0.session.lock().unwrap().get_session_file() {
        ui.push_text(&format!("session: {}", f.display()), width, Style::default().fg(Color::DarkGray), "");
    }
    if !loaded.is_empty() {
        let names: Vec<String> = loaded.iter().map(|e| std::path::Path::new(&e.path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()).collect();
        ui.push_text(&format!("extensions: {}", names.join(", ")), width, Style::default().fg(Color::DarkGray), "");
    }
    for (p, e) in failures {
        ui.push_text(&format!("failed to load {}: {}", p.display(), e.lines().next().unwrap_or("")), width, Style::default().fg(Color::Red), "");
    }
    ui.push_text("type /help for commands", width, Style::default().fg(Color::DarkGray), "");
    ui.blank();

    if let Some(text) = initial {
        submit(&session, &mut ui, text, width).await;
    }

    loop {
        // Flush scrollback.
        if !ui.pending.is_empty() {
            let lines = std::mem::take(&mut ui.pending);
            let height = lines.len() as u16;
            let _ = terminal.insert_before(height, |buf| {
                Paragraph::new(lines).render(buf.area, buf);
            });
        }
        // Expire timed dialogs.
        if let Some(d) = &ui.dialog {
            if d.deadline.map(|dl| std::time::Instant::now() >= dl).unwrap_or(false) {
                if let Some(d) = ui.dialog.take() {
                    let _ = d.request.reply.send(None);
                }
            }
        }
        let model_label = session.current_model().map(|m| format!("{} · thinking {}", m.key(), session.0.agent.thinking_level().as_str())).unwrap_or_default();
        let _ = terminal.draw(|frame| {
            let area = frame.area();
            let cursor = render(&ui, &model_label, area, frame.buffer_mut());
            if let Some((x, y)) = cursor {
                frame.set_cursor_position((x, y));
            }
        });
        *tui.editor_text.lock().unwrap() = ui.input.clone();
        if ui.quit {
            break;
        }

        tokio::select! {
            ev = events.next() => {
                match ev {
                    Some(Ok(Event::Key(key))) => handle_key(&session, &mut ui, key, width).await,
                    Some(Ok(Event::Paste(text))) => {
                        if let Some(d) = ui.dialog.as_mut() { if let DialogKind::Input { .. } = d.request.kind { d.input.push_str(&text); } } else { ui.insert_str(&text); }
                    }
                    Some(Ok(Event::Resize(_, _))) => { let _ = terminal.autoresize(); }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break,
                }
            }
            Some(event) = rx.recv() => handle_ui_event(&session, &mut ui, event, width).await,
            Some(req) = dialog_rx.recv() => {
                crate::agent_session::trace("ui: dialog request received");
                let deadline = req.timeout_ms.map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(ms));
                if let Some(prev) = ui.dialog.take() { let _ = prev.request.reply.send(None); }
                ui.dialog = Some(ActiveDialog { request: req, selected: 0, input: String::new(), deadline });
            }
            _ = tick.tick() => { if ui.running { ui.spinner = ui.spinner.wrapping_add(1); } }
        }
    }

    let _ = terminal.insert_before(1, |buf| {
        Paragraph::new(Line::from(Span::styled("bye", Style::default().fg(Color::DarkGray)))).render(buf.area, buf);
    });
    let _ = terminal.clear();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
    let _ = crossterm::terminal::disable_raw_mode();
    println!();
    0
}

async fn submit(session: &AgentSession, ui: &mut Ui, text: String, width: usize) {
    let text = text.trim_end().to_string();
    if text.is_empty() {
        return;
    }
    ui.history.push(text.clone());
    ui.history_index = None;
    if handle_builtin_command(session, ui, &text, width).await {
        return;
    }
    let steering = ui.running;
    let style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    ui.push_text(&text, width, style, if steering { "↪ " } else { "> " });
    ui.blank();
    // Run on a separate task so the UI loop keeps servicing dialogs and events.
    let session = session.clone();
    tokio::spawn(async move {
        if let Err(e) = session.submit(text, Vec::new(), None).await {
            session.0.ui.emit(UiEvent::Notify { message: format!("error: {e}"), kind: "error".into() });
        }
    });
}

async fn handle_builtin_command(session: &AgentSession, ui: &mut Ui, text: &str, width: usize) -> bool {
    let Some(rest) = text.strip_prefix('/') else { return false };
    let (cmd, arg) = rest.split_once(' ').map(|(c, a)| (c, a.trim())).unwrap_or((rest, ""));
    let info = Style::default().fg(Color::DarkGray);
    match cmd {
        "help" => ui.push_text(&help_text(), width, info, ""),
        "exit" | "quit" => ui.quit = true,
        "clear" => {
            let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::Clear(crossterm::terminal::ClearType::All), crossterm::cursor::MoveTo(0, 0));
        }
        "session" => {
            let f = session.0.session.lock().unwrap().get_session_file().map(|p| p.display().to_string()).unwrap_or_else(|| "(in memory)".into());
            ui.push_text(&format!("session: {f}"), width, info, "");
        }
        "tools" => {
            let names: Vec<String> = session.0.agent.tools().iter().map(|t| t.name()).collect();
            ui.push_text(&format!("active tools: {}", names.join(", ")), width, info, "");
        }
        "extensions" => {
            let cmds: Vec<String> = session.commands().iter().map(|c| format!("/{}", c.invocation.clone().unwrap_or(c.name.clone()))).collect();
            ui.push_text(&format!("extension commands: {}", if cmds.is_empty() { "(none)".into() } else { cmds.join(", ") }), width, info, "");
            for e in session.extension_errors() {
                ui.push_text(&format!("{} [{}]: {}", e.extension_path, e.event, e.error), width, Style::default().fg(Color::Red), "");
            }
        }
        "model" => {
            if arg.is_empty() {
                let avail: Vec<String> = session.0.registry.available().iter().map(|m| m.key()).collect();
                ui.push_text(&format!("current: {}\navailable: {}", session.current_model().map(|m| m.key()).unwrap_or_default(), avail.join(", ")), width, info, "");
            } else if let Some(m) = session.0.registry.find(arg) {
                session.set_model(m.clone(), "set").await;
                ui.push_text(&format!("model: {}", m.key()), width, info, "");
            } else {
                ui.push_text(&format!("unknown model: {arg}"), width, Style::default().fg(Color::Red), "");
            }
        }
        "thinking" => {
            if arg.is_empty() {
                ui.push_text(&format!("thinking: {}", session.0.agent.thinking_level().as_str()), width, info, "");
            } else if let Some(l) = ThinkingLevel::parse(arg) {
                session.set_thinking_level(l);
                ui.push_text(&format!("thinking: {}", l.as_str()), width, info, "");
            } else {
                ui.push_text("levels: off, minimal, low, medium, high, xhigh, max", width, Style::default().fg(Color::Red), "");
            }
        }
        "new" => {
            let cwd = session.0.cwd.to_string_lossy().to_string();
            let dir = session.0.session.lock().unwrap().get_session_dir().to_path_buf();
            match crate::session::SessionManager::create(&cwd, Some(&dir)) {
                Ok(sm) => {
                    *session.0.session.lock().unwrap() = sm;
                    session.0.agent.replace_messages(Vec::new());
                    ui.push_text("started a new session", width, info, "");
                }
                Err(e) => ui.push_text(&format!("could not start session: {e}"), width, Style::default().fg(Color::Red), ""),
            }
        }
        "reload" => {
            match session.reload().await {
                Ok(report) => {
                    ui.push_text(&crate::agent_session::reload_summary(&report), width, info, "");
                    for ext in &report.loaded {
                        ui.push_text(&format!("  {}", ext.path), width, info, "");
                    }
                    for (p, e) in &report.failures {
                        ui.push_text(&format!("  FAILED {}: {}", p.display(), e.lines().next().unwrap_or("")), width, Style::default().fg(Color::Red), "");
                    }
                }
                Err(e) => ui.push_text(&format!("reload failed: {e}"), width, Style::default().fg(Color::Red), ""),
            }
        }
        _ => return false,
    }
    true
}

async fn handle_key(session: &AgentSession, ui: &mut Ui, key: KeyEvent, width: usize) {
    if key.kind == KeyEventKind::Release {
        return;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    if let Some(d) = ui.dialog.as_mut() {
        let mut done: Option<Option<String>> = None;
        match (&mut d.request.kind, key.code) {
            (_, KeyCode::Esc) => done = Some(None),
            (_, KeyCode::Char('c')) if ctrl => done = Some(None),
            (DialogKind::Select { .. }, KeyCode::Up) => d.selected = d.selected.saturating_sub(1),
            (DialogKind::Select { options }, KeyCode::Down) => d.selected = (d.selected + 1).min(options.len().saturating_sub(1)),
            (DialogKind::Select { options }, KeyCode::Enter) => done = Some(options.get(d.selected).cloned()),
            (DialogKind::Select { options }, KeyCode::Char(c)) if c.is_ascii_digit() => {
                let n = c.to_digit(10).unwrap_or(0) as usize;
                if n >= 1 && n <= options.len() {
                    done = Some(options.get(n - 1).cloned());
                }
            }
            (DialogKind::Confirm { .. }, KeyCode::Left | KeyCode::Right | KeyCode::Tab) => d.selected = 1 - d.selected.min(1),
            (DialogKind::Confirm { .. }, KeyCode::Char('y')) => done = Some(Some("yes".into())),
            (DialogKind::Confirm { .. }, KeyCode::Char('n')) => done = Some(Some("no".into())),
            (DialogKind::Confirm { .. }, KeyCode::Enter) => done = Some(Some(if d.selected == 0 { "yes".into() } else { "no".into() })),
            (DialogKind::Input { .. }, KeyCode::Enter) => done = Some(Some(d.input.clone())),
            (DialogKind::Input { .. }, KeyCode::Backspace) => {
                d.input.pop();
            }
            (DialogKind::Input { .. }, KeyCode::Char(c)) if !ctrl => d.input.push(c),
            _ => {}
        }
        if let Some(result) = done {
            if let Some(d) = ui.dialog.take() {
                let _ = d.request.reply.send(result);
            }
        }
        return;
    }
    match key.code {
        KeyCode::Enter if alt || key.modifiers.contains(KeyModifiers::SHIFT) => ui.insert_str("\n"),
        KeyCode::Char('j') if ctrl => ui.insert_str("\n"),
        KeyCode::Enter => {
            let text = std::mem::take(&mut ui.input);
            ui.cursor = 0;
            submit(session, ui, text, width).await;
        }
        KeyCode::Esc => {
            if ui.running {
                session.abort();
                ui.push_text("aborting…", width, Style::default().fg(Color::DarkGray), "");
            } else {
                ui.input.clear();
                ui.cursor = 0;
            }
        }
        KeyCode::Char('c') if ctrl => {
            if ui.running {
                session.abort();
            } else if !ui.input.is_empty() {
                ui.input.clear();
                ui.cursor = 0;
            } else if ui.ctrl_c_armed {
                ui.quit = true;
            } else {
                ui.ctrl_c_armed = true;
                ui.push_text("press ctrl+c again to exit", width, Style::default().fg(Color::DarkGray), "");
                return;
            }
        }
        KeyCode::Char('d') if ctrl => {
            if ui.input.is_empty() {
                ui.quit = true;
            }
        }
        KeyCode::Char('u') if ctrl => {
            ui.input.clear();
            ui.cursor = 0;
        }
        KeyCode::Char('a') if ctrl => ui.cursor = 0,
        KeyCode::Char('e') if ctrl => ui.cursor = ui.input.len(),
        KeyCode::Char('l') if ctrl => {
            let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::Clear(crossterm::terminal::ClearType::All), crossterm::cursor::MoveTo(0, 0));
        }
        KeyCode::Char(c) if !ctrl => {
            let mut b = [0u8; 4];
            ui.insert_str(c.encode_utf8(&mut b));
        }
        KeyCode::Backspace => ui.backspace(),
        KeyCode::Delete => ui.delete(),
        KeyCode::Left => ui.left(),
        KeyCode::Right => ui.right(),
        KeyCode::Home => ui.cursor = 0,
        KeyCode::End => ui.cursor = ui.input.len(),
        KeyCode::Up => {
            if !ui.input[..ui.cursor].contains('\n') {
                ui.history_prev();
            }
        }
        KeyCode::Down => {
            if !ui.input[ui.cursor..].contains('\n') {
                ui.history_next();
            }
        }
        _ => {}
    }
    ui.ctrl_c_armed = false;
}

async fn handle_ui_event(session: &AgentSession, ui: &mut Ui, event: UiEvent, width: usize) {
    let dim = Style::default().fg(Color::DarkGray);
    match event {
        UiEvent::Agent(ev) => handle_agent_event(session, ui, ev, width).await,
        UiEvent::Notify { message, kind } => {
            let style = match kind.as_str() {
                "error" => Style::default().fg(Color::Red),
                "warning" => Style::default().fg(Color::Yellow),
                _ => Style::default().fg(Color::Blue),
            };
            ui.push_text(&message, width, style, "ℹ ");
        }
        UiEvent::Status { key, text } => match text {
            Some(t) => {
                ui.statuses.insert(key, t);
            }
            None => {
                ui.statuses.remove(&key);
            }
        },
        UiEvent::Widget { key, lines, placement } => {
            let target = if placement.as_deref() == Some("belowEditor") { &mut ui.widgets_below } else { &mut ui.widgets_above };
            match lines {
                Some(l) => {
                    target.insert(key, l);
                }
                None => {
                    ui.widgets_above.remove(&key);
                    ui.widgets_below.remove(&key);
                }
            }
        }
        UiEvent::Title(t) => {
            let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::SetTitle(t));
        }
        UiEvent::WorkingMessage(m) => ui.working_message = m,
        UiEvent::ExtensionError(e) => ui.push_text(&format!("extension error {} [{}]: {}", e.extension_path.rsplit('/').next().unwrap_or(""), e.event, e.error.lines().next().unwrap_or("")), width, Style::default().fg(Color::Red), ""),
        UiEvent::Console { level, message } => {
            let style = if level == "error" || level == "warn" { Style::default().fg(Color::Yellow) } else { dim };
            ui.push_text(&message, width, style, "· ");
        }
        UiEvent::SetEditorText(t) => {
            ui.input = t;
            ui.cursor = ui.input.len();
        }
        UiEvent::BashOutput { command, output, exit_code } => {
            ui.push_text(&format!("$ {command}"), width, Style::default().fg(Color::Yellow), "");
            let mut lines: Vec<&str> = output.lines().collect();
            let total = lines.len();
            if lines.len() > 40 {
                lines.truncate(40);
            }
            ui.push_text(&lines.join("\n"), width, dim, "");
            if total > 40 {
                ui.push_text(&format!("… ({} more lines)", total - 40), width, dim, "");
            }
            if let Some(c) = exit_code.filter(|c| *c != 0) {
                ui.push_text(&format!("exit code {c}"), width, Style::default().fg(Color::Red), "");
            }
            ui.blank();
        }
        UiEvent::Shutdown => ui.quit = true,
    }
}

async fn handle_agent_event(session: &AgentSession, ui: &mut Ui, ev: AgentEvent, width: usize) {
    let dim = Style::default().fg(Color::DarkGray);
    match ev {
        AgentEvent::AgentStart => {
            ui.running = true;
            ui.live_text.clear();
            ui.live_thinking.clear();
        }
        AgentEvent::AgentEnd { .. } => {
            ui.running = false;
            ui.live_text.clear();
            ui.live_thinking.clear();
        }
        AgentEvent::MessageStart { message } => {
            if let AgentMessage::Assistant(_) = message {
                ui.live_text.clear();
                ui.live_thinking.clear();
            }
        }
        AgentEvent::MessageUpdate { message, .. } => {
            if let AgentMessage::Assistant(a) = message {
                ui.live_text = a.text();
                ui.live_thinking = a.content.iter().filter_map(|c| if let Content::Thinking { thinking, .. } = c { Some(thinking.as_str()) } else { None }).collect::<Vec<_>>().join("\n");
            }
        }
        AgentEvent::MessageEnd { message } => match message {
            AgentMessage::Assistant(a) => {
                ui.live_text.clear();
                ui.live_thinking.clear();
                let thinking: String = a.content.iter().filter_map(|c| if let Content::Thinking { thinking, redacted: false, .. } = c { Some(thinking.as_str()) } else { None }).collect::<Vec<_>>().join("\n");
                if !thinking.is_empty() && !session.0.settings.hide_thinking_block {
                    ui.push_text(&thinking, width, Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC), "  ");
                    ui.blank();
                }
                let text = a.text();
                if !text.trim().is_empty() {
                    ui.push_text(&text, width, Style::default(), "");
                    ui.blank();
                }
                if let Some(err) = &a.error_message {
                    ui.push_text(err, width, Style::default().fg(Color::Red), "✗ ");
                    ui.blank();
                }
                let u = &a.usage;
                if u.input + u.output > 0 {
                    ui.last_usage = Some((u.input + u.cache_read + u.cache_write, u.output, u.cost.total));
                    ui.total_cost += u.cost.total;
                }
            }
            AgentMessage::Custom(c) if c.display => {
                let text = c.content.plain_text();
                let rendered = session.render_message(&c.custom_type, &serde_json::to_value(&c).unwrap_or(Value::Null), width).await;
                match rendered {
                    Some(lines) => ui.push_text(&lines.join("\n"), width, Style::default().fg(Color::Magenta), ""),
                    None => ui.push_text(&text, width, Style::default().fg(Color::Magenta), &format!("[{}] ", c.custom_type)),
                }
                ui.blank();
            }
            _ => {}
        },
        AgentEvent::ToolExecutionStart { tool_call_id, tool_name, args } => {
            crate::agent_session::trace("ui: tool_execution_start");
            let rendered = session.render_tool_call(&tool_name, &args, width).await;
            match rendered {
                Some(lines) => ui.push_text(&lines.iter().map(|l| strip_ansi(l)).collect::<Vec<_>>().join("\n"), width, Style::default().fg(Color::Yellow), "▸ "),
                None => ui.push_text(&format!("{tool_name} {}", summarize_args(&tool_name, &args)), width, Style::default().fg(Color::Yellow), "▸ "),
            }
            ui.tool_args.insert(tool_call_id, (tool_name, args));
        }
        AgentEvent::ToolExecutionEnd { tool_call_id, tool_name, result, is_error } => {
            ui.tool_args.remove(&tool_call_id);
            let rendered = session.render_tool_result(&tool_name, &serde_json::to_value(&result).unwrap_or(Value::Null), false, width).await;
            let style = if is_error { Style::default().fg(Color::Red) } else { dim };
            let lines = match rendered {
                Some(l) => l.iter().map(|s| strip_ansi(s)).collect::<Vec<_>>(),
                None => result_preview(&result, is_error, 12),
            };
            ui.push_text(&lines.join("\n"), width, style, "  ");
            ui.blank();
        }
        AgentEvent::ToolExecutionUpdate { .. } | AgentEvent::TurnStart | AgentEvent::TurnEnd { .. } => {}
    }
}
