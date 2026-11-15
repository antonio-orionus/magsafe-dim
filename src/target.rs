//! The LED state the helper keeps: off unless chosen otherwise with `magsafe-dim set`.
//!
//! The choice is one word, a `Led` name, in one file. A file needs no connection
//! into the root helper, and the choice survives app updates and restarts.
//!
//! - The helper (root) creates the folder, owned by root and writable by no one
//!   else, and the file in it, owned by root and writable by the admin group. So
//!   an admin user's programs can change the word but can't create, replace,
//!   move or link the file.
//! - Only the first word counts, and only if it is a `Led` name. A writer
//!   overwrites in place and then truncates, so a reader never sees an empty file
//!   and a longer old word leaves only trailing junk behind the new one.
//! - No file means off. A file without a valid word is ignored: the helper keeps
//!   the state it had.

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use crate::smc::Led;

pub const DIR: &str = "/Library/Application Support/MagSafe Dim";
pub const FILE: &str = "/Library/Application Support/MagSafe Dim/led";

/// The `Led` named by the first word of `text`, if it names one.
pub fn parse(text: &str) -> Option<Led> {
    Led::from_name(text.split_whitespace().next()?)
}

/// Opens the file without following a link.
pub fn open(path: &Path, write: bool) -> io::Result<File> {
    OpenOptions::new()
        .read(!write)
        .write(write)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

/// The chosen state: `Ok(None)` when the file holds no valid word. A missing
/// file is an error of kind `NotFound`; callers treat it as off.
pub fn read(path: &Path) -> io::Result<Option<Led>> {
    let mut text = String::new();
    // A file this small is read whole; anything longer isn't ours.
    open(path, false)?.take(64).read_to_string(&mut text)?;
    Ok(parse(&text))
}

/// Writes `led` as the chosen state. Doesn't create the file: the helper does.
pub fn write(path: &Path, led: Led) -> io::Result<()> {
    let mut file = open(path, true)?;
    let text = format!("{}\n", led.name());
    file.rewind()?;
    file.write_all(text.as_bytes())?;
    file.set_len(text.len() as u64)
}

/// For the helper, as root: creates the folder and the file if missing and sets
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

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o664)
        .custom_flags(libc::O_NOFOLLOW)
        .open(FILE)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != 0 {
        return Err(io::Error::other(format!(
            "{FILE} is not a file owned by root"
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
        assert_eq!(parse("green\n"), Some(Led::Green));
        // "blink-slow\n" overwritten with "off\n" before the truncate.
        assert_eq!(parse("off\nk-slow\n"), Some(Led::Off));
        assert_eq!(parse(""), None);
        assert_eq!(parse("purple"), None);
        assert_eq!(parse("Green"), None);
    }

    #[test]
    fn write_then_read() {
        let dir = std::env::temp_dir().join(format!("magsafe-dim-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("led");
        assert_eq!(read(&path).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(
            write(&path, Led::Off).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        fs::write(&path, "blink-slow\n").unwrap();
        write(&path, Led::Off).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "off\n");
        assert_eq!(read(&path).unwrap(), Some(Led::Off));
        let link = dir.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read(&link).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
