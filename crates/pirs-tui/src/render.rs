//! Drawing: sidebar on the left, one page on the right, overlays for the
//! command list and pickers. Everything here is a function of `App`; the
//! only thing it writes back is the view it used, so paging knows where the
//! body starts.

use pirs_protocol::{LoopState, NotifyLevel};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, SidebarItem, RESULT_PREVIEW_LINES};
use crate::config::Theme;
use crate::model::{
    args_summary, fences, truncate, Agent, Block as MsgBlock, Entry, HookTarget, Mode, Page,
};

/// The colours of one theme.
struct Palette {
    fg: Style,
    dim: Style,
    accent: Style,
    user: Style,
    tool: Style,
    warn: Style,
    error: Style,
    selected: Style,
    background: Option<Color>,
}

impl Palette {
    fn for_theme(theme: Theme) -> Palette {
        match theme {
            Theme::Dark => Palette {
                fg: Style::default(),
                dim: Style::default().fg(Color::DarkGray),
                accent: Style::default().fg(Color::Cyan),
                user: Style::default().fg(Color::Green),
                tool: Style::default().fg(Color::Yellow),
                warn: Style::default().fg(Color::Yellow),
                error: Style::default().fg(Color::Red),
                selected: Style::default().add_modifier(Modifier::REVERSED),
                background: None,
            },
            Theme::Light => Palette {
                fg: Style::default().fg(Color::Black).bg(Color::White),
                dim: Style::default().fg(Color::Gray).bg(Color::White),
                accent: Style::default().fg(Color::Blue).bg(Color::White),
                user: Style::default().fg(Color::Green).bg(Color::White),
                tool: Style::default().fg(Color::Magenta).bg(Color::White),
                warn: Style::default().fg(Color::Yellow).bg(Color::White),
                error: Style::default().fg(Color::Red).bg(Color::White),
                selected: Style::default().fg(Color::White).bg(Color::Blue),
                background: Some(Color::White),
            },
        }
    }

    fn notify(&self, level: NotifyLevel) -> Style {
        match level {
            NotifyLevel::Info => self.accent,
            NotifyLevel::Warning => self.warn,
            NotifyLevel::Error => self.error,
        }
    }
}

/// Draw the whole screen.
pub(crate) fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let palette = Palette::for_theme(app.theme);
    if let Some(bg) = palette.background {
        frame.render_widget(Block::default().style(Style::default().bg(bg)), area);
    }
    let sidebar_width = (area.width / 4).clamp(14, 30).min(area.width);
    let [sidebar, main] =
        Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Min(1)]).areas(area);
    draw_sidebar(frame, app, sidebar, &palette);
    draw_main(frame, app, main, &palette);
}

fn draw_sidebar(frame: &mut Frame, app: &App, area: Rect, p: &Palette) {
    let block = Block::default().borders(Borders::RIGHT).style(p.fg);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = inner.width as usize;
    let items = app.sidebar_items();
    let mut lines: Vec<Line> = vec![Line::styled(" agents", p.dim)];
    if app.agents.is_empty() {
        lines.push(Line::styled("   (none)", p.dim));
    }
    for (i, agent) in app.agents.iter().enumerate() {
        let selected = items.get(app.sidebar) == Some(&SidebarItem::Agent(i));
        let cursor = if selected { '>' } else { ' ' };
        let flag = if agent.attention() { '!' } else { ' ' };
        let state = match agent.state() {
            LoopState::Working => "working",
            LoopState::Idle => "idle",
        };
        let label_width = width.saturating_sub(3 + 1 + state.len());
        let label = fit(agent.label(), label_width);
        let text = format!(
            "{cursor}{flag} {label:<label_width$} {state}",
            label_width = label_width
        );
        let style = if selected {
            p.selected
        } else if agent.attention() {
            p.warn.add_modifier(Modifier::BOLD)
        } else if agent.state() == LoopState::Working {
            p.accent
        } else {
            p.fg
        };
        lines.push(Line::styled(fit(&text, width), style));
    }
    lines.push(Line::styled(" conversations", p.dim));
    if app.conversations.is_empty() {
        lines.push(Line::styled("   (none here)", p.dim));
    }
    for (i, conversation) in app.conversations.iter().enumerate() {
        let selected = items.get(app.sidebar) == Some(&SidebarItem::Conversation(i));
        let cursor = if selected { '>' } else { ' ' };
        let text = format!("{cursor}  {}", conversation.label());
        let style = if selected { p.selected } else { p.dim };
        lines.push(Line::styled(fit(&text, width), style));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_main(frame: &mut Frame, app: &mut App, area: Rect, p: &Palette) {
    let page = app.visible_page().cloned();
    let agent_key = match &page {
        Some(Page::Agent { key, .. }) => Some(key.clone()),
        _ => None,
    };
    // What the optional rows under the body show; copied out so `app` can
    // be written to while drawing.
    let (widgets, files): (Vec<(String, Vec<String>)>, Vec<String>) =
        match agent_key.as_ref().and_then(|k| app.agent(k)) {
            Some(a) => (
                a.widgets
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                a.files.iter().map(|p| p.to_string()).collect(),
            ),
            None => (Vec::new(), Vec::new()),
        };
    let widget_cap = (area.height / 3) as usize;
    let mut widget_heights: Vec<u16> = Vec::new();
    let mut used = 0usize;
    for (_, lines) in &widgets {
        let h = lines.len().min(8) + 2;
        if used + h > widget_cap {
            break;
        }
        used += h;
        widget_heights.push(h as u16);
    }
    let files_h: u16 = if files.is_empty() { 0 } else { 1 };
    let mut constraints = vec![Constraint::Length(1), Constraint::Min(1)];
    constraints.extend(widget_heights.iter().map(|h| Constraint::Length(*h)));
    constraints.extend([
        Constraint::Length(files_h),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ]);
    let rects = Layout::vertical(constraints).split(area);
    let tabs = rects[0];
    let body = rects[1];
    let widget_rects = &rects[2..2 + widget_heights.len()];
    let files_rect = rects[2 + widget_heights.len()];
    let status_rect = rects[3 + widget_heights.len()];
    let input_rect = rects[4 + widget_heights.len()];
    let bottom_rect = rects[5 + widget_heights.len()];

    draw_tabs(frame, app, tabs, p);

    let width = body.width as usize;
    let height = body.height as usize;
    app.body_height = height;
    match &page {
        Some(Page::Agent { key, scroll }) => {
            let expand_key = app.config.keys.expand.clone();
            let rows = match app.agent(key) {
                Some(agent) => agent_rows(agent, width, &expand_key, p),
                None => vec![Line::styled("(agent gone)", p.dim)],
            };
            let total = rows.len();
            let max_start = total.saturating_sub(height);
            let start = scroll.map_or(max_start, |s| s.min(max_start));
            app.body_view = (start, max_start);
            let shown: Vec<Line> = rows.into_iter().skip(start).take(height).collect();
            frame.render_widget(Paragraph::new(shown).style(p.fg), body);
        }
        Some(Page::File {
            content, scroll, ..
        }) => {
            let rows: Vec<Line> = match content {
                None => vec![Line::styled("loading…", p.dim)],
                Some(Err(e)) => vec![Line::styled(format!("cannot read: {e}"), p.error)],
                Some(Ok(text)) => text
                    .lines()
                    .enumerate()
                    .map(|(i, l)| {
                        Line::from(vec![
                            Span::styled(format!("{:>4} ", i + 1), p.dim),
                            Span::styled(fit(l, width.saturating_sub(5)), p.fg),
                        ])
                    })
                    .collect(),
            };
            let total = rows.len();
            let max_start = total.saturating_sub(height);
            let start = (*scroll).min(max_start);
            app.body_view = (start, max_start);
            let shown: Vec<Line> = rows.into_iter().skip(start).take(height).collect();
            frame.render_widget(Paragraph::new(shown).style(p.fg), body);
        }
        None => {
            app.body_view = (0, 0);
            let keys = &app.config.keys;
            let hint = format!(
                "no agent selected: {}/{} picks one, /new starts one",
                keys.sidebar_up, keys.sidebar_down
            );
            frame.render_widget(Paragraph::new(Line::styled(hint, p.dim)), body);
        }
    }

    for ((key, lines), rect) in widgets.iter().zip(widget_rects) {
        let block = Block::bordered().title(key.as_str()).style(p.fg);
        let inner = block.inner(*rect);
        frame.render_widget(block, *rect);
        let shown: Vec<Line> = lines
            .iter()
            .take(inner.height as usize)
            .map(|l| Line::styled(fit(l, inner.width as usize), p.fg))
            .collect();
        frame.render_widget(Paragraph::new(shown), inner);
    }

    if files_h > 0 {
        let mut text = String::from("files:");
        for (i, path) in files.iter().enumerate() {
            if i < 9 {
                text.push_str(&format!(" {} {}", i + 1, path));
            } else {
                text.push_str(&format!(" (+{} more)", files.len() - 9));
                break;
            }
        }
        frame.render_widget(
            Paragraph::new(Line::styled(fit(&text, width), p.accent)),
            files_rect,
        );
    }

    // Status line.
    let mut status = String::new();
    if !app.lost.is_empty() {
        status.push_str(&format!(
            "[disconnected: {}] ",
            app.lost.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if let Some(agent) = app.current_agent() {
        let formatted = crate::status::render(&app.config.status, |key| match key {
            "loop.state" => Some(
                match agent.state() {
                    LoopState::Working => "working",
                    LoopState::Idle => "idle",
                }
                .to_owned(),
            ),
            other => agent.status.get(other).cloned(),
        });
        status.push_str(agent.label());
        status.push_str(" · ");
        status.push_str(&formatted);
    }
    frame.render_widget(
        Paragraph::new(Line::styled(fit(&status, width), p.dim)),
        status_rect,
    );

    // Input line.
    let (prompt, text, cursor) = match &app.mode {
        Mode::Normal => ("> ", app.input.clone(), true),
        Mode::Command { filter, .. } => ("/", filter.clone(), true),
        Mode::Prompt { title, value, .. } => (title.as_str(), value.clone(), true),
        Mode::Picker(_) => ("> ", app.input.clone(), false),
    };
    let prompt = match &app.mode {
        Mode::Prompt { .. } => format!("{prompt}: "),
        _ => prompt.to_owned(),
    };
    let line = format!("{prompt}{text}");
    frame.render_widget(
        Paragraph::new(Line::styled(fit(&line, width), p.fg)),
        input_rect,
    );
    if cursor {
        let x = input_rect.x + (line.width() as u16).min(input_rect.width.saturating_sub(1));
        frame.set_cursor_position((x, input_rect.y));
    }

    // Bottom line: a fresh notice, else key hints.
    let bottom = match &app.notice {
        Some(notice) => Line::styled(fit(&notice.text, width), p.notify(notice.level)),
        None => {
            let keys = &app.config.keys;
            Line::styled(
                fit(
                    &format!(
                        "{} commands  {} pages  {} files  {} quit",
                        keys.command, keys.next_page, keys.open_file, keys.quit
                    ),
                    width,
                ),
                p.dim,
            )
        }
    };
    frame.render_widget(Paragraph::new(bottom), bottom_rect);

    draw_overlay(frame, app, body, p);
}

fn draw_tabs(frame: &mut Frame, app: &App, area: Rect, p: &Palette) {
    let mut spans: Vec<Span> = Vec::new();
    for (i, page) in app.pages.iter().enumerate() {
        let label = match page {
            Page::Agent { key, .. } => app
                .agent(key)
                .map(|a| a.label().to_owned())
                .unwrap_or_else(|| key.loop_id.clone()),
            Page::File { path, .. } => tail(path.as_str(), 24),
        };
        let style = if i == app.page { p.selected } else { p.dim };
        spans.push(Span::styled(format!("[{label}]"), style));
        spans.push(Span::raw(" "));
    }
    if let Some(SidebarItem::Conversation(i)) = app.sidebar_items().get(app.sidebar) {
        if let Some(conversation) = app.conversations.get(*i) {
            spans.push(Span::styled(
                format!(
                    "conversation {} in {} ({} opens it)",
                    conversation.label(),
                    conversation.info.cwd,
                    app.config.keys.send
                ),
                p.dim,
            ));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_overlay(frame: &mut Frame, app: &App, body: Rect, p: &Palette) {
    let (title, rows): (String, Vec<Line>) = match &app.mode {
        Mode::Normal | Mode::Prompt { .. } => return,
        Mode::Command { filter, selected } => {
            let entries = app.filtered_commands(filter);
            let rows = if entries.is_empty() {
                vec![Line::styled("(no matching command)", p.dim)]
            } else {
                entries
                    .iter()
                    .enumerate()
                    .map(|(i, e)| {
                        let text = match e.side {
                            crate::model::CommandSide::Conflict => {
                                format!("{:<14} conflict: {}", e.name, e.description)
                            }
                            crate::model::CommandSide::Server => {
                                format!("{:<14} {} (server)", e.name, e.description)
                            }
                            crate::model::CommandSide::Ui(_) => {
                                format!("{:<14} {}", e.name, e.description)
                            }
                        };
                        let style = if i == *selected {
                            p.selected
                        } else if matches!(e.side, crate::model::CommandSide::Conflict) {
                            p.warn
                        } else {
                            p.fg
                        };
                        Line::styled(text, style)
                    })
                    .collect()
            };
            ("commands".to_owned(), rows)
        }
        Mode::Picker(picker) => {
            let mut rows: Vec<Line> = picker
                .title
                .iter()
                .map(|l| Line::styled(l.clone(), p.fg))
                .collect();
            if !rows.is_empty() {
                rows.push(Line::raw(""));
            }
            for (i, option) in picker.options.iter().enumerate() {
                let marker = if i == picker.selected { "> " } else { "  " };
                let style = if i == picker.selected {
                    p.selected
                } else {
                    p.fg
                };
                rows.push(Line::styled(format!("{marker}{option}"), style));
            }
            ("choose (enter picks, esc cancels)".to_owned(), rows)
        }
    };
    if body.width < 4 || body.height < 3 {
        return;
    }
    let height = ((rows.len() + 2) as u16).min(body.height);
    let width = body.width.min(72);
    let rect = Rect {
        x: body.x,
        y: body.y,
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    let block = Block::bordered().title(title).style(p.fg);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let shown: Vec<Line> = rows
        .into_iter()
        .take(inner.height as usize)
        .map(|l| {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            Line::styled(fit(&text, inner.width as usize), l.style)
        })
        .collect();
    frame.render_widget(Paragraph::new(shown), inner);
}

/// The conversation as wrapped, styled rows.
fn agent_rows<'a>(agent: &Agent, width: usize, expand_key: &str, p: &Palette) -> Vec<Line<'a>> {
    let width = width.max(4);
    let mut rows: Vec<Line> = Vec::new();
    let push = |text: &str, style: Style, rows: &mut Vec<Line<'a>>| {
        for row in wrap(text, width) {
            rows.push(Line::styled(row, style));
        }
    };
    for (entry_index, entry) in agent.entries.iter().enumerate() {
        match entry {
            Entry::System(text) => push(&format!("[system] {text}"), p.dim, &mut rows),
            Entry::User(text) => {
                let mut first = true;
                for line in text.lines() {
                    let prefix = if first { "> " } else { "  " };
                    first = false;
                    push(&format!("{prefix}{line}"), p.user, &mut rows);
                }
                if text.is_empty() {
                    push(">", p.user, &mut rows);
                }
            }
            Entry::Assistant { blocks, streaming } => {
                for (block_index, block) in blocks.iter().enumerate() {
                    match block {
                        MsgBlock::Text(text) => {
                            let lines: Vec<&str> = text.lines().collect();
                            let found = fences(text);
                            let mut i = 0;
                            while i < lines.len() {
                                let fence = found.iter().enumerate().find(|(_, f)| f.start == i);
                                match fence {
                                    Some((fi, f))
                                        if agent.rendered.contains_key(&HookTarget::Block {
                                            entry: entry_index,
                                            block: block_index,
                                            fence: fi,
                                        }) =>
                                    {
                                        let target = HookTarget::Block {
                                            entry: entry_index,
                                            block: block_index,
                                            fence: fi,
                                        };
                                        for l in &agent.rendered[&target] {
                                            push(&format!("[{}] {l}", f.tag), p.accent, &mut rows);
                                        }
                                        i = f.end + 1;
                                    }
                                    _ => {
                                        push(lines[i], p.fg, &mut rows);
                                        i += 1;
                                    }
                                }
                            }
                        }
                        MsgBlock::Thinking(text) => {
                            let first = text.lines().next().unwrap_or("").trim();
                            let n = text.chars().count();
                            push(
                                &format!("~ thinking ({n} chars) {}", truncate(first, 60)),
                                p.dim,
                                &mut rows,
                            );
                        }
                        MsgBlock::ToolCall { id, name, args } => {
                            let target = HookTarget::Tool {
                                call_id: id.clone(),
                            };
                            match agent.rendered.get(&target) {
                                Some(lines) => {
                                    for l in lines {
                                        push(&format!("[{name}] {l}"), p.accent, &mut rows);
                                    }
                                }
                                None => push(
                                    &format!("[call] {name} {}", args_summary(args, 120)),
                                    p.tool,
                                    &mut rows,
                                ),
                            }
                        }
                    }
                }
                if *streaming {
                    push("…", p.dim, &mut rows);
                }
            }
            Entry::ToolResult {
                name,
                lines,
                refs,
                is_error,
            } => {
                let (tag, style) = if *is_error {
                    ("error", p.error)
                } else {
                    ("result", p.tool)
                };
                push(&format!("[{tag}] {name}"), style, &mut rows);
                let shown = if agent.expanded {
                    lines.len()
                } else {
                    lines.len().min(RESULT_PREVIEW_LINES)
                };
                for l in lines.iter().take(shown) {
                    push(&format!("  {l}"), p.fg, &mut rows);
                }
                if shown < lines.len() {
                    push(
                        &format!(
                            "  (+{} more lines, {expand_key} expands)",
                            lines.len() - shown
                        ),
                        p.dim,
                        &mut rows,
                    );
                }
                for r in refs {
                    push(
                        &format!("  ref {} ({} bytes)", r.path, r.bytes),
                        p.accent,
                        &mut rows,
                    );
                }
            }
        }
    }
    rows
}

/// Greedy word wrap by display width; a line is always at least one row.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let mut current = String::new();
        let mut current_width = 0usize;
        for word in line.split(' ') {
            let word_width = word.width();
            let space = if current.is_empty() { 0 } else { 1 };
            if current_width + space + word_width <= width {
                if space == 1 {
                    current.push(' ');
                }
                current.push_str(word);
                current_width += space + word_width;
                continue;
            }
            if !current.is_empty() {
                rows.push(std::mem::take(&mut current));
                current_width = 0;
            }
            if word_width <= width {
                current.push_str(word);
                current_width = word_width;
            } else {
                // A word wider than the row: break it by character.
                for c in word.chars() {
                    let w = c.width().unwrap_or(0);
                    if current_width + w > width && !current.is_empty() {
                        rows.push(std::mem::take(&mut current));
                        current_width = 0;
                    }
                    current.push(c);
                    current_width += w;
                }
            }
        }
        rows.push(current);
    }
    rows
}

/// Cut `s` to `width` columns.
pub(crate) fn fit(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

/// The last `n` characters of a label, marked when cut.
fn tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        s.to_owned()
    } else {
        let skip = count - n + 1;
        format!("…{}", s.chars().skip(skip).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_by_words_and_by_chars() {
        assert_eq!(wrap("a bb ccc", 5), vec!["a bb", "ccc"]);
        assert_eq!(wrap("abcdefgh", 3), vec!["abc", "def", "gh"]);
        assert_eq!(wrap("x\n\ny", 3), vec!["x", "", "y"]);
        assert_eq!(wrap("", 3), vec![""]);
        assert_eq!(fit("héllo", 3), "hél");
        assert_eq!(tail("/a/b/c", 3), "…/c");
        assert_eq!(tail("ab", 3), "ab");
    }
}
