use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use protocol::{AgentKind, CreateAgent, ProjectId, Request};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::commands::{self, Context, Output, Prompt};

use super::popup::{Edit, edit};
use super::{AgentView, App, DEFAULT_SIZE, Effect, MessageKind, PendingCreate, Popup, label, wrap};

/// A custom command waiting on its prompts; `vars` fills its template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRun {
    pub command: usize,
    pub step: usize,
    pub vars: HashMap<String, String>,
    pub cwd: PathBuf,
    pub project: ProjectId,
    pub input: String,
    pub choice: usize,
}

impl App {
    pub(super) fn command_key(&self, key: &KeyEvent) -> Option<usize> {
        self.commands
            .iter()
            .position(|c| c.key.is_some_and(|spec| spec.matches(key)))
    }

    pub(super) fn start_command(&mut self, index: usize) -> Vec<Effect> {
        let Some(command) = self.commands.get(index) else {
            return Vec::new();
        };
        let description = command.description.clone();
        let agent = self.selected_agent().cloned();
        let project = match &agent {
            Some(agent) if command.context == Context::Agent || self.view == AgentView::All => {
                self.projects.iter().find(|p| p.id == agent.project)
            }
            _ => self.selected_project(),
        }
        .cloned();
        let (cwd, project) = match (command.context, &agent, project) {
            (Context::Agent, Some(agent), Some(project)) => (agent.cwd.clone(), project),
            (Context::Project, _, Some(project)) => (project.path.clone(), project),
            (Context::Agent, ..) => {
                self.fail(format!("{description} needs a selected agent"));
                return Vec::new();
            }
            (Context::Project, ..) => {
                self.fail(format!("{description} needs a selected project"));
                return Vec::new();
            }
        };
        let mut vars = HashMap::from([
            ("project.name".to_string(), project.name.clone()),
            (
                "project.path".to_string(),
                project.path.display().to_string(),
            ),
        ]);
        if let Some(agent) = &agent {
            let worktree = agent.worktree.as_ref();
            vars.extend([
                ("agent.name".to_string(), agent.name.clone()),
                ("agent.title".to_string(), label(agent).to_string()),
                ("agent.cwd".to_string(), agent.cwd.display().to_string()),
                (
                    "agent.branch".to_string(),
                    worktree.map(|w| w.branch.clone()).unwrap_or_default(),
                ),
                (
                    "agent.base".to_string(),
                    worktree.map(|w| w.base.clone()).unwrap_or_default(),
                ),
            ]);
        }
        let run = CommandRun {
            command: index,
            step: 0,
            vars,
            cwd,
            project: project.id,
            input: String::new(),
            choice: 0,
        };
        self.next_prompt(run)
    }

    /// Opens the run's next prompt, or runs the command once every prompt is answered.
    fn next_prompt(&mut self, mut run: CommandRun) -> Vec<Effect> {
        let prompt = self
            .commands
            .get(run.command)
            .and_then(|c| c.prompts.get(run.step));
        match prompt {
            Some(prompt) => {
                run.input = match prompt {
                    Prompt::Input { initial, .. } => initial.clone(),
                    _ => String::new(),
                };
                run.choice = 0;
                self.popup = Some(Popup::CommandPrompt(run));
                Vec::new()
            }
            None => self.run_command(run),
        }
    }

    pub(super) fn on_command_prompt_key(
        &mut self,
        mut run: CommandRun,
        key: KeyEvent,
    ) -> Vec<Effect> {
        let Some(prompt) = self
            .commands
            .get(run.command)
            .and_then(|c| c.prompts.get(run.step))
            .cloned()
        else {
            return Vec::new();
        };
        let answer = match &prompt {
            Prompt::Input { key: name, .. } => match edit(&mut run.input, &key) {
                Edit::Submit => Some((name.clone(), run.input.clone())),
                Edit::Cancel => return Vec::new(),
                Edit::Continue => None,
            },
            Prompt::Menu {
                key: name, options, ..
            } => match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    run.choice = wrap(run.choice, 1, options.len());
                    None
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    run.choice = wrap(run.choice, -1, options.len());
                    None
                }
                KeyCode::Enter => Some((name.clone(), options[run.choice].clone())),
                KeyCode::Esc | KeyCode::Char('q') => return Vec::new(),
                _ => None,
            },
            Prompt::Confirm { .. } => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => Some((String::new(), String::new())),
                KeyCode::Char('n') | KeyCode::Esc => return Vec::new(),
                _ => None,
            },
        };
        match answer {
            Some((name, value)) => {
                if !name.is_empty() {
                    run.vars.insert(format!("form.{name}"), value);
                }
                run.step += 1;
                self.next_prompt(run)
            }
            None => {
                self.popup = Some(Popup::CommandPrompt(run));
                Vec::new()
            }
        }
    }

    fn run_command(&mut self, run: CommandRun) -> Vec<Effect> {
        let Some(command) = self.commands.get(run.command).cloned() else {
            return Vec::new();
        };
        let script = match commands::expand(&command.command, &run.vars) {
            Ok(script) => script,
            Err(error) => {
                self.fail(format!("{}: {error}", command.description));
                return Vec::new();
            }
        };
        let (title, cwd) = (command.description, run.cwd);
        match command.output {
            Output::Terminal => vec![Effect::RunInTerminal { title, script, cwd }],
            Output::Popup | Output::None => {
                let show = command.output == Output::Popup;
                if show {
                    self.inform(format!("running {title}…"));
                }
                vec![Effect::RunCommand {
                    title,
                    script,
                    cwd,
                    show,
                }]
            }
            Output::Agent => {
                let name = self.free_name_from(run.project, &command.name);
                let (cols, rows) = self.pane_size.unwrap_or(DEFAULT_SIZE);
                self.pending_select = Some(PendingCreate {
                    project: run.project,
                    name: name.clone(),
                    interactions: self.interactions,
                });
                let cwd = commands::shell_quote(&cwd.display().to_string());
                vec![Effect::Send(Request::CreateAgent(CreateAgent {
                    project: run.project,
                    name,
                    kind: AgentKind::Generic,
                    command: vec!["sh".into(), "-c".into(), format!("cd {cwd} && {script}")],
                    cols,
                    rows,
                    worktree: None,
                    prompt: None,
                    setup: None,
                }))]
            }
        }
    }

    pub(super) fn on_commands_key(&mut self, index: usize, key: KeyEvent) -> Vec<Effect> {
        let count = self.commands.len();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down if count > 0 => {
                self.popup = Some(Popup::Commands {
                    index: wrap(index, 1, count),
                });
            }
            KeyCode::Char('k') | KeyCode::Up if count > 0 => {
                self.popup = Some(Popup::Commands {
                    index: wrap(index, -1, count),
                });
            }
            KeyCode::Enter if index < count => return self.start_command(index),
            KeyCode::Esc | KeyCode::Char('q') => {}
            _ => self.popup = Some(Popup::Commands { index }),
        }
        Vec::new()
    }

    pub(super) fn on_output_key(
        &mut self,
        title: String,
        lines: Vec<String>,
        scroll: usize,
        key: KeyEvent,
    ) {
        let page = self.diff_page().max(2);
        let last = lines.len().saturating_sub(1);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let scroll = match (key.code, ctrl) {
            (KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q'), false) => return,
            (KeyCode::Char('j') | KeyCode::Down, false) => scroll + 1,
            (KeyCode::Char('k') | KeyCode::Up, false) => scroll.saturating_sub(1),
            (KeyCode::Char('d'), true) | (KeyCode::PageDown, _) => scroll + page / 2,
            (KeyCode::Char('u'), true) | (KeyCode::PageUp, _) => scroll.saturating_sub(page / 2),
            (KeyCode::Char('g'), false) => 0,
            (KeyCode::Char('G'), false) => last,
            _ => scroll,
        };
        self.popup = Some(Popup::Output {
            title,
            lines,
            scroll: scroll.min(last),
        });
    }

    /// `result` is the exit code and the combined stdout and stderr, or why the command
    /// could not run.
    pub fn command_finished(
        &mut self,
        title: &str,
        show: bool,
        result: Result<(i32, String), String>,
    ) {
        let (code, output) = match result {
            Ok(done) => done,
            Err(error) => {
                self.fail(format!("{title} failed: {error}"));
                return;
            }
        };
        if show && self.popup.is_none() {
            let title = match code {
                0 => title.to_string(),
                code => format!("{title} (exit {code})"),
            };
            self.popup = Some(Popup::Output {
                title,
                lines: output.lines().map(str::to_string).collect(),
                scroll: 0,
            });
            if self
                .shown_message(Instant::now())
                .is_some_and(|m| m.kind == MessageKind::Info)
            {
                self.message = None;
            }
            return;
        }
        let last = output.lines().rev().find(|l| !l.trim().is_empty());
        match (code, last) {
            (0, _) => self.inform(format!("{title}: done")),
            (code, Some(line)) => self.fail(format!("{title} failed (exit {code}): {line}")),
            (code, None) => self.fail(format!("{title} failed (exit {code})")),
        }
    }

    /// For a command run in the real terminal, which only reports a failure.
    pub fn terminal_command_finished(&mut self, title: &str, result: Result<i32, String>) {
        match result {
            Ok(0) => {}
            Ok(code) => self.fail(format!("{title} exited with {code}")),
            Err(error) => self.fail(format!("{title} failed: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{agent, loaded_with, project};
    use protocol::{AgentInfo, Status};
    use protocol::{AgentKind, CreateAgent};

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            let _ = app.on_key(key(c));
        }
    }

    fn with_commands(text: &str) -> (App, AgentInfo) {
        let config = crate::config::parse(text).expect("config");
        assert!(config.warnings.is_empty(), "{:?}", config.warnings);
        let p = project("p");
        let a = AgentInfo {
            worktree: Some(Box::new(protocol::WorktreeInfo {
                path: "/w/api".into(),
                branch: "lazyagents/api".into(),
                base: "main".into(),
            })),
            cwd: "/w/api".into(),
            ..agent(&p, "api", Status::Idle)
        };
        let (app, _) = loaded_with(config, vec![p], vec![a.clone()]);
        (app, a)
    }

    #[test]
    fn a_command_key_runs_its_command_in_the_terminal() {
        let (mut app, _) = with_commands(
            r#"
            [[commands]]
            key = "L"
            description = "lazygit"
            command = "lazygit -p {{agent.cwd}} # {{agent.branch}} {{project.name}}"
            "#,
        );
        assert_eq!(
            app.on_key(key('L')),
            vec![Effect::RunInTerminal {
                title: "lazygit".into(),
                script: "lazygit -p '/w/api' # 'lazyagents/api' 'p'".into(),
                cwd: "/w/api".into(),
            }]
        );
    }

    #[test]
    fn project_commands_run_in_the_project() {
        let (mut app, _) = with_commands(
            r#"
            [[commands]]
            key = "T"
            description = "tests"
            command = "make test"
            context = "project"
            output = "popup"
            "#,
        );
        assert_eq!(
            app.on_key(key('T')),
            vec![Effect::RunCommand {
                title: "tests".into(),
                script: "make test".into(),
                cwd: "/p/p".into(),
                show: true,
            }]
        );
    }

    #[test]
    fn project_commands_in_all_run_in_the_selected_agents_project() {
        let config = crate::config::parse(
            "[[commands]]\nkey = \"T\"\ndescription = \"tests\"\ncommand = \"make test\"\ncontext = \"project\"",
        )
        .expect("config");
        let (p, q) = (project("p"), project("q"));
        let agents = vec![agent(&p, "a", Status::Idle), agent(&q, "b", Status::Idle)];
        let (mut app, _) = loaded_with(config, vec![p, q], agents);
        let _ = app.on_key(key('A'));
        let _ = app.on_key(key('j'));
        let effects = app.on_key(key('T'));
        assert!(
            matches!(
                effects.as_slice(),
                [Effect::RunInTerminal { cwd, .. }] if cwd == std::path::Path::new("/p/q")
            ),
            "{effects:?}"
        );
    }

    #[test]
    fn an_agent_command_needs_an_agent() {
        let config = crate::config::parse(
            "[[commands]]\nkey = \"L\"\ndescription = \"lazygit\"\ncommand = \"lazygit\"",
        )
        .expect("config");
        let (mut app, _) = loaded_with(config, vec![project("p")], Vec::new());
        assert!(app.on_key(key('L')).is_empty());
        assert_eq!(
            app.current_message(Instant::now()),
            Some("lazygit needs a selected agent")
        );
    }

    #[test]
    fn prompts_collect_values_before_the_command_runs() {
        let (mut app, _) = with_commands(
            r#"
            [[commands]]
            key = "C"
            description = "commit"
            command = "git commit -m {{form.message}} --{{form.how}}"
            output = "none"

            [[commands.prompts]]
            type = "input"
            key = "message"
            title = "Message"
            initial = "wip: "

            [[commands.prompts]]
            type = "menu"
            key = "how"
            title = "How"
            options = ["quiet", "verbose"]

            [[commands.prompts]]
            type = "confirm"
            title = "Commit now?"
            "#,
        );
        assert!(app.on_key(key('C')).is_empty());
        typed(&mut app, "fix it");
        assert!(app.on_key(code(KeyCode::Enter)).is_empty());
        assert!(app.on_key(key('j')).is_empty());
        assert!(app.on_key(code(KeyCode::Enter)).is_empty());
        assert_eq!(
            app.on_key(key('y')),
            vec![Effect::RunCommand {
                title: "commit".into(),
                script: "git commit -m 'wip: fix it' --'verbose'".into(),
                cwd: "/w/api".into(),
                show: false,
            }]
        );
        assert_eq!(app.popup, None);
    }

    #[test]
    fn escape_abandons_a_command_at_any_prompt() {
        let (mut app, _) = with_commands(
            r#"
            [[commands]]
            key = "C"
            description = "commit"
            command = "git commit -m {{form.message}}"

            [[commands.prompts]]
            type = "input"
            key = "message"
            "#,
        );
        let _ = app.on_key(key('C'));
        typed(&mut app, "x");
        assert!(app.on_key(code(KeyCode::Esc)).is_empty());
        assert_eq!(app.popup, None);
    }

    #[test]
    fn an_agent_output_starts_a_generic_agent_in_the_directory() {
        let (mut app, a) = with_commands(
            r#"
            [[commands]]
            key = "W"
            description = "watch tests"
            command = "cargo watch -x test"
            output = "agent"
            name = "watch"
            "#,
        );
        let effects = app.on_key(key('W'));
        let [Effect::Send(Request::CreateAgent(create))] = effects.as_slice() else {
            panic!("{effects:?}")
        };
        assert_eq!(
            create,
            &CreateAgent {
                project: a.project,
                name: "watch-1".into(),
                kind: AgentKind::Generic,
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    "cd '/w/api' && cargo watch -x test".into()
                ],
                cols: create.cols,
                rows: create.rows,
                worktree: None,
                prompt: None,
                setup: None,
            }
        );
    }

    #[test]
    fn an_unknown_variable_runs_nothing() {
        let (mut app, _) = with_commands(
            "[[commands]]\nkey = \"L\"\ndescription = \"x\"\ncommand = \"echo {{agent.colour}}\"",
        );
        assert!(app.on_key(key('L')).is_empty());
        assert_eq!(
            app.current_message(Instant::now()),
            Some("x: unknown variable agent.colour")
        );
    }

    #[test]
    fn c_lists_the_commands_and_enter_runs_one() {
        let (mut app, _) = with_commands(
            r#"
            [[commands]]
            description = "first"
            command = "one"
            output = "none"

            [[commands]]
            key = "L"
            description = "second"
            command = "two"
            output = "none"
            "#,
        );
        let _ = app.on_key(key('c'));
        assert_eq!(app.popup, Some(Popup::Commands { index: 0 }));
        let _ = app.on_key(key('j'));
        let effects = app.on_key(code(KeyCode::Enter));
        assert!(
            matches!(effects.as_slice(), [Effect::RunCommand { script, .. }] if script == "two"),
            "{effects:?}"
        );
    }

    #[test]
    fn popup_output_opens_when_the_command_finishes() {
        let (mut app, _) = with_commands("");
        app.command_finished("status", true, Ok((0, "M a.rs\n?? b.rs\n".into())));
        assert_eq!(
            app.popup,
            Some(Popup::Output {
                title: "status".into(),
                lines: vec!["M a.rs".into(), "?? b.rs".into()],
                scroll: 0,
            })
        );
        let _ = app.on_key(code(KeyCode::Esc));
        assert_eq!(app.popup, None);
    }

    #[test]
    fn background_commands_report_how_they_ended() {
        let (mut app, _) = with_commands("");
        app.command_finished("fmt", false, Ok((0, String::new())));
        assert_eq!(app.current_message(Instant::now()), Some("fmt: done"));
        app.command_finished("fmt", false, Ok((2, "boom\nlast line\n".into())));
        assert_eq!(
            app.current_message(Instant::now()),
            Some("fmt failed (exit 2): last line")
        );
        app.command_finished("fmt", true, Err("sh: not found".into()));
        assert_eq!(
            app.current_message(Instant::now()),
            Some("fmt failed: sh: not found")
        );
    }
}
