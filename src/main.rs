//! magsafe-dim: keep the LED on a MacBook's MagSafe 3 connector off.
//!
//! The LED is the one-byte AppleSMC key `ACLC`. This program touches that key
//! and nothing else, and writes only the states in `smc::Led`. While it runs
//! it keeps the LED off, or in the state chosen with `set` (see target.rs);
//! when it stops it gives the LED back to macOS.
//!
//! The MagSafe Dim app registers `magsafe-dim run` as a launchd daemon with
//! SMAppService; this binary is that daemon, plus a few commands for the terminal.

mod codesign;
mod daemon;
mod log;
mod smc;
mod target;

use std::io;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

/// The launchd label of the daemon the app registers.
const DAEMON_LABEL: &str = "dev.orionus.magsafe-dim.daemon";

const USAGE: &str = "\
magsafe-dim: the MagSafe 3 connector LED on a MacBook

The MagSafe Dim app runs this program as a helper that keeps the LED off.
Turn it on with the switch \"Allow light control\" in the app. From a terminal:

  magsafe-dim status          show the LED, the helper and the state it keeps
  magsafe-dim led             print the name of the state the LED shows now
  magsafe-dim set STATE       make the helper keep STATE instead of off
  magsafe-dim target          print the name of the state the helper keeps
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
  - The LED shows the state only while a charger is connected, and macOS turns
    it on while the Mac sleeps.
  - Blinking amber is how a Mac reports a charging problem; set a calm state
    when done with it.
";

/// The usage text: commands, every LED state's name and description, notes.
fn usage() -> String {
    let mut text = USAGE.to_string();
    for led in smc::Led::ALL {
        text += &format!("  {:<12} {}\n", led.name(), led.description());
    }
    text + NOTES
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
            println!("keeps:  {}", chosen().description());
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
fn chosen() -> smc::Led {
    target::read(Path::new(target::FILE))
        .ok()
        .flatten()
        .unwrap_or(smc::Led::Off)
}

fn set(name: &str) -> Result<(), String> {
    let led = smc::Led::from_name(name)
        .ok_or_else(|| format!("unknown LED state '{name}' (see magsafe-dim --help)"))?;
    if daemon_state().is_none() {
        return Err(
            "the helper is off: turn on \"Allow light control\" in the MagSafe Dim app first"
                .into(),
        );
    }
    target::write(Path::new(target::FILE), led).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => {
            "the helper hasn't made its state file yet: it does when it starts. \
             After updating from 1.0.0, turn \"Allow light control\" off and on \
             once in the app (1.0.0's helper doesn't restart itself on updates)"
                .to_string()
        }
        io::ErrorKind::PermissionDenied => "only administrators can set the LED".to_string(),
        _ => format!("cannot write {}: {e}", target::FILE),
    })?;
    println!("the helper keeps the LED {} now", led.description());
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
