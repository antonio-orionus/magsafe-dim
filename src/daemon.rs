//! `magsafe-dim run`: keep the LED in the chosen state until stopped: off,
//! unless `magsafe-dim set` chose another (see target.rs).
//!
//! macOS and the SMC rewrite ACLC on power events (plug-in, charge-state
//! changes, sleep and wake). This waits for those events on a CoreFoundation
//! run loop and sets the LED again. Nothing runs between events except a
//! once-a-minute check of the LED and of whether the app was updated or deleted,
//! and a thread that waits for the chosen state to change.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
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
use crate::smc::{self, Error, Led, Smc};
use crate::target;

/// After a plug-in or a wake the SMC resets the LED in short bursts and ignores
/// writes meanwhile, so keep checking for a few seconds. A new event restarts it.
const BURST_STEP_SECS: f64 = 0.25;
const BURST_STEPS: u32 = 20;
const PERIODIC_SECS: f64 = 60.0;
const PARKED: f64 = 1.0e10;

struct State {
    smc: Smc,
    /// The chosen state.
    target: Led,
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

/// Whether the helper keeps `led` in place, setting it again whenever the SMC
/// changes it. `System` reads back as macOS's own choice, so it would never
/// match, and `BlinkOnce` (three blinks, then dark, while the key still reads
/// 0x05) would blink again at every check: those two are set
/// once, when chosen, and then left alone.
fn kept(led: Led) -> bool {
    !matches!(led, Led::System | Led::BlinkOnce)
}

/// Reads the LED and sets it to `target` if it differs. Writes only on a mismatch.
fn enforce_once(smc: &Smc, target: Led, reason: &str) -> smc::Result<()> {
    let v = smc.read()?;
    if v == target as u8 {
        return Ok(());
    }
    smc.write(target)?;
    log::notice(&format!(
        "{reason}: LED was {} (0x{v:02x}), set it to {}",
        smc::describe(v),
        target.description()
    ));
    Ok(())
}

fn enforce(reason: &str) {
    enforce_with(reason, false);
}

/// `just_chosen` sets the LED even to a state that isn't kept.
fn enforce_with(reason: &str, just_chosen: bool) {
    let mut guard = state();
    let Some(st) = guard.as_mut() else { return };
    let target = st.target;
    if !kept(target) && !just_chosen {
        return;
    }
    let result = enforce_once(&st.smc, target, reason).or_else(|_| {
        // The SMC connection can go stale across sleep: reopen it and try once more.
        st.smc = Smc::open()?;
        enforce_once(&st.smc, target, reason)
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
                log::error(&format!(
                    "{reason}: cannot set the LED to {}: {e}",
                    target.description()
                ));
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

/// Makes `led` the state to keep, and sets the LED to it at once.
fn choose(led: Led) {
    {
        let mut guard = state();
        let Some(st) = guard.as_mut() else { return };
        if st.target == led {
            return;
        }
        st.target = led;
    }
    log::notice(&format!(
        "chosen with magsafe-dim set: {}",
        led.description()
    ));
    enforce_with("new state", true);
}

/// Follows the chosen state: on a thread of its own, waits for the file to
/// change and reads it again. A file without a valid word changes nothing.
fn watch_target() {
    let file = match target::open(Path::new(target::FILE), false) {
        Ok(file) => file,
        Err(e) => {
            log::error(&format!("cannot follow {}: {e}", target::FILE));
            return;
        }
    };
    std::thread::spawn(move || {
        // SAFETY: kqueue has no preconditions.
        let kq = unsafe { libc::kqueue() };
        let change = libc::kevent {
            ident: file.as_raw_fd() as usize,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: libc::NOTE_WRITE
                | libc::NOTE_EXTEND
                | libc::NOTE_ATTRIB
                | libc::NOTE_DELETE
                | libc::NOTE_RENAME,
            data: 0,
            udata: ptr::null_mut(),
        };
        // SAFETY: change is a valid event and the output list is empty.
        if kq < 0 || unsafe { libc::kevent(kq, &change, 1, ptr::null_mut(), 0, ptr::null()) } < 0 {
            log::error(&format!(
                "cannot follow {}: {}",
                target::FILE,
                io::Error::last_os_error()
            ));
            return;
        }
        loop {
            // SAFETY: an all-zero kevent is a valid value to be overwritten.
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            // SAFETY: event is a valid one-element output list.
            let n = unsafe { libc::kevent(kq, ptr::null(), 0, &mut event, 1, ptr::null()) };
            if n < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                log::error(&format!("stopped following {}", target::FILE));
                return;
            }
            if event.fflags & (libc::NOTE_DELETE | libc::NOTE_RENAME) != 0 {
                // Only root can remove it: the folder is root's.
                log::error(&format!(
                    "{} was removed; keeping the current state until the next start",
                    target::FILE
                ));
                return;
            }
            if let Ok(Some(led)) = target::read(Path::new(target::FILE)) {
                choose(led);
            }
        }
    });
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
    if !codesign::remember_this_program() {
        log::error("cannot read this program's signature; app updates take effect after a restart");
    }
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
///   to macOS, remove the state folder and exit cleanly, so launchd doesn't
///   start it again.
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
                    let _ = st.smc.write(Led::System);
                }
                let _ = fs::remove_dir_all(target::DIR);
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
            match st.smc.write(Led::System) {
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
    // Missing, unreadable or without a valid word: off.
    let follow = match target::prepare() {
        Ok(()) => true,
        Err(e) => {
            log::error(&format!(
                "cannot use {} ({e}); keeping the LED off",
                target::FILE
            ));
            false
        }
    };
    let target = if follow {
        target::read(Path::new(target::FILE))
            .ok()
            .flatten()
            .unwrap_or(Led::Off)
    } else {
        Led::Off
    };
    *state() = Some(State {
        smc,
        target,
        failing: false,
    });
    if follow {
        watch_target();
    }

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
