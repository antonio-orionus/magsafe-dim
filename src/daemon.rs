//! `magsafe-dim run`: keep the LED off until stopped.
//!
//! macOS and the SMC rewrite ACLC on power events (plug-in, charge-state
//! changes, sleep and wake). This waits for those events on a CoreFoundation
//! run loop and turns the LED off again. Nothing runs between events except a
//! once-a-minute check of the LED and of whether the app was updated or deleted.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFRetained, CFRunLoop, CFRunLoopTimer, kCFRunLoopDefaultMode,
};
use objc2_io_kit::{
    IOAllowPowerChange, IONotificationPort, IONotificationPortRef,
    IOPSCreateLimitedPowerNotification, IOPSNotificationCreateRunLoopSource,
    IORegisterForSystemPower, io_object_t, io_service_t, kIOMessageCanSystemSleep,
    kIOMessageSystemHasPoweredOn, kIOMessageSystemWillSleep,
};

use crate::codesign;
use crate::log;
use crate::smc::{self, Error, Smc, Write};

/// After a plug-in or a wake the SMC resets the LED in short bursts and ignores
/// writes meanwhile, so keep checking for a few seconds. A new event restarts it.
const BURST_STEP_SECS: f64 = 0.25;
const BURST_STEPS: u32 = 20;
const PERIODIC_SECS: f64 = 60.0;
const PARKED: f64 = 1.0e10;

struct State {
    smc: Smc,
    failing: bool,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static PM_ROOT: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static BURST_TIMER: RefCell<Option<CFRetained<CFRunLoopTimer>>> = const { RefCell::new(None) };
    static BURST_LEFT: Cell<u32> = const { Cell::new(0) };
}

fn state() -> MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Reads the LED and turns it off if something turned it on. Writes only on a mismatch.
fn enforce_once(smc: &Smc, reason: &str) -> smc::Result<()> {
    let v = smc.read()?;
    if v == smc::OFF {
        return Ok(());
    }
    smc.write(Write::Off)?;
    log::notice(&format!(
        "{reason}: LED was {} (0x{v:02x}), turned it off",
        smc::describe(v)
    ));
    Ok(())
}

fn enforce(reason: &str) {
    let mut guard = state();
    let Some(st) = guard.as_mut() else { return };
    let result = enforce_once(&st.smc, reason).or_else(|_| {
        // The SMC connection can go stale across sleep: reopen it and try once more.
        st.smc = Smc::open()?;
        enforce_once(&st.smc, reason)
    });
    match result {
        Ok(()) => {
            if st.failing {
                log::notice("LED control works again");
            }
            st.failing = false;
        }
        Err(e) => {
            if !st.failing {
                log::error(&format!("{reason}: cannot turn the LED off: {e}"));
            }
            st.failing = true;
        }
    }
}

fn park_burst_timer(fire_at: f64) {
    BURST_TIMER.with(|t| {
        if let Some(t) = t.borrow().as_ref() {
            t.set_next_fire_date(fire_at);
        }
    });
}

fn burst() {
    BURST_LEFT.with(|l| l.set(BURST_STEPS));
    park_burst_timer(CFAbsoluteTimeGetCurrent());
}

unsafe extern "C-unwind" fn burst_tick(_: *mut CFRunLoopTimer, _: *mut c_void) {
    enforce("power event");
    let left = BURST_LEFT.with(|l| {
        let n = l.get().saturating_sub(1);
        l.set(n);
        n
    });
    if left == 0 {
        park_burst_timer(CFAbsoluteTimeGetCurrent() + PARKED);
    }
}

/// This program's file when it started: path, device and inode.
static OWN_FILE: OnceLock<(PathBuf, u64, u64)> = OnceLock::new();
/// Consecutive once-a-minute checks that found the file missing.
static MISSING_CHECKS: AtomicU32 = AtomicU32::new(0);
/// Set once a replacement failed the signature check, so it's logged only once.
static REJECTED_UPDATE: AtomicBool = AtomicBool::new(false);
/// How long the file must stay missing before the app counts as deleted. An
/// update often removes the old app first and copies the new one a little later.
const MISSING_CHECKS_BEFORE_DELETED: u32 = 3;

fn remember_own_file() {
    let file = std::env::current_exe().and_then(|path| {
        let m = fs::metadata(&path)?;
        Ok((path, m.dev(), m.ino()))
    });
    match file {
        Ok(file) => {
            let _ = OWN_FILE.set(file);
        }
        Err(e) => log::error(&format!(
            "cannot find this program's file ({e}); app updates take effect after a restart"
        )),
    }
}

/// Notices when the app around this helper was replaced or deleted.
///
/// - Replaced by a copy signed like this one (an update): exit with an error, so
///   launchd starts the new copy at once. The LED stays off meanwhile.
/// - Replaced by anything else: keep running. This helper runs as root and the
///   app folder may be writable by the user, so it never hurries into running
///   an unknown file as root; launchd checks it on its own at the next start.
/// - Missing for a few minutes in a row (the app was deleted): give the LED back
///   to macOS and exit cleanly, so launchd doesn't start it again.
fn check_own_file() {
    let Some((path, dev, ino)) = OWN_FILE.get() else {
        return;
    };
    match fs::metadata(path) {
        Ok(m) if m.dev() == *dev && m.ino() == *ino => {
            MISSING_CHECKS.store(0, Ordering::Relaxed);
        }
        Ok(_) => {
            MISSING_CHECKS.store(0, Ordering::Relaxed);
            if codesign::signed_like_this_program(path) {
                log::notice("the app was updated; restarting with the new version");
                std::process::exit(1);
            }
            if !REJECTED_UPDATE.swap(true, Ordering::Relaxed) {
                log::error(
                    "the app was replaced by a copy not signed like this one; keeping the running version",
                );
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let missing = MISSING_CHECKS.fetch_add(1, Ordering::Relaxed) + 1;
            if missing >= MISSING_CHECKS_BEFORE_DELETED {
                log::notice("the app was deleted; giving the LED back to macOS and stopping");
                if let Some(st) = state().as_ref() {
                    let _ = st.smc.write(Write::System);
                }
                std::process::exit(0);
            }
        }
        // Any other error (a folder renamed mid-update, say): look again next minute.
        Err(_) => {}
    }
}

unsafe extern "C-unwind" fn periodic_tick(_: *mut CFRunLoopTimer, _: *mut c_void) {
    check_own_file();
    enforce("periodic check");
}

/// Plug-in and unplug.
unsafe extern "C-unwind" fn power_source_changed(_: *mut c_void) {
    burst();
}

/// Charge state and level changes; also fires on the short "dark" wakes during sleep.
unsafe extern "C-unwind" fn power_info_changed(_: *mut c_void) {
    enforce("charge state");
}

unsafe extern "C-unwind" fn system_power(
    _: *mut c_void,
    _: io_service_t,
    msg: u32,
    arg: *mut c_void,
) {
    if msg == kIOMessageCanSystemSleep || msg == kIOMessageSystemWillSleep {
        // Never delay sleep.
        IOAllowPowerChange(PM_ROOT.load(Ordering::Relaxed), arg as isize);
    } else if msg == kIOMessageSystemHasPoweredOn {
        burst();
    }
}

/// launchctl bootout sends SIGTERM, Ctrl-C sends SIGINT. Blocks them in every
/// thread and waits for them on one, which gives the LED back to macOS and exits.
fn handle_signals() {
    // SAFETY: plain sigset manipulation on a local value.
    let set = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::sigaddset(&mut set, sig);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, ptr::null_mut());
        set
    };
    std::thread::spawn(move || {
        let mut sig = 0;
        // SAFETY: set and sig are valid for the call.
        while unsafe { libc::sigwait(&set, &mut sig) } != 0 {}
        let guard = state();
        if let Some(st) = guard.as_ref() {
            match st.smc.write(Write::System) {
                Ok(()) => log::notice("stopping, LED given back to macOS"),
                Err(e) => log::error(&format!(
                    "stopping, could not give the LED back to macOS: {e}"
                )),
            }
        }
        std::process::exit(0);
    });
}

fn timer(
    first_in: f64,
    interval: f64,
    callback: unsafe extern "C-unwind" fn(*mut CFRunLoopTimer, *mut c_void),
) -> CFRetained<CFRunLoopTimer> {
    // SAFETY: the callback has the right signature and takes no context.
    unsafe {
        CFRunLoopTimer::new(
            None,
            CFAbsoluteTimeGetCurrent() + first_in,
            interval,
            0,
            0,
            Some(callback),
            ptr::null_mut(),
        )
    }
    .expect("CFRunLoopTimerCreate failed")
}

pub fn run() -> ! {
    // Before any other thread exists, so all of them inherit the blocked mask.
    handle_signals();
    remember_own_file();

    let smc = match Smc::open().and_then(|smc| smc.read().map(|_| smc)) {
        Ok(smc) => smc,
        Err(e @ (Error::NoKey | Error::WrongKeyShape { .. })) => {
            // A clean exit, so launchd doesn't restart it every few seconds.
            log::error(&format!("not starting: {e}"));
            std::process::exit(0);
        }
        Err(e) => {
            log::error(&format!("not starting: {e}"));
            std::process::exit(1);
        }
    };
    *state() = Some(State {
        smc,
        failing: false,
    });

    let run_loop = CFRunLoop::current().expect("no run loop");
    // SAFETY: an immutable CF constant.
    let mode = unsafe { kCFRunLoopDefaultMode };

    let burst_timer = timer(PARKED, BURST_STEP_SECS, burst_tick);
    run_loop.add_timer(Some(&burst_timer), mode);
    BURST_TIMER.with(|t| *t.borrow_mut() = Some(burst_timer));

    let periodic = timer(PERIODIC_SECS, PERIODIC_SECS, periodic_tick);
    run_loop.add_timer(Some(&periodic), mode);

    // SAFETY: the callbacks have the right signature and take no context.
    let power_source =
        unsafe { IOPSCreateLimitedPowerNotification(Some(power_source_changed), ptr::null_mut()) };
    let power_info =
        unsafe { IOPSNotificationCreateRunLoopSource(Some(power_info_changed), ptr::null_mut()) };
    for source in [&power_source, &power_info] {
        match source {
            Some(s) => run_loop.add_source(Some(s), mode),
            None => log::error("cannot register for power-source notifications"),
        }
    }

    let mut port: IONotificationPortRef = ptr::null_mut();
    let mut notifier: io_object_t = 0;
    // SAFETY: valid out-pointers and a callback with the right signature.
    let root = unsafe {
        IORegisterForSystemPower(
            ptr::null_mut(),
            &mut port,
            Some(system_power),
            &mut notifier,
        )
    };
    let _wake_source = if root == 0 {
        log::error("cannot register for wake notifications");
        None
    } else {
        PM_ROOT.store(root, Ordering::Relaxed);
        // SAFETY: port was just returned by IORegisterForSystemPower.
        let src = unsafe { IONotificationPort::run_loop_source(port) };
        run_loop.add_source(src.as_deref(), mode);
        src
    };

    log::notice(concat!(
        "magsafe-dim ",
        env!("CARGO_PKG_VERSION"),
        " started"
    ));
    burst();
    CFRunLoop::run();
    log::error("run loop exited");
    std::process::exit(1);
}
