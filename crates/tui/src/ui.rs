use std::time::Instant;

use protocol::{AgentInfo, AgentKind, Color as ScreenColor, Screen, Status};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::app::{AgentView, App, Field, Focus, NewAgentForm, Popup, Preset};

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const SIDEBAR_HINT: &str = "enter open · n new · x stop · r restart · d remove · R rename · a add project · A all · g next ! · ? help · q quit";
const PANE_HINT: &str = "ctrl-] sidebar · alt-[ alt-] previous/next agent";
const FORM_HINT: &str = "tab field · ←/→ preset · enter start · esc cancel";
const CONFIRM_HINT: &str = "y/enter confirm · n/esc cancel";

const SIDEBAR_KEYS: [(&str, &str); 11] = [
    ("enter", "open"),
    ("n", "new"),
    ("x", "stop"),
    ("r", "restart"),
    ("d", "remove"),
    ("R", "rename"),
    ("a", "add project"),
    ("A", "all agents"),
    ("g", "next agent needing input"),
    ("?", "help"),
    ("q", "quit"),
];
const PANE_KEYS: [(&str, &str); 3] = [
    ("ctrl-]", "back to sidebar"),
    ("alt-[", "previous agent"),
    ("alt-]", "next agent"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Areas {
    pub projects: Rect,
    pub agents: Rect,
    pub info: Rect,
    pub pane: Rect,
    pub pane_inner: Rect,
    pub status: Rect,
}

pub fn areas(area: Rect, project_count: usize) -> Areas {
    let body_height = area.height.saturating_sub(1);
    let status = Rect::new(area.x, area.y + body_height, area.width, area.height.min(1));
    let sidebar_width = (area.width / 4).clamp(24, 40).min(area.width);
    let projects_height = (project_count.clamp(1, 6) as u16 + 2).min(body_height);
    let info_height = 6.min(body_height - projects_height);
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
    }
}

pub fn glyph(status: Status, tick: u64) -> (&'static str, Style) {
    match status {
        Status::Starting => {
            let symbol = if (tick / 5).is_multiple_of(2) {
                "◌"
            } else {
                "○"
            };
            (symbol, Style::new().dim())
        }
        Status::Working => (SPINNER[(tick % 10) as usize], Style::new().yellow()),
        Status::NeedsInput => ("!", Style::new().red().bold()),
        Status::Idle => ("✓", Style::new().green()),
        Status::Exited(0) => ("✗", Style::new().dim()),
        Status::Exited(_) => ("✗", Style::new().red()),
        Status::Stopped => ("■", Style::new().dark_gray()),
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

fn block(title: impl Into<Line<'static>>, focused: bool) -> Block<'static> {
    let border = if focused {
        Color::Green
    } else {
        Color::DarkGray
    };
    Block::bordered().title(title).border_style(border)
}

pub fn draw(frame: &mut Frame, app: &App, areas: &Areas, now: Instant, tick: u64) {
    draw_projects(frame, app, areas.projects);
    draw_agents(frame, app, areas.agents, tick);
    draw_info(frame, app, areas.info);
    draw_pane(frame, app, areas);
    draw_status(frame, app, areas.status, now);
    if let Some(popup) = &app.popup {
        draw_popup(frame, popup, frame.area());
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
    let block = block(" Projects ", app.focus == Focus::Projects);
    if app.projects.is_empty() {
        let open_sides = block.borders(Borders::TOP | Borders::BOTTOM);
        frame.render_widget(
            Paragraph::new("press a to add a project").block(open_sides),
            area,
        );
        return;
    }
    let items = app
        .projects
        .iter()
        .map(|project| {
            let attention = app.attention(project.id);
            let mut text = project.name.clone();
            if attention > 0 {
                text.push_str(&format!(" {attention}!"));
            }
            ListItem::new(text)
        })
        .collect();
    let (list, mut state) = selectable_list(items, app.project_index, block);
    frame.render_stateful_widget(list, area, &mut state);
}

fn agent_row(app: &App, agent: &AgentInfo, tick: u64) -> ListItem<'static> {
    let (symbol, style) = glyph(agent.status, tick);
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
        line.red()
    } else {
        line
    })
}

fn draw_agents(frame: &mut Frame, app: &App, area: Rect, tick: u64) {
    let title = match app.view {
        AgentView::Project => " Agents ",
        AgentView::All => " Agents · all ",
    };
    let block = block(title, app.focus == Focus::Agents);
    let agents = app.visible_agents();
    if agents.is_empty() {
        frame.render_widget(Paragraph::new("no agents — press n").block(block), area);
        return;
    }
    let items = agents.iter().map(|a| agent_row(app, a, tick)).collect();
    let (list, mut state) = selectable_list(items, app.agent_index, block);
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_info(frame: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<String> = match (app.selected_agent(), app.selected_project()) {
        (Some(agent), _) => {
            let kind = match agent.kind {
                AgentKind::Claude => "claude",
                AgentKind::Generic => "generic",
            };
            let command = if agent.command.is_empty() {
                "claude".to_string()
            } else {
                agent.command.join(" ")
            };
            vec![
                format!("kind: {kind}"),
                format!("status: {}", agent.status.label()),
                format!("cmd: {command}"),
                format!("cwd: {}", agent.cwd.display()),
            ]
        }
        (None, Some(project)) => vec![format!("path: {}", project.path.display())],
        (None, None) => Vec::new(),
    };
    let text: Vec<Line> = lines.into_iter().map(Line::from).collect();
    frame.render_widget(Paragraph::new(text).block(block(" Info ", false)), area);
}

fn draw_pane(frame: &mut Frame, app: &App, areas: &Areas) {
    let focused = app.focus == Focus::Pane;
    let agent = app.selected_agent();
    let title = match agent {
        Some(agent) => {
            let kind = match agent.kind {
                AgentKind::Claude => "claude",
                AgentKind::Generic => "generic",
            };
            format!(" {} · {kind} · {} ", agent.name, agent.status.label())
        }
        None => " lazyagents ".to_string(),
    };
    frame.render_widget(block(title, focused), areas.pane);
    if agent.is_none() {
        frame.render_widget(
            Paragraph::new("press n to start an agent"),
            areas.pane_inner,
        );
        return;
    }
    let cursor = render_screen(&app.screen, areas.pane_inner, frame.buffer_mut());
    if let (true, Some(position)) = (focused, cursor) {
        frame.set_cursor_position(position);
    }
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect, now: Instant) {
    let line = match app.current_message(now) {
        Some(message) => Line::from(message.to_string()).red(),
        None if app.focus == Focus::Pane => Line::from(PANE_HINT).dark_gray(),
        None => Line::from(SIDEBAR_HINT).dark_gray(),
    };
    frame.render_widget(Paragraph::new(line), area);
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

fn draw_popup(frame: &mut Frame, popup: &Popup, screen: Rect) {
    let (title, width, height, lines) = match popup {
        Popup::Help => (" Help ", 60, 18, help_lines()),
        Popup::NewAgent(form) => (" New agent ", 56, 8, form_lines(form)),
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
        Popup::Confirm { prompt, .. } => (
            " Confirm ",
            50,
            4,
            vec![
                Line::from(prompt.clone()),
                Line::from(CONFIRM_HINT).dark_gray(),
            ],
        ),
    };
    let area = centered(screen, width, height);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block(title, true)), area);
}

fn help_lines() -> Vec<Line<'static>> {
    let section = |title: &'static str, keys: &[(&'static str, &'static str)]| {
        let heading = Line::from(title).bold();
        let rows = keys
            .iter()
            .map(|(key, action)| Line::from(format!("  {key:<8}{action}")));
        std::iter::once(heading).chain(rows).collect::<Vec<_>>()
    };
    let mut lines = section("Sidebar", &SIDEBAR_KEYS);
    lines.extend(section("Agent pane", &PANE_KEYS));
    lines
}

fn form_lines(form: &NewAgentForm) -> Vec<Line<'static>> {
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
            format!("Preset:  ‹ {} ›", form.preset.label()),
        ),
    ];
    if form.preset == Preset::Custom {
        lines.push(field(Field::Command, format!("Command: {}", form.command)));
    }
    lines.push(Line::default());
    lines.push(Line::from(FORM_HINT).dark_gray());
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{agent, loaded, project};
    use crate::app::{App, Focus, Popup};
    use protocol::{Cell, Color as ScreenColor, Screen, Status, Style as ScreenStyle};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use std::time::Instant;

    fn render(app: &App) -> (String, Buffer) {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        let areas = areas(Rect::new(0, 0, 100, 30), app.projects.len());
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
        let app = App::new("/w".into(), "/bin/sh".into());
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
        assert_eq!(glyph(Status::Working, 0).0, "⠋");
        assert_eq!(glyph(Status::Working, 1).0, "⠙");
        assert_eq!(glyph(Status::Working, 10).0, "⠋");
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
        assert!(text.contains("n new · x stop"), "{text}");
        app.fail("boom happened");
        let (text, _) = render(&app);
        assert!(text.contains("boom happened"), "{text}");
        app.focus = Focus::Pane;
        app.message = None;
        let (text, _) = render(&app);
        assert!(text.contains("ctrl-] sidebar"), "{text}");
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

    #[test]
    fn new_agent_form_shows_command_only_for_custom() {
        use crate::app::{Field, NewAgentForm, Preset};
        let p = project("p");
        let (mut app, _) = loaded(vec![p], vec![]);
        let form = |preset| NewAgentForm {
            name: "api".into(),
            preset,
            command: "make".into(),
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
    fn layout_reserves_status_row_and_sidebar() {
        let a = areas(Rect::new(0, 0, 100, 30), 2);
        assert_eq!(a.status, Rect::new(0, 29, 100, 1));
        assert_eq!(a.projects.width, 25);
        assert_eq!(a.projects.height, 4);
        assert_eq!(a.info.height, 6);
        assert_eq!(a.pane.x, 25);
        assert_eq!(a.pane_inner, Rect::new(26, 1, 73, 27));
    }
}
