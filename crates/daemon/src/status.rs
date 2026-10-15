use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use protocol::{HookKind, Status};

pub const ACTIVITY_WINDOW: Duration = Duration::from_secs(2);

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
    fn exit_codes_and_signals() {
        assert_eq!(exit_code(ExitStatus::from_raw(3 << 8)), 3);
        assert_eq!(exit_code(ExitStatus::from_raw(9)), 128 + 9);
    }
}
