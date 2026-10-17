//! Notifications and the window title, written to the terminal the TUI runs in.

use std::io::Write;

use crate::config::NotifyMethod;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Bel,
    Osc9,
    Osc777,
}

/// `auto` picks OSC 9 in terminals known to show it as a desktop notification, and a
/// bell everywhere else, including inside tmux, which swallows OSC 9 but flags a bell.
pub fn resolve(method: NotifyMethod, env: impl Fn(&str) -> Option<String>) -> Option<Method> {
    match method {
        NotifyMethod::Off => None,
        NotifyMethod::Bel => Some(Method::Bel),
        NotifyMethod::Osc9 => Some(Method::Osc9),
        NotifyMethod::Osc777 => Some(Method::Osc777),
        NotifyMethod::Auto if env("TMUX").is_some() => Some(Method::Bel),
        NotifyMethod::Auto => match env("TERM_PROGRAM").as_deref() {
            Some("iTerm.app" | "WezTerm" | "ghostty") => Some(Method::Osc9),
            _ => Some(Method::Bel),
        },
    }
}

pub fn sequence(method: Method, body: &str) -> String {
    match method {
        Method::Bel => "\x07".to_string(),
        Method::Osc9 => format!("\x1b]9;{}\x07", printable(body)),
        Method::Osc777 => format!(
            "\x1b]777;notify;lazyagents;{}\x07",
            printable(body).replace(';', " ")
        ),
    }
}

pub fn title_sequence(title: &str) -> String {
    format!("\x1b]2;{}\x07", printable(title))
}

fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// Saves the outer terminal's title, so `POP_TITLE` can put it back on exit.
pub const PUSH_TITLE: &str = "\x1b[22;0t";
pub const POP_TITLE: &str = "\x1b[23;0t";

pub fn write(out: &mut impl Write, sequence: &str) {
    let _ = out.write_all(sequence.as_bytes());
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    #[test]
    fn auto_uses_osc9_where_it_shows_and_a_bell_elsewhere() {
        for program in ["iTerm.app", "WezTerm", "ghostty"] {
            assert_eq!(
                resolve(NotifyMethod::Auto, env(&[("TERM_PROGRAM", program)])),
                Some(Method::Osc9),
                "{program}"
            );
        }
        assert_eq!(
            resolve(
                NotifyMethod::Auto,
                env(&[("TERM_PROGRAM", "Apple_Terminal")])
            ),
            Some(Method::Bel)
        );
        assert_eq!(
            resolve(
                NotifyMethod::Auto,
                env(&[
                    ("TERM_PROGRAM", "ghostty"),
                    ("TMUX", "/tmp/tmux-1/default,1,0")
                ])
            ),
            Some(Method::Bel)
        );
        assert_eq!(resolve(NotifyMethod::Auto, env(&[])), Some(Method::Bel));
    }

    #[test]
    fn a_configured_method_is_used_as_is() {
        let inside_tmux = env(&[("TMUX", "x")]);
        assert_eq!(
            resolve(NotifyMethod::Osc9, &inside_tmux),
            Some(Method::Osc9)
        );
        assert_eq!(
            resolve(NotifyMethod::Osc777, &inside_tmux),
            Some(Method::Osc777)
        );
        assert_eq!(resolve(NotifyMethod::Bel, &inside_tmux), Some(Method::Bel));
        assert_eq!(resolve(NotifyMethod::Off, &inside_tmux), None);
    }

    #[test]
    fn sequences_carry_the_body_without_control_characters() {
        assert_eq!(sequence(Method::Bel, "api is done"), "\x07");
        assert_eq!(
            sequence(Method::Osc9, "api\x1b] is\x07 done"),
            "\x1b]9;api] is done\x07"
        );
        assert_eq!(
            sequence(Method::Osc777, "a;b needs input"),
            "\x1b]777;notify;lazyagents;a b needs input\x07"
        );
        assert_eq!(
            title_sequence("lazyagents · 1 needs input"),
            "\x1b]2;lazyagents · 1 needs input\x07"
        );
    }
}
