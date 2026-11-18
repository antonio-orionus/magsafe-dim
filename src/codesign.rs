//! Is a file signed like this program? Checked before restarting into an
//! updated copy of the app, so the root helper never hurries into running a
//! file that someone else put there.
//!
//! This program's own requirement is read once, at startup: an update moves the
//! old app away and deletes it, and after that macOS can no longer read the
//! running program's signature (it fails with "no such file").
//!
//! Also says whether this program is signed with a given Developer ID team, which
//! keeps builds from source off the heartbeat collector (heartbeat.rs).

use std::ffi::c_void;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::sync::OnceLock;

type CFTypeRef = *const c_void;
type OSStatus = i32;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecCodeCopySelf(flags: u32, code: *mut CFTypeRef) -> OSStatus;
    fn SecCodeCopyDesignatedRequirement(
        code: CFTypeRef,
        flags: u32,
        requirement: *mut CFTypeRef,
    ) -> OSStatus;
    fn SecStaticCodeCreateWithPath(path: CFTypeRef, flags: u32, code: *mut CFTypeRef) -> OSStatus;
    fn SecStaticCodeCheckValidity(code: CFTypeRef, flags: u32, requirement: CFTypeRef) -> OSStatus;
    fn SecCodeCheckValidity(code: CFTypeRef, flags: u32, requirement: CFTypeRef) -> OSStatus;
    fn SecRequirementCreateWithString(
        text: CFTypeRef,
        flags: u32,
        requirement: *mut CFTypeRef,
    ) -> OSStatus;
    fn SecRequirementCopyData(requirement: CFTypeRef, flags: u32, data: *mut CFTypeRef)
    -> OSStatus;
    fn SecRequirementCreateWithData(
        data: CFTypeRef,
        flags: u32,
        requirement: *mut CFTypeRef,
    ) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: CFTypeRef,
        buffer: *const u8,
        length: isize,
        is_directory: u8,
    ) -> CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFDataCreate(allocator: CFTypeRef, bytes: *const u8, length: isize) -> CFTypeRef;
    fn CFDataGetLength(data: CFTypeRef) -> isize;
    fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
    fn CFStringCreateWithBytes(
        allocator: CFTypeRef,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        external: u8,
    ) -> CFTypeRef;
}

const UTF8: u32 = 0x0800_0100;

/// A CoreFoundation object this code owns (+1), released on drop.
struct Owned(CFTypeRef);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own exactly one reference to a valid CF object.
            unsafe { CFRelease(self.0) };
        }
    }
}

/// Creates an owned CF object through an out-parameter API, or None on failure.
fn create(f: impl FnOnce(*mut CFTypeRef) -> OSStatus) -> Option<Owned> {
    let mut out: CFTypeRef = ptr::null();
    let status = f(&mut out);
    (status == 0 && !out.is_null()).then_some(Owned(out))
}

/// This program's designated requirement, as `SecRequirementCopyData` bytes.
static REQUIREMENT: OnceLock<Vec<u8>> = OnceLock::new();

/// Reads this program's designated requirement while its own file is still in
/// place, for `signed_like_this_program`. Call it at startup. False if it can't
/// be read; then no copy counts as signed like this program.
pub fn remember_this_program() -> bool {
    if REQUIREMENT.get().is_some() {
        return true;
    }
    // SAFETY: each call gets valid pointers; every returned object is owned and
    // released, and the data's bytes are copied while it is alive.
    let bytes = unsafe {
        let Some(me) = create(|out| SecCodeCopySelf(0, out)) else {
            return false;
        };
        let Some(requirement) = create(|out| SecCodeCopyDesignatedRequirement(me.0, 0, out)) else {
            return false;
        };
        let Some(data) = create(|out| SecRequirementCopyData(requirement.0, 0, out)) else {
            return false;
        };
        std::slice::from_raw_parts(CFDataGetBytePtr(data.0), CFDataGetLength(data.0) as usize)
            .to_vec()
    };
    let _ = REQUIREMENT.set(bytes);
    true
}

/// True if `path` is validly signed and meets this program's designated
/// requirement, as `remember_this_program` read it: for a Developer ID build, the
/// same identifier and Team ID. An ad-hoc build's requirement is its exact code
/// hash, so no other build passes.
pub fn signed_like_this_program(path: &Path) -> bool {
    let Some(remembered) = REQUIREMENT.get() else {
        return false;
    };
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: each call gets valid pointers; every returned object is owned and released.
    unsafe {
        let data = Owned(CFDataCreate(
            ptr::null(),
            remembered.as_ptr(),
            remembered.len() as isize,
        ));
        if data.0.is_null() {
            return false;
        }
        let Some(requirement) = create(|out| SecRequirementCreateWithData(data.0, 0, out)) else {
            return false;
        };
        let url = Owned(CFURLCreateFromFileSystemRepresentation(
            ptr::null(),
            bytes.as_ptr(),
            bytes.len() as isize,
            0,
        ));
        if url.0.is_null() {
            return false;
        }
        let Some(code) = create(|out| SecStaticCodeCreateWithPath(url.0, 0, out)) else {
            return false;
        };
        SecStaticCodeCheckValidity(code.0, 0, requirement.0) == 0
    }
}

/// True if this program is validly signed with a Developer ID certificate of
/// team `team`. False for ad-hoc and other signatures.
pub fn signed_by_team(team: &str) -> bool {
    if !team.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return false;
    }
    let text = format!(
        "anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] \
         and certificate leaf[subject.OU] = \"{team}\""
    );
    // SAFETY: each call gets valid pointers; every returned object is owned and released.
    unsafe {
        let string = Owned(CFStringCreateWithBytes(
            ptr::null(),
            text.as_ptr(),
            text.len() as isize,
            UTF8,
            0,
        ));
        if string.0.is_null() {
            return false;
        }
        let Some(requirement) = create(|out| SecRequirementCreateWithString(string.0, 0, out))
        else {
            return false;
        };
        let Some(me) = create(|out| SecCodeCopySelf(0, out)) else {
            return false;
        };
        SecCodeCheckValidity(me.0, 0, requirement.0) == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_test_binary_matches_itself() {
        assert!(remember_this_program());
        let me = std::env::current_exe().unwrap();
        assert!(signed_like_this_program(&me));
    }

    #[test]
    fn another_binary_does_not_match() {
        assert!(remember_this_program());
        assert!(!signed_like_this_program(Path::new("/bin/ls")));
    }

    /// The update case: this program keeps running while its own file is
    /// replaced by a copy signed the same way. Runs itself as a child process,
    /// replaces the child's file, then lets the child check the new one.
    #[test]
    fn matches_after_its_own_file_was_replaced() {
        const CHILD: &str = "MAGSAFE_DIM_REPLACED_SELF";
        const NAME: &str = "codesign::tests::matches_after_its_own_file_was_replaced";
        let wait_for = |file: &Path| {
            for _ in 0..200 {
                if file.exists() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            panic!("timed out waiting for {}", file.display());
        };
        if let Ok(bin) = std::env::var(CHILD) {
            let bin = Path::new(&bin);
            assert!(remember_this_program());
            std::fs::write(bin.with_extension("ready"), "").unwrap();
            wait_for(&bin.with_extension("replaced"));
            assert!(
                signed_like_this_program(bin),
                "the replaced copy should match"
            );
            return;
        }
        let dir = std::env::temp_dir().join(format!("magsafe-dim-codesign-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Like an app update: the folder holding the running program is moved
        // away and deleted, and a new copy takes its place.
        let app = dir.join("app");
        std::fs::create_dir_all(&app).unwrap();
        let bin = app.join("bin");
        let me = std::env::current_exe().unwrap();
        std::fs::copy(&me, &bin).unwrap();
        let mut child = std::process::Command::new(&bin)
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(CHILD, &bin)
            .spawn()
            .unwrap();
        wait_for(&bin.with_extension("ready"));
        let old = dir.join("old");
        std::fs::rename(&app, &old).unwrap();
        std::fs::remove_dir_all(&old).unwrap();
        std::fs::create_dir_all(&app).unwrap();
        std::fs::copy(&me, &bin).unwrap();
        std::fs::write(bin.with_extension("replaced"), "").unwrap();
        let status = child.wait().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(status.success(), "the child's check failed");
    }

    #[test]
    fn an_unsigned_test_binary_has_no_team() {
        assert!(!signed_by_team("LDDHTN9932"));
        assert!(!signed_by_team("bad\" or anchor apple"));
    }

    #[test]
    fn a_missing_file_does_not_match() {
        assert!(!signed_like_this_program(Path::new(
            "/nonexistent/magsafe-dim"
        )));
    }
}
