//! What the console shows, worked out from what serve said. Pure: no terminal
//! and no network, so every rule here is tested on its own.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::BTreeSet;

use super::client::Remote;
use crate::diagram::{DiagramSnapshot, DiagramTag, EdgeKind};
use crate::serve::{ProposedDesign, RunStatus};
use crate::topology::PortKind;
use crate::topology::TimelineStep;

/// The same four places Mission Control has, in the same order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Chat,
    Design,
    Runs,
    Agents,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Chat, Tab::Design, Tab::Runs, Tab::Agents];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Chat => "Chat",
            Tab::Design => "Design",
            Tab::Runs => "Runs",
            Tab::Agents => "Agents",
        }
    }

    fn index(self) -> usize {
        Tab::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }
}

/// The design tab's three views, as Mission Control's source pane has them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DesignView {
    Topology,
    Source,
    Properties,
}

/// Something the event loop does on the console's behalf.
#[derive(Debug, PartialEq)]
pub enum Action {
    Quit,
    Send(String),
    Deploy(usize),
    Pause(String),
    Resume(String),
    Stop(String),
    OpenAgents,
}

pub struct Console {
    pub session: String,
    pub url: String,
    pub tab: Tab,
    pub remote: Remote,
    /// Why the last refresh failed, while it keeps failing.
    pub offline: Option<String>,
    /// The last thing an action said, good or bad.
    pub notice: Option<String>,
    pub input: String,
    /// Lines scrolled up from the newest message.
    pub chat_scroll: usize,
    /// Which design, by position among the conversation's designs. `None`
    /// follows the newest, so a revised proposal replaces the one shown.
    pub design: Option<usize>,
    pub design_view: DesignView,
    pub property: usize,
    /// The run the operator picked; `None` follows the newest.
    pub run: Option<String>,
    /// The tag the operator picked; `None` follows the run's current tag.
    pub step: Option<usize>,
    /// A stop asked for and not yet confirmed.
    pub confirm_stop: Option<String>,
}

impl Console {
    pub fn new(session: String, url: String) -> Self {
        Self {
            session,
            url,
            tab: Tab::Chat,
            remote: Remote::default(),
            offline: None,
            notice: None,
            input: String::new(),
            chat_scroll: 0,
            design: None,
            design_view: DesignView::Topology,
            property: 0,
            run: None,
            step: None,
            confirm_stop: None,
        }
    }

    /// Every design proposed in this conversation, oldest first.
    pub fn designs(&self) -> Vec<&ProposedDesign> {
        self.remote
            .messages
            .iter()
            .filter_map(|message| message.design.as_ref())
            .collect()
    }

    /// The design shown, and its position.
    pub fn shown_design(&self) -> Option<(usize, &ProposedDesign)> {
        let designs = self.designs();
        let index = self
            .design
            .filter(|index| *index < designs.len())
            .or(designs.len().checked_sub(1))?;
        Some((index, designs[index]))
    }

    pub fn selected_run(&self) -> Option<&crate::serve::RunRecord> {
        let runs = &self.remote.runs;
        self.run
            .as_ref()
            .and_then(|id| runs.iter().find(|run| &run.run_id == id))
            .or(runs.first())
    }

    /// The step shown in the inspector: the one picked, else the run's own.
    pub fn shown_step(&self) -> Option<usize> {
        let view = self.remote.run.as_ref()?;
        let current = view.snapshot.as_ref().and_then(|s| s.current_tag);
        self.step
            .filter(|step| *step < view.steps.len())
            .or_else(|| current.and_then(|tag| view.steps.iter().position(|s| at(s, tag))))
            .or(if view.steps.is_empty() { None } else { Some(0) })
    }

    pub fn key(&mut self, key: KeyEvent) -> Option<Action> {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('q')) {
            return Some(Action::Quit);
        }
        if let Some(run) = self.confirm_stop.take() {
            return (key.code == KeyCode::Char('y')).then_some(Action::Stop(run));
        }
        match key.code {
            KeyCode::Tab => {
                self.tab = Tab::ALL[(self.tab.index() + 1) % Tab::ALL.len()];
                return None;
            }
            KeyCode::BackTab => {
                self.tab = Tab::ALL[(self.tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len()];
                return None;
            }
            _ => {}
        }
        if self.tab == Tab::Chat {
            return self.chat_key(key);
        }
        match key.code {
            KeyCode::Char('q') => return Some(Action::Quit),
            KeyCode::Char(digit @ '1'..='4') => {
                self.tab = Tab::ALL[digit as usize - '1' as usize];
                return None;
            }
            _ => {}
        }
        match self.tab {
            Tab::Chat => None,
            Tab::Design => self.design_key(key),
            Tab::Runs => self.runs_key(key),
            Tab::Agents => (key.code == KeyCode::Enter).then_some(Action::OpenAgents),
        }
    }

    fn chat_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Enter => {
                let text = std::mem::take(&mut self.input);
                self.chat_scroll = 0;
                (!text.trim().is_empty()).then_some(Action::Send(text))
            }
            KeyCode::Backspace => {
                self.input.pop();
                None
            }
            KeyCode::Esc => {
                self.input.clear();
                None
            }
            KeyCode::Up | KeyCode::PageUp => {
                self.chat_scroll += if key.code == KeyCode::Up { 1 } else { 10 };
                None
            }
            KeyCode::Down | KeyCode::PageDown => {
                let by = if key.code == KeyCode::Down { 1 } else { 10 };
                self.chat_scroll = self.chat_scroll.saturating_sub(by);
                None
            }
            KeyCode::Char(c) if !c.is_control() => {
                self.input.push(c);
                None
            }
            _ => None,
        }
    }

    fn design_key(&mut self, key: KeyEvent) -> Option<Action> {
        let count = self.designs().len();
        let shown = self.shown_design().map(|(index, _)| index);
        let properties = self
            .shown_design()
            .map_or(0, |(_, design)| design.properties.len());
        match key.code {
            KeyCode::Char('t') => self.design_view = DesignView::Topology,
            KeyCode::Char('s') => self.design_view = DesignView::Source,
            KeyCode::Char('p') => self.design_view = DesignView::Properties,
            KeyCode::Char('[') => self.design = shown.map(|index| index.saturating_sub(1)),
            KeyCode::Char(']') => {
                self.design = shown
                    .map(|index| index + 1)
                    .filter(|next| *next < count - 1)
            }
            KeyCode::Up => self.property = self.property.saturating_sub(1),
            KeyCode::Down => self.property = (self.property + 1).min(properties.saturating_sub(1)),
            KeyCode::Char('d') => return shown.map(Action::Deploy),
            _ => {}
        }
        None
    }

    fn runs_key(&mut self, key: KeyEvent) -> Option<Action> {
        let runs: Vec<String> = self.remote.runs.iter().map(|r| r.run_id.clone()).collect();
        let selected = self
            .selected_run()
            .map(|run| (run.run_id.clone(), run.status));
        let at = selected
            .as_ref()
            .and_then(|(id, _)| runs.iter().position(|run| run == id));
        let steps = self.remote.run.as_ref().map_or(0, |view| view.steps.len());
        match key.code {
            KeyCode::Char('[') | KeyCode::Char(']') => {
                let next = match (key.code, at) {
                    (KeyCode::Char('['), Some(at)) => at.saturating_sub(1),
                    (_, Some(at)) => (at + 1).min(runs.len().saturating_sub(1)),
                    _ => 0,
                };
                self.run = runs.get(next).cloned();
                self.step = None;
            }
            KeyCode::Up => self.step = self.shown_step().map(|step| step.saturating_sub(1)),
            KeyCode::Down => {
                self.step = self
                    .shown_step()
                    .map(|step| (step + 1).min(steps.saturating_sub(1)))
            }
            KeyCode::Char('f') => self.step = None,
            KeyCode::Char('p') => return selected.map(|(id, _)| Action::Pause(id)),
            KeyCode::Char('r') => return selected.map(|(id, _)| Action::Resume(id)),
            KeyCode::Char('x') => {
                if let Some((id, status)) = selected {
                    if status.is_active() || status == RunStatus::Paused {
                        self.confirm_stop = Some(id);
                    }
                }
            }
            _ => {}
        }
        None
    }
}

fn at(step: &TimelineStep, tag: DiagramTag) -> bool {
    step.timestamp == tag.timestamp && step.microstep == tag.microstep
}

/// Where a run stands relative to one of its tags.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StepState {
    Done,
    Now,
    Next,
    /// The run ended before this tag. The projection over-approximates
    /// (`-> (a | b)` is drawn writing both), so a finished run can skip one.
    NotReached,
    /// The run ended before this client saw where it was.
    Unknown,
}

impl StepState {
    pub fn word(self) -> &'static str {
        match self {
            StepState::Done => "done",
            StepState::Now => "now",
            StepState::Next => "next",
            StepState::NotReached => "not reached",
            StepState::Unknown => "unknown",
        }
    }
}

/// Tags are ordered `(time, microstep)`; the run's current tag is the one it
/// is executing, or the last it executed once it has ended.
pub fn step_state(step: &TimelineStep, current: Option<DiagramTag>, finished: bool) -> StepState {
    let Some(current) = current else {
        return if finished {
            StepState::Unknown
        } else {
            StepState::Next
        };
    };
    let ordering = (step.timestamp, step.microstep).cmp(&(current.timestamp, current.microstep));
    match ordering {
        std::cmp::Ordering::Less => StepState::Done,
        std::cmp::Ordering::Equal if finished => StepState::Done,
        std::cmp::Ordering::Equal => StepState::Now,
        std::cmp::Ordering::Greater if finished => StepState::NotReached,
        std::cmp::Ordering::Greater => StepState::Next,
    }
}

/// `(t, m)` with time in the largest unit that keeps it whole.
pub fn tag_text(timestamp: u64, microstep: u64) -> String {
    format!("({}, {microstep})", duration_text(timestamp))
}

pub fn duration_text(nanos: u64) -> String {
    const UNITS: [(u64, &str); 5] = [
        (3_600_000_000_000, "h"),
        (60_000_000_000, "min"),
        (1_000_000_000, "s"),
        (1_000_000, "ms"),
        (1_000, "us"),
    ];
    if nanos == 0 {
        return "0".into();
    }
    UNITS
        .iter()
        .find(|(unit, _)| nanos.is_multiple_of(*unit))
        .map(|(unit, name)| format!("{}{name}", nanos / unit))
        .unwrap_or_else(|| format!("{nanos}ns"))
}

/// A lag, which is rarely a round number: to a tenth of a millisecond.
pub fn lag_text(nanos: u64) -> String {
    format!("{:.1}ms", nanos as f64 / 1e6)
}

/// What a connection costs, in the language's own words.
pub fn delay_text(delay: Option<u64>) -> String {
    match delay {
        None => String::new(),
        Some(0) => "after 0 (one microstep)".into(),
        Some(nanos) => format!("after {}", duration_text(nanos)),
    }
}

/// One line of the topology outline.
#[derive(Debug, PartialEq)]
pub struct Outline {
    pub depth: usize,
    pub label: String,
    pub detail: String,
    /// The components this line stands for, so a focus can single it out.
    pub ids: Vec<String>,
}

/// The program as an indented outline: each instance, its reactions with
/// what triggers them and what they may write, then the connections between
/// instances. What the diagram draws, said as text.
pub fn outline(snapshot: &DiagramSnapshot) -> Vec<Outline> {
    let mut lines = Vec::new();
    let port_name = |id: &str| {
        snapshot
            .ports
            .iter()
            .find(|port| port.id == id)
            .map(|port| port.name.clone())
            .or_else(|| {
                snapshot
                    .timers
                    .iter()
                    .find(|timer| timer.id == id)
                    .map(|timer| format!("timer {}", timer.name))
            })
            .unwrap_or_else(|| id.to_string())
    };
    let mut instances: Vec<_> = snapshot.instances.iter().collect();
    instances.sort_by(|a, b| a.name.cmp(&b.name));
    for instance in instances {
        let depth = instance.name.matches('.').count();
        lines.push(Outline {
            depth,
            label: instance.name.clone(),
            detail: format!("team {}", instance.team),
            ids: vec![instance.id.clone()],
        });
        for (kind, label) in [
            (PortKind::Input, "inputs"),
            (PortKind::Output, "outputs"),
            (PortKind::Action, "actions"),
        ] {
            let ports: Vec<_> = snapshot
                .ports
                .iter()
                .filter(|port| port.instance == instance.name && port.kind == kind)
                .collect();
            if ports.is_empty() {
                continue;
            }
            let names: Vec<String> = ports
                .iter()
                .map(|port| match (kind, port.delay) {
                    // An action costs a microstep unless it says how long.
                    (PortKind::Action, None | Some(0)) => format!("{} (next microstep)", port.name),
                    (PortKind::Action, Some(nanos)) => {
                        format!("{} ({} later)", port.name, duration_text(nanos))
                    }
                    _ => port.name.clone(),
                })
                .collect();
            lines.push(Outline {
                depth: depth + 1,
                label: label.into(),
                detail: names.join(", "),
                ids: ports.iter().map(|port| port.id.clone()).collect(),
            });
        }
        for timer in snapshot
            .timers
            .iter()
            .filter(|t| t.instance == instance.name)
        {
            let period = if timer.period == 0 {
                "once".to_string()
            } else {
                format!("every {}", duration_text(timer.period))
            };
            lines.push(Outline {
                depth: depth + 1,
                label: format!("timer {}", timer.name),
                detail: format!("at {}, {period}", duration_text(timer.offset)),
                ids: vec![timer.id.clone()],
            });
        }
        let mut reactions: Vec<_> = snapshot
            .reactions
            .iter()
            .filter(|reaction| reaction.instance == instance.name)
            .collect();
        reactions.sort_by_key(|reaction| reaction.order);
        for reaction in reactions {
            let agent = snapshot
                .agents
                .iter()
                .find(|agent| agent.id == reaction.agent)
                .map(|agent| format!("{} ({})", agent.name, agent.backend))
                .unwrap_or_else(|| "code".into());
            let within = reaction
                .within
                .map(|nanos| format!(", within {}", duration_text(nanos)))
                .unwrap_or_default();
            lines.push(Outline {
                depth: depth + 1,
                label: reaction.name.clone(),
                detail: format!("{agent}{within}"),
                ids: vec![reaction.id.clone()],
            });
            let triggers: Vec<_> = reaction.triggers.iter().map(|id| port_name(id)).collect();
            lines.push(Outline {
                depth: depth + 2,
                label: "when".into(),
                detail: triggers.join(" or "),
                ids: reaction.triggers.clone(),
            });
            let contract = if reaction.contract.is_empty() {
                "nothing".to_string()
            } else {
                reaction.contract.clone()
            };
            lines.push(Outline {
                depth: depth + 2,
                label: "writes".into(),
                detail: contract,
                ids: reaction.effects.clone(),
            });
        }
    }
    let connections: Vec<_> = snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Connection)
        .collect();
    if !connections.is_empty() {
        lines.push(Outline {
            depth: 0,
            label: "connections".into(),
            detail: String::new(),
            ids: Vec::new(),
        });
        for edge in connections {
            lines.push(Outline {
                depth: 1,
                label: format!("{} -> {}", port_name(&edge.source), port_name(&edge.target)),
                detail: delay_text(edge.delay),
                ids: vec![edge.source.clone(), edge.target.clone()],
            });
        }
    }
    lines
}

/// The components a property is about, by id, with every instance holding
/// one: what Mission Control lights up, the rest dimmed.
pub fn focus(snapshot: &DiagramSnapshot, mentions: &[String]) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let mut owners = BTreeSet::new();
    for name in mentions {
        for port in snapshot.ports.iter().filter(|port| &port.name == name) {
            ids.insert(port.id.clone());
            owners.insert(port.instance.clone());
        }
        for timer in snapshot.timers.iter().filter(|timer| &timer.name == name) {
            ids.insert(timer.id.clone());
            owners.insert(timer.instance.clone());
        }
        let reaction = format!("reaction::{name}");
        for found in snapshot.reactions.iter().filter(|r| r.id == reaction) {
            ids.insert(found.id.clone());
            owners.insert(found.instance.clone());
        }
    }
    for mut owner in owners {
        while !owner.is_empty() {
            ids.insert(format!("instance::{owner}"));
            owner = owner
                .rsplit_once('.')
                .map(|(parent, _)| parent.to_string())
                .unwrap_or_default();
        }
    }
    ids
}

/// Text from agents and programs, made safe to put in a cell: escape
/// sequences and control characters would move the cursor, and a tab's width
/// is the terminal's to decide.
pub fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                // CSI: ESC [ params final-byte. Anything else: drop ESC alone.
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
            }
            '\t' => out.push_str("    "),
            '\n' => out.push('\n'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(timestamp: u64, microstep: u64) -> TimelineStep {
        TimelineStep {
            timestamp,
            microstep,
            events: Vec::new(),
            reactions: Vec::new(),
        }
    }

    fn tag(timestamp: u64, microstep: u64) -> Option<DiagramTag> {
        Some(DiagramTag {
            timestamp,
            microstep,
        })
    }

    #[test]
    fn tags_are_ordered_by_time_then_microstep() {
        assert_eq!(step_state(&step(0, 0), tag(0, 1), false), StepState::Done);
        assert_eq!(step_state(&step(0, 1), tag(0, 1), false), StepState::Now);
        assert_eq!(step_state(&step(0, 1), tag(0, 1), true), StepState::Done);
        // A later time at microstep 0 still comes after (0, 5).
        assert_eq!(step_state(&step(1, 0), tag(0, 5), false), StepState::Next);
        assert_eq!(step_state(&step(0, 0), None, false), StepState::Next);
        assert_eq!(
            step_state(&step(0, 2), tag(0, 1), true),
            StepState::NotReached
        );
        assert_eq!(step_state(&step(0, 0), None, true), StepState::Unknown);
    }

    #[test]
    fn a_delay_reads_as_the_language_writes_it() {
        assert_eq!(delay_text(None), "");
        assert_eq!(delay_text(Some(0)), "after 0 (one microstep)");
        assert_eq!(delay_text(Some(2_000_000_000)), "after 2s");
        assert_eq!(delay_text(Some(1_500_000)), "after 1500us");
        assert_eq!(tag_text(120_000_000_000, 1), "(2min, 1)");
    }

    #[test]
    fn cleaning_keeps_text_and_drops_what_moves_the_cursor() {
        assert_eq!(clean("a\tb"), "a    b");
        assert_eq!(clean("\u{1b}[31mred\u{1b}[0m\r"), "red");
        assert_eq!(clean("line\nnext \u{7}"), "line\nnext ");
        assert_eq!(clean("∀ e, → ¬"), "∀ e, → ¬");
    }

    fn snapshot() -> DiagramSnapshot {
        serde_json::from_str(include_str!(
            "../../web/tests/fixtures/diagram-snapshot.v1.json"
        ))
        .unwrap()
    }

    #[test]
    fn the_outline_says_what_triggers_each_reaction_and_what_it_writes() {
        let lines = outline(&snapshot());
        let text: Vec<String> = lines
            .iter()
            .map(|line| format!("{}{} {}", "  ".repeat(line.depth), line.label, line.detail))
            .collect();
        assert_eq!(text[0], "flow team ReviewFlow");
        assert!(
            text.iter().any(|line| line == "    when flow.request"),
            "{text:#?}"
        );
        assert!(
            text.iter().any(|line| line == "    writes plan"),
            "{text:#?}"
        );
    }

    #[test]
    fn a_focus_holds_what_a_property_mentions_and_the_instances_around_it() {
        let ids = focus(&snapshot(), &["flow.plan".into(), "flow.reaction.0".into()]);
        assert_eq!(
            ids.into_iter().collect::<Vec<_>>(),
            vec![
                "instance::flow",
                "port::flow.plan",
                "reaction::flow.reaction.0"
            ]
        );
    }

    #[test]
    fn chat_keys_are_text_until_enter_sends_them() {
        let mut console = Console::new("s".into(), "http://x".into());
        for c in "q1 hi".chars() {
            assert_eq!(console.key(KeyEvent::from(KeyCode::Char(c))), None);
        }
        assert_eq!(console.tab, Tab::Chat, "q and 1 are text in the chat");
        assert_eq!(
            console.key(KeyEvent::from(KeyCode::Enter)),
            Some(Action::Send("q1 hi".into()))
        );
        assert_eq!(
            console.key(KeyEvent::from(KeyCode::Enter)),
            None,
            "nothing to send"
        );
        console.key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(console.tab, Tab::Design);
        console.key(KeyEvent::from(KeyCode::Char('3')));
        assert_eq!(console.tab, Tab::Runs);
    }

    #[test]
    fn a_stop_waits_for_a_yes() {
        let mut console = Console::new("s".into(), "http://x".into());
        console.tab = Tab::Runs;
        console.remote.runs = vec![serde_json::from_value(serde_json::json!({
            "run_id": "r1", "team": "Desk", "status": "running",
            "diagram_address": null, "started_at": 1, "finished_at": null, "error": null
        }))
        .unwrap()];
        assert_eq!(console.key(KeyEvent::from(KeyCode::Char('x'))), None);
        assert_eq!(console.confirm_stop.as_deref(), Some("r1"));
        assert_eq!(console.key(KeyEvent::from(KeyCode::Char('n'))), None);
        assert_eq!(console.confirm_stop, None, "anything but y cancels");
        console.key(KeyEvent::from(KeyCode::Char('x')));
        assert_eq!(
            console.key(KeyEvent::from(KeyCode::Char('y'))),
            Some(Action::Stop("r1".into()))
        );
    }
}
