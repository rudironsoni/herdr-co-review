//! Rendering the navigator: header, findings list, detail + related code,
//! footer/status, input line, and a help overlay.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::herdr::AgentState;
use crate::model::{Severity, Verdict};
use crate::tui::app::{Action, App, Hit, Input, Pane, Selection};

const FOOTER_CHIPS: &[(&str, Action)] = &[
    ("[v]alidate", Action::Validate),
    ("[d]ismiss", Action::Dismiss),
    ("[b]lock", Action::ToggleImpact),
    ("[u]ndo", Action::Reset),
    ("[e]dit", Action::Edited),
    ("[n]ote", Action::Note),
    ("[c]hat", Action::Chat),
    ("[P]ost", Action::Overall),
    ("[r]efresh", Action::Refresh),
    ("[?]", Action::Help),
    ("[q]", Action::Quit),
];

pub fn draw(f: &mut Frame, app: &mut App) {
    let size = f.area();
    // Grow the input box with its wrapped content, up to half the screen. The
    // footer chunk spans the full frame width, so `size.width` is its width.
    let input = app.input.is_some().then(|| {
        let para = input_paragraph(app);
        let wrap_width = size.width.saturating_sub(2).max(1);
        let lines = para.line_count(wrap_width) as u16;
        (para, lines)
    });
    let footer_h = match &input {
        Some((_, lines)) => (*lines).clamp(3, (size.height / 2).max(3)),
        None => 1,
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),        // header
            Constraint::Percentage(34),   // list
            Constraint::Min(6),           // detail
            Constraint::Length(footer_h), // footer / input
        ])
        .split(size);

    draw_header(f, app, chunks[0]);
    let list_offset = draw_list(f, app, chunks[1]);
    let detail_max_scroll = draw_detail(f, app, chunks[2]);
    app.record_layout(chunks[1], chunks[2], list_offset, detail_max_scroll);

    let mut hits = vec![(chunks[1], Hit::Findings), (chunks[2], Hit::Detail)];
    match input {
        Some((para, lines)) => {
            // Past the height cap, keep the tail (with the cursor) in view.
            let scroll = lines.saturating_sub(chunks[3].height);
            f.render_widget(para.scroll((scroll, 0)), chunks[3]);
            hits.push((size, Hit::CancelInput));
            hits.push((chunks[3], Hit::Ignore));
        }
        None => draw_footer(f, app, chunks[3], &mut hits),
    }

    if app.show_help {
        let help_area = draw_help(f, size);
        hits.push((size, Hit::DismissOverlay));
        hits.push((help_area, Hit::Help));
        app.help_area = help_area;
    } else {
        app.help_area = Rect::default();
    }
    if app.asking_pr_verdict {
        draw_pr_verdict(f, app, size, &mut hits);
    }
    app.record_hits(hits);
    if let Some(sel) = app.selection.as_mut() {
        paint_selection(f.buffer_mut(), sel);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let s = &app.state;
    let c = s.counts();

    let title = if s.pr.title.is_empty() {
        format!("{}/{} #{}", s.pr.owner, s.pr.repo, s.pr.number)
    } else {
        format!(
            "{}/{} #{} — {}",
            s.pr.owner, s.pr.repo, s.pr.number, s.pr.title
        )
    };

    let mut spans = vec![Span::styled(
        format!(" {} ", s.status.label()),
        Style::default()
            .bg(Color::Blue)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(agent) = app.agent_state() {
        spans.push(Span::styled(
            format!(" agent: {} ", agent.label()),
            Style::default().fg(agent_state_color(agent)),
        ));
    }
    spans.extend([
        Span::raw(format!("  {} findings", c.total)),
        Span::styled(
            format!("  {} pending", c.pending),
            Style::default().fg(Color::Gray),
        ),
        Span::styled(
            format!("  {} validated", c.validated),
            Style::default().fg(Color::Green),
        ),
        Span::styled(
            format!("  {} dismissed", c.dismissed),
            Style::default().fg(Color::Red),
        ),
        Span::styled(
            format!("  {} blocking", c.blocking_validated),
            Style::default().fg(Color::Magenta),
        ),
        Span::styled(
            format!("  {} posted", c.posted),
            Style::default().fg(Color::Cyan),
        ),
    ]);
    let line2 = Line::from(spans);

    let block = Block::default().borders(Borders::ALL).title(" co-review ");
    let para = Paragraph::new(vec![
        Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        )),
        line2,
    ])
    .block(block);
    f.render_widget(para, area);
}

/// Draws the findings list and returns the scroll offset it settled on, which is
/// what turns a clicked row back into a finding index.
fn draw_list(f: &mut Frame, app: &App, area: Rect) -> usize {
    let items: Vec<ListItem> = if app.state.findings.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  waiting for the agent's findings…",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )))]
    } else {
        app.state
            .findings
            .iter()
            .map(|fd| {
                let loc = fd
                    .primary_location()
                    .map(|l| l.label())
                    .unwrap_or_else(|| "—".into());
                let line = Line::from(vec![
                    Span::styled(
                        format!("{} ", fd.severity.glyph()),
                        Style::default().fg(severity_color(fd.severity)),
                    ),
                    verdict_badge(fd.verdict),
                    Span::raw(" "),
                    Span::styled(
                        fd.title.clone(),
                        if fd.posted {
                            Style::default().add_modifier(Modifier::DIM)
                        } else {
                            Style::default()
                        },
                    ),
                    Span::styled(format!("  {loc}"), Style::default().fg(Color::DarkGray)),
                ]);
                ListItem::new(line)
            })
            .collect()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_style(app, Pane::Findings))
        .title(" findings (j/k) ");
    let list = List::default()
        .items(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(40, 40, 55))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▌");

    let mut state = ListState::default().with_offset(app.list_offset);
    if !app.state.findings.is_empty() {
        state.select(Some(app.selected));
    }
    f.render_stateful_widget(list, area, &mut state);
    state.offset()
}

/// Draws the detail pane and returns the largest useful scroll offset.
fn draw_detail(f: &mut Frame, app: &App, area: Rect) -> u16 {
    let mut lines: Vec<Line> = Vec::new();

    if let Some(fd) = app.state.findings.get(app.selected) {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", fd.severity.glyph()),
                Style::default().fg(severity_color(fd.severity)),
            ),
            Span::styled(
                fd.severity.label().to_uppercase(),
                Style::default()
                    .fg(severity_color(fd.severity))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            verdict_badge(fd.verdict),
            Span::raw("  "),
            Span::styled(
                fd.impact.label().to_string(),
                Style::default().fg(if fd.impact == crate::model::Impact::Blocking {
                    Color::Magenta
                } else {
                    Color::DarkGray
                }),
            ),
            Span::raw("  "),
            Span::styled(fd.id.clone(), Style::default().fg(Color::DarkGray)),
        ]));
        lines.push(Line::from(Span::styled(
            fd.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        if let Some(cat) = &fd.category {
            lines.push(Line::from(Span::styled(
                format!("category: {cat}"),
                Style::default().fg(Color::DarkGray),
            )));
        }
        if fd.posted {
            let url = fd.posted_url.clone().unwrap_or_default();
            lines.push(Line::from(Span::styled(
                format!("✓ posted {url}"),
                Style::default().fg(Color::Cyan),
            )));
        }
        lines.push(Line::from(""));
        for bl in fd.body.lines() {
            lines.push(Line::from(bl.to_string()));
        }
        if let Some(sug) = &fd.suggestion {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "suggestion:",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
            for sl in sug.lines() {
                lines.push(Line::from(Span::styled(
                    sl.to_string(),
                    Style::default().fg(Color::Green),
                )));
            }
        }
        if let Some(note) = &fd.user_note {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "your note: ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(note.clone(), Style::default().fg(Color::Yellow)),
            ]));
        }

        // Related code blocks.
        for block in app.code_blocks() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("── {} ──", block.header),
                Style::default()
                    .fg(Color::Rgb(130, 170, 255))
                    .add_modifier(Modifier::BOLD),
            )));
            lines.extend(block.lines.iter().cloned());
        }
    } else {
        lines.push(Line::from(Span::styled(
            "No finding selected.",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let total = lines.len() as u16;
    let max_scroll = total.saturating_sub(1);
    let scroll = app.detail_scroll.min(max_scroll);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_style(app, Pane::Detail))
        .title(" detail & related code (J/K scroll) ");
    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(para, area);
    max_scroll
}

/// The focused pane gets a highlighted border, so it's obvious where the wheel
/// will scroll.
fn focus_style(app: &App, pane: Pane) -> Style {
    if app.focus == pane {
        Style::default().fg(Color::Rgb(130, 170, 255))
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    if let Some(msg) = app.status_line() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {msg}"),
                Style::default().fg(Color::Black).bg(Color::Yellow),
            ))),
            area,
        );
        return;
    }

    let mut x = area.x;
    let y = area.y;
    let max_x = area.right();
    let mut spans: Vec<Span> = Vec::new();
    for (i, (label, action)) in FOOTER_CHIPS.iter().enumerate() {
        let width = label.len() as u16;
        let gap = u16::from(i > 0);
        if x.saturating_add(gap).saturating_add(width) > max_x {
            break;
        }
        if gap == 1 {
            spans.push(Span::raw(" "));
            x = x.saturating_add(1);
        }
        hits.push((
            Rect::new(x, y, width, area.height.max(1)),
            Hit::Action(*action),
        ));
        spans.push(Span::styled(*label, Style::default().fg(Color::Gray)));
        x = x.saturating_add(width);
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn input_paragraph(app: &App) -> Paragraph<'static> {
    let (title, prefix) = match app.input {
        Some(Input::Note) => (" note (Enter save · Esc cancel) ", "note> "),
        Some(Input::Chat) => (" message to agent (Enter send · Esc cancel) ", "chat> "),
        Some(Input::FollowUp) => (
            " extra task for the agent (Enter send · Esc back) ",
            "task> ",
        ),
        None => (" input ", "> "),
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let line = Line::from(vec![
        Span::styled(prefix, Style::default().fg(Color::Cyan)),
        Span::raw(app.input_buffer.clone()),
        Span::styled("▏", Style::default().fg(Color::Cyan)),
    ]);
    Paragraph::new(line).block(block).wrap(Wrap { trim: false })
}

fn draw_help(f: &mut Frame, size: Rect) -> Rect {
    let area = centered_rect(70, 70, size);
    f.render_widget(Clear, area);
    let help = vec![
        Line::from(Span::styled(
            "co-review navigator",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("Navigation"),
        Line::from("  j / k / ↓ / ↑     move between findings"),
        Line::from("  g / G             first / last finding"),
        Line::from("  J / K / PgDn/PgUp scroll the detail & code"),
        Line::from(""),
        Line::from("Mouse"),
        Line::from("  click             select a finding · focus a pane (lit border)"),
        Line::from("  click footer      same actions as the keys in each chip"),
        Line::from("  click overlay     buttons run; outside dismisses"),
        Line::from("  wheel             scrolls the pane under the cursor"),
        Line::from("  drag              select text and copy it"),
        Line::from("  m                 let the terminal/Herdr select text"),
        Line::from(""),
        Line::from(Span::styled(
            "click outside to close this help · drag to copy",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from("Triage (acts on the selected finding)"),
        Line::from("  v / a  validate     d  dismiss     b  toggle blocking"),
        Line::from("  u  reset to pending     e  validate as edited"),
        Line::from("  n  add / edit your note"),
        Line::from(""),
        Line::from("Collaboration"),
        Line::from("  c  message the agent about this finding (into its pane)"),
        Line::from("  P  after all findings: pick/resend the overall result"),
        Line::from(""),
        Line::from("Other"),
        Line::from("  r  force refresh    ?  toggle this help    m  mouse on/off    q / Esc  quit"),
        Line::from(""),
        Line::from(Span::styled(
            "Findings appear live as the agent records them; your verdicts and notes",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "are visible to the agent immediately.",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let block = Block::default().borders(Borders::ALL).title(" help ");
    f.render_widget(
        Paragraph::new(help)
            .block(block)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false }),
        area,
    );
    area
}

fn paint_selection(buf: &mut Buffer, sel: &mut Selection) {
    let mut text = String::new();
    let mut last_row: Option<u16> = None;
    let mut line = String::new();
    for (x, y) in stream_cells(sel.area, sel.start, sel.end) {
        if last_row.is_some() && last_row != Some(y) {
            push_sel_line(&mut text, &mut line);
        }
        last_row = Some(y);
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
            line.push_str(cell.symbol());
        }
    }
    if last_row.is_some() {
        push_sel_line(&mut text, &mut line);
    }
    if text.ends_with('\n') {
        text.pop();
    }
    sel.text = text;
}

fn push_sel_line(text: &mut String, line: &mut String) {
    text.push_str(line.trim_end());
    text.push('\n');
    line.clear();
}

fn stream_cells(area: Rect, start: (u16, u16), end: (u16, u16)) -> Vec<(u16, u16)> {
    let (a, b) = if (start.1, start.0) <= (end.1, end.0) {
        (start, end)
    } else {
        (end, start)
    };
    let mut out = Vec::new();
    let last_x = area.right().saturating_sub(1);
    for y in a.1..=b.1 {
        let x0 = if y == a.1 { a.0 } else { area.x };
        let x1 = if y == b.1 { b.0 } else { last_x };
        for x in x0..=x1 {
            if area.contains((x, y).into()) {
                out.push((x, y));
            }
        }
    }
    out
}

fn draw_pr_verdict(f: &mut Frame, app: &App, size: Rect, hits: &mut Vec<(Rect, Hit)>) {
    let area = centered_rect(64, 50, size);
    f.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).title(" PR review ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    hits.push((size, Hit::DismissOverlay));
    hits.push((area, Hit::Ignore));

    let agent_line = match app.state.agent_pr_verdict {
        Some(v) => format!("Agent recommends: {}", v.label()),
        None => "Agent has not recorded an overall opinion yet.".to_string(),
    };
    let derived = app.state.derived_overall();
    let c = app.state.counts();
    let derived_line = format!(
        "Derived: {}  ({} validated, {} blocking)",
        derived.label(),
        c.validated,
        c.blocking_validated
    );
    let lines = vec![
        Line::from(Span::styled(
            "submit GitHub review",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(agent_line),
        Line::from(derived_line),
        Line::from("One review: inline comments for findings with a line, rest in the body."),
        Line::from("The event is derived. Enter sends it. This is not a finding key."),
        Line::from("Click a button, or click outside to pick later with P."),
    ];

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false }),
        chunks[0],
    );

    let buttons = [
        (
            chunks[1],
            "[Enter] submit the derived review",
            Action::SubmitPrVerdict,
        ),
        (
            chunks[2],
            "[x] another loop (no GitHub review)",
            Action::FollowUp,
        ),
        (
            chunks[3],
            "[Esc] pick later with P",
            Action::CancelPrVerdict,
        ),
    ];
    for (rect, label, action) in buttons {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                label,
                Style::default().fg(Color::Cyan),
            ))),
            rect,
        );
        hits.push((rect, Hit::Action(action)));
    }
}

fn agent_state_color(state: AgentState) -> Color {
    match state {
        AgentState::Working => Color::Green,
        AgentState::Blocked => Color::Yellow,
        AgentState::Done => Color::Cyan,
        AgentState::Idle => Color::Gray,
    }
}

fn severity_color(s: Severity) -> Color {
    match s {
        Severity::Critical => Color::Rgb(255, 85, 85),
        Severity::High => Color::Rgb(255, 135, 95),
        Severity::Medium => Color::Rgb(240, 200, 90),
        Severity::Low => Color::Rgb(120, 170, 255),
        Severity::Nit => Color::Rgb(140, 140, 150),
    }
}

fn verdict_badge(v: Verdict) -> Span<'static> {
    let (text, color) = match v {
        Verdict::Pending => ("pending ", Color::DarkGray),
        Verdict::Validated => ("validated", Color::Green),
        Verdict::Dismissed => ("dismissed", Color::Red),
        Verdict::Edited => ("edited  ", Color::Cyan),
    };
    Span::styled(format!("[{text}]"), Style::default().fg(color))
}

/// A centered rectangle `pct_x`% by `pct_y`% of `r`.
fn centered_rect(pct_x: u16, pct_y: u16, r: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(v[1])[1]
}
