//! Scheduled purchases: "buy this option with this many points at this time".
//!
//! A scheduled buy is a durable record, not a sleeping process. `schedule run`
//! is the process that waits: it loads the file, sleeps until the next due
//! entry, sends it through the same [`crate::guard::MoneyGuard`] every other
//! purchase uses, and writes the outcome back. Killing it loses nothing that was
//! already recorded, and starting it again picks up where the file says things
//! stand.
//!
//! The one rule that makes this safe to leave running: **a record is marked as
//! started before the request is sent.** If the process dies mid-request the
//! entry stays `in_flight`, and a later run refuses to retry it — because the
//! server may well have accepted the purchase. That is the same distinction the
//! exit codes 4 and 5 exist for, applied to a file across process lifetimes.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// How a scheduled buy stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Waiting for its time.
    Pending,
    /// The request was sent and the server accepted it.
    Done,
    /// The request was sent and the server refused it. Not retried by itself:
    /// the reason is recorded and a human decides what to do.
    Rejected,
    /// Marked as sent, but the outcome was never recorded (the process died, or
    /// the answer never arrived). Never retried automatically — the server may
    /// have accepted it.
    InFlight,
    /// The request never left the machine, so retrying is safe.
    NotSent,
}

impl State {
    /// Whether this entry still wants one attempt.
    fn is_open(self) -> bool {
        matches!(self, State::Pending | State::NotSent)
    }
}

/// One scheduled purchase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Stable id, so `remove` and the logs refer to the same thing.
    pub id: String,
    /// When to send, as UTC RFC 3339 (`2026-12-01T03:00:00Z`). The CLI accepts a
    /// local offset and normalises it on the way in, so the file holds one form.
    pub at: String,
    pub event: String,
    pub option: String,
    pub stake: u64,
    pub state: State,
    /// Free text recorded when the entry stopped being pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// When the state last changed (UTC RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// The whole file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Schedule {
    #[serde(default)]
    pub entries: Vec<Entry>,
}

impl Schedule {
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                if text.trim().is_empty() {
                    return Ok(Self::default());
                }
                serde_json::from_str(&text).map_err(|e| format!("{}: {}", path.display(), e))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {}", path.display(), e)),
        }
    }

    /// Write atomically, the same way the session file is written: a temp file
    /// beside the target, fsync, then rename. A crash leaves either the old
    /// file or the new one, never a half-written one.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
        }
        let tmp = temp_path(path);
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        {
            let mut f =
                std::fs::File::create(&tmp).map_err(|e| format!("{}: {}", tmp.display(), e))?;
            f.write_all(text.as_bytes())
                .map_err(|e| format!("{}: {}", tmp.display(), e))?;
            f.write_all(b"\n")
                .map_err(|e| format!("{}: {}", tmp.display(), e))?;
            f.sync_all()
                .map_err(|e| format!("{}: {}", tmp.display(), e))?;
        }
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {}", path.display(), e)
        })?;
        Ok(())
    }

    /// The next entry that wants an attempt, by time.
    pub fn next_due(&self) -> Option<&Entry> {
        self.entries
            .iter()
            .filter(|e| e.state.is_open())
            .min_by(|a, b| a.at.cmp(&b.at))
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }

    /// An id that does not collide with anything already in the file.
    pub fn new_id(&self) -> String {
        let base = now_rfc3339().replace(['-', ':', 'Z'], "").replace('T', "-");
        let base = format!("s{}", base);
        if !self.entries.iter().any(|e| e.id == base) {
            return base;
        }
        let mut n = 2;
        loop {
            let candidate = format!("{}-{}", base, n);
            if !self.entries.iter().any(|e| e.id == candidate) {
                return candidate;
            }
            n += 1;
        }
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Current time as UTC RFC 3339 with seconds precision.
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    format_unix(secs)
}

/// Seconds since the epoch → `YYYY-MM-DDTHH:MM:SSZ`.
fn format_unix(secs: i64) -> String {
    // Days since 1970-01-01 to a civil date. This is Howard Hinnant's
    // days_from_civil inverse; it avoids a date-time crate for the one
    // conversion the scheduler actually needs.
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, hh, mm, ss)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Parse the forms the CLI accepts into one canonical UTC RFC 3339 string.
///
/// Accepted: `YYYY-MM-DDTHH:MM[:SS][Z|+HH:MM|-HH:MM]`, and a bare
/// `YYYY-MM-DD HH:MM` which is read as local time. Anything with a seconds
/// fraction is truncated rather than rounded, because a scheduler that fires
/// 0.4 s early is no worse and the field stays comparable as text.
pub fn parse_at(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("empty time".to_string());
    }
    let (date_part, time_part) = s
        .split_once('T')
        .or_else(|| s.split_once(' '))
        .ok_or_else(|| format!("{}: expected a date and a time", s))?;
    let (y, mo, d) = parse_date(date_part)?;
    let (hh, mm, ss, offset_minutes) = parse_time(time_part)?;
    let utc = to_unix(y, mo, d, hh, mm, ss)? - offset_minutes as i64 * 60;
    Ok(format_unix(utc))
}

fn parse_date(s: &str) -> Result<(i64, u32, u32), String> {
    let mut it = s.split('-');
    let y = it
        .next()
        .and_then(|v| v.parse::<i64>().ok())
        .ok_or_else(|| format!("{}: bad year", s))?;
    let mo = it
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or_else(|| format!("{}: bad month", s))?;
    let d = it
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or_else(|| format!("{}: bad day", s))?;
    if it.next().is_some() {
        return Err(format!("{}: too many parts", s));
    }
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return Err(format!("{}: out of range", s));
    }
    Ok((y, mo, d))
}

fn parse_time(s: &str) -> Result<(u32, u32, u32, i32), String> {
    let mut offset_minutes = 0;
    let mut body = s;
    if let Some(stripped) = body.strip_suffix('Z') {
        body = stripped;
    } else if let Some(idx) = body.rfind(['+', '-']) {
        let (rest, off) = body.split_at(idx);
        body = rest;
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let (oh, om) = off[1..]
            .split_once(':')
            .ok_or_else(|| format!("{}: bad offset", off))?;
        let oh = oh
            .parse::<i32>()
            .map_err(|_| format!("{}: bad offset hour", off))?;
        let om = om
            .parse::<i32>()
            .map_err(|_| format!("{}: bad offset minute", off))?;
        if !(0..=23).contains(&oh) || !(0..=59).contains(&om) {
            return Err(format!("{}: offset out of range", off));
        }
        offset_minutes = sign * (oh * 60 + om);
    }
    let mut it = body.split(':');
    let hh = it
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or_else(|| format!("{}: bad hour", body))?;
    let mm = it
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or_else(|| format!("{}: bad minute", body))?;
    let ss = match it.next() {
        Some(v) => {
            let sec = v.split('.').next().unwrap_or("0");
            sec.parse::<u32>()
                .map_err(|_| format!("{}: bad second", body))?
        }
        None => 0,
    };
    if it.next().is_some() {
        return Err(format!("{}: too many parts", body));
    }
    if hh > 23 || mm > 59 || ss > 59 {
        return Err(format!("{}: out of range", body));
    }
    Ok((hh, mm, ss, offset_minutes))
}

/// Civil date and time in UTC → seconds since the epoch (Hinnant's algorithm).
fn to_unix(y: i64, m: u32, d: u32, hh: u32, mm: u32, ss: u32) -> Result<i64, String> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err("date out of range".to_string());
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe as i64 - 719_468;
    Ok(days * 86_400 + hh as i64 * 3600 + mm as i64 * 60 + ss as i64)
}

/// Parse a naive local time with the given UTC offset in minutes.
///
/// Used by `schedule add`: the operator types their own clock time, and the
/// machine's offset turns it into the UTC the file stores.
pub fn normalise_with_offset(input: &str, offset_minutes: i32) -> Result<String, String> {
    let utc_like = parse_at(input)?;
    // `parse_at` read it as UTC; shift by the offset to get the real instant.
    let secs = unix_from_rfc(&utc_like)?;
    Ok(format_unix(secs - offset_minutes as i64 * 60))
}

pub fn unix_from_rfc(s: &str) -> Result<i64, String> {
    let (date_part, time_part) = s
        .split_once('T')
        .ok_or_else(|| format!("{}: expected a date and time", s))?;
    let (y, mo, d) = parse_date(date_part)?;
    let (hh, mm, ss, off) = parse_time(time_part)?;
    Ok(to_unix(y, mo, d, hh, mm, ss)? - off as i64 * 60)
}
/// Today's entries of one ledger reason, as `(delta, created_at)` in JST days.
///
/// The ledger timestamps are UTC; the game's own day (`level.day`, and the day a
/// minigame clear counts for) is a JST date, so the comparison is done in JST.
/// Returning a Vec rather than a total keeps the caller able to show what it
/// counted instead of only a number.
pub fn sum_today(ledger: &serde_json::Value, reason: &str) -> Vec<(i64, String)> {
    let today = jst_today();
    let mut out = Vec::new();
    let Some(entries) = ledger.get("entries").and_then(|v| v.as_array()) else {
        return out;
    };
    for e in entries {
        if e.get("reason").and_then(|r| r.as_str()) != Some(reason) {
            continue;
        }
        let Some(raw) = e.get("created_at").and_then(|c| c.as_str()) else {
            continue;
        };
        let Some(day) = jst_date_of(raw) else {
            continue;
        };
        if day != today {
            continue;
        }
        let delta = e.get("delta").and_then(|d| d.as_i64()).unwrap_or(0);
        // The day was matched in JST, so the time is reported in JST too. Mixing
        // a JST total with UTC timestamps makes the list look like it belongs to
        // the previous day.
        let stamp = jst_time_of(raw).unwrap_or_else(|| raw.to_string());
        out.push((delta, stamp));
    }
    out
}

/// Today's date in JST as `YYYY-MM-DD`, from the system clock.
fn jst_today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // JST is UTC+9 with no daylight saving, so this is a constant shift.
    let shifted = secs + 9 * 3600;
    let days = shifted.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// The JST wall-clock time of a UTC RFC 3339 timestamp, as `YYYY-MM-DD HH:MM`.
fn jst_time_of(utc: &str) -> Option<String> {
    let secs = unix_from_rfc(utc).ok()?;
    let shifted = secs + 9 * 3600;
    let days = shifted.div_euclid(86_400);
    let rem = shifted.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60
    ))
}

/// The JST date of a UTC RFC 3339 timestamp, as `YYYY-MM-DD`.
fn jst_date_of(utc: &str) -> Option<String> {
    let secs = unix_from_rfc(utc).ok()?;
    let days = (secs + 9 * 3600).div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    Some(format!("{:04}-{:02}-{:02}", y, m, d))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_round_trips() {
        assert_eq!(format_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            parse_at("1970-01-01T00:00:00Z").unwrap(),
            "1970-01-01T00:00:00Z"
        );
        let t = parse_at("2026-12-01T12:00:00Z").unwrap();
        assert_eq!(t, "2026-12-01T12:00:00Z");
    }

    #[test]
    fn offsets_are_normalised_to_utc() {
        // 12:00 at +09:00 is 03:00 UTC.
        assert_eq!(
            parse_at("2026-12-01T12:00+09:00").unwrap(),
            "2026-12-01T03:00:00Z"
        );
        assert_eq!(
            parse_at("2026-12-01T12:00-05:00").unwrap(),
            "2026-12-01T17:00:00Z"
        );
        // A space-separated form without an offset is taken as local time by the
        // caller, which appends the offset; here it is UTC because none is given.
        assert_eq!(
            parse_at("2026-12-01 12:00").unwrap(),
            "2026-12-01T12:00:00Z"
        );
    }

    #[test]
    fn seconds_are_optional_and_fractions_are_truncated() {
        assert_eq!(
            parse_at("2026-12-01T12:00:30Z").unwrap(),
            "2026-12-01T12:00:30Z"
        );
        assert_eq!(
            parse_at("2026-12-01T12:00:30.999Z").unwrap(),
            "2026-12-01T12:00:30Z"
        );
    }

    #[test]
    fn a_bad_time_is_refused_rather_than_guessed() {
        for bad in [
            "",
            "abc",
            "2026-13-01T00:00Z",
            "2026-12-32T00:00Z",
            "2026-12-01T25:00Z",
        ] {
            assert!(parse_at(bad).is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn leap_day_is_handled() {
        assert_eq!(
            parse_at("2024-02-29T00:00:00Z").unwrap(),
            "2024-02-29T00:00:00Z"
        );
    }

    #[test]
    fn next_due_ignores_settled_entries() {
        let mut s = Schedule::default();
        s.entries.push(Entry {
            id: "a".into(),
            at: "2026-12-01T03:00:00Z".into(),
            event: "1".into(),
            option: "2".into(),
            stake: 10,
            state: State::Pending,
            note: None,
            updated_at: None,
        });
        s.entries.push(Entry {
            id: "b".into(),
            at: "2026-11-01T00:00:00Z".into(),
            event: "1".into(),
            option: "2".into(),
            stake: 10,
            state: State::Done,
            note: None,
            updated_at: None,
        });
        assert_eq!(s.next_due().map(|e| e.id.as_str()), Some("a"));
    }

    #[test]
    fn in_flight_is_never_considered_due() {
        let mut s = Schedule::default();
        s.entries.push(Entry {
            id: "x".into(),
            at: "2020-01-01T00:00:00Z".into(),
            event: "1".into(),
            option: "2".into(),
            stake: 10,
            state: State::InFlight,
            note: None,
            updated_at: None,
        });
        assert!(
            s.next_due().is_none(),
            "an in-flight entry must not be retried"
        );
    }

    #[test]
    fn ids_do_not_collide() {
        let mut s = Schedule::default();
        let first = s.new_id();
        s.entries.push(Entry {
            id: first.clone(),
            at: "2026-12-01T00:00:00Z".into(),
            event: "1".into(),
            option: "2".into(),
            stake: 10,
            state: State::Pending,
            note: None,
            updated_at: None,
        });
        let second = s.new_id();
        assert_ne!(first, second);
    }

    #[test]
    fn utc_timestamps_are_reported_in_jst() {
        // 16:49 UTC is 01:49 the next day in JST, which is the point: the day
        // filter is JST, so the displayed time has to be too.
        assert_eq!(
            jst_time_of("2026-11-26T16:49:49.336Z").as_deref(),
            Some("2026-11-27 01:49")
        );
        assert_eq!(
            jst_time_of("2026-11-27T00:00:00.000Z").as_deref(),
            Some("2026-11-27 09:00")
        );
    }

    #[test]
    fn the_file_round_trips_through_json() {
        let mut s = Schedule::default();
        s.entries.push(Entry {
            id: "s1".into(),
            at: "2026-12-01T03:00:00Z".into(),
            event: "88013".into(),
            option: "106459".into(),
            stake: 100,
            state: State::Pending,
            note: None,
            updated_at: None,
        });
        let text = serde_json::to_string_pretty(&s).unwrap();
        let back: Schedule = serde_json::from_str(&text).unwrap();
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.entries[0].state, State::Pending);
    }
}
