//! The contents of `daemon.lock`: the holder's pid and, when known, its start time.

use crate::process::{self, Start};

/// Who wrote the lock file. `start` is the text recorded for `pid`: a [`Start`] in
/// decimal, or, from daemons that read it with `ps`, the `ps -o lstart=` text in the C
/// locale and UTC; `None` in the pid-only format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    pub start: Option<String>,
}

pub fn format(pid: u32, start: Option<Start>) -> String {
    match start {
        Some(start) => format!("{pid}\n{start}\n"),
        None => format!("{pid}\n"),
    }
}

pub fn parse(text: &str) -> Option<Holder> {
    let mut lines = text.lines();
    let pid = lines.next()?.trim().parse().ok()?;
    let start = lines
        .next()
        .map(str::trim)
        .filter(|start| !start.is_empty())
        .map(String::from);
    Some(Holder { pid, start })
}

/// True unless the holder recorded a start time that `pid` no longer has, as when the
/// pid was recycled after the holder died. An `lstart` text is compared to the second,
/// as precisely as `ps` printed it.
pub fn is_current(holder: &Holder) -> bool {
    holder
        .start
        .as_deref()
        .is_none_or(|recorded| match recorded.parse::<Start>() {
            Ok(start) => process::start(holder.pid) == Some(start),
            Err(_) => lstart_seconds(recorded)
                .is_some_and(|seconds| process::started_at(holder.pid) == Some(seconds)),
        })
}

/// The seconds since the Unix epoch of an `lstart` text in the C locale and UTC, such
/// as `Thu Oct 15 10:00:00 2026`.
fn lstart_seconds(text: &str) -> Option<u64> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let [_, month, day, time, year] = text.split_whitespace().collect::<Vec<_>>()[..] else {
        return None;
    };
    let month = MONTHS.iter().position(|name| *name == month)? as u64 + 1;
    let day: u64 = day.parse().ok()?;
    let year: u64 = year.parse().ok()?;
    let [hours, minutes, seconds] = time
        .split(':')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?[..]
    else {
        return None;
    };
    if year < 1970 || !(1..=31).contains(&day) || hours > 23 || minutes > 59 || seconds > 60 {
        return None;
    }
    let days = days_since_epoch(year, month, day);
    Some(days * 86_400 + hours * 3_600 + minutes * 60 + seconds)
}

/// Days from 1970-01-01 to a proleptic Gregorian date from 1970 on.
fn days_since_epoch(year: u64, month: u64, day: u64) -> u64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_and_without_a_start() {
        assert_eq!(
            parse(&format(42, Some(Start(1_792_058_400_123_456)))),
            Some(Holder {
                pid: 42,
                start: Some("1792058400123456".into())
            })
        );
        assert_eq!(
            parse("42\nThu Oct 16 09:00:00 2026\n"),
            Some(Holder {
                pid: 42,
                start: Some("Thu Oct 16 09:00:00 2026".into())
            })
        );
        assert_eq!(
            parse(&format(42, None)),
            Some(Holder {
                pid: 42,
                start: None
            })
        );
        assert_eq!(parse(""), None);
        assert_eq!(parse("x"), None);
    }

    #[test]
    fn the_current_process_is_current_and_a_wrong_start_is_not() {
        let pid = std::process::id();
        let start = process::start(pid).expect("our own start");
        assert!(is_current(&Holder {
            pid,
            start: Some(start.to_string())
        }));
        assert!(!is_current(&Holder {
            pid,
            start: Some(Start(start.0 + 1).to_string())
        }));
        assert!(is_current(&Holder { pid, start: None }));
    }

    #[test]
    fn an_lstart_from_ps_is_checked_to_the_second() {
        let pid = std::process::id();
        let output = std::process::Command::new("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .env("LC_ALL", "C")
            .env("TZ", "UTC0")
            .output()
            .expect("ps");
        let lstart = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert!(
            is_current(&Holder {
                pid,
                start: Some(lstart.clone())
            }),
            "{lstart:?}"
        );
        assert!(!is_current(&Holder {
            pid,
            start: Some("Mon Jan  1 00:00:00 1990".into())
        }));
        assert!(!is_current(&Holder {
            pid,
            start: Some("garbage".into())
        }));
    }

    #[test]
    fn lstart_texts_read_as_utc_seconds() {
        assert_eq!(lstart_seconds("Thu Jan  1 00:00:00 1970"), Some(0));
        assert_eq!(
            lstart_seconds("Tue Feb 29 23:59:59 2000"),
            Some(951_868_799)
        );
        assert_eq!(
            lstart_seconds("Thu Oct 15 10:00:00 2026"),
            Some(1_792_058_400)
        );
        assert_eq!(lstart_seconds("Thu Oct 15 10:00 2026"), None);
        assert_eq!(lstart_seconds("Thu Foo 15 10:00:00 2026"), None);
        assert_eq!(lstart_seconds("Thu Oct 15 10:00:00"), None);
        assert_eq!(lstart_seconds("Thu Jan  1 00:00:00 1969"), None);
    }
}
