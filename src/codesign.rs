//! Is a file signed like this program? Checked before restarting into an
//! updated copy of the app, so the root helper never hurries into running a
//! file that someone else put there.

use std::ffi::c_void;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

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
}

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

/// True if `path` is validly signed and meets this program's designated
/// requirement: for a Developer ID build, the same identifier and Team ID.
/// An ad-hoc build's requirement is its exact code hash, so no other build passes.
pub fn signed_like_this_program(path: &Path) -> bool {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: each call gets valid pointers; every returned object is owned and released.
    unsafe {
        let Some(me) = create(|out| SecCodeCopySelf(0, out)) else {
            return false;
        };
        let Some(requirement) = create(|out| SecCodeCopyDesignatedRequirement(me.0, 0, out)) else {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_test_binary_matches_itself() {
        let me = std::env::current_exe().unwrap();
        assert!(signed_like_this_program(&me));
    }

    #[test]
    fn another_binary_does_not_match() {
        assert!(!signed_like_this_program(Path::new("/bin/ls")));
    }

    #[test]
    fn a_missing_file_does_not_match() {
        assert!(!signed_like_this_program(Path::new(
            "/nonexistent/magsafe-dim"
        )));
    }
}
