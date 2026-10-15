use std::time::Instant;

use protocol::{AgentInfo, AgentKind, Color as ScreenColor, Screen, Status};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{
    AgentView, App, DiffView, Field, Focus, MIN_PANE, NewAgentForm, Popup, Preset, Selection,
};
use crate::config::Theme;
use crate::keymap::{Action, Keymap};

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const FORM_HINT: &str = "tab field · ←/→ change · enter start · esc cancel";
const CONFIRM_HINT: &str = "y/enter confirm · n/esc cancel";

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
    (Action::NextAttention, "next !"),
    (Action::Diff, "diff"),
];

const SIDEBAR_HELP: [(Action, &str); 19] = [
    (Action::Down, "move down in the focused box"),
    (Action::Up, "move up in the focused box"),
    (Action::FocusProjects, "focus Projects"),
    (Action::FocusAgents, "focus Agents"),
    (Action::ToggleFocus, "switch between Projects and Agents"),
    (Action::Open, "open the agent's pane"),
    (Action::NewAgent, "new agent"),
    (Action::Stop, "stop agent"),
    (Action::Restart, "restart agent"),
    (Action::Remove, "remove agent"),
    (Action::Rename, "rename agent"),
    (Action::AddProject, "add project"),
    (Action::ToggleAllAgents, "all agents / this project"),
    (Action::NextAttention, "next agent needing input"),
    (Action::Diff, "diff of the agent's changes"),
    (Action::Scroll, "scroll the agent's history"),
    (Action::Help, "help"),
    (Action::Quit, "quit (agents keep running)"),
    (Action::StopDaemon, "stop the daemon and quit"),
];
const SCROLL_HINT: &str = "SCROLL · j/k ctrl-u/d g/G · q exit";
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
    areas(area, app.projects.len(), info_lines(app).len())
}

/// The Info box fits `info_lines` lines, but never takes the Agents box below
/// `AGENTS_MIN_HEIGHT` rows to do so; it keeps `INFO_MIN_HEIGHT` rows while they fit.
pub fn areas(area: Rect, project_count: usize, info_lines: usize) -> Areas {
    let body_height = area.height.saturating_sub(1);
    let status = Rect::new(area.x, area.y + body_height, area.width, area.height.min(1));
    let sidebar_width = (area.width / 4).clamp(24, 40).min(area.width);
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
        Status::Starting => {
            let symbol = if (tick / 5).is_multiple_of(2) {
                "◌"
            } else {
                "○"
            };
            (symbol, Style::new().dim())
        }
        Status::Working => (
            SPINNER[(tick % 10) as usize],
            Style::new().fg(theme.working),
        ),
        Status::NeedsInput => ("!", Style::new().fg(theme.attention).bold()),
        Status::Idle => ("✓", Style::new().fg(theme.idle)),
        Status::Exited(0) => ("✗", Style::new().dim()),
        Status::Exited(_) => ("✗", Style::new().fg(theme.attention)),
        Status::Stopped => ("■", Style::new().fg(theme.dim)),
    }
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

fn block(title: impl Into<Line<'static>>, focused: bool, theme: &Theme) -> Block<'static> {
    let border = if focused { theme.accent } else { theme.border };
    Block::bordered().title(title).border_style(border)
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
    if let Some(popup) = &app.popup {
        draw_popup(frame, popup, app, frame.area());
    }
}

fn selectable_list(
    items: Vec<ListItem<'static>>,
    selected: usize,
    block: Block<'static>,
) -> (List<'static>, ListState) {
    let list = List::new(items)
        .block(block)
        .highlight_symbol("▸ ")
        .highlight_style(Style::new().reversed());
    (list, ListState::default().with_selected(Some(selected)))
}

fn draw_projects(frame: &mut Frame, app: &App, area: Rect) {
    let block = block(" Projects ", app.focus == Focus::Projects, &app.theme);
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
                spans.push(Span::raw(format!("{attention}!")).fg(app.theme.attention));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let (list, mut state) = selectable_list(items, app.project_index, block);
    frame.render_stateful_widget(list, area, &mut state);
}

fn agent_row(app: &App, agent: &AgentInfo, tick: u64) -> ListItem<'static> {
    let (symbol, style) = glyph(agent.status, tick, &app.theme);
    let name = match app.view {
        AgentView::All => {
            let project = app.projects.iter().find(|p| p.id == agent.project);
            format!(
                "{}/{}",
                project.map_or("?", |p| p.name.as_str()),
                agent.name
            )
        }
        AgentView::Project => agent.name.clone(),
    };
    let rest = format!(" {name}  {}", agent.status.label());
    let line = Line::from(vec![Span::styled(symbol, style), Span::raw(rest)]);
    ListItem::new(if agent.status == Status::NeedsInput {
        line.fg(app.theme.attention)
    } else {
        line
    })
}

fn draw_agents(frame: &mut Frame, app: &App, area: Rect, tick: u64) {
    let title = match app.view {
        AgentView::Project => " Agents ",
        AgentView::All => " Agents · all ",
    };
    let block = block(title, app.focus == Focus::Agents, &app.theme);
    let agents = app.visible_agents();
    if agents.is_empty() {
        let new = app.keymap.first_key(Action::NewAgent);
        let text = format!("no agents — press {new}");
        frame.render_widget(Paragraph::new(text).block(block), area);
        return;
    }
    let items = agents.iter().map(|a| agent_row(app, a, tick)).collect();
    let (list, mut state) = selectable_list(items, app.agent_index, block);
    frame.render_stateful_widget(list, area, &mut state);
}

fn kind_label(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "claude",
        AgentKind::Generic => "generic",
    }
}

/// Git lines come right after the status, so they stay visible when a short terminal
/// clips the panel.
pub fn info_lines(app: &App) -> Vec<String> {
    match (app.selected_agent(), app.selected_project()) {
        (Some(agent), _) => {
            let command = if agent.command.is_empty() {
                "claude".to_string()
            } else {
                agent.command.join(" ")
            };
            let mut lines = vec![format!("status: {}", agent.status.label())];
            if let Some(worktree) = &agent.worktree {
                lines.push(format!("branch: {}", worktree.branch));
            }
            if let Some(stat) = app.git_stats.get(&agent.id) {
                lines.push(format!(
                    "+{} −{} · {} files",
                    stat.insertions, stat.deletions, stat.files
                ));
                lines.push(format!("{} uncommitted", stat.uncommitted));
            }
            lines.extend([
                format!("cmd: {command}"),
                format!("cwd: {}", agent.cwd.display()),
                format!("kind: {}", kind_label(agent.kind)),
            ]);
            lines
        }
        (None, Some(project)) => vec![format!("path: {}", project.path.display())],
        (None, None) => Vec::new(),
    }
}

fn draw_info(frame: &mut Frame, app: &App, area: Rect) {
    let text: Vec<Line> = info_lines(app).into_iter().map(Line::from).collect();
    let block = block(" Info ", false, &app.theme);
    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn draw_pane(frame: &mut Frame, app: &App, areas: &Areas) {
    let focused = app.focus == Focus::Pane;
    let agent = app.selected_agent();
    if let (Some(view), Some(agent)) = (&app.diff, agent) {
        let title = format!(" diff · {} ", agent.name);
        frame.render_widget(block(title, true, &app.theme), areas.pane);
        frame.render_widget(
            diff_text(view, &app.theme, areas.pane_inner.height),
            areas.pane_inner,
        );
        return;
    }
    let title = match agent {
        Some(agent) => format!(
            " {} · {} · {} ",
            agent.name,
            kind_label(agent.kind),
            agent.status.label()
        ),
        None => " lazyagents ".to_string(),
    };
    let pane_block = if app.focus == Focus::Scroll {
        Block::bordered()
            .title(title)
            .border_style(app.theme.scroll)
    } else {
        block(title, focused, &app.theme)
    };
    frame.render_widget(pane_block, areas.pane);
    let Some(agent) = agent else {
        let new = app.keymap.first_key(Action::NewAgent);
        frame.render_widget(
            Paragraph::new(format!("press {new} to start an agent")),
            areas.pane_inner,
        );
        return;
    };
    if let Some(error) = &agent.last_error
        && !agent.status.is_running()
        && app.screen.text().trim().is_empty()
    {
        let retry = app.keymap.first_key(Action::Restart);
        let text = vec![
            Line::from(format!("failed to start: {error}")).fg(app.theme.attention),
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

fn diff_text<'a>(view: &'a DiffView, theme: &Theme, height: u16) -> Paragraph<'a> {
    let Some(lines) = &view.lines else {
        return Paragraph::new("loading diff…").fg(theme.dim);
    };
    if lines.is_empty() {
        return Paragraph::new("no changes").fg(theme.dim);
    }
    let visible = lines.iter().skip(view.scroll).take(usize::from(height));
    let styled = visible.map(|line| {
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
        Line::styled(line.as_str(), style)
    });
    Paragraph::new(styled.collect::<Vec<_>>())
}

fn diff_hint(keymap: &Keymap) -> String {
    let mut close = vec!["q".to_string(), "esc".to_string(), "D".to_string()];
    for key in keymap.keys(Action::Diff) {
        let key = key.to_string();
        if !close.contains(&key) {
            close.push(key);
        }
    }
    format!(
        "{} close · j/k scroll · ctrl-d/ctrl-u half page · pagedown/pageup page · g/G top/bottom",
        close.join("/")
    )
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

fn draw_status(frame: &mut Frame, app: &App, area: Rect, now: Instant) {
    let theme = &app.theme;
    let mut spans = Vec::new();
    if let Some(message) = app.current_message(now) {
        spans.push(Span::raw(message.to_string()).fg(theme.attention));
        spans.push(Span::raw(" │ ").fg(theme.dim));
    }
    spans.push(match app.focus {
        Focus::Pane if app.prefix_pending => {
            Span::raw(format!("{} …", app.keymap.prefix())).fg(theme.working)
        }
        Focus::Pane => Span::raw(pane_hint(&app.keymap)).fg(theme.dim),
        Focus::Diff => Span::raw(diff_hint(&app.keymap)).fg(theme.dim),
        Focus::Scroll => Span::raw(SCROLL_HINT).fg(theme.scroll),
        Focus::Projects | Focus::Agents => Span::raw(sidebar_hint(&app.keymap)).fg(theme.dim),
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

pub fn draw_mismatch(frame: &mut Frame, daemon_version: &str, protocol_version: u32) {
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
        Line::from("y restart · n quit").fg(Color::DarkGray),
    ];
    let block = Block::default().borders(Borders::ALL).title(" lazyagents ");
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

pub fn popup_area(app: &App, screen: Rect) -> Option<Rect> {
    let popup = app.popup.as_ref()?;
    let (_, width, height, _) = popup_content(popup, app);
    Some(centered(screen, width, height))
}

fn popup_content(popup: &Popup, app: &App) -> (&'static str, u16, u16, Vec<Line<'static>>) {
    let theme = &app.theme;
    match popup {
        Popup::Help => {
            let lines = help_lines(&app.keymap);
            let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4;
            let height = lines.len() as u16 + 2;
            (" Help ", width, height, lines)
        }
        Popup::NewAgent(form) => {
            let lines = form_lines(form, app);
            (" New agent ", 56, lines.len() as u16 + 2, lines)
        }
        Popup::Rename { input, .. } => (
            " Rename ",
            60,
            5,
            vec![Line::from(format!("Name: {input}"))],
        ),
        Popup::AddProject { input } => (
            " Add project ",
            60,
            5,
            vec![Line::from(format!("Path: {input}"))],
        ),
        Popup::Confirm { prompt, action } => {
            let mut lines: Vec<Line> = prompt.lines().map(|l| Line::from(l.to_string())).collect();
            if !action.names_its_keys() {
                lines.push(Line::from(CONFIRM_HINT).fg(theme.dim));
            }
            let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4;
            (" Confirm ", width.max(50), lines.len() as u16 + 2, lines)
        }
    }
}

fn draw_popup(frame: &mut Frame, popup: &Popup, app: &App, screen: Rect) {
    let (title, width, height, lines) = popup_content(popup, app);
    let area = centered(screen, width, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(block(title, true, &app.theme)),
        area,
    );
}

fn help_lines(keymap: &Keymap) -> Vec<Line<'static>> {
    let sidebar: Vec<(String, String)> = SIDEBAR_HELP
        .iter()
        .map(|(action, text)| (keymap.describe(*action), text.to_string()))
        .collect();
    let mut pane = vec![(
        keymap.describe(Action::PaneLeave),
        "back to the sidebar".to_string(),
    )];
    if !keymap.direct_leave().is_empty() {
        pane.push((
            crate::keymap::join(keymap.direct_leave()),
            "back to the sidebar".to_string(),
        ));
    }
    pane.extend([
        (keymap.describe(Action::PaneNext), "next agent".to_string()),
        (
            keymap.describe(Action::PanePrevious),
            "previous agent".to_string(),
        ),
        (
            keymap.describe(Action::PaneScroll),
            "scroll the agent's history".to_string(),
        ),
        (
            keymap.describe(Action::PaneSendPrefix),
            format!("send {} to the agent", keymap.prefix()),
        ),
    ]);
    let width = sidebar
        .iter()
        .chain(&pane)
        .map(|(key, _)| key.chars().count())
        .max()
        .unwrap_or(0);
    let section = |title: &'static str, rows: Vec<(String, String)>| {
        let heading = Line::from(title).bold();
        let rows = rows
            .into_iter()
            .map(move |(key, action)| Line::from(format!("  {key:<width$}  {action}")));
        std::iter::once(heading).chain(rows).collect::<Vec<_>>()
    };
    let mut lines = section("Sidebar", sidebar);
    lines.push(Line::default());
    lines.extend(section("Agent pane", pane));
    lines
}

fn form_lines(form: &NewAgentForm, app: &App) -> Vec<Line<'static>> {
    let field = |focus: Field, text: String| {
        let line = Line::from(text);
        if form.field == focus {
            line.reversed()
        } else {
            line
        }
    };
    let mut lines = vec![
        field(Field::Name, format!("Name:    {}", form.name)),
        field(
            Field::Preset,
            format!("Preset:  ‹ {} ›", form.preset.label(&app.presets)),
        ),
    ];
    if form.preset == Preset::Custom {
        lines.push(field(Field::Command, format!("Command: {}", form.command)));
    }
    let toggle = if form.worktree { "[x]" } else { "[ ]" };
    lines.push(field(Field::Worktree, format!("Worktree: {toggle}")));
    if form.worktree {
        let base = if form.base.is_empty() && form.field != Field::Base {
            "(current branch)"
        } else {
            form.base.as_str()
        };
        lines.push(field(Field::Base, format!("Base:    {base}")));
    }
    lines.push(Line::default());
    lines.push(Line::from(FORM_HINT).fg(app.theme.dim));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{agent, loaded, loaded_with, project, worktree_agent};
    use crate::app::{App, Focus, Popup};
    use crate::config::Config;
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
        assert!(text.contains("alpha-repo 1!"), "{text}");
        assert!(text.contains("! api  needs input"), "{text}");
        assert!(text.contains("✓ docs  idle"), "{text}");
        assert!(text.contains(" api · generic · needs input "), "{text}");
        assert!(text.contains("status: needs input"), "{text}");
    }

    #[test]
    fn working_agents_animate_and_all_view_shows_projects() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "busy", Status::Working)]);
        assert!(app.animating());
        assert_eq!(glyph(Status::Working, 0, &Theme::default()).0, "⠋");
        assert_eq!(glyph(Status::Working, 1, &Theme::default()).0, "⠙");
        assert_eq!(glyph(Status::Working, 10, &Theme::default()).0, "⠋");
        let (text, _) = render(&app);
        assert!(
            text.contains("⠋ busy  working") && text.contains(" Agents "),
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
        assert!(!status.contains("g next !"), "{status}");
        let y = buffer.area.height - 1;
        assert_eq!(buffer[(0, y)].fg, Color::Red);
        assert_eq!(buffer[(12, y)].fg, Color::Red);
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
            status.starts_with("SCROLL · j/k ctrl-u/d g/G · q exit"),
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
        app.popup = Some(Popup::AddProject {
            input: "/x/repo".into(),
        });
        let (text, _) = render(&app);
        assert!(
            text.contains(" Add project ") && text.contains("Path: /x/repo"),
            "{text}"
        );
        app.popup = Some(Popup::Help);
        let (text, _) = render(&app);
        assert!(text.contains(" Help ") && text.contains("ctrl-]"), "{text}");
    }

    fn help_row(key: &str, action: &str) -> String {
        format!("  {key:<16}{action}")
    }

    #[test]
    fn help_lists_every_binding_at_100x30() {
        let mut app = App::new("/w".into(), "/bin/sh".into(), None, Config::default());
        app.popup = Some(Popup::Help);
        let (text, _) = render(&app);
        for (key, action) in [
            ("j/down", "move down in the focused box"),
            ("k/up", "move up in the focused box"),
            ("h/left/backtab", "focus Projects"),
            ("l/right", "focus Agents"),
            ("tab", "switch between Projects and Agents"),
            ("enter", "open the agent's pane"),
            ("n", "new agent"),
            ("x", "stop agent"),
            ("r", "restart agent"),
            ("d", "remove agent"),
            ("R", "rename agent"),
            ("a", "add project"),
            ("A", "all agents / this project"),
            ("g", "next agent needing input"),
            ("?", "help"),
            ("q/ctrl-c", "quit (agents keep running)"),
            ("Q", "stop the daemon and quit"),
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
            new_agent = ["N", "ctrl-n"]
            quit = "ctrl-x"
            add_project = "P"

            [keys.pane]
            prefix = "ctrl-a"
            leave = ["x"]
            "#,
        );
        let p = project("p");
        let (mut app, _) = loaded_with(config, vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        app.popup = Some(Popup::Help);
        let (text, _) = render(&app);
        for (key, action) in [
            ("N/ctrl-n", "new agent"),
            ("ctrl-x", "quit (agents keep running)"),
            ("ctrl-a x", "back to the sidebar"),
            ("ctrl-a j", "next agent"),
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
            status.starts_with("? help · ctrl-x quit · enter open · N new · x stop"),
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
            ("h/left/backtab", "focus Projects"),
        ] {
            assert_eq!(
                column(line_with(&text, action), action),
                start,
                "{key}\n{text}"
            );
        }
    }

    fn find(buffer: &Buffer, area: Rect, symbol: &str) -> (u16, u16) {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if buffer[(x, y)].symbol() == symbol {
                    return (x, y);
                }
            }
        }
        panic!("{symbol:?} not found in {area:?}");
    }

    #[test]
    fn theme_colours_apply() {
        let config = configured(
            r#"
            [theme]
            accent = "blue"
            attention = "magenta"
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

        let attention = find(&buffer, layout.agents, "!");
        assert_eq!(fg(attention), Color::Magenta);
        assert_eq!(fg((attention.0 + 2, attention.1)), Color::Magenta);
        assert_eq!(fg(find(&buffer, layout.agents, SPINNER[0])), Color::Cyan);
        assert_eq!(fg(find(&buffer, layout.agents, "✓")), Color::LightGreen);
        assert_eq!(fg(find(&buffer, layout.agents, "■")), Color::White);
        assert_eq!(fg(find(&buffer, layout.agents, "✗")), Color::Magenta);
        assert_eq!(fg(find(&buffer, layout.projects, "1")), Color::Magenta);
        assert_eq!(fg((0, layout.status.y)), Color::White);

        app.fail("boom");
        let (_, buffer) = render(&app);
        assert_eq!(buffer[(0, layout.status.y)].fg, Color::Magenta);
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
            field: Field::Name,
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
            !text.contains("files") && !text.contains("branch:"),
            "{text}"
        );
        app.apply_git_stat(plain.id, Ok(stat(3, 4)));
        let text = info(&app);
        assert!(text.contains("+3 −1 · 2 files"), "{text}");
        assert!(text.contains("4 uncommitted"), "{text}");
        assert!(!text.contains("branch:"), "{text}");

        let _ = app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        let text = info(&app);
        assert!(text.contains("branch: lazyagents/wt"), "{text}");
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
            row("branch: lazyagents/wt") < row("+10 −1 · 2 files"),
            "{text}"
        );
        assert!(row("+10 −1 · 2 files") < row("1 uncommitted"), "{text}");
        assert!(text.contains("status: idle"), "{text}");

        app.apply_git_stat(wt.id, Err("git failed".into()));
        let text = info(&app);
        assert!(text.contains("branch: lazyagents/wt"), "{text}");
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
        assert!(text.contains(" diff · a "), "{text}");
        let status = text.lines().last().expect("status line");
        assert!(status.contains("q/esc/D close"), "{status}");
        let _ = app.set_pane_size(73, 5);
        let _ = app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
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
                "status: idle",
                "branch: lazyagents/wt",
                "+4 −2 · 3 files",
                "1 uncommitted",
                "cmd: sh",
                "cwd: /s/worktrees/wt",
                "kind: generic",
            ],
            "{text}"
        );
    }

    #[test]
    fn mismatch_popup_text() {
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).expect("terminal");
        terminal
            .draw(|f| draw_mismatch(f, "0.1.0", 1))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let text = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
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
