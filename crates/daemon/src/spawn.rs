use std::collections::HashMap;
use std::path::{Path, PathBuf};

use protocol::{AgentKind, HookKind, StatePaths};
use serde_json::{Value, json};

use crate::store::AgentRecord;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Launch {
    Fresh,
    Resume,
}

pub struct SpawnContext<'a> {
    pub paths: &'a StatePaths,
    pub project_root: &'a Path,
    pub direnv: Option<&'a Path>,
}

pub fn spawn_spec(record: &AgentRecord, launch: Launch, ctx: &SpawnContext<'_>) -> SpawnSpec {
    let mut argv = record.command.clone();
    if argv.is_empty() {
        argv.push("claude".to_string());
    }
    if record.kind == AgentKind::Claude {
        if let Some(session) = record.session_id {
            let flag = if launch == Launch::Resume && record.resumable {
                "--resume"
            } else {
                "--session-id"
            };
            argv.push(flag.to_string());
            argv.push(session.to_string());
        }
        argv.push("--settings".to_string());
        argv.push(ctx.paths.hook_settings(record.id).display().to_string());
    }

    let (program, args) = match ctx.direnv.zip(find_envrc(&record.cwd, ctx.project_root)) {
        Some((direnv, dir)) => {
            let mut args = vec!["exec".to_string(), dir.display().to_string()];
            args.extend(argv);
            (direnv.display().to_string(), args)
        }
        None => {
            let program = argv.remove(0);
            (program, argv)
        }
    };

    let env = HashMap::from([
        ("LAZYAGENTS_AGENT_ID".to_string(), record.id.to_string()),
        (
            "LAZYAGENTS_SOCKET".to_string(),
            ctx.paths.socket().display().to_string(),
        ),
    ]);
    SpawnSpec {
        program,
        args,
        cwd: record.cwd.clone(),
        env,
    }
}

pub fn find_envrc(cwd: &Path, root: &Path) -> Option<PathBuf> {
    for dir in cwd.ancestors() {
        if dir.join(".envrc").is_file() {
            return Some(dir.to_path_buf());
        }
        if dir == root || !dir.starts_with(root) {
            break;
        }
    }
    None
}

pub fn hook_settings(exe: &Path) -> Value {
    let exe = shell_quote(&exe.display().to_string());
    let hooks: serde_json::Map<String, Value> = HookKind::ALL
        .into_iter()
        .map(|kind| {
            let command = format!("{exe} hook {}", kind.name());
            let entry =
                json!([{ "hooks": [{ "type": "command", "command": command, "timeout": 5 }] }]);
            (kind.name().to_string(), entry)
        })
        .collect();
    json!({ "hooks": hooks })
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

pub fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AgentId, ProjectId};
    use uuid::Uuid;

    fn record(kind: AgentKind, command: &[&str], cwd: &Path) -> AgentRecord {
        AgentRecord {
            id: AgentId::random(),
            project: ProjectId::random(),
            name: "a".into(),
            kind,
            command: command.iter().map(|s| s.to_string()).collect(),
            cwd: cwd.to_path_buf(),
            session_id: (kind == AgentKind::Claude).then(Uuid::new_v4),
            resumable: false,
        }
    }

    fn ctx<'a>(
        paths: &'a StatePaths,
        root: &'a Path,
        direnv: Option<&'a Path>,
    ) -> SpawnContext<'a> {
        SpawnContext {
            paths,
            project_root: root,
            direnv,
        }
    }

    #[test]
    fn generic_runs_its_command() {
        let paths = StatePaths::new("/s");
        let rec = record(
            AgentKind::Generic,
            &["sh", "-c", "echo hi"],
            Path::new("/p"),
        );
        let spec = spawn_spec(&rec, Launch::Fresh, &ctx(&paths, Path::new("/p"), None));
        assert_eq!(spec.program, "sh");
        assert_eq!(spec.args, vec!["-c", "echo hi"]);
        assert_eq!(spec.cwd, PathBuf::from("/p"));
        assert_eq!(spec.env["LAZYAGENTS_AGENT_ID"], rec.id.to_string());
        assert_eq!(spec.env["LAZYAGENTS_SOCKET"], "/s/daemon.sock");
    }

    #[test]
    fn claude_fresh_gets_session_id_and_settings() {
        let paths = StatePaths::new("/s");
        let rec = record(AgentKind::Claude, &[], Path::new("/p"));
        let spec = spawn_spec(&rec, Launch::Fresh, &ctx(&paths, Path::new("/p"), None));
        let session = rec.session_id.expect("session").to_string();
        let settings = paths.hook_settings(rec.id).display().to_string();
        assert_eq!(spec.program, "claude");
        assert_eq!(
            spec.args,
            vec![
                "--session-id",
                session.as_str(),
                "--settings",
                settings.as_str()
            ]
        );
    }

    #[test]
    fn claude_resume_needs_a_resumable_session() {
        let paths = StatePaths::new("/s");
        let mut rec = record(
            AgentKind::Claude,
            &["/bin/claude", "--model", "x"],
            Path::new("/p"),
        );
        let not_yet = spawn_spec(&rec, Launch::Resume, &ctx(&paths, Path::new("/p"), None));
        assert_eq!(not_yet.args[2], "--session-id");
        rec.resumable = true;
        let resumed = spawn_spec(&rec, Launch::Resume, &ctx(&paths, Path::new("/p"), None));
        assert_eq!(resumed.program, "/bin/claude");
        assert_eq!(resumed.args[..3], ["--model", "x", "--resume"]);
        assert_eq!(
            resumed.args[3],
            rec.session_id.expect("session").to_string()
        );
    }

    #[test]
    fn direnv_wraps_when_envrc_is_inside_the_project() {
        let root = tempfile::tempdir().expect("tempdir");
        let sub = root.path().join("sub");
        std::fs::create_dir_all(&sub).expect("mkdir");
        std::fs::write(root.path().join(".envrc"), "use flake\n").expect("envrc");
        let paths = StatePaths::new("/s");
        let rec = record(AgentKind::Generic, &["sh"], &sub);
        let spec = spawn_spec(
            &rec,
            Launch::Fresh,
            &ctx(&paths, root.path(), Some(Path::new("/nix/direnv"))),
        );
        assert_eq!(spec.program, "/nix/direnv");
        assert_eq!(
            spec.args,
            vec![
                "exec".to_string(),
                root.path().display().to_string(),
                "sh".to_string()
            ]
        );
    }

    #[test]
    fn envrc_outside_the_project_is_ignored() {
        let outer = tempfile::tempdir().expect("tempdir");
        let root = outer.path().join("project");
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(outer.path().join(".envrc"), "").expect("envrc");
        assert_eq!(find_envrc(&root, &root), None);
    }

    #[test]
    fn no_direnv_binary_means_no_wrapping() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join(".envrc"), "").expect("envrc");
        let paths = StatePaths::new("/s");
        let rec = record(AgentKind::Generic, &["sh"], root.path());
        let spec = spawn_spec(&rec, Launch::Fresh, &ctx(&paths, root.path(), None));
        assert_eq!(spec.program, "sh");
    }

    #[test]
    fn hook_settings_cover_every_event_with_quoted_exe() {
        let value = hook_settings(Path::new("/opt/my apps/lazyagents"));
        for kind in HookKind::ALL {
            let hook = &value["hooks"][kind.name()][0]["hooks"][0];
            assert_eq!(hook["type"], "command");
            assert_eq!(
                hook["command"],
                format!("'/opt/my apps/lazyagents' hook {}", kind.name())
            );
            assert_eq!(hook["timeout"], 5);
        }
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
