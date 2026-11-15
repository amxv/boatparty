//! `runs.jsonl`: one JSON object per finished invocation, appended.
//!
//! The file never holds credentials or command input; it records the command
//! line as the user gave it.

use std::fs::{self, OpenOptions};
use std::io::Write;

use serde_json::Value;

use crate::error::AppError;
use crate::paths::Paths;

/// Lines kept when the file is trimmed; it is trimmed when twice this long.
const KEEP_LINES: usize = 5_000;

/// Append one record. Failure to record never fails the command.
pub fn append(paths: &Paths, record: &Value) -> Result<(), AppError> {
    let path = paths.runs_file();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|error| AppError::io(format!("cannot create {}", dir.display()), error))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| AppError::io(format!("cannot open {}", path.display()), error))?;
    // One write per record, so concurrent appenders never interleave a line.
    let line = format!("{record}\n");
    file.write_all(line.as_bytes())
        .map_err(|error| AppError::io(format!("cannot write {}", path.display()), error))
}

/// The last `limit` records, oldest first. Unreadable lines are skipped.
pub fn read(paths: &Paths, limit: usize) -> Result<Vec<Value>, AppError> {
    let path = paths.runs_file();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(AppError::io(
                format!("cannot read {}", path.display()),
                error,
            ));
        }
    };
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    if lines.len() > KEEP_LINES * 2 {
        let kept = lines[lines.len() - KEEP_LINES..].join("\n") + "\n";
        let temp = path.with_extension(format!("jsonl.{}.tmp", std::process::id()));
        if fs::write(&temp, kept).is_ok() {
            let _ = fs::rename(&temp, &path);
        }
    }
    Ok(lines[lines.len().saturating_sub(limit)..]
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

/// Milliseconds since the epoch as RFC 3339 UTC, to the second.
pub fn rfc3339(ms: u64) -> String {
    let secs = ms / 1000;
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// An RFC 3339 timestamp (`2026-11-15T01:12:30.000Z`, or with a numeric
/// offset) as milliseconds since the epoch. `None` when it cannot be read.
pub fn parse_rfc3339(text: &str) -> Option<u64> {
    let text = text.trim();
    let (date, rest) = text.split_once(['T', 't', ' '])?;
    let mut parts = date.splitn(3, '-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    let (time, offset_secs) = if let Some(time) = rest.strip_suffix(['Z', 'z']) {
        (time, 0)
    } else {
        let at = rest.rfind(['+', '-'])?;
        let (time, offset) = rest.split_at(at);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let (hours, minutes) = offset[1..].split_once(':')?;
        let offset: i64 = hours.parse::<i64>().ok()? * 3600 + minutes.parse::<i64>().ok()? * 60;
        (time, sign * offset)
    };
    let mut clock = time.splitn(3, ':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let seconds = clock.next()?;
    let (whole, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    let second: i64 = whole.parse().ok()?;
    let millis: i64 = format!("{fraction:0<3}").get(..3)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second - offset_secs;
    u64::try_from(secs * 1000 + millis).ok()
}

/// A proleptic Gregorian date to days since 1970-01-01 (the inverse of
/// [`civil_from_days`]).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_render_as_utc() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400_000), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_794_700_000_123), "2026-11-14T23:46:40Z");
    }

    #[test]
    fn timestamps_parse_back() {
        for ms in [0, 951_782_400_000, 1_794_700_000_000] {
            assert_eq!(parse_rfc3339(&rfc3339(ms)), Some(ms));
        }
        assert_eq!(
            parse_rfc3339("2026-11-14T23:46:40.123Z"),
            Some(1_794_700_000_123)
        );
        assert_eq!(
            parse_rfc3339("2026-11-15T01:46:40+02:00"),
            Some(1_794_700_000_000)
        );
        assert_eq!(parse_rfc3339("yesterday"), None);
        assert_eq!(parse_rfc3339("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn records_append_and_the_last_ones_read_back() {
        let dir = std::env::temp_dir().join(format!("boatparty-runs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let home = dir.clone().into_os_string();
        let paths =
            Paths::resolve(|var| (var == crate::paths::HOME_ENV).then(|| home.clone())).unwrap();
        assert!(read(&paths, 5).unwrap().is_empty());
        for n in 0..4 {
            append(&paths, &serde_json::json!({ "n": n })).unwrap();
        }
        let last: Vec<i64> = read(&paths, 2)
            .unwrap()
            .iter()
            .map(|run| run["n"].as_i64().unwrap())
            .collect();
        assert_eq!(last, [2, 3]);
        let _ = fs::remove_dir_all(&dir);
    }
}
