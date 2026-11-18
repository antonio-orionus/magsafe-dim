//! Sunset and sunrise for `magsafe-dim set night`: off at night, macOS's own
//! choice by day.
//!
//! The times come from macOS's Night Shift schedule, which `corebrightnessdiag
//! sunschedule` prints. The tool is undocumented and may move or change its
//! output, and needs a location; without one, night is 20:00 to 07:00 local time.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const TOOL: &str = "/usr/libexec/corebrightnessdiag";
/// How long the tool may run before it is killed and the fallback used.
const TOOL_TIMEOUT: Duration = Duration::from_secs(10);
/// The fallback night, in local hours.
const FALLBACK_SUNSET_HOUR: i32 = 20;
const FALLBACK_SUNRISE_HOUR: i32 = 7;

/// One day's schedule, in seconds since 1970 (UTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    pub previous_sunset: i64,
    pub sunrise: i64,
    pub sunset: i64,
    pub next_sunrise: i64,
    /// From macOS's schedule, not the 20:00 to 07:00 fallback.
    pub from_macos: bool,
}

/// Day or night now, with the night it is in or comes next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Phase {
    pub night: bool,
    /// Start and end of the current night, or of the next one by day.
    pub sunset: i64,
    pub sunrise: i64,
}

impl Phase {
    /// When this phase ends: sunrise at night, sunset by day.
    pub fn until(self) -> i64 {
        if self.night {
            self.sunrise
        } else {
            self.sunset
        }
    }
}

impl Schedule {
    pub fn phase(&self, now: i64) -> Phase {
        if now < self.sunrise {
            Phase {
                night: true,
                sunset: self.previous_sunset,
                sunrise: self.sunrise,
            }
        } else {
            Phase {
                night: now >= self.sunset,
                sunset: self.sunset,
                sunrise: self.next_sunrise,
            }
        }
    }

    /// True once `now` is past the day this schedule describes, so a fresh one
    /// is needed.
    pub fn stale(&self, now: i64) -> bool {
        now >= self.next_sunrise
    }

    /// This schedule moved by whole days so that it covers `now`: sunset and
    /// sunrise move only minutes a day, so yesterday's times are a far better guess
    /// than the fallback while macOS hasn't rolled its schedule over yet, and a
    /// clock set back doesn't leave a schedule from the future.
    pub fn around(mut self, now: i64) -> Schedule {
        for _ in 0..366 {
            let shift = if self.stale(now) {
                86_400
            } else if now < self.previous_sunset {
                -86_400
            } else {
                return self;
            };
            self.previous_sunset += shift;
            self.sunrise += shift;
            self.sunset += shift;
            self.next_sunrise += shift;
        }
        fallback(now)
    }

    /// The line `magsafe-dim sun` prints and the helper writes to `SUN_FILE`:
    /// "night" or "day", the night's sunset and sunrise, and where they come from.
    pub fn line(&self, now: i64) -> String {
        let phase = self.phase(now);
        format!(
            "{} {} {} {}",
            if phase.night { "night" } else { "day" },
            phase.sunset,
            phase.sunrise,
            if self.from_macos { "macos" } else { "fallback" }
        )
    }
}

/// Whether `line` has the shape `Schedule::line` writes.
pub fn valid_line(line: &str) -> bool {
    let words: Vec<&str> = line.split_whitespace().collect();
    words.len() == 4
        && matches!(words[0], "night" | "day")
        && words[1].parse::<i64>().is_ok()
        && words[2].parse::<i64>().is_ok()
        && matches!(words[3], "macos" | "fallback")
}

/// Parses `corebrightnessdiag sunschedule`, which prints lines such as
/// `    sunrise = "2026-11-18 04:58:13 +0000";`.
pub fn parse(text: &str) -> Option<Schedule> {
    let field = |key: &str| {
        text.lines().find_map(|line| {
            let rest = line.trim_start().strip_prefix(key)?;
            let value = rest.trim_start().strip_prefix('=')?.trim();
            parse_date(value.trim_end_matches(';').trim_matches('"'))
        })
    };
    let schedule = Schedule {
        previous_sunset: field("previousSunset")?,
        sunrise: field("sunrise")?,
        sunset: field("sunset")?,
        next_sunrise: field("nextSunrise")?,
        from_macos: true,
    };
    let ordered = schedule.previous_sunset < schedule.sunrise
        && schedule.sunrise < schedule.sunset
        && schedule.sunset < schedule.next_sunrise;
    ordered.then_some(schedule)
}

/// `2026-11-18 04:58:13 +0000` in seconds since 1970.
fn parse_date(text: &str) -> Option<i64> {
    let mut parts = text.split(' ');
    let (date, time, zone) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let num = |s: &str, len: usize| -> Option<i64> {
        (s.len() == len && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
    };
    let mut d = date.split('-');
    let (y, m, day) = (num(d.next()?, 4)?, num(d.next()?, 2)?, num(d.next()?, 2)?);
    let mut t = time.split(':');
    let (h, min, s) = (num(t.next()?, 2)?, num(t.next()?, 2)?, num(t.next()?, 2)?);
    if d.next().is_some() || t.next().is_some() {
        return None;
    }
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || h > 23 || min > 59 || s > 60 {
        return None;
    }
    let (sign, zone) = match zone.split_at_checked(1)? {
        ("+", z) => (1, z),
        ("-", z) => (-1, z),
        _ => return None,
    };
    if zone.len() != 4 || !zone.is_ascii() {
        return None;
    }
    let offset = sign * (num(&zone[..2], 2)? * 3600 + num(&zone[2..], 2)? * 60);
    Some(days_from_civil(y, m, day) * 86_400 + h * 3600 + min * 60 + s - offset)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// macOS's schedule, or None if the tool is missing, fails, hangs or prints
/// something else.
pub fn from_macos() -> Option<Schedule> {
    let mut child = Command::new(TOOL)
        .arg("sunschedule")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Read on a thread, so a tool (or a child of it) that keeps its output open
    // can't block past the timeout; such a reader is left behind.
    let (sent, received) = mpsc::channel();
    thread::spawn(move || {
        let mut text = String::new();
        let _ = (&mut stdout).take(64 * 1024).read_to_string(&mut text);
        let _ = sent.send(text);
    });
    let deadline = Instant::now() + TOOL_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let left = deadline.saturating_duration_since(Instant::now());
    parse(&received.recv_timeout(left).ok()?)
}

/// The fallback night, 20:00 to 07:00 local time, around `now`.
pub fn fallback(now: i64) -> Schedule {
    let at = |day: i32, hour: i32| local_time(now, day, hour);
    Schedule {
        previous_sunset: at(-1, FALLBACK_SUNSET_HOUR),
        sunrise: at(0, FALLBACK_SUNRISE_HOUR),
        sunset: at(0, FALLBACK_SUNSET_HOUR),
        next_sunrise: at(1, FALLBACK_SUNRISE_HOUR),
        from_macos: false,
    }
}

/// `hour`:00 local time on the day `day_offset` days from `now`'s day.
fn local_time(now: i64, day_offset: i32, hour: i32) -> i64 {
    let t = now as libc::time_t;
    // SAFETY: an all-zero tm is valid; localtime_r and mktime get valid pointers.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return now;
        }
        tm.tm_mday += day_offset;
        tm.tm_hour = hour;
        tm.tm_min = 0;
        tm.tm_sec = 0;
        tm.tm_isdst = -1;
        libc::mktime(&mut tm) as i64
    }
}

/// macOS's schedule moved to cover `now`, or the fallback.
pub fn schedule(now: i64) -> Schedule {
    from_macos().map_or_else(|| fallback(now), |s| s.around(now))
}

/// `t` as local "HH:MM".
pub fn clock(t: i64) -> String {
    let t = t as libc::time_t;
    // SAFETY: an all-zero tm is valid; localtime_r gets valid pointers.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return "?".into();
        }
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTPUT: &str = r#"Night Shift Sunset/Sunrise
{
    isDaylight = 1;
    nextSunrise = "2026-11-19 04:59:49 +0000";
    nextSunset = "2026-11-19 13:39:57 +0000";
    previousSunrise = "2026-11-17 04:56:36 +0000";
    previousSunset = "2026-11-17 13:42:18 +0000";
    sunrise = "2026-11-18 04:58:13 +0000";
    sunset = "2026-11-18 13:41:06 +0000";
}
"#;

    #[test]
    fn dates() {
        assert_eq!(parse_date("1970-01-01 00:00:00 +0000"), Some(0));
        assert_eq!(parse_date("2026-11-18 04:58:13 +0000"), Some(1_794_977_893));
        assert_eq!(parse_date("2026-11-18 06:58:13 +0200"), Some(1_794_977_893));
        assert_eq!(parse_date("2000-02-29 00:00:00 -0130"), Some(951_787_800));
        assert_eq!(parse_date("2026-13-18 04:58:13 +0000"), None);
        assert_eq!(parse_date("2026-11-18 04:58 +0000"), None);
        assert_eq!(parse_date("2026-11-18T04:58:13Z"), None);
        assert_eq!(parse_date(""), None);
    }

    #[test]
    fn parses_the_tool_output() {
        let s = parse(OUTPUT).unwrap();
        assert_eq!(s.sunrise, parse_date("2026-11-18 04:58:13 +0000").unwrap());
        assert_eq!(s.sunset, parse_date("2026-11-18 13:41:06 +0000").unwrap());
        // previousSunrise and nextSunset don't shadow sunrise and sunset.
        assert_eq!(
            s.previous_sunset,
            parse_date("2026-11-17 13:42:18 +0000").unwrap()
        );
        assert_eq!(
            s.next_sunrise,
            parse_date("2026-11-19 04:59:49 +0000").unwrap()
        );
        assert!(parse("isDaylight = 1;").is_none());
        // Out of order: not today's schedule.
        assert!(parse(&OUTPUT.replace("2026-11-18 13:41", "2026-11-18 03:41")).is_none());
    }

    #[test]
    fn phases() {
        let s = parse(OUTPUT).unwrap();
        let before_dawn = s.phase(s.sunrise - 1);
        assert!(before_dawn.night);
        assert_eq!(
            (before_dawn.sunset, before_dawn.until()),
            (s.previous_sunset, s.sunrise)
        );
        let day = s.phase(s.sunrise);
        assert!(!day.night);
        assert_eq!(
            (day.sunset, day.sunrise, day.until()),
            (s.sunset, s.next_sunrise, s.sunset)
        );
        let evening = s.phase(s.sunset);
        assert!(evening.night);
        assert_eq!(evening.until(), s.next_sunrise);
        assert!(!s.stale(s.next_sunrise - 1));
        assert!(s.stale(s.next_sunrise));
    }

    #[test]
    fn around_moves_by_days() {
        let s = parse(OUTPUT).unwrap();
        assert_eq!(s.around(s.sunrise), s);
        // macOS hasn't rolled over yet: tomorrow uses today's times plus a day.
        let tomorrow = s.around(s.next_sunrise + 60);
        assert_eq!(tomorrow.sunrise, s.sunrise + 86_400);
        assert!(tomorrow.from_macos && !tomorrow.stale(s.next_sunrise + 60));
        // A clock set back two days.
        let past = s.around(s.previous_sunset - 2 * 86_400 + 60);
        assert_eq!(past.sunset, s.sunset - 2 * 86_400);
        assert!(past.phase(s.previous_sunset - 2 * 86_400 + 60).night);
    }

    #[test]
    fn lines() {
        let s = parse(OUTPUT).unwrap();
        let line = s.line(s.sunrise);
        assert_eq!(line, format!("day {} {} macos", s.sunset, s.next_sunrise));
        assert!(valid_line(&line));
        assert!(!valid_line("night 1 2"));
        assert!(!valid_line("dusk 1 2 macos"));
        assert!(!valid_line("night 1 x macos"));
    }

    #[test]
    fn fallback_brackets_now() {
        let now = 1_794_977_893;
        let s = fallback(now);
        assert!(!s.from_macos);
        assert!(s.previous_sunset < s.sunrise && s.sunrise < s.sunset && s.sunset < s.next_sunrise);
        assert!(s.previous_sunset <= now && now < s.next_sunrise);
    }
}
