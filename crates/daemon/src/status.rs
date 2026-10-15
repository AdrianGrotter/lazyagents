use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use protocol::{AgentKind, HookKind, Status};

pub const ACTIVITY_WINDOW: Duration = Duration::from_secs(2);
pub const SETTLE_AFTER: Duration = Duration::from_secs(2);
pub const CLAUDE_SILENCE: Duration = Duration::from_secs(10);

pub fn on_hook(kind: HookKind) -> Status {
    match kind {
        HookKind::SessionStart | HookKind::Stop => Status::Idle,
        HookKind::UserPromptSubmit | HookKind::PreToolUse | HookKind::PostToolUse => {
            Status::Working
        }
        HookKind::Notification => Status::NeedsInput,
    }
}

pub fn generic_status(last_output: Option<Instant>, now: Instant) -> Status {
    match last_output {
        Some(at) if now.duration_since(at) < ACTIVITY_WINDOW => Status::Working,
        _ => Status::Idle,
    }
}

/// The status a running agent moves to from output timing alone, or `None` to keep it.
pub fn settle(
    kind: AgentKind,
    status: Status,
    started: Instant,
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
            if status == Status::Working && now.duration_since(quiet_since) >= CLAUDE_SILENCE {
                Status::Idle
            } else {
                status
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
        let settle_at = |now| settle(AgentKind::Generic, Status::Starting, started, None, now);
        assert_eq!(settle_at(before), None);
        assert_eq!(settle_at(after), Some(Status::Idle));
        assert_eq!(
            settle(AgentKind::Generic, Status::Idle, started, None, after),
            None
        );
    }

    #[test]
    fn generic_agent_with_output_follows_activity() {
        let started = Instant::now();
        let output = started + Duration::from_millis(100);
        let soon = output + Duration::from_millis(500);
        let later = output + ACTIVITY_WINDOW;
        let generic = |status, now| settle(AgentKind::Generic, status, started, Some(output), now);
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
    fn claude_needs_input_is_not_changed_by_silence() {
        let started = Instant::now();
        let late = started + CLAUDE_SILENCE * 3;
        for status in [Status::NeedsInput, Status::Starting, Status::Idle] {
            assert_eq!(settle(AgentKind::Claude, status, started, None, late), None);
            assert_eq!(
                settle(AgentKind::Claude, status, started, Some(started), late),
                None
            );
        }
    }

    #[test]
    fn stopped_and_exited_never_change() {
        let started = Instant::now();
        let late = started + CLAUDE_SILENCE * 3;
        for kind in [AgentKind::Generic, AgentKind::Claude] {
            for status in [Status::Stopped, Status::Exited(0), Status::Exited(137)] {
                assert_eq!(settle(kind, status, started, None, late), None);
                assert_eq!(settle(kind, status, started, Some(late), late), None);
            }
        }
    }

    #[test]
    fn exit_codes_and_signals() {
        assert_eq!(exit_code(ExitStatus::from_raw(3 << 8)), 3);
        assert_eq!(exit_code(ExitStatus::from_raw(9)), 128 + 9);
    }
}
