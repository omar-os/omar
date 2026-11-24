//! Deterministic image projection of the checked diagram snapshot. Never an
//! AI-generated diagram; no browser, external images, URLs or source execution.
use crate::diagram::{DiagramSnapshot, EdgeKind};
use crate::topology::PortKind;
use anyhow::{ensure, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::sync::{Arc, OnceLock};

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
}
fn short(text: &str, limit: usize) -> String {
    let first = text.chars().take(limit).collect::<String>();
    escape(&if text.chars().count() > limit {
        format!("{first}…")
    } else {
        first
    })
}
fn text(svg: &mut String, x: f32, y: f32, label: &str, size: u32, color: &str) {
    write!(svg,r#"<text x="{x}" y="{y}" font-family="DejaVu Sans,sans-serif" font-size="{size}" fill="{color}">{label}</text>"#).unwrap();
}

pub(super) fn svg(snapshot: &DiagramSnapshot) -> Result<String> {
    ensure!(
        snapshot.ports.len() + snapshot.reactions.len() + snapshot.timers.len() <= 300
            && snapshot.edges.len() <= 100,
        "Graph exceeds preview size limit"
    );
    let mut groups = BTreeSet::new();
    let mut owners = BTreeMap::new();
    for p in &snapshot.ports {
        groups.insert(p.instance.clone());
        owners.insert(p.id.clone(), p.instance.clone());
    }
    for r in &snapshot.reactions {
        groups.insert(r.instance.clone());
        owners.insert(r.id.clone(), r.instance.clone());
    }
    for t in &snapshot.timers {
        groups.insert(t.instance.clone());
        owners.insert(t.id.clone(), t.instance.clone());
    }
    if groups.is_empty() {
        groups.insert(String::new());
    }
    ensure!(groups.len() <= 32, "Too many teams for graph preview");
    // Longest forward level in the instance dependency DAG. Cycle members stay
    // together; their real edges are still drawn rather than silently omitted.
    let mut links = BTreeSet::new();
    for e in &snapshot.edges {
        if let (Some(a), Some(b)) = (owners.get(&e.source), owners.get(&e.target)) {
            if a != b {
                links.insert((a.clone(), b.clone()));
            }
        }
    }
    let mut levels: BTreeMap<String, usize> = groups.iter().map(|g| (g.clone(), 0)).collect();
    let mut remaining = groups.clone();
    loop {
        let ready = remaining
            .iter()
            .filter(|g| !links.iter().any(|(a, b)| b == *g && remaining.contains(a)))
            .cloned()
            .collect::<Vec<_>>();
        if ready.is_empty() {
            break;
        }
        for group in ready {
            let level = links
                .iter()
                .filter(|(_, b)| b == &group)
                .map(|(a, _)| levels[a] + 1)
                .max()
                .unwrap_or(0);
            levels.insert(group.clone(), level);
            remaining.remove(&group);
        }
    }
    let mut boxes = BTreeMap::new();
    let mut rows = BTreeMap::<usize, f32>::new();
    let mut width = 500f32;
    let mut height = 250f32;
    for group in &groups {
        let level = levels[group];
        let reactions = snapshot
            .reactions
            .iter()
            .filter(|r| &r.instance == group)
            .count();
        let ports = snapshot
            .ports
            .iter()
            .filter(|p| &p.instance == group)
            .count();
        let timers = snapshot
            .timers
            .iter()
            .filter(|t| &t.instance == group)
            .count();
        let h = (reactions.max(1) as f32 * 104. + 54.)
            .max(ports as f32 * 26. + 48.)
            .max(timers as f32 * 35. + 100.);
        let x = 36. + level as f32 * 468.;
        let y = *rows.entry(level).or_insert(115.);
        rows.insert(level, y + h + 64.);
        boxes.insert(group.clone(), (x, y, 400f32, h));
        width = width.max(x + 436.);
        height = height.max(y + h + 36.);
    }
    ensure!(
        width <= 8192. && height <= 8192.,
        "Graph is too large for a readable preview"
    );
    let mut svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}"><defs><pattern id="grid" width="24" height="24" patternUnits="userSpaceOnUse"><path d="M24 0H0V24" fill="none" stroke="#24242a" stroke-width="0.7"/></pattern><marker id="arrow" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto"><path d="M0 0L8 4L0 8Z" fill="#c9c7d2"/></marker></defs><rect width="100%" height="100%" fill="#111114"/><rect width="100%" height="100%" fill="url(#grid)"/>"##
    );
    text(
        &mut svg,
        36.,
        35.,
        &short(&snapshot.team, 90),
        19,
        "#eceaf2",
    );
    text(
        &mut svg,
        36.,
        58.,
        "PROPOSED WORKFLOW · READY FOR REVIEW · NOT RUN",
        10,
        "#aaa6b6",
    );
    let mut points = BTreeMap::new();
    for group in &groups {
        let (x, y, w, h) = boxes[group];
        let label = if group.is_empty() {
            snapshot.team.as_str()
        } else {
            group.as_str()
        };
        text(&mut svg, x, y - 18., &short(label, 45), 13, "#eceaf2");
        let team = snapshot
            .instances
            .iter()
            .find(|i| i.name == *group)
            .map(|i| i.team.as_str())
            .unwrap_or("TEAM");
        text(
            &mut svg,
            x,
            y - 5.,
            &format!("{} · READY", short(team, 40)),
            9,
            "#aaa6b6",
        );
        write!(svg,r##"<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="9" fill="#1c1b22" stroke="#45434f"/>"##).unwrap();
        let ports = snapshot
            .ports
            .iter()
            .filter(|p| p.instance == *group)
            .collect::<Vec<_>>();
        for kind in [PortKind::Input, PortKind::Output, PortKind::Action] {
            let items = ports.iter().filter(|p| p.kind == kind).collect::<Vec<_>>();
            for (n, p) in items.iter().enumerate() {
                let offset = if kind == PortKind::Action {
                    ports.iter().filter(|p| p.kind == PortKind::Input).count()
                } else {
                    0
                };
                let py = y + 62. + (n + offset) as f32 * 28.;
                let px = if kind == PortKind::Output { x + w } else { x };
                points.insert(p.id.clone(), (px, py));
                write!(
                    svg,
                    r##"<path d="M{} {}l{} -5v10Z" fill="#d9d6e3"/>"##,
                    px + 5.,
                    py,
                    -9.
                )
                .unwrap();
                let name = p.name.rsplit('.').next().unwrap_or(&p.name);
                let label_x = if kind == PortKind::Output {
                    x + w - 90.
                } else {
                    x + 9.
                };
                text(&mut svg, label_x, py - 10., &short(name, 14), 10, "#ddd9e7");
            }
        }
        for (n, r) in snapshot
            .reactions
            .iter()
            .filter(|r| r.instance == *group)
            .enumerate()
        {
            let rx = x + 108.;
            let ry = y + 22. + n as f32 * 104.;
            let rw = 188.;
            let rh = 80.;
            points.insert(r.id.clone(), (rx + rw / 2., ry + rh / 2.));
            write!(svg,r##"<rect x="{rx}" y="{ry}" width="{rw}" height="{rh}" rx="8" fill="#2b2933" stroke="#686471"/><path d="M{} {}H{}" stroke="#85818e" stroke-width="3"/>"##,rx+8.,ry+2.,rx+rw-8.).unwrap();
            let agent = snapshot
                .agents
                .iter()
                .find(|a| a.id == r.agent || a.name == r.agent);
            let name = agent
                .map(|a| a.name.rsplit('.').next().unwrap_or(&a.name))
                .unwrap_or("Rust reaction");
            text(
                &mut svg,
                rx + 13.,
                ry + 24.,
                &format!("{:02}   {}", n + 1, short(name, 22)),
                11,
                "#f0edf6",
            );
            let prompt = r.prompt.as_deref().unwrap_or(&r.contract);
            text(
                &mut svg,
                rx + 14.,
                ry + 43.,
                &short(&prompt.replace(['\n', '\r'], " "), 24),
                9,
                "#a9a4b5",
            );
            let backend = agent.map(|a| a.backend.as_str()).unwrap_or("Rust");
            text(
                &mut svg,
                rx + 14.,
                ry + 64.,
                &format!("{} · idle", short(backend, 22)),
                10,
                "#bbb5c9",
            );
        }
        for (n, t) in snapshot
            .timers
            .iter()
            .filter(|t| t.instance == *group)
            .enumerate()
        {
            let tx = x + 35.;
            let ty = y + h - 22. - n as f32 * 28.;
            points.insert(t.id.clone(), (tx, ty));
            write!(
                svg,
                r##"<circle cx="{tx}" cy="{ty}" r="7" fill="#292630" stroke="#a9a1bc"/>"##
            )
            .unwrap();
            text(
                &mut svg,
                tx + 12.,
                ty + 4.,
                &short(&t.name, 20),
                9,
                "#bfb7cf",
            );
        }
    }
    for edge in &snapshot.edges {
        let (Some(&(mut x1, y1)), Some(&(mut x2, y2))) =
            (points.get(&edge.source), points.get(&edge.target))
        else {
            anyhow::bail!("Diagram edge has no drawable endpoint");
        };
        if snapshot.reactions.iter().any(|r| r.id == edge.source) {
            x1 += 94.;
        }
        if snapshot.reactions.iter().any(|r| r.id == edge.target) {
            x2 -= 94.;
        }
        let dash = if edge.kind == EdgeKind::Connection {
            ""
        } else {
            r#" stroke-dasharray="3 3""#
        };
        let bend = ((x2 - x1).abs() / 2.).max(22.);
        write!(svg,r##"<path d="M{x1} {y1}C{} {y1},{} {y2},{x2} {y2}" fill="none" stroke="#b7b1c5" stroke-width="1.3"{dash} marker-end="url(#arrow)"/>"##,x1+bend,x2-bend).unwrap();
    }
    svg.push_str("</svg>");
    Ok(svg)
}

pub(super) fn png(snapshot: &DiagramSnapshot) -> Result<Vec<u8>> {
    static FONTS: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    let fonts = FONTS
        .get_or_init(|| {
            let mut db = resvg::usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone();
    ensure!(
        !fonts.is_empty(),
        "Graph previews need a system font, for example fonts-dejavu-core"
    );
    let options = resvg::usvg::Options {
        fontdb: fonts,
        ..Default::default()
    };
    let tree =
        resvg::usvg::Tree::from_str(&svg(snapshot)?, &options).context("Invalid graph image")?;
    let scale = 2f32
        .min(4096. / tree.size().width())
        .min(4096. / tree.size().height());
    let mut image = resvg::tiny_skia::Pixmap::new(
        (tree.size().width() * scale).ceil() as u32,
        (tree.size().height() * scale).ceil() as u32,
    )
    .context("Cannot allocate graph image")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut image.as_mut(),
    );
    image.encode_png().context("Cannot encode graph preview")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn sample() -> DiagramSnapshot {
        serde_json::from_value(json!({
            "protocol_version":1,"team":"CompanyNews","sequence":0,"status":"ready","current_tag":null,"lag":null,"timers":[],
            "instances":[{"id":"i-gather","name":"gather","team":"Gatherer","parent":""},{"id":"i-check","name":"check","team":"Checker","parent":""}],
            "agents":[{"id":"a-gather","name":"gather.agent","backend":"ClaudeCode","instance":"gather"},{"id":"a-check","name":"check.agent","backend":"ClaudeCode","instance":"check"}],
            "ports":[
                {"id":"topic","name":"gather.topic","kind":"input","type":"string","delay":null,"value":null,"last_tag":null,"instance":"gather"},
                {"id":"sources","name":"gather.sources","kind":"output","type":"string","delay":null,"value":null,"last_tag":null,"instance":"gather"},
                {"id":"in","name":"check.sources","kind":"input","type":"string","delay":null,"value":null,"last_tag":null,"instance":"check"},
                {"id":"report","name":"check.report","kind":"output","type":"string","delay":null,"value":null,"last_tag":null,"instance":"check"}
            ],
            "reactions":[
                {"id":"gather-task","name":"gather.reaction.0","agent":"a-gather","order":0,"triggers":["topic"],"effects":["sources"],"contract":"","status":"idle","invocation_id":null,"instance":"gather","prompt":"You are the source gatherer."},
                {"id":"check-task","name":"check.reaction.0","agent":"a-check","order":1,"triggers":["in"],"effects":["report"],"contract":"","status":"idle","invocation_id":null,"instance":"check","prompt":"You are the fact checker."}
            ],
            "edges":[
                {"id":"e1","kind":"trigger","source":"topic","target":"gather-task","delay":null},
                {"id":"e2","kind":"effect","source":"gather-task","target":"sources","delay":null},
                {"id":"e3","kind":"connection","source":"sources","target":"in","delay":null},
                {"id":"e4","kind":"trigger","source":"in","target":"check-task","delay":null},
                {"id":"e5","kind":"effect","source":"check-task","target":"report","delay":null}
            ]
        })).unwrap()
    }
    #[test]
    fn graph_preview_uses_checked_nodes_and_connections_and_exports_a_real_png() {
        let snapshot = sample();
        let image = png(&snapshot).unwrap();
        assert!(image.starts_with(b"\x89PNG\r\n\x1a\n"));
        let width = u32::from_be_bytes(image[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(image[20..24].try_into().unwrap());
        assert!(width > 1000 && width <= 4096 && height > 200 && height <= 4096);
        let vector = svg(&snapshot).unwrap();
        assert_eq!(vector.matches("marker-end").count(), snapshot.edges.len());
        assert!(vector.contains("ClaudeCode"));
        assert!(vector.contains("report"));
        if let Some(path) = std::env::var_os("OMAR_GRAPH_TEST_OUTPUT") {
            let image = if let Some(source) = std::env::var_os("OMAR_GRAPH_TEST_SNAPSHOT") {
                let real: DiagramSnapshot =
                    serde_json::from_slice(&std::fs::read(source).unwrap()).unwrap();
                png(&real).unwrap()
            } else {
                image
            };
            std::fs::write(path, image).unwrap();
        }
    }
    #[test]
    fn graph_labels_are_escaped_and_missing_endpoints_are_refused() {
        let mut snapshot = sample();
        snapshot.team = "<script>&unsafe".into();
        let vector = svg(&snapshot).unwrap();
        assert!(vector.contains("&lt;script&gt;&amp;unsafe"));
        assert!(!vector.contains("<script>"));
        snapshot.edges[0].source = "missing".into();
        assert!(svg(&snapshot).is_err());
    }
}
