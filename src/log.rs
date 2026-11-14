//! Logging through syslog(3), which macOS routes into the unified log:
//! `log show --predicate 'process == "magsafe-dim"'`.

use std::ffi::CString;

pub fn init(also_stderr: bool) {
    let flags = libc::LOG_PID | if also_stderr { libc::LOG_PERROR } else { 0 };
    // SAFETY: the ident is a 'static C string, as openlog requires.
    unsafe { libc::openlog(c"magsafe-dim".as_ptr(), flags, libc::LOG_DAEMON) };
}

fn emit(level: libc::c_int, msg: &str) {
    let Ok(msg) = CString::new(msg.replace('\0', "")) else {
        return;
    };
    // SAFETY: a constant "%s" format with one C-string argument.
    unsafe { libc::syslog(level, c"%s".as_ptr(), msg.as_ptr()) };
}

pub fn notice(msg: &str) {
    emit(libc::LOG_NOTICE, msg);
}

pub fn error(msg: &str) {
    emit(libc::LOG_ERR, msg);
}
