//! The one SMC key this program touches: `ACLC`, the MagSafe 3 connector LED.
//!
//! Talks to the AppleSMC kernel driver through its user client: one 80-byte
//! struct in, one out. The layout and selectors match smcFanControl's `smc.c`
//! and batt's gosmc.

use std::ffi::{CStr, c_void};
use std::fmt;
use std::mem::size_of;

use objc2_core_foundation::{CFDictionary, CFRetained};
use objc2_io_kit::{
    IOConnectCallStructMethod, IOServiceClose, IOServiceGetMatchingService, IOServiceMatching,
    IOServiceOpen, io_connect_t, kIOReturnNotPrivileged, kIOReturnSuccess,
};

const SELECTOR: u32 = 2;
const CMD_READ_BYTES: u8 = 5;
const CMD_WRITE_BYTES: u8 = 6;
const CMD_READ_KEYINFO: u8 = 9;
const RESULT_KEY_NOT_FOUND: u8 = 132;

const fn four_cc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

/// The LED key, and the only type it may have: one unsigned byte.
const KEY: u32 = four_cc(b"ACLC");
const KEY_TYPE: u32 = four_cc(b"ui8 ");

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Vers {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct PLimit {
    version: u16,
    length: u16,
    cpu: u32,
    gpu: u32,
    mem: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct KeyInfo {
    size: u32,
    kind: u32,
    attributes: u8,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Msg {
    key: u32,
    vers: Vers,
    plimit: PLimit,
    info: KeyInfo,
    result: u8,
    status: u8,
    cmd: u8,
    data32: u32,
    bytes: [u8; 32],
}

const _: () = assert!(size_of::<Msg>() == 80, "AppleSMC expects an 80-byte struct");

/// The `ACLC` values whose meaning is known, and the only values this program
/// can write: `Smc::write` takes a `Led`, so nothing else can reach the SMC.
/// Each has a name, the word the command line prints and accepts (and the app
/// reads), and a description for people. The values come from batt and magled,
/// which agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Led {
    /// macOS decides. Read back, the key then shows macOS's own choice instead.
    System = 0x00,
    Off = 0x01,
    Green = 0x03,
    Amber = 0x04,
    BlinkOnce = 0x05,
    /// batt names 0x06 and 0x07 slow and fast blinking, but filmed on a MacBook
    /// Pro M5 Pro both blink identically (0.5 s on, 1 s off), so only 0x06 is offered.
    Blink = 0x06,
}

impl Led {
    pub const ALL: [Led; 6] = [
        Led::System,
        Led::Off,
        Led::Green,
        Led::Amber,
        Led::BlinkOnce,
        Led::Blink,
    ];

    pub fn from_value(v: u8) -> Option<Led> {
        Led::ALL.into_iter().find(|led| *led as u8 == v)
    }

    pub fn from_name(name: &str) -> Option<Led> {
        Led::ALL.into_iter().find(|led| led.name() == name)
    }

    pub fn name(self) -> &'static str {
        match self {
            Led::System => "system",
            Led::Off => "off",
            Led::Green => "green",
            Led::Amber => "amber",
            Led::BlinkOnce => "blink-once",
            Led::Blink => "blink",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Led::System => "macOS decides",
            Led::Off => "off",
            Led::Green => "green",
            Led::Amber => "amber",
            Led::BlinkOnce => "amber, blinks three times, then dark",
            Led::Blink => "amber, blinking",
        }
    }
}

/// The name of a raw `ACLC` value, or "unknown".
pub fn name(v: u8) -> &'static str {
    Led::from_value(v).map_or("unknown", Led::name)
}

/// What a raw `ACLC` value shows, for people, or "unknown".
pub fn describe(v: u8) -> &'static str {
    Led::from_value(v).map_or("unknown", Led::description)
}

#[derive(Debug)]
pub enum Error {
    NoSmc,
    NotPermitted,
    NoKey,
    WrongKeyShape { size: u32, kind: u32 },
    Call(i32),
    Smc(u8),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoSmc => write!(f, "cannot open the AppleSMC driver"),
            Error::NotPermitted => write!(f, "not permitted (needs root)"),
            Error::NoKey => write!(f, "this Mac has no ACLC key (no MagSafe 3 LED?)"),
            Error::WrongKeyShape { size, kind } => write!(
                f,
                "ACLC is {size} byte(s) of type '{}', expected 1 byte of 'ui8 '; refusing to touch it",
                String::from_utf8_lossy(&kind.to_be_bytes())
            ),
            Error::Call(kr) => write!(f, "SMC call failed (0x{kr:08x})"),
            Error::Smc(r) => write!(f, "SMC returned error {r}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

unsafe extern "C" {
    static mach_task_self_: libc::mach_port_t;
}

pub struct Smc {
    conn: io_connect_t,
}

impl Smc {
    pub fn open() -> Result<Smc> {
        const NAME: &CStr = c"AppleSMC";
        // SAFETY: NAME is a valid C string. IOServiceGetMatchingService consumes
        // the matching dictionary, and 0 is kIOMainPortDefault.
        let svc = unsafe {
            let matching = IOServiceMatching(NAME.as_ptr()).ok_or(Error::NoSmc)?;
            let matching: CFRetained<CFDictionary> = CFRetained::cast_unchecked(matching);
            IOServiceGetMatchingService(0, Some(matching))
        };
        if svc == 0 {
            return Err(Error::NoSmc);
        }
        let mut conn: io_connect_t = 0;
        // SAFETY: svc is a valid service; conn is a valid out-pointer.
        let kr = unsafe { IOServiceOpen(svc, mach_task_self_, 0, &mut conn) };
        // We own the reference IOServiceGetMatchingService returned.
        objc2_io_kit::IOObjectRelease(svc);
        if kr != kIOReturnSuccess {
            return Err(Error::NoSmc);
        }
        Ok(Smc { conn })
    }

    fn call(&self, input: &Msg) -> Result<Msg> {
        let mut out = Msg::default();
        let mut out_size = size_of::<Msg>();
        // SAFETY: both pointers point at live 80-byte Msg values, and the sizes match.
        let kr = unsafe {
            IOConnectCallStructMethod(
                self.conn,
                SELECTOR,
                (input as *const Msg).cast::<c_void>(),
                size_of::<Msg>(),
                (&mut out as *mut Msg).cast::<c_void>(),
                &mut out_size,
            )
        };
        if kr as u32 == kIOReturnNotPrivileged {
            return Err(Error::NotPermitted);
        }
        if kr != kIOReturnSuccess {
            return Err(Error::Call(kr));
        }
        match out.result {
            0 => Ok(out),
            RESULT_KEY_NOT_FOUND => Err(Error::NoKey),
            r => Err(Error::Smc(r)),
        }
    }

    /// Checks that ACLC exists and is one `ui8 ` byte, so a write can't land anywhere else.
    fn check_key(&self) -> Result<()> {
        let out = self.call(&Msg {
            key: KEY,
            cmd: CMD_READ_KEYINFO,
            ..Msg::default()
        })?;
        let (size, kind) = (out.info.size, out.info.kind);
        if size != 1 || kind != KEY_TYPE {
            return Err(Error::WrongKeyShape { size, kind });
        }
        Ok(())
    }

    pub fn read(&self) -> Result<u8> {
        self.check_key()?;
        let input = Msg {
            key: KEY,
            cmd: CMD_READ_BYTES,
            info: KeyInfo {
                size: 1,
                ..KeyInfo::default()
            },
            ..Msg::default()
        };
        Ok(self.call(&input)?.bytes[0])
    }

    pub fn write(&self, value: Led) -> Result<()> {
        self.check_key()?;
        let mut input = Msg {
            key: KEY,
            cmd: CMD_WRITE_BYTES,
            info: KeyInfo {
                size: 1,
                ..KeyInfo::default()
            },
            ..Msg::default()
        };
        input.bytes[0] = value as u8;
        self.call(&input).map(|_| ())
    }
}

impl Drop for Smc {
    fn drop(&mut self) {
        IOServiceClose(self.conn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;

    #[test]
    fn layout_matches_the_c_struct() {
        assert_eq!(offset_of!(Msg, vers), 4);
        assert_eq!(offset_of!(Msg, plimit), 12);
        assert_eq!(offset_of!(Msg, info), 28);
        assert_eq!(offset_of!(Msg, result), 40);
        assert_eq!(offset_of!(Msg, cmd), 42);
        assert_eq!(offset_of!(Msg, data32), 44);
        assert_eq!(offset_of!(Msg, bytes), 48);
    }

    #[test]
    fn four_cc_is_big_endian() {
        assert_eq!(KEY, 0x41434C43);
        assert_eq!(KEY_TYPE, 0x75693820);
    }

    #[test]
    fn names_and_values_round_trip() {
        for led in Led::ALL {
            assert_eq!(Led::from_value(led as u8), Some(led));
            assert_eq!(Led::from_name(led.name()), Some(led));
        }
        assert_eq!(Led::from_value(0x02), None);
        assert_eq!(name(0x02), "unknown");
    }
}
