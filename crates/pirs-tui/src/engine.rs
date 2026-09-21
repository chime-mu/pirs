//! The event loop: one engine for the real terminal and the headless
//! backend. It draws, then waits for a driver control, an I/O message or the
//! once-a-second tick (config polling, notice expiry), and never blocks on
//! the server.

use std::time::Duration;

use ratatui::backend::Backend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::app::App;
use crate::model::{Control, Msg};
use crate::render;

/// Messages drained before each draw, so a burst of deltas is one frame.
const DRAIN_LIMIT: usize = 512;

/// Run until the app quits or the driver goes away. Returns the exit code.
pub(crate) async fn run<B>(
    mut terminal: Terminal<B>,
    mut app: App,
    mut control: mpsc::UnboundedReceiver<Control>,
    mut msgs: mpsc::UnboundedReceiver<Msg>,
) -> anyhow::Result<i32>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        drain(&mut msgs, &mut app);
        draw(&mut terminal, &mut app)?;
        if app.quit {
            break;
        }
        tokio::select! {
            ctrl = control.recv() => match ctrl {
                None => break,
                Some(Control::Key(key)) => app.handle_key(key),
                Some(Control::Text(text)) => app.handle_text(&text),
                Some(Control::Resize) => {}
                Some(Control::Screen(reply)) => {
                    drain(&mut msgs, &mut app);
                    let screen = draw(&mut terminal, &mut app)?;
                    let _ = reply.send(screen);
                }
                Some(Control::Quit) => app.quit = true,
            },
            msg = msgs.recv() => match msg {
                Some(msg) => app.handle_msg(msg),
                // `App` holds a sender through `Io`, so this cannot happen
                // while the app lives; treat it as the end all the same.
                None => break,
            },
            _ = tick.tick() => app.poll_config(),
        }
    }
    Ok(0)
}

fn drain(msgs: &mut mpsc::UnboundedReceiver<Msg>, app: &mut App) {
    for _ in 0..DRAIN_LIMIT {
        match msgs.try_recv() {
            Ok(msg) => app.handle_msg(msg),
            Err(_) => break,
        }
    }
}

/// Draw one frame and return it as text, one line per row.
fn draw<B>(terminal: &mut Terminal<B>, app: &mut App) -> anyhow::Result<String>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    app.before_draw();
    let frame = terminal.draw(|frame| render::draw(frame, app))?;
    Ok(screen_text(frame.buffer, frame.area))
}

/// The buffer as text: rows joined by newlines, trailing spaces trimmed.
pub(crate) fn screen_text(buffer: &Buffer, area: Rect) -> String {
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        let mut row = String::new();
        for x in area.left()..area.right() {
            if let Some(cell) = buffer.cell((x, y)) {
                row.push_str(cell.symbol());
            }
        }
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}
