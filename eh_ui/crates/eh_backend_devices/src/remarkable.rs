//! reMarkable — detection + wiring, ported from KOReader
//! `frontend/device/remarkable/device.lua` and FBInk's
//! `mxcfb-remarkable.h`.
//!
//! Detection reads `/sys/devices/soc0/machine` (KOReader `getModel()`):
//!
//! | machine string | model | panel path |
//! |---|---|---|
//! | `Google. InkCross` | reMarkable 1 | direct fb0 + MXCFB V2 |
//! | `reMarkable 2.0` | reMarkable 2 | needs the rm2fb server (ddvk/remarkable2-framebuffer) — the panel sits behind a secure coprocessor |
//! | `reMarkable Ferrari` / `Chiappa` / `Tatsu` | reMarkable Paper Pro | needs the qtfb shim |
//!
//! Like KOReader — which errors out with "reMarkable 2 requires a RM2FB
//! server" — the shimmed models are detected and refused rather than
//! mis-driven.
//!
//! Input (KOReader remarkable `event_map.lua`): the Wacom digitizer is the
//! primary pointer (raw ranges scaled onto the screen — KOReader's
//! `wacom_scale_x/y`), the multitouch panel reports in screen coordinates,
//! and gpio-keys carry Home / page rocker / Power / Resume.

use crate::{wire, Device};
use eh_backend_linuxfb::{LinuxFb, MxcFlavor, TouchQuirks, REMARKABLE_BUTTONS};

/// A detected reMarkable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remarkable {
    /// reMarkable 1 ("Google. InkCross") — directly drivable.
    Rm1,
    /// reMarkable 2 — needs the rm2fb userspace server.
    Rm2,
    /// reMarkable Paper Pro family — needs the qtfb shim.
    Rmpp,
}

impl Remarkable {
    /// Detect via `/sys/devices/soc0/machine` (KOReader `getModel()`).
    pub fn detect() -> Result<Self, Error> {
        let machine = std::fs::read_to_string("/sys/devices/soc0/machine")
            .map_err(|_| Error::NotARemarkable)?;
        Self::from_machine(machine.trim())
    }

    /// Machine string → model (KOReader's table).  Trims the sysfs
    /// trailing newline so callers can pass the raw read.
    pub fn from_machine(machine: &str) -> Result<Self, Error> {
        match machine.trim() {
            "Google. InkCross" => Ok(Self::Rm1),
            "reMarkable 2.0" => Ok(Self::Rm2),
            "reMarkable Ferrari" | "reMarkable Chiappa" | "reMarkable Tatsu" => Ok(Self::Rmpp),
            _ => Err(Error::NotARemarkable),
        }
    }

    /// Open the framebuffer + input.  Only the reMarkable 1 drives fb0
    /// directly; the others name the shim they need (KOReader parity).
    pub fn open(&self, fb_path: &str) -> std::io::Result<Device> {
        match self {
            Self::Rm1 => {
                let fb = LinuxFb::open(fb_path)?;
                // The digitizer is the primary pointer; the touch panel
                // reports untransformed screen coordinates (KOReader
                // applies no swap/mirror on reMarkable).
                let quirks = TouchQuirks {
                    switch_xy: false,
                    mirrored_x: false,
                    mirrored_y: false,
                    main_slot: 0,
                };
                wire(
                    fb,
                    MxcFlavor::V2,
                    quirks,
                    REMARKABLE_BUTTONS,
                    &crate::RM_INPUT_PATHS,
                    "reMarkable 1",
                )
            }
            Self::Rm2 => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "reMarkable 2 requires a RM2FB server \
                 (github.com/ddvk/remarkable2-framebuffer) — the panel is \
                 behind a secure coprocessor",
            )),
            Self::Rmpp => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "reMarkable Paper Pro requires the qtfb shim for framebuffer access",
            )),
        }
    }
}

/// Detection failure reasons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// No reMarkable machine string (not a reMarkable board).
    NotARemarkable,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NotARemarkable => {
                write!(f, "/sys/devices/soc0/machine is not a reMarkable board")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_strings_match_koreader() {
        assert_eq!(
            Remarkable::from_machine("Google. InkCross").unwrap(),
            Remarkable::Rm1
        );
        assert_eq!(
            Remarkable::from_machine("reMarkable 2.0").unwrap(),
            Remarkable::Rm2
        );
        assert_eq!(
            Remarkable::from_machine("reMarkable Ferrari").unwrap(),
            Remarkable::Rmpp
        );
        // Trailing newline (sysfs) is trimmed inside from_machine.
        assert_eq!(
            Remarkable::from_machine("Google. InkCross\n").unwrap(),
            Remarkable::Rm1
        );
        assert!(Remarkable::from_machine("Pine64 PineNote").is_err());
    }

    #[test]
    fn rm2_and_rmpp_name_their_shims() {
        let err = Remarkable::Rm2.open("/dev/fb0").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("RM2FB"));
        let err = Remarkable::Rmpp.open("/dev/fb0").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("qtfb"));
    }

    #[test]
    fn rm_buttons_match_event_map() {
        // event_map.lua: 102 Home, 105 LPgBack, 106 RPgFwd, 116 Power.
        let keys: std::collections::HashSet<u16> =
            REMARKABLE_BUTTONS.iter().map(|(c, _)| *c).collect();
        assert!(keys.contains(&102) && keys.contains(&105) && keys.contains(&106));
    }
}
