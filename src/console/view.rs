//! Drawing the console. Only stock widgets (tabs, tables, lists, wrapped
//! text) and no hand-placed characters: nothing here depends on how a
//! terminal or font measures a glyph. Meaning never rides on colour alone —
//! states are words and a focus is marked `>` — so `NO_COLOR` loses nothing.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs, Wrap};
use ratatui::Frame;
use std::collections::BTreeSet;

use super::state::{
    clean, focus, outline, step_state, tag_text, Console, DesignView, StepState, Tab,
};
use crate::diagram::DiagramSnapshot;
use crate::serve::{ChatRole, RunStatus};

const ACCENT: Color = Color::Cyan;
const GOOD: Color = Color::Green;
const BAD: Color = Color::Red;

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

pub fn render(frame: &mut Frame, console: &Console) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    render_header(frame, console, header);
    match console.tab {
        Tab::Chat => render_chat(frame, console, body),
        Tab::Design => render_design(frame, console, body),
        Tab::Runs => render_runs(frame, console, body),
        Tab::Agents => render_agents(frame, console, body),
    }
    render_footer(frame, console, footer);
}

fn render_header(frame: &mut Frame, console: &Console, area: Rect) {
    let where_ = match &console.offline {
        Some(reason) => Span::styled(
            format!("offline: {}", clean(reason)),
            Style::default().fg(BAD),
        ),
        None => Span::styled(format!("{} {}", console.session, console.url), dim()),
    };
    let right = where_.width() as u16;
    let [left, right_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(right.min(area.width / 2)),
    ])
    .areas(area);
    let titles = Tab::ALL.iter().enumerate().map(|(index, tab)| {
        let mut title = tab.title().to_string();
        if *tab == Tab::Design {
            let designs = console.designs().len();
            if designs > 0 {
                title.push_str(&format!(" ({designs})"));
            }
        }
        if *tab == Tab::Runs {
            let live = console
                .remote
                .runs
                .iter()
                .filter(|r| r.status.is_active())
                .count();
            if live > 0 {
                title.push_str(&format!(" ({live} live)"));
            }
        }
        Line::from(format!("{} {title}", index + 1))
    });
    let tabs = Tabs::new(titles)
        .select(
            Tab::ALL
                .iter()
                .position(|tab| *tab == console.tab)
                .unwrap_or(0),
        )
        .highlight_style(
            Style::default()
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        )
        .divider(" ");
    frame.render_widget(tabs, left);
    frame.render_widget(Paragraph::new(Line::from(where_)), right_area);
}

fn render_footer(frame: &mut Frame, console: &Console, area: Rect) {
    let line = if let Some(run) = &console.confirm_stop {
        Line::from(Span::styled(
            format!("Stop run {run}? y stops it; any other key keeps it running."),
            Style::default().fg(BAD).add_modifier(Modifier::BOLD),
        ))
    } else if let Some(notice) = &console.notice {
        Line::from(Span::styled(clean(notice), bold()))
    } else {
        let keys = match console.tab {
            Tab::Chat => "Enter send  Up/Down scroll  Tab next view  Ctrl-C quit",
            Tab::Design => {
                "t topology  s source  p properties  Up/Down property  [ ] design  d deploy  q quit"
            }
            Tab::Runs => "[ ] run  Up/Down tag  f follow  p pause  r resume  x stop  q quit",
            Tab::Agents => "Enter open the agent dashboard (z there comes back)  q quit",
        };
        Line::from(Span::styled(keys, dim()))
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn pane(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(format!(" {title} "), bold()))
}

/// Words fitted to `width` columns, breaking a word only when it alone is
/// wider. A line's indent is kept on every line it wraps to, so code keeps
/// its shape. Widths are the terminal's own idea of them, via ratatui.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let rest = paragraph.trim_start_matches(' ');
        let indent = &paragraph[..paragraph.len() - rest.len()];
        let indent = &indent[..indent.len().min(width / 2)];
        let width = width.saturating_sub(indent.len()).max(1);
        let start = lines.len();
        let mut line = String::new();
        for word in rest.split(' ') {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if Span::raw(candidate.as_str()).width() <= width {
                line = candidate;
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            for c in word.chars() {
                if Span::raw(format!("{line}{c}")).width() > width && !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                }
                line.push(c);
            }
        }
        lines.push(line);
        for line in &mut lines[start..] {
            line.insert_str(0, indent);
        }
    }
    lines
}

fn render_chat(frame: &mut Frame, console: &Console, area: Rect) {
    let [messages, input] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(area);
    let block = pane("Conversation with the assistant");
    let inner = block.inner(messages);
    frame.render_widget(block, messages);
    let width = inner.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();
    let mut design = 0;
    for message in &console.remote.messages {
        let (who, style) = match message.role {
            ChatRole::Operator => (
                "you",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            ChatRole::Assistant => (
                "assistant",
                Style::default().fg(GOOD).add_modifier(Modifier::BOLD),
            ),
        };
        let mut head = vec![Span::styled(who, style)];
        if message.progress {
            head.push(Span::styled("  working", dim()));
        }
        lines.push(Line::from(head));
        let body = if message.progress {
            dim()
        } else {
            Style::default()
        };
        for paragraph in clean(&message.text).split('\n') {
            // The assistant writes Markdown; its bold is the one mark that
            // reads wrong as text, so a bold line is drawn bold and the
            // markers go. Mission Control renders the rest.
            let heading =
                paragraph.len() > 4 && paragraph.starts_with("**") && paragraph.ends_with("**");
            let style = if heading {
                body.add_modifier(Modifier::BOLD)
            } else {
                body
            };
            for line in wrap(&paragraph.replace("**", ""), width) {
                lines.push(Line::from(Span::styled(format!("  {line}"), style)));
            }
        }
        if let Some(proposal) = &message.design {
            design += 1;
            let proved = proposal.properties.iter().filter(|p| p.proved).count();
            let properties = if proposal.properties.is_empty() {
                String::new()
            } else {
                format!(
                    ", {proved} of {} properties proved",
                    proposal.properties.len()
                )
            };
            let summary = format!(
                "Design {design}: {}, {} instances, {} reactions{properties}. Open it in Design (2).",
                proposal.preview.team,
                proposal.preview.instances.len(),
                proposal.preview.reactions.len()
            );
            for line in wrap(&summary, width) {
                lines.push(Line::from(Span::styled(
                    format!("  {line}"),
                    Style::default().fg(ACCENT),
                )));
            }
        }
        lines.push(Line::default());
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No messages yet. Describe the workflow you want; the assistant proposes a design.",
            dim(),
        )));
    }
    // Newest at the bottom, like Mission Control; scrolling walks back.
    let height = inner.height as usize;
    let end = lines
        .len()
        .saturating_sub(console.chat_scroll.min(lines.len().saturating_sub(height)));
    let start = end.saturating_sub(height);
    frame.render_widget(Paragraph::new(lines[start..end].to_vec()), inner);

    let prompt = Line::from(vec![
        Span::styled("> ", Style::default().fg(ACCENT)),
        Span::raw(console.input.as_str()),
        Span::styled("_", Style::default().add_modifier(Modifier::SLOW_BLINK)),
    ]);
    // Long drafts keep their end in view.
    let shown = Span::raw(console.input.as_str()).width() as u16 + 3;
    let scroll = shown.saturating_sub(input.width.saturating_sub(2));
    frame.render_widget(
        Paragraph::new(prompt)
            .block(pane("Message"))
            .scroll((0, scroll)),
        input,
    );
}

fn outline_lines(
    snapshot: &DiagramSnapshot,
    focus: Option<&BTreeSet<String>>,
) -> Vec<Line<'static>> {
    outline(snapshot)
        .into_iter()
        .map(|line| {
            let focused = focus.map(|ids| line.ids.iter().any(|id| ids.contains(id)));
            let (marker, style) = match focused {
                Some(true) => ("> ", Style::default().fg(GOOD).add_modifier(Modifier::BOLD)),
                Some(false) => ("  ", dim()),
                None => ("  ", Style::default()),
            };
            let label_style = if line.depth == 0 && focused != Some(false) {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            };
            Line::from(vec![
                Span::styled(format!("{marker}{}", "  ".repeat(line.depth)), style),
                Span::styled(clean(&line.label), label_style),
                Span::styled(
                    format!("  {}", clean(&line.detail)),
                    style.add_modifier(Modifier::DIM),
                ),
            ])
        })
        .collect()
}

fn render_design(frame: &mut Frame, console: &Console, area: Rect) {
    let Some((index, design)) = console.shown_design() else {
        frame.render_widget(
            Paragraph::new("No design yet. Ask the assistant for one in Chat (1).")
                .style(dim())
                .block(pane("Design")),
            area,
        );
        return;
    };
    let [header, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let view = |view: DesignView, key: &str, name: &str| {
        let style = if console.design_view == view {
            Style::default()
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            dim()
        };
        vec![
            Span::styled(format!("{key} {name}"), style),
            Span::raw("  "),
        ]
    };
    let mut spans = vec![Span::styled(
        format!(
            "Design {} of {}: {}  ",
            index + 1,
            console.designs().len(),
            design.preview.team
        ),
        bold(),
    )];
    spans.extend(view(DesignView::Topology, "t", "Topology"));
    spans.extend(view(DesignView::Source, "s", "Source"));
    let proved = design.properties.iter().filter(|p| p.proved).count();
    spans.extend(view(
        DesignView::Properties,
        "p",
        &format!("Properties {proved}/{}", design.properties.len()),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), header);

    match console.design_view {
        DesignView::Topology => {
            let mut lines = outline_lines(&design.preview, None);
            let inputs: Vec<_> = design.inputs.keys().cloned().collect();
            if !inputs.is_empty() {
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    format!("  deploys with {}", inputs.join(", ")),
                    dim(),
                )));
            }
            frame.render_widget(
                Paragraph::new(lines)
                    .block(pane("Topology"))
                    .wrap(Wrap { trim: false }),
                body,
            );
        }
        DesignView::Source => {
            frame.render_widget(
                Paragraph::new(numbered(
                    &design.program,
                    &BTreeSet::new(),
                    body.width.saturating_sub(2) as usize,
                ))
                .block(pane("program.omar")),
                body,
            );
        }
        DesignView::Properties => render_properties(frame, console, design, body),
    }
}

/// Source with line numbers. A long line wraps under its own text, so the
/// numbers stay a column of their own.
fn numbered(source: &str, marked: &BTreeSet<usize>, width: usize) -> Vec<Line<'static>> {
    let source = clean(source);
    let lines: Vec<&str> = source.trim_end_matches('\n').split('\n').collect();
    let digits = lines.len().to_string().len();
    let mut out = Vec::new();
    for (index, text) in lines.iter().enumerate() {
        let style = if marked.contains(&(index + 1)) {
            Style::default().fg(BAD)
        } else {
            Style::default()
        };
        let room = width.saturating_sub(digits + 4).max(8);
        for (part, piece) in wrap(text, room).into_iter().enumerate() {
            // A continuation sits two further in, so it reads as one.
            let gutter = if part == 0 {
                format!("{:>digits$}  ", index + 1)
            } else {
                " ".repeat(digits + 4)
            };
            out.push(Line::from(vec![
                Span::styled(gutter, dim()),
                Span::styled(piece, style),
            ]));
        }
    }
    out
}

fn render_properties(
    frame: &mut Frame,
    console: &Console,
    design: &crate::serve::ProposedDesign,
    area: Rect,
) {
    if design.properties.is_empty() {
        frame.render_widget(
            Paragraph::new(
                "This design states no safety properties. Ask the assistant which it guarantees.",
            )
            .style(dim())
            .block(pane("Formal guarantees")),
            area,
        );
        return;
    }
    let selected = console.property.min(design.properties.len() - 1);
    let verdict = &design.properties[selected];
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(area);
    let [list, proof] = Layout::vertical([
        Constraint::Length(design.properties.len() as u16 + 2),
        Constraint::Min(0),
    ])
    .areas(left);

    let rows = design
        .properties
        .iter()
        .enumerate()
        .map(|(index, property)| {
            let (word, colour) = if property.proved {
                ("Proved", GOOD)
            } else {
                ("Not proved", BAD)
            };
            let mut style = Style::default();
            if index == selected {
                style = style.add_modifier(Modifier::REVERSED);
            }
            Row::new(vec![
                Cell::from(if index == selected { ">" } else { " " }),
                Cell::from(Span::styled(
                    word,
                    Style::default().fg(colour).add_modifier(Modifier::BOLD),
                )),
                Cell::from(clean(&property.name)),
            ])
            .style(style)
        });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(1),
                Constraint::Length(11),
                Constraint::Min(0),
            ],
        )
        .block(pane("Formal guarantees, checked by Lean")),
        list,
    );

    let width = proof.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = wrap(&clean(&verdict.description), width)
        .into_iter()
        .map(|line| Line::from(Span::styled(line, bold())))
        .collect();
    lines.push(Line::from(vec![
        Span::styled("about ", dim()),
        Span::styled(verdict.mentions.join(", "), Style::default().fg(GOOD)),
    ]));
    lines.push(Line::default());
    let problems: BTreeSet<usize> = verdict.problems.iter().map(|p| p.line as usize).collect();
    lines.extend(numbered(&verdict.theorem, &problems, width));
    for problem in &verdict.problems {
        lines.push(Line::from(Span::styled(
            format!("line {}: {}", problem.line, clean(&problem.message)),
            Style::default().fg(BAD),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(pane("Lean proof"))
            .wrap(Wrap { trim: false }),
        proof,
    );

    let ids = focus(&design.preview, &verdict.mentions);
    frame.render_widget(
        Paragraph::new(outline_lines(&design.preview, Some(&ids)))
            .block(pane("What it is about"))
            .wrap(Wrap { trim: false }),
        right,
    );
}

fn status_style(status: RunStatus) -> Style {
    match status {
        RunStatus::Failed => Style::default().fg(BAD),
        RunStatus::Completed => Style::default().fg(GOOD),
        status if status.is_active() => Style::default().fg(ACCENT),
        _ => dim(),
    }
}

fn status_word(status: RunStatus) -> String {
    crate::diagram::wire_name(&status).unwrap_or_default()
}

fn render_runs(frame: &mut Frame, console: &Console, area: Rect) {
    if console.remote.runs.is_empty() {
        frame.render_widget(
            Paragraph::new("No runs yet. Deploy a design from Design (2), or `omar run` one.")
                .style(dim())
                .block(pane("Runs")),
            area,
        );
        return;
    }
    let list_width = (area.width / 3).clamp(20, 36);
    let [list, detail] =
        Layout::horizontal([Constraint::Length(list_width), Constraint::Min(0)]).areas(area);
    let selected = console.selected_run().map(|run| run.run_id.clone());
    let rows = console.remote.runs.iter().map(|run| {
        let mut style = Style::default();
        if Some(&run.run_id) == selected.as_ref() {
            style = style.add_modifier(Modifier::REVERSED);
        }
        Row::new(vec![
            Cell::from(Span::styled(
                status_word(run.status),
                status_style(run.status),
            )),
            Cell::from(clean(&run.team)),
        ])
        .style(style)
    });
    frame.render_widget(
        Table::new(rows, [Constraint::Length(10), Constraint::Min(0)]).block(pane("Runs")),
        list,
    );

    let Some(record) = console.selected_run() else {
        return;
    };
    let view = console
        .remote
        .run
        .as_ref()
        .filter(|view| view.run_id == record.run_id);
    let snapshot = view.and_then(|view| view.snapshot.as_ref());
    let current = snapshot.and_then(|snapshot| snapshot.current_tag);
    let finished = !(record.status.is_active() || record.status == RunStatus::Paused);

    let [summary, timeline, inspector] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(8),
    ])
    .areas(detail);
    let mut head = vec![
        Span::styled(
            status_word(record.status),
            status_style(record.status).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  at "),
        Span::styled(
            match current {
                Some(tag) => tag_text(tag.timestamp, tag.microstep),
                None if finished => "a tag it ended at before this console looked".into(),
                None => "no tag yet".into(),
            },
            bold(),
        ),
    ];
    if let Some(lag) = snapshot
        .and_then(|snapshot| snapshot.lag)
        .filter(|lag| *lag > 0)
    {
        head.push(Span::styled(
            format!("  lag {}", super::state::lag_text(lag)),
            dim(),
        ));
    }
    let mut summary_lines = vec![Line::from(head)];
    if let Some(error) = &record.error {
        summary_lines.push(Line::from(Span::styled(
            clean(error),
            Style::default().fg(BAD),
        )));
    }
    frame.render_widget(
        Paragraph::new(summary_lines).block(pane(&format!("{} {}", record.team, record.run_id))),
        summary,
    );

    let Some(view) = view else {
        frame.render_widget(
            Paragraph::new("Reading the run...")
                .style(dim())
                .block(pane("Logical time")),
            timeline,
        );
        return;
    };
    let shown = console.shown_step();
    let rows = view.steps.iter().enumerate().map(|(index, step)| {
        let state = step_state(step, current, finished);
        let style = match state {
            StepState::Done => Style::default(),
            StepState::Now => Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            StepState::Next | StepState::NotReached | StepState::Unknown => dim(),
        };
        let marker = if Some(index) == shown { ">" } else { " " };
        Row::new(vec![
            Cell::from(marker),
            Cell::from(tag_text(step.timestamp, step.microstep)),
            Cell::from(state.word()),
            Cell::from(clean(&step.reactions.join(", "))),
        ])
        .style(if Some(index) == shown {
            style.add_modifier(Modifier::REVERSED)
        } else {
            style
        })
    });
    let title = if view.truncated {
        "Logical time (projected; the program goes on)"
    } else {
        "Logical time"
    };
    // Keep the shown step in view on a long timeline.
    let visible = timeline.height.saturating_sub(3) as usize;
    let skip = shown.map_or(0, |shown| (shown + 1).saturating_sub(visible));
    frame.render_widget(
        Table::new(
            rows.skip(skip),
            [
                Constraint::Length(1),
                Constraint::Length(12),
                Constraint::Length(11),
                Constraint::Min(0),
            ],
        )
        .header(Row::new(vec!["", "TAG", "STATE", "REACTIONS"]).style(dim()))
        .block(pane(title)),
        timeline,
    );

    let mut lines = Vec::new();
    if let Some(step) = shown.and_then(|index| view.steps.get(index)) {
        let state = step_state(step, current, finished);
        let tag = tag_text(step.timestamp, step.microstep);
        for name in &step.reactions {
            let live = snapshot
                .and_then(|s| {
                    s.reactions
                        .iter()
                        .find(|r| r.id == format!("reaction::{name}"))
                })
                .map(|r| r.status);
            let word = match (state, live) {
                (StepState::Now, Some(status)) => {
                    crate::diagram::wire_name(&status).unwrap_or_default()
                }
                (StepState::Done, _) => "ran".into(),
                _ => "will run".into(),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{word:>9}  "), dim()),
                Span::raw(clean(name)),
            ]));
        }
        if let Some(snapshot) = snapshot {
            for port in snapshot.ports.iter().filter(|port| {
                port.last_tag
                    .is_some_and(|t| t.timestamp == step.timestamp && t.microstep == step.microstep)
            }) {
                let value = port
                    .value
                    .as_ref()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| v.to_string())
                    })
                    .unwrap_or_default();
                lines.push(Line::from(vec![
                    Span::styled(format!("{:>9}  ", "holds"), dim()),
                    Span::styled(clean(&port.name), Style::default().fg(GOOD)),
                    Span::raw(" = "),
                    Span::raw(clean(&value).replace('\n', " ")),
                ]));
            }
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(pane(&format!("At {tag}")))
                .wrap(Wrap { trim: false }),
            inspector,
        );
    }
}

fn render_agents(frame: &mut Frame, console: &Console, area: Rect) {
    let snapshot = console
        .remote
        .run
        .as_ref()
        .and_then(|view| view.snapshot.as_ref())
        .or(console.shown_design().map(|(_, design)| &design.preview));
    let mut lines = vec![
        Line::from("Each agent's terminal lives in this session's tmux server."),
        Line::from("Enter opens the agent dashboard there; z in it comes back here."),
        Line::default(),
        Line::from(vec![
            Span::raw("Mission Control, for the drawn diagram: "),
            Span::styled(console.url.clone(), Style::default().fg(ACCENT)),
        ]),
        Line::default(),
    ];
    if let Some(snapshot) = snapshot {
        for agent in &snapshot.agents {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<24}", clean(&agent.name)), bold()),
                Span::styled(clean(&agent.backend), dim()),
            ]));
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(pane("Agents"))
            .wrap(Wrap { trim: false }),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn console() -> Console {
        let mut console = Console::new("dev".into(), "http://127.0.0.1:4100".into());
        let preview: DiagramSnapshot = serde_json::from_str(include_str!(
            "../../web/tests/fixtures/diagram-snapshot.v1.json"
        ))
        .unwrap();
        let properties: Vec<crate::properties::PropertyVerdict> = serde_json::from_str(
            include_str!("../../web/tests/fixtures/review-flow-properties.json"),
        )
        .unwrap();
        console.remote.messages = serde_json::from_value(serde_json::json!([
            {"sequence": 1, "role": "operator", "text": "Plan a release, reviewed.\tThanks", "progress": false, "design": null},
            {"sequence": 2, "role": "assistant", "text": "Checking the plan with Lean\u{1b}[31m", "progress": true, "design": null},
            {"sequence": 3, "role": "assistant", "text": "Here is a design.", "progress": false, "design": null}
        ]))
        .unwrap();
        console.remote.messages[2].design = Some(crate::serve::ProposedDesign {
            program: include_str!("../../web/tests/fixtures/review-flow.omar").into(),
            files: Default::default(),
            inputs: [("flow.request".to_string(), serde_json::json!("ship it"))].into(),
            preview: preview.clone(),
            lemmas: String::new(),
            properties,
        });
        let mut live = preview;
        live.current_tag = Some(crate::diagram::DiagramTag {
            timestamp: 0,
            microstep: 1,
        });
        console.remote.runs = vec![serde_json::from_value(serde_json::json!({
            "run_id": "run-1", "team": "ReviewFlow", "status": "running",
            "diagram_address": "127.0.0.1:1", "started_at": 1, "finished_at": null, "error": null
        }))
        .unwrap()];
        console.remote.run = Some(super::super::client::RunView {
            run_id: "run-1".into(),
            steps: vec![
                crate::topology::TimelineStep {
                    timestamp: 0,
                    microstep: 0,
                    events: vec![],
                    reactions: vec!["flow.reaction.0".into()],
                },
                crate::topology::TimelineStep {
                    timestamp: 0,
                    microstep: 1,
                    events: vec![],
                    reactions: vec!["flow.reaction.1".into()],
                },
                crate::topology::TimelineStep {
                    timestamp: 0,
                    microstep: 2,
                    events: vec![],
                    reactions: vec!["flow.reaction.2".into()],
                },
            ],
            truncated: false,
            snapshot: Some(live),
        });
        console
    }

    fn screen(console: &Console, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, console)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    const SIZES: [(u16, u16); 4] = [(80, 24), (120, 40), (200, 60), (50, 16)];

    #[test]
    fn every_view_draws_at_every_size_without_escapes_leaking() {
        let mut console = console();
        for tab in Tab::ALL {
            console.tab = tab;
            for view in [
                DesignView::Topology,
                DesignView::Source,
                DesignView::Properties,
            ] {
                console.design_view = view;
                for (width, height) in SIZES.into_iter().chain([(1, 1), (10, 3)]) {
                    let text = screen(&console, width, height);
                    assert!(
                        !text.contains('\u{1b}'),
                        "{tab:?} {view:?} {width}x{height}"
                    );
                    assert!(!text.contains('\t'), "{tab:?} {view:?} {width}x{height}");
                }
            }
        }
    }

    #[test]
    fn the_chat_shows_the_conversation_and_points_at_its_design() {
        let console = console();
        let text = screen(&console, 100, 24);
        assert!(text.contains("you"), "{text}");
        assert!(
            text.contains("Plan a release, reviewed.    Thanks"),
            "{text}"
        );
        assert!(text.contains("assistant  working"), "{text}");
        assert!(
            text.contains(
                "Design 1: ReviewFlow, 1 instances, 3 reactions, 2 of 3 properties proved."
            ),
            "{text}"
        );
    }

    #[test]
    fn the_runs_view_places_the_run_on_its_logical_timeline() {
        let mut console = console();
        console.tab = Tab::Runs;
        let text = screen(&console, 120, 30);
        assert!(text.contains("running  at (0, 1)"), "{text}");
        let rows: Vec<&str> = text
            .lines()
            .filter(|line| line.contains("flow.reaction."))
            .collect();
        assert!(
            rows[0].contains("(0, 0)") && rows[0].contains("done"),
            "{rows:#?}"
        );
        assert!(
            rows[1].contains("> (0, 1)") && rows[1].contains("now"),
            "{rows:#?}"
        );
        assert!(
            rows[2].contains("(0, 2)") && rows[2].contains("next"),
            "{rows:#?}"
        );
        assert!(text.contains("At (0, 1)"), "{text}");
    }

    #[test]
    fn wrapping_keeps_a_lines_indent() {
        assert_eq!(wrap("    intro g hi", 40), vec!["    intro g hi"]);
        assert_eq!(wrap("  aaa bbb", 7), vec!["  aaa", "  bbb"]);
        assert_eq!(wrap("x", 0), vec!["x"]);
    }

    #[test]
    fn a_finished_run_says_which_tags_it_never_reached() {
        let mut console = console();
        console.tab = Tab::Runs;
        console.remote.runs[0].status = RunStatus::Completed;
        let text = screen(&console, 120, 30);
        let rows: Vec<&str> = text
            .lines()
            .filter(|line| line.contains("flow.reaction."))
            .collect();
        assert!(rows[1].contains("done"), "{rows:#?}");
        assert!(rows[2].contains("not reached"), "{rows:#?}");
    }

    #[test]
    fn a_property_marks_what_it_is_about() {
        let mut console = console();
        console.tab = Tab::Design;
        console.design_view = DesignView::Properties;
        let text = screen(&console, 160, 40);
        assert!(text.contains("Proved"), "{text}");
        assert!(text.contains("Not proved"), "{text}");
        assert!(text.contains("theorem"), "{text}");
        // Marked in text, not only in colour.
        assert!(
            text.lines()
                .any(|line| line.contains("> ") && line.contains("flow")),
            "{text}"
        );
    }
}
