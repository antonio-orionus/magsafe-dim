//! The helper's settings, one word per file in one folder: the LED state it keeps
//! (off unless chosen otherwise with `magsafe-dim set`) and whether it sends the
//! daily heartbeat. A file needs no connection into the root helper, and the
//! settings survive app updates and restarts.
//!
//! - The helper (root) creates the folder, owned by root and writable by no one
//!   else, and the file in it, owned by root and writable by the admin group. So
//!   an admin user's programs can change the word but can't create, replace,
//!   move or link the file.
//! - Only the first word counts, and only if it is one the file takes. A writer
//!   overwrites in place and then truncates, so a reader never sees an empty file
//!   and a longer old word leaves only trailing junk behind the new one.
//! - No `led` file means off, no `heartbeat` file means on. A file without a
//!   valid word is ignored: the helper keeps what it had.

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use crate::smc::Led;

pub const DIR: &str = "/Library/Application Support/MagSafe Dim";
pub const FILE: &str = "/Library/Application Support/MagSafe Dim/led";
/// `on` or `off`: whether the helper sends the daily heartbeat (heartbeat.rs).
pub const HEARTBEAT_FILE: &str = "/Library/Application Support/MagSafe Dim/heartbeat";
/// Root's alone: day or night as the helper keeps it now, while it keeps `night`
/// (`sun::Schedule::line`), so `magsafe-dim sun` and the app show its view.
pub const SUN_FILE: &str = "/Library/Application Support/MagSafe Dim/sun";

/// What the helper keeps: one LED state all the time, or off at night only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Led(Led),
    /// Off from sunset to sunrise, macOS's own choice by day (sun.rs).
    Night,
}

impl Choice {
    pub fn from_name(name: &str) -> Option<Choice> {
        match name {
            "night" => Some(Choice::Night),
            _ => Led::from_name(name).map(Choice::Led),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Choice::Led(led) => led.name(),
            Choice::Night => "night",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Choice::Led(led) => led.description(),
            Choice::Night => "off from sunset to sunrise (macOS decides by day)",
        }
    }
}

/// The `Choice` named by the first word of `text`, if it names one.
pub fn parse(text: &str) -> Option<Choice> {
    Choice::from_name(text.split_whitespace().next()?)
}

/// The heartbeat switch in `text`: `on`, `off`, or None for anything else.
pub fn parse_switch(text: &str) -> Option<bool> {
    match text.split_whitespace().next()? {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

/// Opens the file without following a link.
pub fn open(path: &Path, write: bool) -> io::Result<File> {
    OpenOptions::new()
        .read(!write)
        .write(write)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

fn read_text(path: &Path) -> io::Result<String> {
    let mut text = String::new();
    // A file this small is read whole; anything longer isn't ours.
    open(path, false)?.take(64).read_to_string(&mut text)?;
    Ok(text)
}

/// The chosen state: `Ok(None)` when the file holds no valid word. A missing
/// file is an error of kind `NotFound`; callers treat it as off.
pub fn read(path: &Path) -> io::Result<Option<Choice>> {
    Ok(parse(&read_text(path)?))
}

/// The heartbeat switch: on unless the file says `off`.
pub fn heartbeat_on(path: &Path) -> bool {
    read_text(path)
        .ok()
        .and_then(|text| parse_switch(&text))
        .unwrap_or(true)
}

/// Writes `word` over the file's first word. Doesn't create the file: the helper does.
pub fn write_word(path: &Path, word: &str) -> io::Result<()> {
    let mut file = open(path, true)?;
    let text = format!("{word}\n");
    file.rewind()?;
    file.write_all(text.as_bytes())?;
    file.set_len(text.len() as u64)
}

/// For the helper, as root: replaces one of its own files in the folder with
/// `text`, readable by everyone. The folder is root's, so a rename is safe.
pub fn write_private(path: &str, text: &str) -> io::Result<()> {
    let tmp = format!("{path}.new");
    let _ = fs::remove_file(&tmp);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    file.write_all(text.as_bytes())?;
    fs::rename(&tmp, path)
}

/// For the helper, as root: creates the folder and the files if missing and sets
/// their owners and modes. Fails if the folder is anything but a directory
/// owned by root, so the helper never trusts a file someone else could replace.
pub fn prepare() -> io::Result<()> {
    match fs::create_dir(DIR) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    let dir = fs::symlink_metadata(DIR)?;
    if !dir.is_dir() || dir.uid() != 0 {
        return Err(io::Error::other(format!(
            "{DIR} is not a folder owned by root"
        )));
    }
    fs::set_permissions(DIR, fs::Permissions::from_mode(0o755))?;
    prepare_file(FILE)?;
    prepare_file(HEARTBEAT_FILE)
}

/// Creates `path` if missing: owned by root, writable by the admin group.
fn prepare_file(path: &str) -> io::Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o664)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != 0 {
        return Err(io::Error::other(format!(
            "{path} is not a file owned by root"
        )));
    }
    std::os::unix::fs::fchown(&file, Some(0), Some(admin_gid()?))?;
    file.set_permissions(fs::Permissions::from_mode(0o664))
}

fn admin_gid() -> io::Result<libc::gid_t> {
    let name = CString::new("admin").expect("no NUL");
    // SAFETY: name is a valid C string; the returned record is read at once.
    let group = unsafe { libc::getgrnam(name.as_ptr()) };
    if group.is_null() {
        return Err(io::Error::other("no admin group"));
    }
    // SAFETY: group is non-null and points at a valid record.
    Ok(unsafe { (*group).gr_gid })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_first_word_counts() {
        assert_eq!(parse("green\n"), Some(Choice::Led(Led::Green)));
        // "blink-slow\n" overwritten with "off\n" before the truncate.
        assert_eq!(parse("off\nk-slow\n"), Some(Choice::Led(Led::Off)));
        assert_eq!(parse("night\n"), Some(Choice::Night));
        assert_eq!(parse(""), None);
        assert_eq!(parse("purple"), None);
        assert_eq!(parse("Green"), None);
        assert_eq!(parse_switch("off\nn\n"), Some(false));
        assert_eq!(parse_switch("on"), Some(true));
        assert_eq!(parse_switch("yes"), None);
    }

    #[test]
    fn write_then_read() {
        let dir = std::env::temp_dir().join(format!("magsafe-dim-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("led");
        assert_eq!(read(&path).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(
            write_word(&path, "off").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(heartbeat_on(&path));
        fs::write(&path, "blink-slow\n").unwrap();
        write_word(&path, "off").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "off\n");
        assert_eq!(read(&path).unwrap(), Some(Choice::Led(Led::Off)));
        assert!(!heartbeat_on(&path));
        write_word(&path, "night").unwrap();
        assert_eq!(read(&path).unwrap(), Some(Choice::Night));
        assert!(heartbeat_on(&path));
        let link = dir.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read(&link).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
