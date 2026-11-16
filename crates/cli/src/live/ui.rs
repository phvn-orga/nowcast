//! Draws the [`Model`] on a terminal frame.

use jiff::Timestamp;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use serde_json::Value;

use super::model::{Link, Model, Screen, Tab, brick_of, items, text};
use crate::display::{self, Health};

/// Where the clickable parts landed on the last frame.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Hits {
    pub tabs: Vec<(Rect, Tab)>,
    /// The list's rows, inside its border, and the row shown first.
    pub list: Option<(Rect, usize)>,
}

const HELP: &str = "←/→ or Tab views · ↑/↓ move · Enter open · Esc back · q quit";

pub fn draw(frame: &mut Frame, model: &Model, now: Timestamp) -> Hits {
    let [header, banner, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(u16::from(model.link != Link::Up)),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let mut hits = Hits {
        tabs: tabs(frame, header, model.tab()),
        list: None,
    };
    if let Link::Down(reason) = &model.link {
        let line = format!(" {reason}: waiting for the service to run again, without starting it");
        frame.render_widget(
            Paragraph::new(line).style(Style::new().fg(Color::Black).bg(Color::Yellow)),
            banner,
        );
    }
    let status = model.status.as_ref().unwrap_or(&Value::Null);
    match &model.screen {
        Screen::Tab(Tab::Overview) => overview(frame, body, model, status, now),
        Screen::Tab(Tab::Bricks) => {
            let rows = items(&status["bricks"])
                .iter()
                .map(|brick| brick_row(brick, model))
                .collect();
            hits.list = list(frame, body, "Bricks", rows, model.selected());
        }
        Screen::Tab(Tab::Sessions) => sessions(frame, body, model, status, now, &mut hits),
        Screen::Tab(Tab::Flows) => {
            let rows = model
                .flow_ids()
                .iter()
                .map(|id| flow_row(id, status, model, now))
                .collect();
            hits.list = list(frame, body, "Flows", rows, model.selected());
        }
        Screen::Flow(flow) => flow_screen(frame, body, model, status, flow, now, &mut hits),
        Screen::Event { flow, id } => event_screen(frame, body, model, flow, id, now),
    }
    frame.render_widget(Paragraph::new(HELP).dark_gray(), footer);
    hits
}

fn tabs(frame: &mut Frame, area: Rect, current: Tab) -> Vec<(Rect, Tab)> {
    let titles: Vec<_> = Tab::ALL
        .iter()
        .map(|t| format!(" {} ", t.title()))
        .collect();
    let index = Tab::ALL.iter().position(|&t| t == current).unwrap_or(0);
    frame.render_widget(
        Tabs::new(titles.clone())
            .select(index)
            .highlight_style(Style::new().reversed())
            .divider("│")
            .padding("", ""),
        area,
    );
    // Each title, then a one-column divider.
    let mut x = area.x;
    let mut hits = Vec::new();
    for (title, &tab) in titles.iter().zip(Tab::ALL.iter()) {
        let width = u16::try_from(title.chars().count()).unwrap_or(u16::MAX);
        hits.push((Rect::new(x, area.y, width, 1).intersection(area), tab));
        x = x.saturating_add(width + 1);
    }
    hits
}

/// A bordered list with `selected` highlighted; where its rows landed.
fn list(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    rows: Vec<ListItem>,
    selected: usize,
) -> Option<(Rect, usize)> {
    let empty = rows.is_empty();
    let block = Block::bordered().title(format!(" {title} "));
    let inner = block.inner(area);
    if empty {
        frame.render_widget(Paragraph::new("none").dark_gray().block(block), area);
        return None;
    }
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        List::new(rows)
            .block(block)
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
        area,
        &mut state,
    );
    Some((inner, state.offset()))
}

fn health_color(health: Health) -> Color {
    match health {
        Health::Up => Color::Green,
        Health::Moving => Color::Yellow,
        Health::Idle => Color::DarkGray,
        Health::Failing => Color::Red,
    }
}

fn state_span(state: &str) -> Span<'static> {
    let health = Health::of(state);
    Span::styled(
        format!("{} {state}", health.symbol()),
        Style::new().fg(health_color(health)),
    )
}

fn overview(frame: &mut Frame, area: Rect, model: &Model, status: &Value, now: Timestamp) {
    let service = &status["service"];
    let installed = env!("CARGO_PKG_VERSION");
    let version = text(service, "version");
    let up = service["started_at"]
        .as_str()
        .and_then(|t| display::since(t, now))
        .map_or_else(|| "?".to_owned(), display::duration);
    let bricks = items(&status["bricks"]);
    let running = bricks
        .iter()
        .filter(|b| Health::of(&text(b, "state")) == Health::Up)
        .count();
    let failing: Vec<String> = bricks
        .iter()
        .filter(|b| Health::of(&text(b, "state")) == Health::Failing)
        .map(|b| text(b, "name"))
        .collect();
    let mut lines = vec![
        Line::from(vec![
            Span::raw("service    "),
            Span::raw(version.clone()).bold(),
            Span::raw(format!(", up {up}")),
        ]),
        Line::from(if version == installed {
            format!("installed  {installed}")
        } else {
            format!("installed  {installed}, not the service's")
        }),
        Line::from(format!(
            "bricks     {} enabled, {running} running",
            bricks.len()
        )),
    ];
    if !failing.is_empty() {
        lines.push(Line::from(format!("           failing: {}", failing.join(", "))).red());
    }
    lines.push(Line::from(format!(
        "sessions   {}",
        items(&status["sessions"]).len()
    )));
    lines.push(Line::from(format!("flows      {}", model.flow_ids().len())));
    let [top, recent] = Layout::vertical([
        Constraint::Length(u16::try_from(lines.len()).unwrap_or(u16::MAX) + 2),
        Constraint::Fill(1),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" nowcast ")),
        top,
    );
    let changes: Vec<ListItem> = model
        .recent
        .iter()
        .map(|change| {
            let ago = display::duration(std::time::Duration::from_secs(
                (now.as_second() - change.at.as_second())
                    .max(0)
                    .unsigned_abs(),
            ));
            let mut spans = vec![
                Span::raw(format!("{ago:>9} ago  ")).dark_gray(),
                Span::raw(change.what.clone()),
            ];
            if change.times > 1 {
                spans.push(Span::raw(format!("  ×{}", change.times)).dark_gray());
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let block = Block::bordered().title(" Recent changes ");
    if changes.is_empty() {
        frame.render_widget(
            Paragraph::new("nothing yet").dark_gray().block(block),
            recent,
        );
    } else {
        frame.render_widget(List::new(changes).block(block), recent);
    }
}

fn brick_row(brick: &Value, model: &Model) -> ListItem<'static> {
    let name = text(brick, "name");
    let mut spans = vec![
        Span::raw(format!("{name:<16}")),
        state_span(&text(brick, "state")),
    ];
    if let Some(pid) = brick["pid"].as_u64() {
        spans.push(Span::raw(format!("  pid {pid}")));
    }
    match brick["crashes"].as_u64() {
        Some(0) | None => {}
        Some(n) => spans.push(Span::raw(format!("  {n} crashes in a row")).red()),
    }
    let flows = model
        .flow_ids()
        .into_iter()
        .filter(|f| brick_of(f) == name)
        .count();
    let plural = if flows == 1 { "" } else { "s" };
    spans.push(Span::raw(format!("  serves {flows} flow{plural}")).dark_gray());
    ListItem::new(Line::from(spans))
}

fn flow_row(id: &str, status: &Value, model: &Model, now: Timestamp) -> ListItem<'static> {
    ListItem::new(Line::from(flow_spans(
        model.label(id),
        id,
        status,
        model,
        now,
    )))
}

/// A flow shown as `label`: its subscribers, its last event, its error.
fn flow_spans(
    label: String,
    id: &str,
    status: &Value,
    model: &Model,
    now: Timestamp,
) -> Vec<Span<'static>> {
    let summary = items(&status["flows"]).iter().find(|f| text(f, "id") == id);
    let subscribers = summary.and_then(|f| f["subscribers"].as_u64()).unwrap_or(0);
    let log = model.flows.get(id);
    let last = log
        .and_then(|l| l.events.back())
        .map(|e| e.time.clone())
        .or_else(|| summary.and_then(|f| f["last_event"]["time"].as_str().map(str::to_owned)));
    let mut spans = vec![
        Span::raw(label),
        Span::raw(format!("  {subscribers} sub.")).dark_gray(),
        Span::raw(match last {
            Some(time) => format!("  last {}", display::ago(&time, now)),
            None => "  nothing yet".to_owned(),
        })
        .dark_gray(),
    ];
    if let Some(error) = log.and_then(|l| l.error.as_ref()) {
        spans.push(Span::raw(format!("  ▲ {error}")).red());
    }
    spans
}

fn sessions(
    frame: &mut Frame,
    area: Rect,
    model: &Model,
    status: &Value,
    now: Timestamp,
    hits: &mut Hits,
) {
    // One row per subscription, led by its session: the rows the model
    // selects.
    let width = items(&status["sessions"])
        .iter()
        .map(|s| display::short_session(&text(s, "id")).chars().count())
        .max()
        .unwrap_or(0);
    let mut rows = Vec::new();
    for session in items(&status["sessions"]) {
        let id = text(session, "id");
        let short = display::short_session(&id).to_owned();
        for subscription in items(&session["subscriptions"]) {
            let flow = text(subscription, "flow");
            let shown = display::flow_in_session(&flow, &id);
            let mut line = vec![Span::raw(format!("{short:<width$}  ")).bold()];
            line.extend(flow_spans(shown, &flow, status, model, now));
            rows.push(ListItem::new(Line::from(line)));
        }
    }
    let idle: Vec<String> = items(&status["sessions"])
        .iter()
        .filter(|s| items(&s["subscriptions"]).is_empty())
        .map(|s| display::short_session(&text(s, "id")).to_owned())
        .collect();
    let [list_area, idle_area] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(u16::from(!idle.is_empty())),
    ])
    .areas(area);
    hits.list = list(frame, list_area, "Sessions", rows, model.selected());
    if !idle.is_empty() {
        frame.render_widget(
            Paragraph::new(format!(" no subscription: {}", idle.join(", "))).dark_gray(),
            idle_area,
        );
    }
}

fn flow_screen(
    frame: &mut Frame,
    area: Rect,
    model: &Model,
    status: &Value,
    flow: &str,
    now: Timestamp,
    hits: &mut Hits,
) {
    let subscribers: Vec<String> = items(&status["sessions"])
        .iter()
        .filter(|s| {
            items(&s["subscriptions"])
                .iter()
                .any(|sub| text(sub, "flow") == flow)
        })
        .map(|s| display::short_session(&text(s, "id")).to_owned())
        .collect();
    let log = model.flows.get(flow);
    let mut head = vec![Line::from(format!(
        "subscribers  {}",
        if subscribers.is_empty() {
            "none".to_owned()
        } else {
            subscribers.join(", ")
        }
    ))];
    if let Some(error) = log.and_then(|l| l.error.as_ref()) {
        head.push(Line::from(format!("error        {error}")).red());
    }
    let [top, events] = Layout::vertical([
        Constraint::Length(u16::try_from(head.len()).unwrap_or(u16::MAX) + 2),
        Constraint::Fill(1),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(head).block(Block::bordered().title(format!(" {} ", model.label(flow)))),
        top,
    );
    let rows = log
        .map(|l| {
            l.events
                .iter()
                .rev()
                .map(|e| {
                    let detail = e
                        .event
                        .get("data")
                        .map(Value::to_string)
                        .unwrap_or_default();
                    ListItem::new(Line::from(vec![
                        Span::raw(format!("{:>9} ago  ", short_ago(&e.time, now))).dark_gray(),
                        Span::raw(format!("{}  ", e.data_type)),
                        Span::raw(detail).dark_gray(),
                    ]))
                })
                .collect()
        })
        .unwrap_or_default();
    hits.list = list(frame, events, "Events", rows, model.selected());
}

fn short_ago(time: &str, now: Timestamp) -> String {
    display::since(time, now).map_or_else(|| "?".to_owned(), display::duration)
}

fn event_screen(
    frame: &mut Frame,
    area: Rect,
    model: &Model,
    flow: &str,
    id: &str,
    now: Timestamp,
) {
    let event = model
        .flows
        .get(flow)
        .and_then(|l| l.events.iter().find(|e| e.id == id));
    let lines: Vec<Line> = match event {
        Some(e) => {
            let mut lines = vec![
                Line::from(format!("type    {}", e.data_type)),
                Line::from(format!(
                    "time    {}, {}",
                    e.time,
                    display::ago(&e.time, now)
                )),
                Line::from(format!("source  {}", text(&e.event, "source"))),
                Line::from(""),
            ];
            let detail = e.event.get("data").unwrap_or(&Value::Null);
            lines.extend(pretty(detail).lines().map(|l| Line::from(l.to_owned())));
            lines
        }
        None => vec![Line::from("this event is no longer kept").dark_gray()],
    };
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((model.scroll, 0))
            .block(Block::bordered().title(format!(" {} · event {id} ", model.label(flow)))),
        area,
    );
}

/// JSON indented by two spaces, an array of plain values kept on one line.
fn pretty(value: &Value) -> String {
    let mut out = String::new();
    write_pretty(value, 0, &mut out);
    out
}

fn write_pretty(value: &Value, depth: usize, out: &mut String) {
    let pad = |depth: usize| "  ".repeat(depth);
    match value {
        Value::Array(items) if items.iter().all(|v| !v.is_array() && !v.is_object()) => {
            out.push_str(&value.to_string());
        }
        Value::Array(items) => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                out.push_str(&pad(depth + 1));
                write_pretty(item, depth + 1, out);
                out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(depth));
            out.push(']');
        }
        Value::Object(fields) if !fields.is_empty() => {
            out.push_str("{\n");
            for (i, (key, item)) in fields.iter().enumerate() {
                out.push_str(&pad(depth + 1));
                out.push_str(&Value::from(key.as_str()).to_string());
                out.push_str(": ");
                write_pretty(item, depth + 1, out);
                out.push_str(if i + 1 < fields.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(depth));
            out.push('}');
        }
        _ => out.push_str(&value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn plain_arrays_stay_on_one_line() {
        let value = json!({ "cpu": { "cores": [1, 2], "load": [] }, "disks": [ { "name": "a" } ], "e": {} });
        assert_eq!(
            pretty(&value),
            "{\n  \"cpu\": {\n    \"cores\": [1,2],\n    \"load\": []\n  },\n  \"disks\": [\n    {\n      \"name\": \"a\"\n    }\n  ],\n  \"e\": {}\n}"
        );
    }
}
