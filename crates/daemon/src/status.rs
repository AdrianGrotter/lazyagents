use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use protocol::{AgentKind, HookKind, KeyCode, Status};

pub const ACTIVITY_WINDOW: Duration = Duration::from_secs(2);
pub const SETTLE_AFTER: Duration = Duration::from_secs(2);
pub const CLAUDE_SILENCE: Duration = Duration::from_secs(10);
pub const CLAUDE_STARTUP_SILENCE: Duration = Duration::from_secs(30);

pub fn on_hook(kind: HookKind) -> Status {
    match kind {
        HookKind::SessionStart | HookKind::Stop | HookKind::StopFailure => Status::Idle,
        HookKind::UserPromptSubmit | HookKind::PreToolUse | HookKind::PostToolUse => {
            Status::Working
        }
        HookKind::Notification | HookKind::PermissionRequest => Status::NeedsInput,
    }
}

/// Whether a Claude screen shows the workspace-trust dialog, which Claude shows before it
/// runs any hook, whatever the line wrapping.
pub fn awaits_trust(screen: &str) -> bool {
    let squeezed: String = screen.chars().filter(|c| !c.is_whitespace()).collect();
    squeezed.contains("Yes,Itrustthisfolder") || squeezed.contains("Doyoutrustthefiles")
}

/// The keys that answer the trust dialog on `screen` with yes: arrows from the option the
/// `❯` cursor is on to the yes option, then enter. `None` when the screen shows no cursor on
/// an option or no yes option.
pub fn trust_keys(screen: &str) -> Option<Vec<KeyCode>> {
    let options: Vec<(bool, &str)> = screen.lines().filter_map(trust_option).collect();
    let cursor = options.iter().position(|(selected, _)| *selected)?;
    let yes = options
        .iter()
        .position(|(_, label)| label.starts_with("Yes,"))?;
    let step = if yes > cursor {
        KeyCode::Down
    } else {
        KeyCode::Up
    };
    let mut keys = vec![step; yes.abs_diff(cursor)];
    keys.push(KeyCode::Enter);
    Some(keys)
}

/// Whether `line` starts a dialog option, as (carries the cursor, label). A wrapped
/// option's later lines start with neither `Yes,` nor `No,`, so they are not options.
fn trust_option(line: &str) -> Option<(bool, &str)> {
    let line = line.trim_start();
    let (selected, rest) = match line.strip_prefix('❯') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, line),
    };
    let label = match rest.split_once(". ") {
        Some((number, label)) if number.chars().all(|c| c.is_ascii_digit()) => label,
        _ => rest,
    };
    (label.starts_with("Yes,") || label.starts_with("No,")).then_some((selected, label))
}

pub fn generic_status(last_output: Option<Instant>, now: Instant) -> Status {
    match last_output {
        Some(at) if now.duration_since(at) < ACTIVITY_WINDOW => Status::Working,
        _ => Status::Idle,
    }
}

/// The status a running agent moves to from output timing alone, or `None` to keep it.
/// `status_since` is when the agent entered `status` or, for a Claude agent, last
/// reported a hook.
pub fn settle(
    kind: AgentKind,
    status: Status,
    started: Instant,
    status_since: Instant,
    last_output: Option<Instant>,
    now: Instant,
) -> Option<Status> {
    if !status.is_running() {
        return None;
    }
    let next = match kind {
        AgentKind::Generic if last_output.is_some() => generic_status(last_output, now),
        AgentKind::Generic => {
            if status == Status::Starting && now.duration_since(started) >= SETTLE_AFTER {
                Status::Idle
            } else {
                status
            }
        }
        AgentKind::Claude => {
            let quiet_since = last_output.map_or(started, |at| at.max(started));
            let quiet = now.duration_since(quiet_since.max(status_since));
            match status {
                Status::Working if quiet >= CLAUDE_SILENCE => Status::Idle,
                Status::Starting if quiet >= CLAUDE_STARTUP_SILENCE => Status::Idle,
                _ => status,
            }
        }
    };
    (next != status).then_some(next)
}

pub fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_map_to_statuses() {
        assert_eq!(on_hook(HookKind::SessionStart), Status::Idle);
        assert_eq!(on_hook(HookKind::UserPromptSubmit), Status::Working);
        assert_eq!(on_hook(HookKind::PreToolUse), Status::Working);
        assert_eq!(on_hook(HookKind::PostToolUse), Status::Working);
        assert_eq!(on_hook(HookKind::Notification), Status::NeedsInput);
        assert_eq!(on_hook(HookKind::Stop), Status::Idle);
        assert_eq!(on_hook(HookKind::PermissionRequest), Status::NeedsInput);
        assert_eq!(on_hook(HookKind::StopFailure), Status::Idle);
    }

    #[test]
    fn generic_agents_work_while_output_is_recent() {
        let now = Instant::now();
        assert_eq!(generic_status(None, now), Status::Idle);
        assert_eq!(generic_status(Some(now), now), Status::Working);
        assert_eq!(
            generic_status(Some(now - Duration::from_secs(3)), now),
            Status::Idle
        );
    }

    #[test]
    fn silent_generic_agent_settles_to_idle() {
        let started = Instant::now();
        let before = started + SETTLE_AFTER - Duration::from_millis(1);
        let after = started + SETTLE_AFTER;
        let settle_at = |now| {
            settle(
                AgentKind::Generic,
                Status::Starting,
                started,
                started,
                None,
                now,
            )
        };
        assert_eq!(settle_at(before), None);
        assert_eq!(settle_at(after), Some(Status::Idle));
        assert_eq!(
            settle(
                AgentKind::Generic,
                Status::Idle,
                started,
                started,
                None,
                after
            ),
            None
        );
    }

    #[test]
    fn generic_agent_with_output_follows_activity() {
        let started = Instant::now();
        let output = started + Duration::from_millis(100);
        let soon = output + Duration::from_millis(500);
        let later = output + ACTIVITY_WINDOW;
        let generic = |status, now| {
            settle(
                AgentKind::Generic,
                status,
                started,
                started,
                Some(output),
                now,
            )
        };
        assert_eq!(generic(Status::Starting, soon), Some(Status::Working));
        assert_eq!(generic(Status::Working, soon), None);
        assert_eq!(generic(Status::Working, later), Some(Status::Idle));
        assert_eq!(generic(Status::Idle, later), None);
        assert_eq!(generic(Status::Idle, soon), Some(Status::Working));
    }

    #[test]
    fn claude_working_goes_idle_after_silence() {
        let started = Instant::now();
        let claude = |last_output, now| {
            settle(
                AgentKind::Claude,
                Status::Working,
                started,
                started,
                last_output,
                now,
            )
        };
        assert_eq!(
            claude(None, started + CLAUDE_SILENCE - Duration::from_millis(1)),
            None
        );
        assert_eq!(claude(None, started + CLAUDE_SILENCE), Some(Status::Idle));
        let output = started + Duration::from_secs(5);
        assert_eq!(claude(Some(output), started + CLAUDE_SILENCE), None);
        assert_eq!(
            claude(Some(output), output + CLAUDE_SILENCE),
            Some(Status::Idle)
        );
    }

    #[test]
    fn claude_working_from_a_late_hook_is_not_cut_short() {
        let started = Instant::now();
        let output = started + Duration::from_secs(1);
        let prompt = started + Duration::from_secs(60);
        let claude = |now| {
            settle(
                AgentKind::Claude,
                Status::Working,
                started,
                prompt,
                Some(output),
                now,
            )
        };
        assert_eq!(claude(prompt + Duration::from_millis(33)), None);
        assert_eq!(
            claude(prompt + CLAUDE_SILENCE - Duration::from_millis(1)),
            None
        );
        assert_eq!(claude(prompt + CLAUDE_SILENCE), Some(Status::Idle));
    }

    #[test]
    fn claude_needs_input_is_not_changed_by_silence() {
        let started = Instant::now();
        let late = started + CLAUDE_SILENCE * 3;
        for status in [Status::NeedsInput, Status::Idle] {
            assert_eq!(
                settle(AgentKind::Claude, status, started, started, None, late),
                None
            );
            assert_eq!(
                settle(
                    AgentKind::Claude,
                    status,
                    started,
                    started,
                    Some(started),
                    late
                ),
                None
            );
        }
    }

    #[test]
    fn silent_claude_start_settles_to_idle() {
        let started = Instant::now();
        let claude = |last_output, now| {
            settle(
                AgentKind::Claude,
                Status::Starting,
                started,
                started,
                last_output,
                now,
            )
        };
        assert_eq!(
            claude(
                None,
                started + CLAUDE_STARTUP_SILENCE - Duration::from_millis(1)
            ),
            None
        );
        assert_eq!(
            claude(None, started + CLAUDE_STARTUP_SILENCE),
            Some(Status::Idle)
        );
        let output = started + Duration::from_secs(5);
        assert_eq!(claude(Some(output), started + CLAUDE_STARTUP_SILENCE), None);
        assert_eq!(
            claude(Some(output), output + CLAUDE_STARTUP_SILENCE),
            Some(Status::Idle)
        );
    }

    #[test]
    fn stopped_and_exited_never_change() {
        let started = Instant::now();
        let late = started + CLAUDE_SILENCE * 3;
        for kind in [AgentKind::Generic, AgentKind::Claude] {
            for status in [Status::Stopped, Status::Exited(0), Status::Exited(137)] {
                assert_eq!(settle(kind, status, started, started, None, late), None);
                assert_eq!(
                    settle(kind, status, started, started, Some(late), late),
                    None
                );
            }
        }
    }

    #[test]
    fn the_trust_dialog_is_recognised_however_it_wraps() {
        let dialog = " Quick safety check: Is this a project you created or one you trust?\n\n \
                      ❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm";
        assert!(awaits_trust(dialog));
        assert!(awaits_trust("   Yes, I trust\n this folder"));
        assert!(awaits_trust("Do you trust the files in this folder?"));
        assert!(!awaits_trust("✻ Welcome to Claude Code!\n> "));
        assert!(!awaits_trust("direnv: loading ~/p/.envrc"));
    }

    #[test]
    fn trusting_moves_the_cursor_down_from_no_to_yes() {
        let dialog = " Quick safety check: Is this a project you created or one you trust?\n\n \
                      ❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm";
        assert_eq!(
            trust_keys(dialog),
            Some(vec![KeyCode::Down, KeyCode::Enter])
        );
    }

    #[test]
    fn trusting_with_the_cursor_on_yes_only_confirms() {
        let dialog = " Quick safety check\n\n   No, exit\n ❯ Yes, I trust this folder\n";
        assert_eq!(trust_keys(dialog), Some(vec![KeyCode::Enter]));
    }

    #[test]
    fn trusting_moves_the_cursor_up_when_yes_is_listed_first() {
        let dialog = " Quick safety check\n\n   Yes, I trust this folder\n ❯ No, exit\n";
        assert_eq!(trust_keys(dialog), Some(vec![KeyCode::Up, KeyCode::Enter]));
        let dialog = " Quick safety check\n\n ❯ Yes, I trust this folder\n   No, exit\n";
        assert_eq!(trust_keys(dialog), Some(vec![KeyCode::Enter]));
    }

    #[test]
    fn trusting_counts_options_not_wrapped_lines() {
        let dialog = " Quick safety\n check\n\n ❯ No,\n exit\n   Yes, I trust\n this folder\n";
        assert_eq!(
            trust_keys(dialog),
            Some(vec![KeyCode::Down, KeyCode::Enter])
        );
        let dialog = "   Yes, I\n trust this\n folder\n ❯ No, exit\n";
        assert_eq!(trust_keys(dialog), Some(vec![KeyCode::Up, KeyCode::Enter]));
    }

    #[test]
    fn trusting_reads_numbered_options() {
        let dialog =
            " Do you trust the files in this folder?\n\n ❯ 1. Yes, proceed\n   2. No, exit\n";
        assert_eq!(trust_keys(dialog), Some(vec![KeyCode::Enter]));
        let dialog = " ❯ 1. No, exit\n   2. Yes, I trust this folder\n";
        assert_eq!(
            trust_keys(dialog),
            Some(vec![KeyCode::Down, KeyCode::Enter])
        );
    }

    #[test]
    fn trusting_needs_a_cursor_on_an_option_and_a_yes_option() {
        assert_eq!(
            trust_keys("   No, exit\n   Yes, I trust this folder\n"),
            None
        );
        assert_eq!(trust_keys(" ❯ No, exit\n"), None);
        assert_eq!(
            trust_keys("   Yes, I trust this folder\n   No, exit\n ❯ Something else\n"),
            None
        );
        assert_eq!(trust_keys("✻ Welcome to Claude Code!\n❯ "), None);
    }

    #[test]
    fn exit_codes_and_signals() {
        assert_eq!(exit_code(ExitStatus::from_raw(3 << 8)), 3);
        assert_eq!(exit_code(ExitStatus::from_raw(9)), 128 + 9);
    }
}
