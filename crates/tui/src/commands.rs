//! User-defined commands (`[[commands]]`), run by the TUI against the selected agent or
//! project.

use std::collections::HashMap;

use toml::Value;

use crate::keymap::KeySpec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    Agent,
    Project,
}

/// Where a command's process runs and where its output goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// In the real terminal, with the TUI suspended until it exits.
    Terminal,
    /// As a new generic agent in the sidebar.
    Agent,
    /// In the background; its output is shown in a popup when it exits.
    Popup,
    /// In the background; the status bar says how it ended.
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prompt {
    Input {
        key: String,
        title: String,
        initial: String,
    },
    Menu {
        key: String,
        title: String,
        options: Vec<String>,
    },
    Confirm {
        title: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomCommand {
    pub key: Option<KeySpec>,
    pub description: String,
    pub command: String,
    pub context: Context,
    pub output: Output,
    /// The base of the agent's name for `output = "agent"`.
    pub name: String,
    pub prompts: Vec<Prompt>,
}

/// Parses one `[[commands]]` table, or returns the warnings that make it unusable.
pub fn parse(number: usize, value: Value) -> Result<(CustomCommand, Vec<String>), Vec<String>> {
    let label = format!("command #{number}");
    let Value::Table(table) = value else {
        return Err(vec![format!("{label}: expected a table")]);
    };
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    let (mut description, mut command) = (None, None);
    let mut parsed = CustomCommand {
        key: None,
        description: String::new(),
        command: String::new(),
        context: Context::Agent,
        output: Output::Terminal,
        name: "command".into(),
        prompts: Vec::new(),
    };
    for (setting, value) in table {
        let text = value.as_str().map(str::to_string);
        match setting.as_str() {
            "description" => description = text.filter(|d| !d.is_empty()),
            "command" => command = text.filter(|c| !c.trim().is_empty()),
            "key" => match text.as_deref().map(KeySpec::parse) {
                Some(Ok(key)) => parsed.key = Some(key),
                Some(Err(reason)) => errors.push(format!("{label}: key {reason}")),
                None => errors.push(format!("{label}: key must be a string")),
            },
            "context" => match text.as_deref() {
                Some("agent") => parsed.context = Context::Agent,
                Some("project") => parsed.context = Context::Project,
                _ => errors.push(format!("{label}: context must be agent or project")),
            },
            "output" => match text.as_deref() {
                Some("terminal") => parsed.output = Output::Terminal,
                Some("agent") => parsed.output = Output::Agent,
                Some("popup") => parsed.output = Output::Popup,
                Some("none") => parsed.output = Output::None,
                _ => errors.push(format!(
                    "{label}: output must be terminal, agent, popup or none"
                )),
            },
            "name" => match text.filter(|n| !n.trim().is_empty()) {
                Some(name) => parsed.name = name,
                None => errors.push(format!("{label}: name must be a non-empty string")),
            },
            "prompts" => match value {
                Value::Array(items) => {
                    for (index, item) in items.into_iter().enumerate() {
                        match prompt(item) {
                            Ok(prompt) => parsed.prompts.push(prompt),
                            Err(reason) => {
                                errors.push(format!("{label}: prompt #{}: {reason}", index + 1))
                            }
                        }
                    }
                }
                _ => errors.push(format!(
                    "{label}: prompts must be [[commands.prompts]] tables"
                )),
            },
            _ => warnings.push(format!("unknown setting {setting} in {label}")),
        }
    }
    match (description, command) {
        (Some(description), Some(command)) if errors.is_empty() => {
            parsed.description = description;
            parsed.command = command;
            Ok((parsed, warnings))
        }
        (description, command) => {
            if description.is_none() || command.is_none() {
                errors.push(format!("{label}: needs a description and a command"));
            }
            errors.extend(warnings);
            Err(errors)
        }
    }
}

fn prompt(value: Value) -> Result<Prompt, String> {
    let Value::Table(table) = value else {
        return Err("expected a table".into());
    };
    let text = |name: &str| table.get(name).and_then(Value::as_str).map(str::to_string);
    let title = text("title").unwrap_or_default();
    let key = || text("key").filter(|k| !k.is_empty()).ok_or("needs a key");
    match text("type").as_deref() {
        Some("input") => Ok(Prompt::Input {
            key: key()?,
            title,
            initial: text("initial").unwrap_or_default(),
        }),
        Some("menu") => {
            let options: Vec<String> = table
                .get("options")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|o| o.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if options.is_empty() {
                return Err("a menu needs options".into());
            }
            Ok(Prompt::Menu {
                key: key()?,
                title,
                options,
            })
        }
        Some("confirm") => Ok(Prompt::Confirm { title }),
        _ => Err("type must be input, menu or confirm".into()),
    }
}

/// Replaces each `{{name}}` with its value, shell-quoted, so a value can never run as
/// code. An unknown name is an error.
pub fn expand(template: &str, vars: &HashMap<String, String>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find("}}").ok_or("unclosed {{ in the command")?;
        let name = after[..end].trim();
        let value = vars
            .get(name)
            .ok_or_else(|| format!("unknown variable {name}"))?;
        out.push_str(&shell_quote(value));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> Result<(CustomCommand, Vec<String>), Vec<String>> {
        let table: toml::Table = text.parse().expect("toml");
        parse(1, Value::Table(table))
    }

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_minimal_command_runs_in_the_terminal_for_the_agent() {
        let (command, warnings) = parsed(
            r#"
            description = "lazygit"
            command = "lazygit"
            "#,
        )
        .expect("parses");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            command,
            CustomCommand {
                key: None,
                description: "lazygit".into(),
                command: "lazygit".into(),
                context: Context::Agent,
                output: Output::Terminal,
                name: "command".into(),
                prompts: Vec::new(),
            }
        );
    }

    #[test]
    fn every_setting_is_read() {
        let (command, warnings) = parsed(
            r#"
            key = "ctrl-g"
            description = "commit"
            command = "git commit -m {{form.message}}"
            context = "project"
            output = "popup"
            name = "committer"

            [[prompts]]
            type = "input"
            key = "message"
            title = "Message"
            initial = "wip"

            [[prompts]]
            type = "menu"
            key = "how"
            title = "How"
            options = ["fast", "slow"]

            [[prompts]]
            type = "confirm"
            title = "Sure?"
            "#,
        )
        .expect("parses");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(command.key, KeySpec::parse("ctrl-g").ok());
        assert_eq!(
            (command.context, command.output, command.name.as_str()),
            (Context::Project, Output::Popup, "committer")
        );
        assert_eq!(
            command.prompts,
            [
                Prompt::Input {
                    key: "message".into(),
                    title: "Message".into(),
                    initial: "wip".into()
                },
                Prompt::Menu {
                    key: "how".into(),
                    title: "How".into(),
                    options: vec!["fast".into(), "slow".into()]
                },
                Prompt::Confirm {
                    title: "Sure?".into()
                },
            ]
        );
    }

    #[test]
    fn unusable_commands_say_why() {
        for (text, expected) in [
            (
                "description = \"x\"",
                "command #1: needs a description and a command",
            ),
            (
                "command = \"x\"",
                "command #1: needs a description and a command",
            ),
            (
                "description = \"x\"\ncommand = \"x\"\noutput = \"loud\"",
                "command #1: output must be terminal, agent, popup or none",
            ),
            (
                "description = \"x\"\ncommand = \"x\"\ncontext = \"diff\"",
                "command #1: context must be agent or project",
            ),
            (
                "description = \"x\"\ncommand = \"x\"\nkey = \"ctrl-\"",
                "command #1: key",
            ),
            (
                "description = \"x\"\ncommand = \"x\"\n[[prompts]]\ntype = \"menu\"\nkey = \"k\"\ntitle = \"t\"",
                "command #1: prompt #1: a menu needs options",
            ),
            (
                "description = \"x\"\ncommand = \"x\"\n[[prompts]]\ntype = \"input\"\ntitle = \"t\"",
                "command #1: prompt #1: needs a key",
            ),
            (
                "description = \"x\"\ncommand = \"x\"\n[[prompts]]\ntype = \"slider\"",
                "command #1: prompt #1: type must be input, menu or confirm",
            ),
        ] {
            let warnings = parsed(text).expect_err(text);
            assert!(
                warnings.iter().any(|w| w.starts_with(expected)),
                "{text}: {warnings:?}"
            );
        }
    }

    #[test]
    fn unknown_settings_warn_but_keep_the_command() {
        let (command, warnings) = parsed(
            r#"
            description = "x"
            command = "x"
            colour = "red"
            "#,
        )
        .expect("parses");
        assert_eq!(command.description, "x");
        assert_eq!(warnings, ["unknown setting colour in command #1"]);
    }

    #[test]
    fn expansion_quotes_every_value() {
        let vars = vars(&[
            ("agent.cwd", "/w/it's here"),
            ("agent.title", "$(rm -rf ~)"),
        ]);
        assert_eq!(
            expand("cd {{agent.cwd}} && echo {{ agent.title }}", &vars),
            Ok(r"cd '/w/it'\''s here' && echo '$(rm -rf ~)'".to_string())
        );
        assert_eq!(expand("no variables", &vars), Ok("no variables".into()));
        assert_eq!(
            expand("echo {{agent.nope}}", &vars),
            Err("unknown variable agent.nope".into())
        );
        assert_eq!(
            expand("echo {{agent.cwd", &vars),
            Err("unclosed {{ in the command".into())
        );
    }
}
