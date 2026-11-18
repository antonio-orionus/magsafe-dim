//! The daily heartbeat: one anonymous request a day that tells the author how
//! many Macs run MagSafe Dim, on which versions.
//!
//! It goes to a self-hosted Counterscale (the same collector as Arroxy's
//! heartbeat), as one HTTPS GET through `/usr/bin/curl`. It carries the app
//! version, the macOS major version, the Mac's model identifier and a coarse
//! install age, and nothing that identifies the Mac or its user: no ID, no
//! serial number, no name. The collector adds the country from the request and
//! stores no IP address.
//!
//! - Only builds signed with the author's Developer ID send it, so builds from
//!   source and test builds never reach the collector.
//! - `magsafe-dim heartbeat off` (or the switch in the app's About window) stops it.
//! - Once per UTC day. A failed request (no network yet after a wake, say) is
//!   tried again ten minutes later, at most six times a day.

use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{codesign, log, sun, target};

const URL: &str = "https://stats.orionus.dev/collect";
const SITE_ID: &str = "magsafe-dim";
/// Counterscale is made for websites: the app poses as one host.
const HOST: &str = "https://app.magsafe-dim";
/// The author's Developer ID team: only builds signed by it send heartbeats.
const TEAM_ID: &str = "LDDHTN9932";
/// The install's first day and the last day a heartbeat went out, root's alone.
pub const DAYS_FILE: &str = "/Library/Application Support/MagSafe Dim/heartbeat-days";
const RETRY_SECS: i64 = 600;
const TRIES_PER_DAY: u32 = 6;
const CURL_TIMEOUT_SECS: &str = "15";

/// `first`: the UTC day the helper first ran, or None for an install that
/// predates the heartbeat. `last`: the UTC day of the last heartbeat sent.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Days {
    pub first: Option<String>,
    pub last: Option<String>,
}

impl Days {
    fn parse(text: &str) -> Days {
        let mut days = Days::default();
        for line in text.lines() {
            let mut words = line.split_whitespace();
            let (Some(key), Some(day)) = (words.next(), words.next()) else {
                continue;
            };
            let day = parse_day(day).is_some().then(|| day.to_string());
            match key {
                "first" => days.first = day,
                "last" => days.last = day,
                _ => {}
            }
        }
        days
    }

    fn text(&self) -> String {
        format!(
            "first {}\nlast {}\n",
            self.first.as_deref().unwrap_or("existing"),
            self.last.as_deref().unwrap_or("never")
        )
    }
}

/// The day of `t` (seconds since 1970) in UTC, as YYYY-MM-DD.
pub fn utc_day(t: i64) -> String {
    // Howard Hinnant's civil_from_days.
    let z = t.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A YYYY-MM-DD day as days since 1970, if it is one.
fn parse_day(day: &str) -> Option<i64> {
    let t = day_start(day)?;
    (utc_day(t) == day).then_some(t / 86_400)
}

fn day_start(day: &str) -> Option<i64> {
    let b = day.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |s: &str| {
        s.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| s.parse::<i64>().ok())?
    };
    let (y, m, d) = (num(&day[..4])?, num(&day[5..7])?, num(&day[8..])?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Days since 1970 by Hinnant's days_from_civil; utc_day rejects rolled-over dates.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400)
}

/// How long ago the install started, in buckets shared by many installs, so it
/// can never single one out the way an exact date could.
pub fn install_age(first: Option<&str>, today: &str) -> &'static str {
    let (Some(first), Some(now)) = (first.and_then(parse_day), parse_day(today)) else {
        return "existing";
    };
    match now - first {
        0 => "new",
        // A first day in the future means the clock was set back: not new.
        ..=7 => "1-7d",
        8..=30 => "8-30d",
        31..=90 => "31-90d",
        _ => "over-90d",
    }
}

/// What a heartbeat reports, besides the install age.
pub struct Snapshot {
    pub version: &'static str,
    /// The macOS major version, e.g. "macos-27".
    pub macos: String,
    /// The Mac's model identifier, e.g. "Mac17,8": shared by every Mac of that model.
    pub model: String,
}

impl Snapshot {
    pub fn current() -> Snapshot {
        let macos = sysctl("kern.osproductversion")
            .and_then(|v| v.split('.').next().map(|major| format!("macos-{major}")))
            .unwrap_or_else(|| "macos-unknown".into());
        Snapshot {
            version: env!("CARGO_PKG_VERSION"),
            macos,
            model: sysctl("hw.model").unwrap_or_else(|| "unknown".into()),
        }
    }
}

fn sysctl(name: &str) -> Option<String> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut buf = [0u8; 128];
    let mut len = buf.len();
    // SAFETY: name is a valid C string, buf and len describe a writable buffer.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let text = std::str::from_utf8(&buf[..len])
        .ok()?
        .trim_end_matches('\0');
    (!text.is_empty()).then(|| text.to_string())
}

/// Percent-encodes everything but unreserved characters, so no collector can
/// read a value two ways.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The request URL. Fields ride in the slots Counterscale's dashboard breaks
/// down: Path = version, Referrer = install age, UTM Medium = model, UTM
/// Campaign = macOS version. `ht=1` counts every heartbeat as a visit.
pub fn url(snapshot: &Snapshot, age: &str) -> String {
    let params = [
        ("sid", SITE_ID),
        ("h", HOST),
        ("p", &format!("/{}", snapshot.version)),
        ("r", age),
        ("ht", "1"),
        ("um", &snapshot.model),
        ("uc", &snapshot.macos),
    ];
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect();
    format!("{URL}?{}", query.join("&"))
}

pub fn read_days() -> io::Result<Days> {
    Ok(Days::parse(&fs::read_to_string(DAYS_FILE)?))
}

fn write_days(days: &Days) -> io::Result<()> {
    target::write_private(DAYS_FILE, &days.text())
}

/// For the helper at startup: records the install's first day once.
/// `earlier_install`: the state folder was there before this start, so this is
/// an update from a version without the heartbeat, not a new install.
pub fn record_first_day(earlier_install: bool) {
    match read_days() {
        Ok(_) => return,
        Err(e) if e.kind() != io::ErrorKind::NotFound => return,
        Err(_) => {}
    }
    let days = Days {
        first: (!earlier_install).then(|| utc_day(sun::now())),
        last: None,
    };
    if let Err(e) = write_days(&days) {
        log::error(&format!("cannot write {DAYS_FILE}: {e}"));
    }
}

/// Whether this build may send heartbeats: signed with the author's Developer ID.
static ALLOWED: AtomicBool = AtomicBool::new(false);
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Today's attempts: the day, tries so far, the last try, and whether one went
/// out (also kept here, so a days file that can't be written doesn't resend).
struct Tries {
    day: String,
    count: u32,
    last: i64,
    sent: bool,
}

static TRIES: Mutex<Tries> = Mutex::new(Tries {
    day: String::new(),
    count: 0,
    last: 0,
    sent: false,
});

/// Whether this build sends heartbeats at all: signed with the author's Developer ID.
pub fn this_build_sends() -> bool {
    codesign::signed_by_team(TEAM_ID)
}

/// For the helper at startup.
pub fn init() {
    ALLOWED.store(this_build_sends(), Ordering::Relaxed);
}

/// Sends today's heartbeat on a thread of its own, unless it went out already,
/// is switched off, or was tried too recently or too often today.
pub fn maybe_send() {
    if !ALLOWED.load(Ordering::Relaxed) || !target::heartbeat_on(Path::new(target::HEARTBEAT_FILE))
    {
        return;
    }
    let now = sun::now();
    let today = utc_day(now);
    let Ok(days) = read_days() else { return };
    if days.last.as_deref() == Some(today.as_str()) {
        return;
    }
    {
        let mut tries = TRIES.lock().unwrap_or_else(|e| e.into_inner());
        if tries.day != today {
            *tries = Tries {
                day: today.clone(),
                count: 0,
                last: 0,
                sent: false,
            };
        }
        if tries.sent
            || tries.count >= TRIES_PER_DAY
            || (tries.count > 0 && now - tries.last < RETRY_SECS)
        {
            return;
        }
        if IN_FLIGHT.swap(true, Ordering::Relaxed) {
            return;
        }
        tries.count += 1;
        tries.last = now;
    }
    let age = install_age(days.first.as_deref(), &today);
    let request = url(&Snapshot::current(), age);
    std::thread::spawn(move || {
        let result = curl(&request);
        IN_FLIGHT.store(false, Ordering::Relaxed);
        match result {
            Ok(code) => {
                TRIES.lock().unwrap_or_else(|e| e.into_inner()).sent = true;
                let sent = Days {
                    last: Some(today.clone()),
                    ..days
                };
                if let Err(e) = write_days(&sent) {
                    log::error(&format!("cannot write {DAYS_FILE}: {e}"));
                }
                log::notice(&format!("heartbeat sent for {today} ({age}, HTTP {code})"));
            }
            Err(e) => log::notice(&format!("heartbeat not sent: {e}")),
        }
    });
}

/// One GET, HTTPS only. Any HTTP answer counts as delivered: only a request that
/// got no answer is tried again.
fn curl(url: &str) -> Result<String, String> {
    let out = Command::new("/usr/bin/curl")
        .args([
            "--silent",
            "--proto",
            "=https",
            "--max-time",
            CURL_TIMEOUT_SECS,
            "--output",
            "/dev/null",
            "--write-out",
            "%{http_code}",
            "--user-agent",
            concat!("MagSafeDim/", env!("CARGO_PKG_VERSION")),
            url,
        ])
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "no answer (curl exit {})",
            out.status.code().unwrap_or(-1)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days() {
        assert_eq!(utc_day(0), "1970-01-01");
        assert_eq!(utc_day(1_794_977_893), "2026-11-18");
        assert_eq!(utc_day(951_782_400), "2000-02-29");
        assert_eq!(utc_day(-1), "1969-12-31");
        assert_eq!(parse_day("2026-11-18"), Some(1_794_977_893 / 86_400));
        assert_eq!(parse_day("2026-02-30"), None);
        assert_eq!(parse_day("2026-1-18"), None);
        assert_eq!(parse_day("existing"), None);
    }

    #[test]
    fn ages() {
        assert_eq!(install_age(Some("2026-11-18"), "2026-11-18"), "new");
        assert_eq!(install_age(Some("2026-11-11"), "2026-11-18"), "1-7d");
        assert_eq!(install_age(Some("2026-11-19"), "2026-11-18"), "1-7d");
        assert_eq!(install_age(Some("2026-10-19"), "2026-11-18"), "8-30d");
        assert_eq!(install_age(Some("2026-08-20"), "2026-11-18"), "31-90d");
        assert_eq!(install_age(Some("2026-08-19"), "2026-11-18"), "over-90d");
        assert_eq!(install_age(None, "2026-11-18"), "existing");
        assert_eq!(install_age(Some("junk"), "2026-11-18"), "existing");
    }

    #[test]
    fn days_file_round_trip() {
        let days = Days {
            first: Some("2026-11-18".into()),
            last: None,
        };
        assert_eq!(Days::parse(&days.text()), days);
        let existing = Days::parse("first existing\nlast 2026-11-19\n");
        assert_eq!(existing.first, None);
        assert_eq!(existing.last.as_deref(), Some("2026-11-19"));
        assert_eq!(Days::parse("garbage"), Days::default());
    }

    #[test]
    fn request_url() {
        let snapshot = Snapshot {
            version: "1.2.0",
            macos: "macos-27".into(),
            model: "Mac17,8".into(),
        };
        assert_eq!(
            url(&snapshot, "new"),
            "https://stats.orionus.dev/collect?sid=magsafe-dim&h=https%3A%2F%2Fapp.magsafe-dim\
             &p=%2F1.2.0&r=new&ht=1&um=Mac17%2C8&uc=macos-27"
        );
    }
}
