//! Cervantes (BQ) — detection + wiring, ported from KOReader
//! `frontend/device/cervantes/device.lua`.
//!
//! Detection probes the PCB id via `ntxinfo /dev/mmcblk0` (the same NTX
//! board family as Kobo): 22 Touch, 23 TouchLight, 33 2013, 51 Cervantes 3,
//! 68 Cervantes 4.  The stack is Kobo's MXCFB V1 (KOReader: "Cervantes
//! MXCFB_SEND_UPDATE == 0x4044462e" — the pointer-free V1 struct), with a
//! legacy single-touch panel (KOReader `touch_legacy`) that swaps axes and
//! mirrors X.

use crate::{wire, Device};
use eh_backend_linuxfb::{LinuxFb, MxcFlavor, TouchQuirks, CERVANTES_BUTTONS};

/// A detected Cervantes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cervantes {
    pub pcb: u16,
    pub model: &'static str,
}

impl Cervantes {
    /// Detect via `ntxinfo /dev/mmcblk0` (KOReader `getProductId`).
    pub fn detect() -> Result<Self, Error> {
        let out = std::process::Command::new("/usr/bin/ntxinfo")
            .arg("/dev/mmcblk0")
            .output()
            .map_err(|_| Error::NotACervantes)?;
        let text = String::from_utf8_lossy(&out.stdout);
        // "pcb : <id>" — KOReader greps + cuts.
        for line in text.lines() {
            if let Some((_key, val)) = line.split_once(':') {
                if let Ok(pcb) = val.trim().parse::<u16>() {
                    return Self::from_pcb(pcb).ok_or(Error::UnknownModel(pcb));
                }
            }
        }
        Err(Error::NotACervantes)
    }

    /// PCB id → model (KOReader's dispatch).
    pub fn from_pcb(pcb: u16) -> Option<Self> {
        let model = match pcb {
            22 => "Cervantes Touch",
            23 => "Cervantes TouchLight",
            33 => "Cervantes 2013",
            51 => "Cervantes 3",
            68 => "Cervantes 4",
            _ => return None,
        };
        Some(Self { pcb, model })
    }

    /// Open the framebuffer + input.
    pub fn open(&self, fb_path: &str) -> std::io::Result<Device> {
        let fb = LinuxFb::open(fb_path)?;
        // touch_legacy: single-touch, axis-swapped, x-mirrored (KOReader
        // cervantes device flags).
        let quirks = TouchQuirks::default();
        wire(
            fb,
            MxcFlavor::V1Ntx,
            quirks,
            CERVANTES_BUTTONS,
            &crate::CERVANTES_INPUT_PATHS,
            self.model,
        )
    }
}

/// Detection failure reasons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// `ntxinfo` missing or silent (not a Cervantes board).
    NotACervantes,
    /// A PCB id we don't know.
    UnknownModel(u16),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NotACervantes => write!(f, "ntxinfo did not report a Cervantes PCB id"),
            Error::UnknownModel(pcb) => write!(f, "unknown Cervantes PCB id {pcb}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcb_table_matches_koreader() {
        assert_eq!(Cervantes::from_pcb(22).unwrap().model, "Cervantes Touch");
        assert_eq!(
            Cervantes::from_pcb(23).unwrap().model,
            "Cervantes TouchLight"
        );
        assert_eq!(Cervantes::from_pcb(33).unwrap().model, "Cervantes 2013");
        assert_eq!(Cervantes::from_pcb(51).unwrap().model, "Cervantes 3");
        assert_eq!(Cervantes::from_pcb(68).unwrap().model, "Cervantes 4");
        assert!(Cervantes::from_pcb(7).is_none());
    }
}
