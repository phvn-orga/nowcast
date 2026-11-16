//! What `live` knows and shows, kept apart from the terminal so that it can
//! be tested: the last state the service told, the events observed, what
//! changed recently, and where the user is.

use std::collections::{BTreeMap, VecDeque};

use jiff::Timestamp;
use serde_json::Value;

/// How many events each flow keeps on screen, as many as the service keeps.
pub const FLOW_EVENTS: usize = 100;

/// How many recent changes the overview keeps.
pub const RECENT: usize = 200;

/// The views, in tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Bricks,
    Sessions,
    Flows,
}

impl Tab {
    pub const ALL: [Self; 4] = [Self::Overview, Self::Bricks, Self::Sessions, Self::Flows];

    pub fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Bricks => "Bricks",
            Self::Sessions => "Sessions",
            Self::Flows => "Flows",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|&t| t == self).unwrap_or(0)
    }
}

/// What the screen shows: a tab's list, or what was opened from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Tab(Tab),
    /// One flow: its subscribers, then its events, newest first.
    Flow(String),
    /// One event of a flow, by its id.
    Event {
        flow: String,
        id: String,
    },
}

/// An event seen on the observer's stream.
#[derive(Debug, Clone, PartialEq)]
pub struct Observed {
    pub id: String,
    pub time: String,
    pub data_type: String,
    /// The CloudEvents event, as the service sent it.
    pub event: Value,
}

/// What `live` knows of a flow beyond the state: its events and signals.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FlowLog {
    /// Newest last.
    pub events: VecDeque<Observed>,
    /// The last `flow-error`, until the next update.
    pub error: Option<String>,
}

/// Something that changed, for the overview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// When it last happened.
    pub at: Timestamp,
    pub what: String,
    /// How many times in a row: a flow publishing again adds to its last
    /// change rather than burying the others.
    pub times: u32,
}

/// The connection to the service, as the user sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Up,
    /// Lost, with why: `live` waits for the service to run again.
    Down(String),
}

#[derive(Debug)]
pub struct Model {
    /// The service's last `GET /v1/status` answer.
    pub status: Option<Value>,
    pub link: Link,
    pub flows: BTreeMap<String, FlowLog>,
    /// Newest first.
    pub recent: VecDeque<Change>,
    pub screen: Screen,
    /// The selected row of each screen's list, kept when leaving it.
    selected: BTreeMap<String, usize>,
    /// How far the event screen is scrolled.
    pub scroll: u16,
    pub quit: bool,
}

impl Model {
    pub fn new(status: Value) -> Self {
        Self {
            status: Some(status),
            link: Link::Up,
            flows: BTreeMap::new(),
            recent: VecDeque::new(),
            screen: Screen::Tab(Tab::Overview),
            selected: BTreeMap::new(),
            scroll: 0,
            quit: false,
        }
    }

    /// A new state from the service: what differs from the last one goes to
    /// the recent changes.
    pub fn status(&mut self, status: Value, now: Timestamp) {
        if self.link != Link::Up {
            self.link = Link::Up;
            self.note(now, "connected to the service".to_owned());
        }
        if let Some(old) = &self.status {
            for what in changes(old, &status) {
                self.note(now, what);
            }
        }
        self.status = Some(status);
    }

    /// The service is gone: said once per outage.
    pub fn down(&mut self, reason: String, now: Timestamp) {
        if self.link == Link::Up {
            self.note(now, format!("lost the service: {reason}"));
        }
        self.link = Link::Down(reason);
    }

    /// An event of the observer's stream: `name` its SSE event, `data` its
    /// data as JSON.
    pub fn event(&mut self, name: &str, data: &Value, now: Timestamp) {
        let flow = match name {
            "update" => data.get("subject"),
            _ => data.get("flow"),
        }
        .and_then(Value::as_str)
        .map(str::to_owned);
        let Some(flow) = flow else {
            return;
        };
        let label = self.label(&flow);
        match name {
            "update" => {
                let id = text(data, "id");
                let log = self.flows.entry(flow.clone()).or_default();
                // On reconnection the stream sends each last value again.
                if log.events.iter().any(|e| e.id == id) {
                    return;
                }
                log.error = None;
                log.events.push_back(Observed {
                    id,
                    time: text(data, "time"),
                    data_type: text(data, "type"),
                    event: data.clone(),
                });
                if log.events.len() > FLOW_EVENTS {
                    log.events.pop_front();
                }
                self.note(now, format!("{label} published {}", text(data, "type")));
            }
            "flow-error" => {
                let message = text(data, "message");
                self.flows.entry(flow.clone()).or_default().error = Some(message.clone());
                self.note(now, format!("{label} failed: {message}"));
            }
            "flow-ended" => {
                self.flows.remove(&flow);
                self.note(now, format!("{label} ended"));
            }
            _ => {}
        }
    }

    /// A flow id as shown: a declared session's id in it is cut short.
    pub fn label(&self, flow: &str) -> String {
        let status = self.status.as_ref().unwrap_or(&Value::Null);
        let mut label = flow.to_owned();
        for session in names(&status["sessions"], "id") {
            let short = crate::display::short_session(&session);
            if short != session {
                label = label.replace(&session, short);
            }
        }
        label
    }

    fn note(&mut self, at: Timestamp, what: String) {
        if let Some(last) = self.recent.front_mut()
            && last.what == what
        {
            last.at = at;
            last.times += 1;
            return;
        }
        self.recent.push_front(Change { at, what, times: 1 });
        self.recent.truncate(RECENT);
    }

    // Navigation.

    /// The rows the current screen lists, as the keys it opens: brick names,
    /// session ids, flow ids, event ids.
    pub fn rows(&self) -> Vec<String> {
        let status = self.status.as_ref().unwrap_or(&Value::Null);
        match &self.screen {
            Screen::Tab(Tab::Overview) | Screen::Event { .. } => Vec::new(),
            Screen::Tab(Tab::Bricks) => names(&status["bricks"], "name"),
            Screen::Tab(Tab::Sessions) => {
                // A session's subscriptions, each a row opening its flow.
                let mut rows = Vec::new();
                for session in items(&status["sessions"]) {
                    for subscription in items(&session["subscriptions"]) {
                        rows.push(text(subscription, "flow"));
                    }
                }
                rows
            }
            Screen::Tab(Tab::Flows) => self.flow_ids(),
            Screen::Flow(flow) => self
                .flows
                .get(flow)
                .map(|log| log.events.iter().rev().map(|e| e.id.clone()).collect())
                .unwrap_or_default(),
        }
    }

    /// Every flow known: the state's, with a subscriber, and those observed.
    pub fn flow_ids(&self) -> Vec<String> {
        let status = self.status.as_ref().unwrap_or(&Value::Null);
        let mut ids = names(&status["flows"], "id");
        for id in self.flows.keys() {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        ids.sort();
        ids
    }

    fn key(&self) -> String {
        match &self.screen {
            Screen::Tab(tab) => tab.title().to_owned(),
            Screen::Flow(flow) => format!("flow {flow}"),
            Screen::Event { flow, id } => format!("event {flow} {id}"),
        }
    }

    /// The selected row of the current screen, kept within its rows.
    pub fn selected(&self) -> usize {
        let last = self.rows().len().saturating_sub(1);
        self.selected
            .get(&self.key())
            .copied()
            .unwrap_or(0)
            .min(last)
    }

    pub fn select(&mut self, row: usize) {
        let key = self.key();
        self.selected.insert(key, row);
    }

    pub fn tab(&self) -> Tab {
        match self.screen {
            Screen::Tab(tab) => tab,
            Screen::Flow(_) | Screen::Event { .. } => Tab::Flows,
        }
    }

    pub fn show_tab(&mut self, tab: Tab) {
        self.screen = Screen::Tab(tab);
    }

    pub fn next_tab(&mut self, forward: bool) {
        let count = Tab::ALL.len();
        let index = self.tab().index();
        let next = if forward {
            (index + 1) % count
        } else {
            (index + count - 1) % count
        };
        self.show_tab(Tab::ALL[next]);
    }

    /// Up or down by `by` rows, or scrolls the event screen.
    pub fn move_by(&mut self, by: isize) {
        if let Screen::Event { .. } = self.screen {
            self.scroll = self
                .scroll
                .saturating_add_signed(by.clamp(-100, 100) as i16);
            return;
        }
        let rows = self.rows().len();
        if rows == 0 {
            return;
        }
        let row = self.selected().saturating_add_signed(by).min(rows - 1);
        self.select(row);
    }

    /// Opens the selected row: a brick's or a subscription's flow, a flow's
    /// event.
    pub fn open(&mut self) {
        let rows = self.rows();
        let Some(row) = rows.get(self.selected()).cloned() else {
            return;
        };
        match self.screen.clone() {
            Screen::Tab(Tab::Sessions | Tab::Flows) => self.screen = Screen::Flow(row),
            Screen::Tab(Tab::Bricks) => {
                // A brick opens the flows tab, on its first flow.
                let flows = self.flow_ids();
                self.screen = Screen::Tab(Tab::Flows);
                if let Some(first) = flows.iter().position(|f| brick_of(f) == row) {
                    self.select(first);
                }
            }
            Screen::Flow(flow) => {
                self.scroll = 0;
                self.screen = Screen::Event { flow, id: row };
            }
            Screen::Tab(Tab::Overview) | Screen::Event { .. } => {}
        }
    }

    /// Back to where the current screen was opened from.
    pub fn back(&mut self) {
        self.screen = match &self.screen {
            Screen::Event { flow, .. } => Screen::Flow(flow.clone()),
            Screen::Flow(_) => Screen::Tab(Tab::Flows),
            Screen::Tab(_) => Screen::Tab(Tab::Overview),
        };
    }
}

/// The brick a flow id belongs to: what comes before its first `/`.
pub fn brick_of(flow: &str) -> &str {
    flow.split_once('/').map_or(flow, |(brick, _)| brick)
}

/// What differs between two states, said for a human.
fn changes(old: &Value, new: &Value) -> Vec<String> {
    let mut said = Vec::new();
    if old["service"]["started_at"] != new["service"]["started_at"] {
        said.push(format!(
            "the service restarted, version {}",
            text(&new["service"], "version")
        ));
    }
    for brick in items(&new["bricks"]) {
        let name = text(brick, "name");
        let before = items(&old["bricks"])
            .iter()
            .find(|b| text(b, "name") == name)
            .map(|b| text(b, "state"));
        let state = text(brick, "state");
        match before {
            None => said.push(format!("brick {name} enabled, {state}")),
            Some(before) if before != state => {
                said.push(format!("brick {name} {before} → {state}"));
            }
            Some(_) => {}
        }
    }
    let old_bricks = names(&old["bricks"], "name");
    let new_bricks = names(&new["bricks"], "name");
    for gone in old_bricks.iter().filter(|b| !new_bricks.contains(b)) {
        said.push(format!("brick {gone} gone"));
    }
    let subscriptions = |status: &Value| -> Vec<(String, String)> {
        items(&status["sessions"])
            .iter()
            .flat_map(|s| {
                let session = text(s, "id");
                items(&s["subscriptions"])
                    .iter()
                    .map(move |sub| (session.clone(), text(sub, "flow")))
            })
            .collect()
    };
    let old_sessions = names(&old["sessions"], "id");
    let new_sessions = names(&new["sessions"], "id");
    for session in new_sessions.iter().filter(|s| !old_sessions.contains(s)) {
        said.push(format!(
            "session {} declared",
            crate::display::short_session(session)
        ));
    }
    for session in old_sessions.iter().filter(|s| !new_sessions.contains(s)) {
        said.push(format!(
            "session {} gone",
            crate::display::short_session(session)
        ));
    }
    let (before, after) = (subscriptions(old), subscriptions(new));
    for (session, flow) in after.iter().filter(|s| !before.contains(s)) {
        said.push(format!(
            "session {} subscribed to {flow}",
            crate::display::short_session(session)
        ));
    }
    for (session, flow) in before.iter().filter(|s| !after.contains(s)) {
        // A session gone takes its subscriptions with it: said once.
        if new_sessions.contains(session) {
            said.push(format!(
                "session {} unsubscribed from {flow}",
                crate::display::short_session(session)
            ));
        }
    }
    said
}

pub fn items(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

/// A string field, `?` when absent.
pub fn text(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_owned()
}

fn names(list: &Value, field: &str) -> Vec<String> {
    items(list).iter().map(|item| text(item, field)).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn now() -> Timestamp {
        "2026-11-08T16:45:12Z".parse().unwrap()
    }

    fn state(brick_state: &str, sessions: Value) -> Value {
        json!({
            "service": { "version": "0.2.0", "started_at": "2026-11-08T16:41:00Z", "path": null },
            "bricks": [ { "name": "forge", "state": brick_state, "pid": null, "crashes": 0 } ],
            "sessions": sessions,
            "flows": [ { "id": "forge/x", "subscribers": 1, "last_event": null } ],
        })
    }

    fn update(flow: &str, id: &str) -> Value {
        json!({ "id": id, "subject": flow, "type": "forge.pr/v1", "time": "2026-11-08T16:45:00Z", "data": { "n": id } })
    }

    fn recent(model: &Model) -> Vec<&str> {
        model.recent.iter().map(|c| c.what.as_str()).collect()
    }

    #[test]
    fn a_new_state_tells_what_changed() {
        let mut model = Model::new(state("stopped", json!([])));
        let sessions = json!([ { "id": "5f1c2e9a-0b1c-4d2e", "subscriptions": [ { "id": "s-1", "flow": "forge/x" } ] } ]);
        model.status(state("running", sessions), now());
        assert_eq!(
            recent(&model),
            [
                "session 5f1c2e9a subscribed to forge/x",
                "session 5f1c2e9a declared",
                "brick forge stopped → running",
            ]
        );
        let unsubscribed = json!([ { "id": "5f1c2e9a-0b1c-4d2e", "subscriptions": [] } ]);
        model.status(state("running", unsubscribed), now());
        assert_eq!(
            recent(&model)[0],
            "session 5f1c2e9a unsubscribed from forge/x"
        );
        model.status(state("running", json!([])), now());
        assert_eq!(recent(&model)[0], "session 5f1c2e9a gone");
    }

    #[test]
    fn an_outage_is_said_once_and_its_end_too() {
        let mut model = Model::new(state("running", json!([])));
        model.down("the service closed the stream".into(), now());
        model.down("the service is stopped".into(), now());
        assert_eq!(model.link, Link::Down("the service is stopped".into()));
        model.status(state("running", json!([])), now());
        assert_eq!(model.link, Link::Up);
        assert_eq!(
            recent(&model),
            [
                "connected to the service",
                "lost the service: the service closed the stream"
            ]
        );
    }

    #[test]
    fn a_change_again_adds_to_the_last_one() {
        let mut model = Model::new(state("running", json!([])));
        model.event("update", &update("forge/x", "1"), now());
        model.event("update", &update("forge/x", "2"), now());
        assert_eq!(model.recent.len(), 1);
        assert_eq!(model.recent[0].times, 2);
        model.event("update", &update("forge/y", "3"), now());
        model.event("update", &update("forge/x", "4"), now());
        assert_eq!(recent(&model).len(), 3, "not in a row: said again");
    }

    #[test]
    fn events_are_kept_per_flow_once_each() {
        let mut model = Model::new(state("running", json!([])));
        model.event("update", &update("forge/x", "1"), now());
        model.event("update", &update("forge/x", "2"), now());
        // The last value again, after a reconnection.
        model.event("update", &update("forge/x", "2"), now());
        let ids: Vec<_> = model.flows["forge/x"]
            .events
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(ids, ["1", "2"]);

        model.event(
            "flow-error",
            &json!({ "flow": "forge/x", "message": "down" }),
            now(),
        );
        assert_eq!(model.flows["forge/x"].error.as_deref(), Some("down"));
        model.event("update", &update("forge/x", "3"), now());
        assert_eq!(model.flows["forge/x"].error, None);

        model.event("flow-ended", &json!({ "flow": "forge/x" }), now());
        assert!(!model.flows.contains_key("forge/x"));
        assert_eq!(recent(&model)[0], "forge/x ended");
    }

    #[test]
    fn a_flow_keeps_a_bounded_history() {
        let mut model = Model::new(state("running", json!([])));
        for id in 0..=FLOW_EVENTS {
            model.event("update", &update("forge/x", &id.to_string()), now());
        }
        let events = &model.flows["forge/x"].events;
        assert_eq!(events.len(), FLOW_EVENTS);
        assert_eq!(events[0].id, "1");
    }

    #[test]
    fn opening_goes_from_a_flow_to_its_events_and_back() {
        let mut model = Model::new(state("running", json!([])));
        model.event("update", &update("forge/x", "1"), now());
        model.event("update", &update("forge/x", "2"), now());
        model.show_tab(Tab::Flows);
        assert_eq!(model.rows(), ["forge/x"]);
        model.open();
        assert_eq!(model.screen, Screen::Flow("forge/x".into()));
        // Newest first.
        assert_eq!(model.rows(), ["2", "1"]);
        model.move_by(1);
        model.open();
        assert_eq!(
            model.screen,
            Screen::Event {
                flow: "forge/x".into(),
                id: "1".into()
            }
        );
        model.back();
        assert_eq!(model.selected(), 1, "the selection is kept");
        model.back();
        assert_eq!(model.screen, Screen::Tab(Tab::Flows));
    }

    #[test]
    fn a_brick_opens_its_flows() {
        let mut model = Model::new(state("running", json!([])));
        model.event("update", &update("claude-code/a", "1"), now());
        model.show_tab(Tab::Bricks);
        model.open();
        assert_eq!(model.screen, Screen::Tab(Tab::Flows));
        assert_eq!(model.flow_ids()[model.selected()], "forge/x");
    }

    #[test]
    fn tabs_wrap_around() {
        let mut model = Model::new(state("running", json!([])));
        model.next_tab(false);
        assert_eq!(model.tab(), Tab::Flows);
        model.next_tab(true);
        assert_eq!(model.tab(), Tab::Overview);
    }

    #[test]
    fn moving_stays_within_the_rows() {
        let mut model = Model::new(state("running", json!([])));
        model.show_tab(Tab::Flows);
        model.move_by(5);
        assert_eq!(model.selected(), 0);
        model.move_by(-5);
        assert_eq!(model.selected(), 0);
    }
}
