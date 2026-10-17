use std::ffi::OsString;
use std::path::{Path, PathBuf};

use protocol::AgentKind;
use ratatui::style::Color;
use toml::{Table, Value};

use crate::keymap::{Action, KeySpec, Keymap, Rebound, join};

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub keymap: Keymap,
    pub presets: Vec<PresetDef>,
    pub theme: Theme,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetDef {
    pub name: String,
    pub kind: AgentKind,
    pub command: Vec<String>,
}

pub const IDLE_GREEN: Color = Color::Rgb(0x43, 0x84, 0x40);
pub const NEEDS_PURPLE: Color = Color::Rgb(0x82, 0x59, 0xDD);
pub const ERROR_RED: Color = Color::Rgb(0xC9, 0x45, 0x3C);
pub const WORKING_YELLOW: Color = Color::Rgb(0xD2, 0x99, 0x22);
pub const DONE_BLUE: Color = Color::Rgb(0x2F, 0x81, 0xF7);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub accent: Color,
    pub attention: Color,
    pub error: Color,
    pub working: Color,
    pub idle: Color,
    pub done: Color,
    pub dim: Color,
    pub border: Color,
    pub scroll: Color,
    pub selected: Color,
    pub diff_add: Color,
    pub diff_remove: Color,
    pub diff_hunk: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            accent: Color::Green,
            attention: NEEDS_PURPLE,
            error: ERROR_RED,
            working: WORKING_YELLOW,
            idle: IDLE_GREEN,
            done: DONE_BLUE,
            dim: Color::DarkGray,
            border: Color::DarkGray,
            scroll: Color::Yellow,
            selected: Color::Blue,
            diff_add: Color::Green,
            diff_remove: Color::Red,
            diff_hunk: Color::Cyan,
        }
    }
}

const PANE_ACTIONS: [(&str, Action); 5] = [
    ("next", Action::PaneNext),
    ("previous", Action::PanePrevious),
    ("leave", Action::PaneLeave),
    ("scroll", Action::PaneScroll),
    ("send_prefix", Action::PaneSendPrefix),
];

const DIRECT_ACTIONS: [(&str, Action); 3] = [
    ("direct_next", Action::PaneNext),
    ("direct_previous", Action::PanePrevious),
    ("direct_leave", Action::PaneLeave),
];

pub fn default_path() -> Option<PathBuf> {
    default_path_from(
        std::env::var_os("LAZYAGENTS_CONFIG"),
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// `$LAZYAGENTS_CONFIG`, else `$XDG_CONFIG_HOME/lazyagents/config.toml`, else
/// `~/.config/lazyagents/config.toml`; empty values count as unset.
pub fn default_path_from(
    config: Option<OsString>,
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    let non_empty = |value: Option<OsString>| value.filter(|v| !v.is_empty()).map(PathBuf::from);
    non_empty(config)
        .or_else(|| non_empty(xdg_config_home).map(|xdg| xdg.join("lazyagents/config.toml")))
        .or_else(|| non_empty(home).map(|home| home.join(".config/lazyagents/config.toml")))
}

/// Never fails: a missing file gives the defaults, and anything unusable is left at its
/// default and reported in `warnings`.
pub fn load(path: Option<&Path>) -> Config {
    let Some(path) = path else {
        return Config::default();
    };
    let failed = |problem: String| Config {
        warnings: vec![format!("{}: {problem}", path.display())],
        ..Config::default()
    };
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).unwrap_or_else(failed),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(error) => failed(error.to_string()),
    }
}

/// Fails only on TOML syntax errors.
pub fn parse(text: &str) -> Result<Config, String> {
    let root: Table = text.parse().map_err(|error: toml::de::Error| {
        let line = error
            .span()
            .map_or(1, |span| text[..span.start].lines().count().max(1));
        format!("line {line}: {}", error.message().trim())
    })?;
    let mut config = Config::default();
    for (name, value) in root {
        match (name.as_str(), value) {
            ("keys", Value::Table(table)) => config.apply_keys(table),
            ("presets", Value::Array(items)) => {
                for (index, item) in items.into_iter().enumerate() {
                    if let Some(preset) = config.preset(index + 1, item) {
                        config.presets.push(preset);
                    }
                }
            }
            ("theme", Value::Table(table)) => config.apply_theme(table),
            ("keys" | "theme", _) => config.warn(format!("{name}: expected a table")),
            ("presets", _) => config.warn("presets: expected [[presets]] tables"),
            (_, value) => config.warn(unknown(&name, &value)),
        }
    }
    Ok(config)
}

fn unknown(path: &str, value: &Value) -> String {
    if value.is_table() {
        format!("unknown table [{path}]")
    } else {
        format!("unknown setting {path}")
    }
}

fn pane_action_name(action: Action) -> String {
    action_name(&PANE_ACTIONS, action)
}

fn direct_action_name(action: Action) -> String {
    action_name(&DIRECT_ACTIONS, action)
}

fn action_name(names: &[(&str, Action)], action: Action) -> String {
    names
        .iter()
        .find(|(_, a)| *a == action)
        .map_or_else(String::new, |(n, _)| n.to_string())
}

impl Config {
    fn warn(&mut self, warning: impl Into<String>) {
        self.warnings.push(warning.into());
    }

    fn apply_keys(&mut self, table: Table) {
        for (scope, value) in table {
            match (scope.as_str(), value) {
                ("sidebar", Value::Table(table)) => self.apply_sidebar_keys(table),
                ("pane", Value::Table(table)) => self.apply_pane_keys(table),
                ("sidebar" | "pane", _) => self.warn(format!("keys.{scope}: expected a table")),
                (_, value) => self.warn(unknown(&format!("keys.{scope}"), &value)),
            }
        }
    }

    fn apply_sidebar_keys(&mut self, table: Table) {
        let mut bindings = Vec::new();
        for (name, value) in table {
            let path = format!("keys.sidebar.{name}");
            let action = Value::String(name.clone())
                .try_into::<Action>()
                .ok()
                .filter(|action| !action.is_pane());
            let Some(action) = action else {
                self.warn(format!("unknown action {path}"));
                continue;
            };
            if let Some(keys) = self.key_list(&path, &value) {
                bindings.push((action, keys));
            }
        }
        for rebound in self.keymap.rebind_sidebar(bindings) {
            let name = |action: Action| {
                toml::Value::try_from(action)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default()
            };
            self.warn_rebound("keys.sidebar", rebound, name);
        }
    }

    fn apply_pane_keys(&mut self, table: Table) {
        let mut bindings = Vec::new();
        let mut direct = Vec::new();
        let mut prefix = None;
        for (name, value) in table {
            let path = format!("keys.pane.{name}");
            let keys = |config: &mut Config| config.key_list(&path, &value);
            if name == "prefix" {
                match keys(self).as_deref() {
                    Some([key]) => prefix = Some(*key),
                    Some(_) => self.warn(format!("{path}: expected one key")),
                    None => {}
                }
            } else if let Some((_, action)) = DIRECT_ACTIONS.iter().find(|(n, _)| *n == name) {
                if let Some(keys) = keys(self) {
                    direct.push((*action, keys));
                }
            } else if let Some((_, action)) = PANE_ACTIONS.iter().find(|(n, _)| *n == name) {
                if let Some(keys) = keys(self) {
                    bindings.push((*action, keys));
                }
            } else {
                self.warn(format!("unknown action {path}"));
            }
        }
        for rebound in self.keymap.rebind_direct(direct) {
            self.warn_rebound("keys.pane", rebound, direct_action_name);
        }
        if let Some(prefix) = prefix {
            self.keymap.set_prefix(prefix);
            if !bindings.iter().any(|(a, _)| *a == Action::PaneSendPrefix) {
                bindings.push((Action::PaneSendPrefix, vec![prefix]));
            }
        }
        for rebound in self.keymap.rebind_pane(bindings) {
            self.warn_rebound("keys.pane", rebound, pane_action_name);
        }
        let prefix = self.keymap.prefix();
        if let Some(action) = self.keymap.unbind_direct(prefix) {
            let name = direct_action_name(action);
            self.warn(format!("keys.pane.{name}: {prefix} is the prefix"));
        }
        if self.keymap.keys(Action::PaneLeave).is_empty()
            && self.keymap.direct_keys(Action::PaneLeave).is_empty()
        {
            let defaults = Keymap::defaults().keys(Action::PaneLeave);
            self.warn(format!(
                "keys.pane: no way to leave the pane; leave restored to {}",
                join(&defaults)
            ));
            for rebound in self.keymap.rebind_pane(vec![(Action::PaneLeave, defaults)]) {
                self.warn_rebound("keys.pane", rebound, pane_action_name);
            }
        }
    }

    fn warn_rebound(&mut self, scope: &str, rebound: Rebound, name: impl Fn(Action) -> String) {
        let (key, lost, won) = (rebound.key, name(rebound.lost), name(rebound.won));
        self.warn(format!(
            "{scope}: {key} is bound to both {lost} and {won}; {won} wins"
        ));
    }

    /// A key or a list of keys; `None`, with a warning, if any of them is unusable.
    fn key_list(&mut self, path: &str, value: &Value) -> Option<Vec<KeySpec>> {
        let texts: Option<Vec<&str>> = match value {
            Value::String(text) => Some(vec![text.as_str()]),
            Value::Array(items) => items.iter().map(Value::as_str).collect(),
            _ => None,
        };
        let Some(texts) = texts else {
            self.warn(format!("{path}: expected a key or a list of keys"));
            return None;
        };
        let mut keys = Vec::new();
        for text in texts {
            match KeySpec::parse(text) {
                Ok(key) if keys.contains(&key) => {}
                Ok(key) => keys.push(key),
                Err(error) => {
                    self.warn(format!("{path}: {error}"));
                    return None;
                }
            }
        }
        Some(keys)
    }

    fn preset(&mut self, number: usize, value: Value) -> Option<PresetDef> {
        let label = format!("preset #{number}");
        let Value::Table(table) = value else {
            self.warn(format!("{label}: expected a table"));
            return None;
        };
        let (mut name, mut command, mut kind) = (None, None, AgentKind::Generic);
        for (setting, value) in table {
            match setting.as_str() {
                "name" => name = value.as_str().filter(|n| !n.is_empty()).map(str::to_string),
                "command" => {
                    command = value
                        .as_array()
                        .and_then(|items| {
                            items
                                .iter()
                                .map(|item| item.as_str().map(str::to_string))
                                .collect::<Option<Vec<_>>>()
                        })
                        .filter(|command| !command.is_empty());
                }
                "kind" => match value.as_str() {
                    Some("claude") => kind = AgentKind::Claude,
                    Some("generic") => kind = AgentKind::Generic,
                    _ => self.warn(format!("{label}: kind must be \"claude\" or \"generic\"")),
                },
                _ => self.warn(format!("unknown setting {setting} in {label}")),
            }
        }
        match (name, command) {
            (Some(name), Some(command)) => Some(PresetDef {
                name,
                kind,
                command,
            }),
            _ => {
                self.warn(format!("{label}: needs a name and a command"));
                None
            }
        }
    }

    fn apply_theme(&mut self, table: Table) {
        for (name, value) in table {
            let theme = &mut self.theme;
            let slot = match name.as_str() {
                "accent" => &mut theme.accent,
                "attention" => &mut theme.attention,
                "error" => &mut theme.error,
                "working" => &mut theme.working,
                "idle" => &mut theme.idle,
                "done" => &mut theme.done,
                "dim" => &mut theme.dim,
                "border" => &mut theme.border,
                "scroll" => &mut theme.scroll,
                "selected" => &mut theme.selected,
                "diff_add" => &mut theme.diff_add,
                "diff_remove" => &mut theme.diff_remove,
                "diff_hunk" => &mut theme.diff_hunk,
                _ => {
                    self.warn(format!("unknown setting theme.{name}"));
                    continue;
                }
            };
            match colour(&value) {
                Some(colour) => *slot = colour,
                None => self.warn(format!("theme.{name}: unknown colour {value}")),
            }
        }
    }
}

/// A colour name, `#rrggbb`, or a 0-255 palette index as a number or a string.
fn colour(value: &Value) -> Option<Color> {
    match value {
        Value::String(text) => text.parse().ok(),
        Value::Integer(index) => u8::try_from(*index).ok().map(Color::Indexed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Preset;
    use crate::keymap::Action;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn load_text(text: &str) -> Config {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).expect("write config");
        load(Some(&path))
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn warned(config: &Config, needle: &str) -> bool {
        config.warnings.iter().any(|w| w.contains(needle))
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        for config in [load(Some(&path)), load(None)] {
            assert!(config.warnings.is_empty(), "{:?}", config.warnings);
            assert!(config.presets.is_empty());
            assert_eq!(config.theme, Theme::default());
            assert_eq!(config.keymap, Keymap::defaults());
        }
        assert!(!path.exists());
    }

    #[test]
    fn keys_override_only_named_actions() {
        let config = load_text(
            r#"
            [keys.sidebar]
            new_agent = "N"
            quit = ["ctrl-x", "f10"]

            [keys.pane]
            prefix = "ctrl-a"
            leave = ["x"]
            direct_leave = []
            "#,
        );
        assert!(config.warnings.is_empty(), "{:?}", config.warnings);
        let keymap = &config.keymap;
        assert_eq!(keymap.sidebar(&key('N')), Some(Action::NewAgent));
        assert_eq!(keymap.sidebar(&key('n')), None);
        assert_eq!(keymap.sidebar(&ctrl('x')), Some(Action::Quit));
        assert_eq!(
            keymap.sidebar(&KeyEvent::new(KeyCode::F(10), KeyModifiers::NONE)),
            Some(Action::Quit)
        );
        assert_eq!(keymap.sidebar(&key('q')), None);
        assert_eq!(keymap.sidebar(&ctrl('c')), None);
        assert_eq!(keymap.sidebar(&key('x')), Some(Action::Stop));
        assert_eq!(keymap.describe(Action::Down), "j/down");

        assert!(keymap.is_prefix(&ctrl('a')));
        assert!(!keymap.is_prefix(&ctrl('q')));
        assert_eq!(keymap.after_prefix(&key('x')), Some(Action::PaneLeave));
        assert_eq!(keymap.after_prefix(&key('q')), None);
        assert_eq!(
            keymap.after_prefix(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            None
        );
        assert_eq!(keymap.after_prefix(&key('j')), Some(Action::PaneNext));
        assert_eq!(
            keymap.after_prefix(&ctrl('a')),
            Some(Action::PaneSendPrefix)
        );
        assert_eq!(keymap.direct(&ctrl(']')), None);
        assert_eq!(keymap.direct(&ctrl('5')), None);
    }

    #[test]
    fn direct_next_and_previous_are_unbound_by_default() {
        let keymap = Keymap::defaults();
        assert!(keymap.direct_keys(Action::PaneNext).is_empty());
        assert!(keymap.direct_keys(Action::PanePrevious).is_empty());
        assert_eq!(keymap.direct(&ctrl('j')), None);
        assert_eq!(keymap.direct(&ctrl('k')), None);
    }

    #[test]
    fn direct_next_and_previous_bind_keys_without_the_prefix() {
        let config = load_text(
            r#"
            [keys.pane]
            direct_next = "ctrl-j"
            direct_previous = ["ctrl-k", "ctrl-q"]
            "#,
        );
        let keymap = &config.keymap;
        assert_eq!(keymap.direct(&ctrl('j')), Some(Action::PaneNext));
        assert_eq!(keymap.direct(&ctrl('k')), Some(Action::PanePrevious));
        assert_eq!(keymap.direct(&ctrl('q')), None);
        assert_eq!(keymap.direct(&ctrl(']')), Some(Action::PaneLeave));
        assert_eq!(
            config.warnings,
            vec!["keys.pane.direct_previous: ctrl-q is the prefix".to_string()]
        );
    }

    #[test]
    fn a_direct_key_moves_between_direct_actions_with_a_warning() {
        let config = load_text(
            r#"
            [keys.pane]
            direct_next = "ctrl-]"
            "#,
        );
        let keymap = &config.keymap;
        assert_eq!(keymap.direct(&ctrl(']')), Some(Action::PaneNext));
        assert_eq!(keymap.direct(&ctrl('5')), Some(Action::PaneLeave));
        assert_eq!(
            config.warnings,
            vec![
                "keys.pane: ctrl-] is bound to both direct_leave and direct_next; direct_next wins"
                    .to_string()
            ]
        );
    }

    #[test]
    fn unbinding_every_way_out_of_the_pane_restores_leave() {
        let config = load_text(
            r#"
            [keys.pane]
            leave = []
            direct_leave = []
            "#,
        );
        assert_eq!(
            config.warnings,
            vec!["keys.pane: no way to leave the pane; leave restored to q/esc".to_string()]
        );
        let keymap = &config.keymap;
        assert_eq!(keymap.after_prefix(&key('q')), Some(Action::PaneLeave));
        assert_eq!(
            keymap.after_prefix(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(Action::PaneLeave)
        );
        assert!(keymap.direct_keys(Action::PaneLeave).is_empty());
    }

    #[test]
    fn duplicate_bindings_warn_and_the_last_wins() {
        let config = load_text(
            r#"
            [keys.sidebar]
            stop = "d"
            rename = "z"
            restart = "z"

            [keys.pane]
            next = "q"
            direct_leave = ["ctrl-q", "ctrl-]"]
            "#,
        );
        let keymap = &config.keymap;
        assert_eq!(keymap.sidebar(&key('d')), Some(Action::Stop));
        assert_eq!(keymap.describe(Action::Remove), "unbound");
        assert!(
            warned(
                &config,
                "keys.sidebar: d is bound to both remove and stop; stop wins"
            ),
            "{:?}",
            config.warnings
        );
        assert_eq!(keymap.sidebar(&key('z')), Some(Action::Restart));
        assert!(
            warned(
                &config,
                "keys.sidebar: z is bound to both rename and restart; restart wins"
            ),
            "{:?}",
            config.warnings
        );
        assert_eq!(keymap.after_prefix(&key('q')), Some(Action::PaneNext));
        assert_eq!(keymap.describe(Action::PaneLeave), "ctrl-q esc");
        assert!(
            warned(
                &config,
                "keys.pane: q is bound to both leave and next; next wins"
            ),
            "{:?}",
            config.warnings
        );
        assert!(keymap.is_prefix(&ctrl('q')));
        assert_eq!(keymap.direct(&ctrl('q')), None);
        assert_eq!(keymap.direct(&ctrl(']')), Some(Action::PaneLeave));
        assert!(
            warned(&config, "keys.pane.direct_leave: ctrl-q is the prefix"),
            "{:?}",
            config.warnings
        );
        assert_eq!(config.warnings.len(), 4, "{:?}", config.warnings);
    }

    #[test]
    fn unknown_entries_warn() {
        let config = load_text(
            r#"
            colour = "red"

            [keybindings]
            quit = "x"

            [keys.sidebar]
            frobnicate = "z"
            pane_next = "z"

            [keys.pane]
            jump = "z"

            [keys.popup]
            confirm = "y"

            [theme]
            glow = "red"

            [[presets]]
            name = "codex"
            command = ["codex"]
            shell = true
            "#,
        );
        for expected in [
            "unknown setting colour",
            "unknown table [keybindings]",
            "unknown action keys.sidebar.frobnicate",
            "unknown action keys.sidebar.pane_next",
            "unknown action keys.pane.jump",
            "unknown table [keys.popup]",
            "unknown setting theme.glow",
            "unknown setting shell in preset #1",
        ] {
            assert!(
                warned(&config, expected),
                "{expected}: {:?}",
                config.warnings
            );
        }
        assert_eq!(config.warnings.len(), 8, "{:?}", config.warnings);
        assert_eq!(config.keymap, Keymap::defaults());
        assert_eq!(config.theme, Theme::default());
        assert_eq!(config.presets.len(), 1);
    }

    #[test]
    fn bad_key_strings_warn_and_keep_defaults() {
        let config = load_text(
            r#"
            [keys.sidebar]
            new_agent = "hyper-n"
            stop = ["x", "ctrl-"]
            help = 3

            [keys.pane]
            prefix = ["ctrl-a", "ctrl-b"]
            leave = "Esc"
            "#,
        );
        for expected in [
            "keys.sidebar.new_agent: \"hyper-n\"",
            "keys.sidebar.stop: \"ctrl-\"",
            "keys.sidebar.help: expected a key or a list of keys",
            "keys.pane.prefix: expected one key",
            "keys.pane.leave: \"Esc\"",
        ] {
            assert!(
                warned(&config, expected),
                "{expected}: {:?}",
                config.warnings
            );
        }
        assert_eq!(config.warnings.len(), 5, "{:?}", config.warnings);
        assert_eq!(config.keymap, Keymap::defaults());
    }

    #[test]
    fn syntax_errors_warn_with_the_line_and_keep_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[theme]\naccent = \"blue\"\nbroken = \n").expect("write");
        let config = load(Some(&path));
        assert_eq!(config.warnings.len(), 1, "{:?}", config.warnings);
        let warning = &config.warnings[0];
        assert!(
            warning.starts_with(&format!("{}: line 3: ", path.display())),
            "{warning}"
        );
        assert_eq!(config.theme, Theme::default());
    }

    #[test]
    fn presets_append_after_builtins() {
        let config = load_text(
            r#"
            [[presets]]
            name = "codex"
            command = ["codex", "--full-auto"]

            [[presets]]
            name = "opus"
            kind = "claude"
            command = ["claude", "--model", "opus"]

            [[presets]]
            name = "broken"
            "#,
        );
        assert_eq!(
            config.presets,
            vec![
                PresetDef {
                    name: "codex".into(),
                    kind: AgentKind::Generic,
                    command: vec!["codex".into(), "--full-auto".into()],
                },
                PresetDef {
                    name: "opus".into(),
                    kind: AgentKind::Claude,
                    command: vec!["claude".into(), "--model".into(), "opus".into()],
                },
            ]
        );
        assert!(
            warned(&config, "preset #3: needs a name and a command"),
            "{:?}",
            config.warnings
        );
        let labels: Vec<&str> = Preset::all(config.presets.len())
            .into_iter()
            .map(|preset| preset.label(&config.presets))
            .collect();
        assert_eq!(labels, ["claude", "shell", "custom", "codex", "opus"]);
    }

    #[test]
    fn theme_parses_names_hex_and_indexed() {
        let config = load_text(
            r##"
            [theme]
            accent = "blue"
            attention = "#ff8800"
            working = 208
            idle = "light-green"
            dim = "Magenta"
            border = "12"
            "##,
        );
        assert!(config.warnings.is_empty(), "{:?}", config.warnings);
        assert_eq!(
            config.theme,
            Theme {
                accent: Color::Blue,
                attention: Color::Rgb(0xff, 0x88, 0x00),
                working: Color::Indexed(208),
                idle: Color::LightGreen,
                dim: Color::Magenta,
                border: Color::Indexed(12),
                ..Theme::default()
            }
        );

        let config = load_text(
            r#"
            [theme]
            accent = "purple"
            working = 300
            idle = true
            "#,
        );
        for expected in [
            "theme.accent: unknown colour \"purple\"",
            "theme.working: unknown colour 300",
            "theme.idle: unknown colour true",
        ] {
            assert!(
                warned(&config, expected),
                "{expected}: {:?}",
                config.warnings
            );
        }
        assert_eq!(config.theme, Theme::default());
    }

    #[test]
    fn default_path_prefers_env_then_xdg() {
        let some = |s: &str| Some(OsString::from(s));
        assert_eq!(
            default_path_from(some("/c/lazy.toml"), some("/xdg"), some("/home/u")),
            Some(PathBuf::from("/c/lazy.toml"))
        );
        assert_eq!(
            default_path_from(None, some("/xdg"), some("/home/u")),
            Some(PathBuf::from("/xdg/lazyagents/config.toml"))
        );
        assert_eq!(
            default_path_from(some(""), some(""), some("/home/u")),
            Some(PathBuf::from("/home/u/.config/lazyagents/config.toml"))
        );
        assert_eq!(default_path_from(None, None, None), None);
        assert_eq!(default_path_from(None, None, some("")), None);
    }
}
