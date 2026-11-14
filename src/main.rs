//! magsafe-dim: keep the LED on a MacBook's MagSafe 3 connector off.
//!
//! The LED is the one-byte AppleSMC key `ACLC`. This program touches that key
//! and nothing else, and can only write two values to it: off while it runs,
//! and "macOS decides" when it stops.
//!
//! The MagSafe Dim app registers `magsafe-dim run` as a launchd daemon with
//! SMAppService; this binary is that daemon, plus a few commands for the terminal.

mod codesign;
mod daemon;
mod log;
mod smc;

use std::process::{Command, ExitCode, Stdio};

/// The launchd label of the daemon the app registers.
const DAEMON_LABEL: &str = "dev.orionus.magsafe-dim.daemon";

const USAGE: &str = "\
magsafe-dim: keep the MagSafe 3 connector LED off

Install and switch it on with the MagSafe Dim app. From a terminal:

  magsafe-dim status          show the LED and the daemon state
  magsafe-dim led             print the LED state only (off, green, amber, ..., unsupported)
  sudo magsafe-dim run        keep the LED off in the foreground (Ctrl-C to stop)
  sudo magsafe-dim restore    give the LED back to macOS once
  magsafe-dim --version
";

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
        Some(state) => println!("daemon: {state} ({DAEMON_LABEL})"),
        None => println!("daemon: not running (switch it on in the MagSafe Dim app)"),
    }
    Ok(())
}

/// For the app: the LED's state in a word, or "unsupported" on a Mac without a
/// MagSafe 3 LED (no SMC, no ACLC key, or an ACLC key of another shape).
fn led() -> Result<(), String> {
    match smc::Smc::open().and_then(|smc| smc.read()) {
        Ok(v) => println!("{}", smc::describe(v)),
        Err(smc::Error::NoSmc | smc::Error::NoKey | smc::Error::WrongKeyShape { .. }) => {
            println!("unsupported")
        }
        Err(e) => return Err(e.to_string()),
    }
    Ok(())
}

fn restore() -> Result<(), String> {
    need_root("restore")?;
    let smc = smc::Smc::open().map_err(|e| e.to_string())?;
    smc.write(smc::Write::System).map_err(|e| e.to_string())?;
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
        ["restore"] => restore(),
        ["--version" | "-V"] => {
            println!("magsafe-dim {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        ["--help" | "-h" | "help"] => {
            print!("{USAGE}");
            Ok(())
        }
        _ => {
            eprint!("{USAGE}");
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
