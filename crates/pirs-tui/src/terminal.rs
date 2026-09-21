//! The real terminal: raw mode, the alternate screen, crossterm's
//! `EventStream`, and restoring all of it on every way out, panics included.

use std::io::{self, Stdout, Write};

use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, Event};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::app::App;
use crate::engine;
use crate::keys::Key;
use crate::model::{Control, Msg};

// ---------------------------------------------------------------------------
// Never query the terminal for the cursor position.
//
// crossterm answers `cursor::position()` by writing a query and reading the
// tty for the reply. While the async `EventStream` exists, its reader thread
// owns the input, so the query blocks for its two-second timeout and the
// reply it eventually gets is read as keystrokes, corrupting the viewport.
// ratatui asks the backend for the cursor position (on `clear`, on resize);
// this wrapper answers from what it last set, and nothing in this crate calls
// `crossterm::cursor::position()`.
// ---------------------------------------------------------------------------

struct TrackedBackend {
    inner: CrosstermBackend<Stdout>,
    cursor: Position,
}

impl Backend for TrackedBackend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Ok(self.cursor)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let position = position.into();
        self.cursor = position;
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }

    fn scroll_region_up(&mut self, region: std::ops::Range<u16>, amount: u16) -> io::Result<()> {
        self.inner.scroll_region_up(region, amount)
    }

    fn scroll_region_down(&mut self, region: std::ops::Range<u16>, amount: u16) -> io::Result<()> {
        self.inner.scroll_region_down(region, amount)
    }
}

/// Put the terminal back the way it was. Safe to call more than once.
fn restore() {
    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = crossterm::execute!(
        stdout,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = stdout.flush();
}

/// Run the UI on the terminal until it quits.
pub(crate) async fn run(app: App, msgs: mpsc::UnboundedReceiver<Msg>) -> anyhow::Result<i32> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(e) = crossterm::execute!(stdout, EnterAlternateScreen, EnableBracketedPaste) {
        restore();
        return Err(e.into());
    }
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous_hook(info);
    }));

    let result = drive(app, msgs).await;
    restore();
    result
}

async fn drive(app: App, msgs: mpsc::UnboundedReceiver<Msg>) -> anyhow::Result<i32> {
    let backend = TrackedBackend {
        inner: CrosstermBackend::new(io::stdout()),
        cursor: Position::ORIGIN,
    };
    let terminal = Terminal::new(backend)?;
    let (control, control_rx) = mpsc::unbounded_channel();
    let input = tokio::spawn(async move {
        let mut events = crossterm::event::EventStream::new();
        while let Some(event) = events.next().await {
            let control_msg = match event {
                Ok(Event::Key(key)) => match Key::from_crossterm(&key) {
                    Some(key) => Control::Key(key),
                    None => continue,
                },
                Ok(Event::Paste(text)) => Control::Text(text),
                Ok(Event::Resize(_, _)) => Control::Resize,
                Ok(_) => continue,
                Err(_) => break,
            };
            if control.send(control_msg).is_err() {
                break;
            }
        }
    });
    let code = engine::run(terminal, app, control_rx, msgs).await;
    // Dropping the stream ends crossterm's reader thread.
    input.abort();
    code
}
