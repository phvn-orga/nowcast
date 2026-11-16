//! `nowcast live`: a full-screen view of what the service does, as it does it.
//!
//! The state comes from `GET /v1/status`, read every second, and the events
//! from `GET /v1/observe`, which counts as no subscriber. Like `status`, it
//! never starts a stopped service: under systemd, it asks systemd whether the
//! service runs before every connection. When the service goes away, a banner
//! says so and `live` waits for it to run again.

mod model;
mod ui;

use std::io::{self, IsTerminal};
use std::time::Duration;

use jiff::Timestamp;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::Position;
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::client::{Client, EventStream};
use crate::commands::status::{systemd_states, waits_for_a_connection};
use crate::error::CliError;

use model::{Model, Tab};
use ui::Hits;

/// How often the state is read again.
const POLL: Duration = Duration::from_secs(1);

pub async fn live(client: &Client, ask_systemd: bool) -> Result<(), CliError> {
    let status = match running(client, ask_systemd).await? {
        Some(status) => status,
        None => {
            return Err(CliError::Unreachable {
                socket: client.socket().to_owned(),
                reason: "the service is stopped, and nowcast live does not start it: \
                         nowcast status tells what runs"
                    .into(),
            });
        }
    };
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        return Err(CliError::Usage(
            "nowcast live needs a terminal: nowcast status prints the state once, \
             nowcast watch the events of a session, one per line"
                .into(),
        ));
    }
    let mut terminal = ratatui::try_init().map_err(CliError::Io)?;
    // `try_init`'s panic hook restores the screen; the mouse is ours.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        hook(info);
    }));
    let result = match execute!(io::stdout(), EnableMouseCapture) {
        Ok(()) => run(client, ask_systemd, status, &mut terminal).await,
        Err(e) => Err(CliError::Io(e)),
    };
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::try_restore().map_err(CliError::Io)?;
    result
}

/// The service's state, `None` when it is stopped. Under systemd, a service
/// a connection would start counts as stopped, and is left alone.
async fn running(client: &Client, ask_systemd: bool) -> Result<Option<Value>, CliError> {
    if ask_systemd && waits_for_a_connection(&systemd_states()) {
        return Ok(None);
    }
    client.status().await
}

/// What the screen reacts to.
enum Message {
    Input(Event),
    Status(Value),
    Down(String),
    Observed(String, Value),
}

async fn run(
    client: &Client,
    ask_systemd: bool,
    status: Value,
    terminal: &mut ratatui::DefaultTerminal,
) -> Result<(), CliError> {
    let (tx, mut rx) = unbounded_channel();
    read_input(tx.clone());
    let mut model = Model::new(status);
    let mut hits = Hits::default();
    let mut service = std::pin::pin!(follow(client, ask_systemd, tx));
    while !model.quit {
        terminal
            .draw(|frame| hits = ui::draw(frame, &model, Timestamp::now()))
            .map_err(CliError::Io)?;
        let message = tokio::select! {
            message = rx.recv() => message,
            // Only ends on an error of the screen's own: it loops otherwise.
            () = &mut service => None,
        };
        let now = Timestamp::now();
        match message {
            Some(Message::Input(event)) => handle(&mut model, &hits, &event),
            Some(Message::Status(status)) => model.status(status, now),
            Some(Message::Down(reason)) => model.down(reason, now),
            Some(Message::Observed(name, data)) => model.event(&name, &data, now),
            None => break,
        }
    }
    Ok(())
}

/// Reads the terminal's events on a thread of its own, since reading
/// blocks. It ends with the process.
fn read_input(tx: UnboundedSender<Message>) {
    std::thread::spawn(move || {
        while let Ok(event) = event::read() {
            if tx.send(Message::Input(event)).is_err() {
                return;
            }
        }
    });
}

/// Follows the service until `tx` closes: its state every [`POLL`], its
/// events as they come. Asks systemd before each connection, so that a
/// stopped service stays stopped.
async fn follow(client: &Client, ask_systemd: bool, tx: UnboundedSender<Message>) {
    let mut poll = tokio::time::interval(POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut stream: Option<EventStream> = None;
    loop {
        let sent = tokio::select! {
            _ = poll.tick() => match running(client, ask_systemd).await {
                Ok(Some(status)) => {
                    if stream.is_none() {
                        match client.observe().await {
                            Ok(opened) => stream = Some(opened),
                            Err(e) => {
                                let _ = tx.send(Message::Down(e.message()));
                                continue;
                            }
                        }
                    }
                    tx.send(Message::Status(status))
                }
                Ok(None) => {
                    stream = None;
                    tx.send(Message::Down("the service is stopped".into()))
                }
                Err(e) => {
                    stream = None;
                    tx.send(Message::Down(e.message()))
                }
            },
            event = next(&mut stream) => match event {
                Ok(Some(event)) => match serde_json::from_str(&event.data) {
                    Ok(data) => tx.send(Message::Observed(event.event, data)),
                    Err(_) => continue,
                },
                Ok(None) => {
                    stream = None;
                    tx.send(Message::Down("the service closed the connection".into()))
                }
                Err(e) => {
                    stream = None;
                    tx.send(Message::Down(e.message()))
                }
            },
        };
        if sent.is_err() {
            return;
        }
    }
}

/// The stream's next event; never, without a stream.
async fn next(stream: &mut Option<EventStream>) -> Result<Option<crate::sse::SseEvent>, CliError> {
    match stream {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

/// What a key or the mouse does.
fn handle(model: &mut Model, hits: &Hits, event: &Event) {
    match event {
        Event::Key(key) => handle_key(model, *key),
        Event::Mouse(mouse) => handle_mouse(model, hits, *mouse),
        _ => {}
    }
}

fn handle_key(model: &mut Model, key: KeyEvent) {
    if key.kind == KeyEventKind::Release {
        return;
    }
    match key.code {
        KeyCode::Char('q') => model.quit = true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => model.quit = true,
        KeyCode::Tab | KeyCode::Right => model.next_tab(true),
        KeyCode::BackTab | KeyCode::Left => model.next_tab(false),
        KeyCode::Char(digit @ '1'..='4') => {
            let index = usize::from(digit as u8 - b'1');
            model.show_tab(Tab::ALL[index]);
        }
        KeyCode::Up | KeyCode::Char('k') => model.move_by(-1),
        KeyCode::Down | KeyCode::Char('j') => model.move_by(1),
        KeyCode::PageUp => model.move_by(-10),
        KeyCode::PageDown => model.move_by(10),
        KeyCode::Enter => model.open(),
        KeyCode::Esc | KeyCode::Backspace => model.back(),
        _ => {}
    }
}

fn handle_mouse(model: &mut Model, hits: &Hits, mouse: MouseEvent) {
    let at = Position::new(mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::ScrollUp => model.move_by(-1),
        MouseEventKind::ScrollDown => model.move_by(1),
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some((_, tab)) = hits.tabs.iter().find(|(area, _)| area.contains(at)) {
                model.show_tab(*tab);
                return;
            }
            let Some((area, offset)) = hits.list else {
                return;
            };
            if !area.contains(at) {
                return;
            }
            let row = offset + usize::from(at.y - area.y);
            if row >= model.rows().len() {
                return;
            }
            // A click selects; a click on the selected row opens it.
            if row == model.selected() {
                model.open();
            } else {
                model.select(row);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use serde_json::json;

    use super::model::{Screen, Tab};
    use super::*;

    fn now() -> Timestamp {
        "2026-11-08T16:45:12Z".parse().unwrap()
    }

    fn model() -> Model {
        let session = "5f1c2e9a-0b1c-4d2e-8f3a-4b5c6d7e8f90";
        let flow = format!("claude-code/session/{session}");
        let mut model = Model::new(json!({
            "service": { "version": env!("CARGO_PKG_VERSION"), "started_at": "2026-11-08T16:41:00Z", "path": null },
            "bricks": [
                { "name": "claude-code", "state": "running", "pid": 868412, "crashes": 0 },
                { "name": "forge", "state": "waiting", "pid": null, "crashes": 2 },
            ],
            "sessions": [
                { "id": session, "subscriptions": [ { "id": "s-1", "flow": flow } ] },
                { "id": "9a0b", "subscriptions": [] },
            ],
            "flows": [ { "id": flow, "subscribers": 1, "last_event": null } ],
        }));
        model.event(
            "update",
            &json!({
                "id": "42", "subject": flow, "type": "claude-code.session/v1",
                "time": "2026-11-08T16:45:09Z", "data": { "model": "opus" },
            }),
            now(),
        );
        model
    }

    /// The screen `model` draws, as lines of text.
    fn screen(model: &Model) -> (String, Hits) {
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        let mut hits = Hits::default();
        terminal
            .draw(|frame| hits = ui::draw(frame, model, now()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        (text, hits)
    }

    fn key(model: &mut Model, code: KeyCode) {
        handle_key(model, KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn the_overview_sums_the_service_up() {
        let (text, _) = screen(&model());
        assert!(
            text.contains(&format!(
                "service    {}, up 4 min",
                env!("CARGO_PKG_VERSION")
            )),
            "{text}"
        );
        assert!(text.contains("bricks     2 enabled, 1 running"), "{text}");
        assert!(text.contains("failing: forge"), "{text}");
        assert!(
            text.contains("claude-code/session/5f1c2e9a published claude-code.session/v1"),
            "{text}"
        );
    }

    #[test]
    fn every_view_draws() {
        let mut model = model();
        key(&mut model, KeyCode::Char('2'));
        let (text, _) = screen(&model);
        assert!(text.contains("● running  pid 868412"), "{text}");
        assert!(text.contains("▲ waiting  2 crashes in a row"), "{text}");

        key(&mut model, KeyCode::Tab);
        let (text, _) = screen(&model);
        assert!(text.contains("5f1c2e9a  claude-code/session/…"), "{text}");
        assert!(text.contains("no subscription: 9a0b"), "{text}");

        key(&mut model, KeyCode::Enter);
        let (text, _) = screen(&model);
        assert!(text.contains("subscribers  5f1c2e9a"), "{text}");
        assert!(text.contains("3 s ago  claude-code.session/v1"), "{text}");

        key(&mut model, KeyCode::Enter);
        let (text, _) = screen(&model);
        assert!(text.contains("type    claude-code.session/v1"), "{text}");
        assert!(text.contains("\"model\": \"opus\""), "{text}");
    }

    #[test]
    fn an_outage_shows_a_banner() {
        let mut model = model();
        model.down("the service is stopped".into(), now());
        let (text, _) = screen(&model);
        assert!(
            text.contains("the service is stopped: waiting for the service to run again"),
            "{text}"
        );
    }

    #[test]
    fn the_mouse_picks_a_tab_and_a_row() {
        let mut model = model();
        let (_, hits) = screen(&model);
        let flows = hits.tabs.iter().find(|(_, t)| *t == Tab::Flows).unwrap().0;
        let click = |x, y| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse(&mut model, &hits, click(flows.x, flows.y));
        assert_eq!(model.screen, Screen::Tab(Tab::Flows));

        let (_, hits) = screen(&model);
        let (rows, _): (Rect, usize) = hits.list.unwrap();
        // The first row is selected already: a click opens it.
        handle_mouse(&mut model, &hits, click(rows.x, rows.y));
        assert!(matches!(model.screen, Screen::Flow(_)));
        // Below the last row: nothing.
        let (_, hits) = screen(&model);
        let (rows, _) = hits.list.unwrap();
        handle_mouse(&mut model, &hits, click(rows.x, rows.y + 5));
        assert!(matches!(model.screen, Screen::Flow(_)));
    }

    #[test]
    fn q_quits() {
        let mut model = model();
        key(&mut model, KeyCode::Char('q'));
        assert!(model.quit);
    }
}
