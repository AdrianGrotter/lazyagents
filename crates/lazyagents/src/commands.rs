use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use client::{Client, VersionMismatch};
use protocol::{
    AgentInfo, AgentKind, CreateAgent, Project, Request, Response, SHUTDOWN_WAIT, StatePaths,
};

use crate::{Command, Kind, ProjectCommand};

const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 40;

pub async fn run(command: Command) -> anyhow::Result<()> {
    let paths = StatePaths::from_env()?;
    let mut stdout = io::stdout();
    if matches!(command, Command::Shutdown) {
        return shutdown(&paths, &mut stdout).await;
    }
    let exe = std::env::current_exe().context("locating the lazyagents binary")?;
    let mut client = Client::connect_or_spawn(&paths, &exe).await?;
    execute(&mut client, command, &std::env::current_dir, &mut stdout).await
}

/// Runs a command that talks to the daemon through `client` and writes its report to
/// `out`. `cwd` gives the caller's directory; only commands that need it ask.
pub async fn execute(
    client: &mut Client,
    command: Command,
    cwd: &dyn Fn() -> io::Result<PathBuf>,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    match command {
        Command::Ls => {
            let (projects, agents) = snapshot(client).await?;
            write!(out, "{}", format_ls(&projects, &agents))?;
        }
        Command::Project(ProjectCommand::Add { path, name }) => {
            let path = match path {
                Some(path) if path.is_absolute() || path.as_os_str().is_empty() => path,
                Some(path) => cwd()
                    .with_context(|| format!("resolving {}", path.display()))?
                    .join(path),
                None => cwd()?,
            };
            let path = std::path::absolute(&path)
                .with_context(|| format!("resolving {}", path.display()))?;
            let project = add_project(client, path, name).await?;
            writeln!(out, "added {} ({})", project.name, project.path.display())?;
        }
        Command::Project(ProjectCommand::Rm { project }) => {
            let (projects, _) = snapshot(client).await?;
            let project = resolve_project(&projects, &project)?;
            client
                .request(Request::RemoveProject { id: project.id })
                .await?;
            writeln!(out, "removed project {}", project.name)?;
        }
        Command::New {
            name,
            project,
            kind,
            prompt,
            command,
        } => {
            let (projects, _) = snapshot(client).await?;
            let project = match project {
                Some(query) => resolve_project(&projects, &query)?,
                None => {
                    let cwd = cwd()?.canonicalize()?;
                    match containing_project(&projects, &cwd) {
                        Some(project) => project,
                        None => add_project(client, cwd, None).await?,
                    }
                }
            };
            let kind = match (kind, command.is_empty()) {
                (Some(Kind::Claude), _) | (None, true) => AgentKind::Claude,
                (Some(Kind::Generic), _) | (None, false) => AgentKind::Generic,
            };
            let request = Request::CreateAgent(CreateAgent {
                project: project.id,
                name: name.clone(),
                kind,
                command,
                cols: DEFAULT_COLS,
                rows: DEFAULT_ROWS,
                worktree: None,
                prompt,
                setup: None,
            });
            let info = match client.request(request).await {
                Ok(response) => expect_agent(response)?,
                Err(e) => return Err(explain_kept_agent(client, &project, &name, e).await),
            };
            writeln!(out, "started {}", info.name)?;
        }
        Command::Send { agent, text } => {
            let info = find_agent(client, &agent).await?;
            let enter = protocol::Key::new(protocol::KeyCode::Enter, protocol::Modifiers::NONE);
            for event in [
                protocol::InputEvent::Paste(text),
                protocol::InputEvent::Key(enter),
            ] {
                client
                    .request(Request::Input { id: info.id, event })
                    .await?;
            }
            writeln!(out, "sent to {}", info.name)?;
        }
        Command::Kill { agent } => {
            let info = find_agent(client, &agent).await?;
            client.request(Request::KillAgent { id: info.id }).await?;
            writeln!(out, "stopped {}", info.name)?;
        }
        Command::Restart { agent } => {
            let info = find_agent(client, &agent).await?;
            let request = Request::RestartAgent {
                id: info.id,
                cols: DEFAULT_COLS,
                rows: DEFAULT_ROWS,
            };
            client.request(request).await?;
            writeln!(out, "restarted {}", info.name)?;
        }
        Command::Rm { agent } => {
            let info = find_agent(client, &agent).await?;
            client
                .request(Request::RemoveAgent {
                    id: info.id,
                    remove_worktree: false,
                    force: false,
                    delete_branch: false,
                })
                .await?;
            writeln!(out, "{}", removed_message(&info))?;
        }
        Command::Rename { agent, name } => {
            let info = find_agent(client, &agent).await?;
            let renamed = expect_agent(
                client
                    .request(Request::RenameAgent { id: info.id, name })
                    .await?,
            )?;
            writeln!(out, "renamed {} to {}", info.name, renamed.name)?;
        }
        Command::Daemon | Command::Hook { .. } | Command::Statusline | Command::Shutdown => {
            unreachable!("dispatched in main")
        }
    }
    Ok(())
}

pub async fn shutdown(paths: &StatePaths, out: &mut dyn Write) -> anyhow::Result<()> {
    let mut client = match Client::connect(paths).await {
        Ok(client) => client,
        Err(e) => {
            let Some(mismatch) = e.downcast_ref::<VersionMismatch>() else {
                writeln!(out, "daemon is not running")?;
                return Ok(());
            };
            client::stop_daemon(paths, SHUTDOWN_WAIT).with_context(|| {
                format!(
                    "stopping daemon {} (protocol {})",
                    mismatch.daemon_version, mismatch.protocol_version
                )
            })?;
            writeln!(out, "daemon stopped")?;
            return Ok(());
        }
    };
    let deadline = tokio::time::Instant::now() + SHUTDOWN_WAIT;
    tokio::time::timeout_at(deadline, client.request(Request::Shutdown))
        .await
        .map_err(|_| {
            anyhow!(
                "daemon did not answer the shutdown request within {}s",
                SHUTDOWN_WAIT.as_secs()
            )
        })??;
    let socket = paths.socket();
    while socket.exists() {
        if tokio::time::Instant::now() >= deadline {
            bail!("daemon did not stop within {}s", SHUTDOWN_WAIT.as_secs());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    writeln!(out, "daemon stopped")?;
    Ok(())
}

async fn snapshot(client: &mut Client) -> anyhow::Result<(Vec<Project>, Vec<AgentInfo>)> {
    let Response::Projects(projects) = client.request(Request::ListProjects).await? else {
        bail!("unexpected reply to ListProjects");
    };
    let Response::Agents(agents) = client.request(Request::ListAgents).await? else {
        bail!("unexpected reply to ListAgents");
    };
    Ok((projects, agents))
}

async fn add_project(
    client: &mut Client,
    path: PathBuf,
    name: Option<String>,
) -> anyhow::Result<Project> {
    match client.request(Request::AddProject { path, name }).await? {
        Response::Project(project) => Ok(project),
        other => bail!("unexpected reply to AddProject: {other:?}"),
    }
}

/// A command that cannot start still leaves its agent behind, so a retry under the same
/// name would be refused; the error says so and how to move on.
async fn explain_kept_agent(
    client: &mut Client,
    project: &Project,
    name: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    let message = error.to_string();
    let kept = match snapshot(client).await {
        Ok((_, agents)) => agents.iter().any(|a| {
            a.project == project.id && a.name == name && a.last_error.as_deref() == Some(&message)
        }),
        Err(_) => false,
    };
    if !kept {
        return error;
    }
    let target = format!("{}/{name}", project.name);
    anyhow!(
        "{message}\nagent {name} was kept; fix the command and run `lazyagents restart {target}`, \
         or remove it with `lazyagents rm {target}`"
    )
}

async fn find_agent(client: &mut Client, query: &str) -> anyhow::Result<AgentInfo> {
    let (projects, agents) = snapshot(client).await?;
    resolve_agent(&projects, &agents, query)
}

fn expect_agent(response: Response) -> anyhow::Result<AgentInfo> {
    match response {
        Response::Agent(info) => Ok(info),
        other => bail!("unexpected reply: {other:?}"),
    }
}

fn containing_project(projects: &[Project], cwd: &Path) -> Option<Project> {
    projects
        .iter()
        .filter(|p| cwd.starts_with(&p.path))
        .max_by_key(|p| p.path.components().count())
        .cloned()
}

fn resolve_project(projects: &[Project], query: &str) -> anyhow::Result<Project> {
    if let Some(project) = projects.iter().find(|p| p.name == query) {
        return Ok(project.clone());
    }
    let path = Path::new(query).canonicalize().ok();
    projects
        .iter()
        .find(|p| Some(&p.path) == path.as_ref())
        .cloned()
        .ok_or_else(|| anyhow!("no project named or located at {query}"))
}

fn resolve_agent(
    projects: &[Project],
    agents: &[AgentInfo],
    query: &str,
) -> anyhow::Result<AgentInfo> {
    if let Some(agent) = agents.iter().find(|a| a.id.to_string() == query) {
        return Ok(agent.clone());
    }
    if let Some((project, name)) = query.split_once('/') {
        let project = resolve_project(projects, project)?;
        return agents
            .iter()
            .find(|a| a.project == project.id && a.name == name)
            .cloned()
            .ok_or_else(|| anyhow!("no agent {name} in {}", project.name));
    }
    let by_name: Vec<&AgentInfo> = agents.iter().filter(|a| a.name == query).collect();
    match by_name.as_slice() {
        [one] => return Ok((*one).clone()),
        [] => {}
        _ => bail!("{query} is ambiguous; use <project>/{query}"),
    }
    if query.len() >= 4 && query.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        let by_prefix: Vec<&AgentInfo> = agents
            .iter()
            .filter(|a| a.id.to_string().starts_with(query))
            .collect();
        if let [one] = by_prefix.as_slice() {
            return Ok((*one).clone());
        }
    }
    bail!("no agent matches {query}")
}

/// `rm` keeps an agent's worktree and branch, so the message says where they are.
fn removed_message(info: &AgentInfo) -> String {
    match &info.worktree {
        Some(worktree) => format!(
            "removed {}; kept worktree {} (branch {})",
            info.name,
            worktree.path.display(),
            worktree.branch
        ),
        None => format!("removed {}", info.name),
    }
}

fn format_ls(projects: &[Project], agents: &[AgentInfo]) -> String {
    if projects.is_empty() {
        return "no projects yet; run lazyagents new <name> inside a repo\n".to_string();
    }
    let width = agents.iter().map(|a| a.name.len()).max().unwrap_or(0);
    let mut out = String::new();
    for project in projects {
        out.push_str(&format!("{}  {}\n", project.name, project.path.display()));
        for agent in agents.iter().filter(|a| a.project == project.id) {
            let command = if agent.command.is_empty() {
                "claude".to_string()
            } else {
                agent.command.join(" ")
            };
            let usage = agent
                .usage
                .as_ref()
                .map(|usage| usage.summary())
                .filter(|summary| !summary.is_empty())
                .map(|summary| format!("  ({summary})"))
                .unwrap_or_default();
            out.push_str(&format!(
                "  {:width$}  {:12}  {command}{usage}\n",
                agent.name,
                agent.status.label()
            ));
            if let Some(error) = &agent.last_error {
                out.push_str(&format!("    last error: {error}\n"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AgentId, ProjectId, Status};

    fn project(name: &str, path: &str) -> Project {
        Project {
            id: ProjectId::random(),
            name: name.into(),
            path: path.into(),
        }
    }

    fn agent(project: &Project, name: &str) -> AgentInfo {
        AgentInfo {
            id: AgentId::random(),
            project: project.id,
            name: name.into(),
            kind: AgentKind::Generic,
            command: vec!["sh".into()],
            cwd: project.path.clone(),
            status: Status::Idle,
            worktree: None,
            last_error: None,
            title: None,
            waiting_for: None,
            usage: None,
        }
    }

    #[test]
    fn agents_resolve_by_name_project_slash_name_and_prefix() {
        let a = project("a", "/a");
        let b = project("b", "/b");
        let agents = vec![agent(&a, "api"), agent(&a, "docs"), agent(&b, "api")];
        let projects = vec![a.clone(), b.clone()];
        assert!(
            resolve_agent(&projects, &agents, "api").is_err(),
            "ambiguous"
        );
        assert_eq!(
            resolve_agent(&projects, &agents, "b/api")
                .expect("slash")
                .id,
            agents[2].id
        );
        assert_eq!(
            resolve_agent(&projects, &agents, "docs").expect("name").id,
            agents[1].id
        );
        let prefix: String = agents[1].id.to_string().chars().take(8).collect();
        assert_eq!(
            resolve_agent(&projects, &agents, &prefix)
                .expect("prefix")
                .id,
            agents[1].id
        );
        assert!(resolve_agent(&projects, &agents, "nope").is_err());
    }

    #[test]
    fn innermost_project_contains_the_cwd() {
        let outer = project("outer", "/w");
        let inner = project("inner", "/w/inner");
        let projects = vec![outer, inner.clone()];
        assert_eq!(
            containing_project(&projects, Path::new("/w/inner/src")),
            Some(inner)
        );
        assert_eq!(containing_project(&projects, Path::new("/elsewhere")), None);
    }

    #[test]
    fn removing_a_worktree_agent_names_the_kept_worktree() {
        let p = project("p", "/git/p");
        let plain = agent(&p, "plain");
        assert_eq!(removed_message(&plain), "removed plain");
        let mut wt = agent(&p, "wt");
        wt.worktree = Some(Box::new(protocol::WorktreeInfo {
            path: "/s/worktrees/p-1234abcd/wt".into(),
            branch: "lazyagents/wt".into(),
            base: "main".into(),
        }));
        assert_eq!(
            removed_message(&wt),
            "removed wt; kept worktree /s/worktrees/p-1234abcd/wt (branch lazyagents/wt)"
        );
    }

    #[test]
    fn ls_adds_what_a_claude_status_line_reported() {
        let p = project("lazyagents", "/git/lazyagents");
        let mut a = agent(&p, "api");
        a.usage = Some(Box::new(protocol::Usage {
            model: Some("Opus".into()),
            cost_micros: Some(420_000),
            ..protocol::Usage::default()
        }));
        assert_eq!(
            format_ls(&[p], &[a]),
            "lazyagents  /git/lazyagents\n  api  idle          sh  (Opus · $0.42)\n"
        );
    }

    #[test]
    fn ls_shows_why_an_agent_failed_to_start() {
        let p = project("p", "/git/p");
        let mut a = agent(&p, "ghost");
        a.status = Status::Exited(127);
        a.last_error = Some("failed to start `nope`: not found".into());
        assert_eq!(
            format_ls(&[p], &[a]),
            "p  /git/p\n  ghost  exited (127)  sh\n    last error: failed to start `nope`: not found\n"
        );
    }

    #[test]
    fn ls_lists_projects_then_agents() {
        let p = project("lazyagents", "/git/lazyagents");
        let mut a = agent(&p, "api-refactor");
        a.status = Status::NeedsInput;
        let text = format_ls(&[p], &[a]);
        assert_eq!(
            text,
            "lazyagents  /git/lazyagents\n  api-refactor  needs input   sh\n"
        );
    }
}
