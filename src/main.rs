//! magsafe-dim: keep the LED on a MacBook's MagSafe 3 connector off.
//!
//! The LED is the one-byte AppleSMC key `ACLC`. This program touches that key
//! and nothing else, and writes only the states in `smc::Led`. While it runs
//! it keeps the LED off, or in the state chosen with `set` (see target.rs), or
//! off at night only (see sun.rs); when it stops it gives the LED back to macOS.
//! Once a day it sends an anonymous heartbeat, unless switched off (see heartbeat.rs).
//!
//! The MagSafe Dim app registers `magsafe-dim run` as a launchd daemon with
//! SMAppService; this binary is that daemon, plus a few commands for the terminal.

mod codesign;
mod daemon;
mod heartbeat;
mod log;
mod smc;
mod sun;
mod target;

use std::io;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use target::Choice;

/// The launchd label of the daemon the app registers.
const DAEMON_LABEL: &str = "dev.orionus.magsafe-dim.daemon";

const USAGE: &str = "\
magsafe-dim: the MagSafe 3 connector LED on a MacBook

The MagSafe Dim app runs this program as a helper that keeps the LED off.
Turn it on with the switch \"Allow light control\" in the app. From a terminal:

  magsafe-dim status          show the LED, the helper and the state it keeps
  magsafe-dim led             print the name of the state the LED shows now
  magsafe-dim set STATE       make the helper keep STATE instead of off
  magsafe-dim set night       keep the LED off from sunset to sunrise only
  magsafe-dim target          print the name of the state the helper keeps
  magsafe-dim sun             print night or day, then the night's sunset and
                              sunrise (seconds since 1970)
  magsafe-dim heartbeat [on|off|show]
                              print, switch or show the daily heartbeat
  sudo magsafe-dim run        be the helper in the foreground (switch the app off
                              first; Ctrl-C gives the LED back to macOS)
  sudo magsafe-dim restore    give the LED back to macOS once
  magsafe-dim --version

LED states:
";

const NOTES: &str = "
About set:
  - Needs no sudo, but an administrator account, and works only while
    \"Allow light control\" is on in the app. The app's \"Keep the light off\"
    switch sets off or system; turning light control off ends any chosen state.
  - The helper sets the LED to STATE at once and again after every plug-in and
    wake, until the next set, also across restarts. system and blink-once are
    set once and then left alone.
  - night keeps the LED off from sunset to sunrise and leaves it to macOS by
    day. The times come from macOS's Night Shift schedule (they need Location
    Services); without them, night is 20:00 to 07:00.
  - The LED shows the state only while a charger is connected, and macOS turns
    it on while the Mac sleeps.
  - Blinking amber is how a Mac reports a charging problem; set a calm state
    when done with it.
";

const HEARTBEAT_NOTES: &str = "
About the heartbeat:
  Once a day the helper sends one anonymous request to the author's Counterscale
  (stats.orionus.dev): the app version, the macOS version, the Mac's model and how
  long ago the app was installed (new, 1-7d, ..., over-90d). No ID, no name, no
  serial number; the collector keeps the country, not the IP address. Only
  official releases send it. See exactly what would go out with
  `magsafe-dim heartbeat show`; stop it with `magsafe-dim heartbeat off` or the
  switch in the app's About window (administrators only).
";

/// The usage text: commands, every LED state's name and description, notes.
fn usage() -> String {
    let mut text = USAGE.to_string();
    for led in smc::Led::ALL {
        text += &format!("  {:<12} {}\n", led.name(), led.description());
    }
    text + NOTES + HEARTBEAT_NOTES
}

fn need_root(cmd: &str) -> Result<(), String> {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        Ok(())
    } else {
        Err(format!("{cmd} needs root: sudo magsafe-dim {cmd}"))
    }
}

fn read_led() -> Result<u8, String> {
    let smc = smc::Smc::open().map_err(|e| e.to_string())?;
    smc.read().map_err(|e| e.to_string())
}

/// The daemon's launchd state, e.g. "running (pid 123)", or None if it isn't loaded.
fn daemon_state() -> Option<String> {
    let out = Command::new("/bin/launchctl")
        .args(["print", &format!("system/{DAEMON_LABEL}")])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // Top-level fields only: one tab of indent. Nested blocks repeat "state = ".
    let field = |name: &str| {
        text.lines().find_map(|l| {
            let l = l.strip_prefix('\t').filter(|l| !l.starts_with('\t'))?;
            l.strip_prefix(name).map(|v| v.trim().to_string())
        })
    };
    let state = field("state = ").unwrap_or_else(|| "unknown".into());
    Some(match field("pid = ") {
        Some(pid) => format!("{state} (pid {pid})"),
        None => state,
    })
}

fn status() -> Result<(), String> {
    let v = read_led()?;
    println!("LED:    {} (ACLC = 0x{v:02x})", smc::describe(v));
    match daemon_state() {
        Some(state) => {
            println!("daemon: {state} ({DAEMON_LABEL})");
            let choice = chosen();
            println!("keeps:  {}", choice.description());
            if choice == Choice::Night {
                let (phase, schedule) = sun_phase();
                println!(
                    "sun:    {} until {} (night {} to {}{})",
                    if phase.night { "night" } else { "day" },
                    sun::clock(phase.until()),
                    sun::clock(phase.sunset),
                    sun::clock(phase.sunrise),
                    if schedule.from_macos {
                        ""
                    } else {
                        ", fallback: macOS has no times"
                    }
                );
            }
            println!(
                "heartbeat: {}",
                if !heartbeat::this_build_sends() {
                    "never (this build isn't signed by the author)"
                } else if target::heartbeat_on(Path::new(target::HEARTBEAT_FILE)) {
                    "on"
                } else {
                    "off"
                }
            );
        }
        None => {
            println!("daemon: not running (turn on \"Allow light control\" in the MagSafe Dim app)")
        }
    }
    Ok(())
}

/// For the app and scripts: the LED state's name (`smc::Led::name`), "unknown"
/// for a value without one, or "unsupported" on a Mac without a MagSafe 3 LED
/// (no SMC, no ACLC key, or an ACLC key of another shape).
fn led() -> Result<(), String> {
    match smc::Smc::open().and_then(|smc| smc.read()) {
        Ok(v) => println!("{}", smc::name(v)),
        Err(smc::Error::NoSmc | smc::Error::NoKey | smc::Error::WrongKeyShape { .. }) => {
            println!("unsupported")
        }
        Err(e) => return Err(e.to_string()),
    }
    Ok(())
}

/// The state the helper keeps: what the file says, or off without a valid one.
fn chosen() -> Choice {
    target::read(Path::new(target::FILE))
        .ok()
        .flatten()
        .unwrap_or(Choice::Led(smc::Led::Off))
}

/// Day or night now, and the schedule it comes from.
fn sun_phase() -> (sun::Phase, sun::Schedule) {
    let now = sun::now();
    let schedule = sun::schedule(now);
    (schedule.phase(now), schedule)
}

/// For the app: "night" or "day", then the current or next night's sunset and
/// sunrise in seconds since 1970, then "macos" or "fallback". While the helper
/// keeps `night`, as the helper decided it (it runs as root and reads the times
/// itself); otherwise as this program reads them now.
fn print_sun() {
    if daemon_state().is_some() && chosen() == Choice::Night {
        if let Ok(text) = std::fs::read_to_string(target::SUN_FILE) {
            if let Some(line) = text.lines().next().filter(|l| sun::valid_line(l)) {
                println!("{line}");
                return;
            }
        }
    }
    let now = sun::now();
    println!("{}", sun::schedule(now).line(now));
}

fn need_helper() -> Result<(), String> {
    if daemon_state().is_none() {
        return Err(
            "the helper is off: turn on \"Allow light control\" in the MagSafe Dim app first"
                .into(),
        );
    }
    Ok(())
}

/// Writes one of the helper's setting files, with errors for people.
fn write_setting(path: &str, word: &str) -> Result<(), String> {
    target::write_word(Path::new(path), word).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => {
            "the helper hasn't made its settings file yet: it does when it starts. \
             After updating from 1.0.0, turn \"Allow light control\" off and on \
             once in the app (1.0.0's helper doesn't restart itself on updates)"
                .to_string()
        }
        io::ErrorKind::PermissionDenied => "only administrators can change this".to_string(),
        _ => format!("cannot write {path}: {e}"),
    })
}

fn set(name: &str) -> Result<(), String> {
    let choice = Choice::from_name(name)
        .ok_or_else(|| format!("unknown LED state '{name}' (see magsafe-dim --help)"))?;
    need_helper()?;
    write_setting(target::FILE, choice.name())?;
    println!("now the helper keeps the LED {}", choice.description());
    Ok(())
}

fn heartbeat_cmd(arg: Option<&str>) -> Result<(), String> {
    let file = Path::new(target::HEARTBEAT_FILE);
    match arg {
        // "unavailable": this build never sends one (heartbeat.rs).
        None if !heartbeat::this_build_sends() => println!("unavailable"),
        None => println!(
            "{}",
            if target::heartbeat_on(file) {
                "on"
            } else {
                "off"
            }
        ),
        Some(word @ ("on" | "off")) => {
            need_helper()?;
            write_setting(target::HEARTBEAT_FILE, word)?;
            println!("heartbeat {word}");
        }
        Some("show") => {
            let today = heartbeat::utc_day(sun::now());
            let days = heartbeat::read_days().unwrap_or_default();
            let age = heartbeat::install_age(days.first.as_deref(), &today);
            println!("{}", heartbeat::url(&heartbeat::Snapshot::current(), age));
            println!(
                "switched {}; last sent: {}",
                if target::heartbeat_on(file) {
                    "on"
                } else {
                    "off"
                },
                days.last.as_deref().unwrap_or("never")
            );
        }
        Some(other) => {
            return Err(format!(
                "unknown heartbeat command '{other}': on, off or show"
            ));
        }
    }
    Ok(())
}

fn restore() -> Result<(), String> {
    need_root("restore")?;
    let smc = smc::Smc::open().map_err(|e| e.to_string())?;
    smc.write(smc::Led::System).map_err(|e| e.to_string())?;
    println!("LED given back to macOS");
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["status"] => status(),
        ["led"] => led(),
        ["run"] => need_root("run").map(|()| {
            // SAFETY: isatty has no preconditions.
            log::init(unsafe { libc::isatty(2) } == 1);
            daemon::run()
        }),
        ["set", name] => set(name),
        ["sun"] => {
            print_sun();
            Ok(())
        }
        ["heartbeat"] => heartbeat_cmd(None),
        ["heartbeat", arg] => heartbeat_cmd(Some(arg)),
        ["target"] => {
            println!("{}", chosen().name());
            Ok(())
        }
        ["restore"] => restore(),
        ["--version" | "-V"] => {
            println!("magsafe-dim {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        ["--help" | "-h" | "help"] => {
            print!("{}", usage());
            Ok(())
        }
        _ => {
            eprint!("{}", usage());
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("magsafe-dim: {e}");
            ExitCode::FAILURE
        }
    }
}
