//! Kobo — detection + wiring, ported from KOReader
//! `frontend/device/kobo/device.lua` and FBInk's hardware table
//! (`fbink_device_id.c`), the two hardware-proven references.
//!
//! Detection layers (KOReader `getCodeName`/`getProductId`):
//! 1. `PRODUCT` env (KSM) → codename; `MODEL_NUMBER` env → product id
//! 2. `/bin/kobo_config.sh` (device-tree name) → codename
//! 3. `/usr/bin/hwdetect.sh` (firmware 5+) → codename
//! 4. `/mnt/onboard/.kobo/version` → product id (last three characters of
//!    the first line) → the FBInk id table
//!
//! The product id drives the EPDC flavor:
//! - **Mk7** (imx6sll V2 driver, REAGL partials): Clara HD 376, Forma
//!   377/380, Aura H2O² r2 378, Aura SE r2 379, Nia 382, Libra H2O 384,
//!   Clara 2E 386, Libra 2 388
//! - **MTK/HWTCON**: Elipsa 2E 389, Libra Colour 390, Clara B&W 391/395,
//!   Clara Colour 393
//! - **sunxi**: Elipsa 387, Sage 383 — detected but *unsupported* (KOReader
//!   drives them through an ION/G2D layer, not the fb0 mmap)
//! - everything else: the NTX V1 driver (imx505/imx6)

use crate::{wire, Device};
use eh_backend_linuxfb::{LinuxFb, MxcFlavor, TouchQuirks, KOBO_BUTTONS};

/// The EPDC driver family of a Kobo board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    /// NTX V1 driver (`MXCFB_SEND_UPDATE_V1_NTX`) — every pre-Mk7 board.
    V1Ntx,
    /// Mk7 V2 driver (`MXCFB_SEND_UPDATE_V2`, REAGL partials).
    Mk7,
    /// HWTCON (MTK) driver — 2024+ colour and B&W boards.
    Mtk,
    /// Allwinner sunxi — unsupported here (ION/G2D layer stack).
    Sunxi,
}

/// A detected Kobo: codename, product id, driver family, touch quirks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kobo {
    pub codename: &'static str,
    /// Marketing name for logs.
    pub model: &'static str,
    pub pid: u16,
    pub flavor: Flavor,
    pub quirks: TouchQuirks,
}

impl Kobo {
    /// Detect the board: env → probe scripts → the `.kobo/version`
    /// product id.  Errors name what was tried; a sunxi board comes back
    /// as `Error::Unsupported`.
    pub fn detect() -> Result<Self, Error> {
        let codename = probe_codename();
        if let Some(codename) = codename {
            return Self::from_codename(codename);
        }
        let pid = probe_pid().ok_or(Error::NotAKobo)?;
        Self::from_pid(pid).ok_or(Error::UnknownModel(pid))
    }

    /// Codename → device class (KOReader's dispatch table).
    pub fn from_codename(codename: &str) -> Result<Self, Error> {
        let pid = probe_pid().unwrap_or(0);
        from_codename_table(codename, pid).ok_or(Error::UnknownModel(pid))
    }

    /// Product id → device class (FBInk's table; the offline fallback).
    pub fn from_pid(pid: u16) -> Option<Self> {
        from_pid_table(pid)
    }

    /// Open the framebuffer + input, sized for this board.
    ///
    /// `fb_path` is `/dev/fb0` on a device; pbemu's fake fb0 also works,
    /// which is how the backend is exercised without hardware.
    pub fn open(&self, fb_path: &str) -> std::io::Result<Device> {
        let flavor = match self.flavor {
            Flavor::V1Ntx => MxcFlavor::V1Ntx,
            Flavor::Mk7 => MxcFlavor::V2,
            Flavor::Mtk => MxcFlavor::Hwtcon,
            Flavor::Sunxi => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "Kobo sunxi boards (Elipsa/Sage) need the ION/G2D layer \
                     stack KOReader uses; the fb0 path cannot drive them",
                ));
            }
        };
        let fb = LinuxFb::open(fb_path)?;
        wire(
            fb,
            flavor,
            self.quirks,
            KOBO_BUTTONS,
            &crate::KOBO_INPUT_PATHS,
            self.model,
        )
    }
}

/// Detection failure reasons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// No Kobo markers anywhere (not a Kobo board).
    NotAKobo,
    /// A product id we don't know (newer than the table).
    UnknownModel(u16),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NotAKobo => write!(
                f,
                "no Kobo markers (PRODUCT/kobo_config.sh/hwdetect.sh/.kobo/version)"
            ),
            Error::UnknownModel(pid) => {
                write!(f, "unknown Kobo product id {pid} (newer than the table)")
            }
        }
    }
}

/// Codename probe: `PRODUCT` env → kobo_config.sh → hwdetect.sh
/// (KOReader `getCodeName`).
fn probe_codename() -> Option<&'static str> {
    if let Ok(c) = std::env::var("PRODUCT") {
        return leak(c);
    }
    for script in ["/bin/kobo_config.sh", "/usr/bin/hwdetect.sh"] {
        if let Ok(out) = std::process::Command::new(script).output() {
            if let Ok(name) = std::str::from_utf8(&out.stdout) {
                if let Some(c) = name.lines().next() {
                    if !c.trim().is_empty() {
                        return leak(c.trim().to_string());
                    }
                }
            }
        }
    }
    None
}

fn leak(s: String) -> Option<&'static str> {
    Some(Box::leak(s.into_boxed_str()))
}

/// Product id probe: `MODEL_NUMBER` env → the last three characters of the
/// first line of `/mnt/onboard/.kobo/version` (KOReader `getProductId`).
fn probe_pid() -> Option<u16> {
    if let Ok(s) = std::env::var("MODEL_NUMBER") {
        return s.trim().parse().ok();
    }
    let v = std::fs::read_to_string("/mnt/onboard/.kobo/version").ok()?;
    let line = v.lines().next()?;
    let tail = line.len().checked_sub(3)?;
    line[tail..].trim().parse().ok()
}

/// KOReader's dispatch (`device.lua` tail) + FBInk's id table, collapsed
/// into one codename entry point.  `pid` only disambiguates the trilogy /
/// snow / star hardware revisions.
fn from_codename_table(codename: &str, pid: u16) -> Option<Kobo> {
    let (model, flavor, quirks) = match codename {
        "trilogy" if pid == 310 => ("Kobo Touch A", Flavor::V1Ntx, TouchQuirks::default()),
        // Touch B uses the_mk3 touch protocol — a raw-panel decoder this
        // port does not carry; it still gets standard MT-B coordinates.
        "trilogy" => ("Kobo Touch", Flavor::V1Ntx, TouchQuirks::default()),
        "pixie" => ("Kobo Mini", Flavor::V1Ntx, TouchQuirks::default()),
        "kraken" => ("Kobo Glo", Flavor::V1Ntx, TouchQuirks::default()),
        "dragon" => ("Kobo Aura HD", Flavor::V1Ntx, TouchQuirks::default()),
        "phoenix" => ("Kobo Aura", Flavor::V1Ntx, TouchQuirks::default()),
        "dahlia" => (
            "Kobo Aura H2O",
            Flavor::V1Ntx,
            TouchQuirks {
                main_slot: 1,
                ..TouchQuirks::default()
            },
        ),
        "alyssum" => ("Kobo Glo HD", Flavor::V1Ntx, TouchQuirks::default()),
        "pika" => ("Kobo Touch 2.0", Flavor::V1Ntx, TouchQuirks::default()),
        "daylight" => ("Kobo Aura One", Flavor::V1Ntx, TouchQuirks::default()),
        "snow" => (
            "Kobo Aura H2O2",
            Flavor::V1Ntx,
            TouchQuirks {
                mirrored_x: false,
                ..TouchQuirks::default()
            },
        ),
        "star" => ("Kobo Aura SE", Flavor::V1Ntx, TouchQuirks::default()),
        "nova" | "frost" | "storm" | "luna" | "io" | "goldfinch" => {
            (mk7_model(codename)?, Flavor::Mk7, TouchQuirks::default())
        }
        "europa" | "cadmus" => return Some(sunxi(codename, pid)),
        "condor" | "monza" | "monzaKobo" | "monzaTolino" | "spaBW" | "spaKoboBW"
        | "spaTolinoBW" | "spaBWTPV" | "spaColour" | "spaKoboColour" | "spaTolinoColour" => {
            (mtk_model(codename)?, Flavor::Mtk, TouchQuirks::default())
        }
        _ => return None,
    };
    Some(Kobo {
        codename: leak_name(codename),
        model,
        pid,
        flavor,
        quirks,
    })
}

fn mk7_model(codename: &str) -> Option<&'static str> {
    Some(match codename {
        "nova" => "Kobo Clara HD",
        "frost" => "Kobo Forma",
        "storm" => "Kobo Libra H2O",
        "luna" => "Kobo Nia",
        "io" => "Kobo Libra 2",
        "goldfinch" => "Kobo Clara 2E",
        _ => return None,
    })
}

fn mtk_model(codename: &str) -> Option<&'static str> {
    Some(match codename {
        "condor" => "Kobo Elipsa 2E",
        "monza" | "monzaKobo" | "monzaTolino" => "Kobo Libra Colour",
        "spaBW" | "spaKoboBW" | "spaTolinoBW" | "spaBWTPV" => "Kobo Clara B&W",
        "spaColour" | "spaKoboColour" | "spaTolinoColour" => "Kobo Clara Colour",
        _ => return None,
    })
}

fn sunxi(codename: &str, pid: u16) -> Kobo {
    let model = match codename {
        "cadmus" => "Kobo Sage",
        _ => "Kobo Elipsa",
    };
    Kobo {
        codename: leak_name(codename),
        model,
        pid,
        flavor: Flavor::Sunxi,
        quirks: TouchQuirks::default(),
    }
}

fn leak_name(codename: &str) -> &'static str {
    match codename {
        "trilogy" => "trilogy",
        "pixie" => "pixie",
        "kraken" => "kraken",
        "dragon" => "dragon",
        "phoenix" => "phoenix",
        "dahlia" => "dahlia",
        "alyssum" => "alyssum",
        "pika" => "pika",
        "daylight" => "daylight",
        "snow" => "snow",
        "star" => "star",
        "nova" => "nova",
        "frost" => "frost",
        "storm" => "storm",
        "luna" => "luna",
        "io" => "io",
        "goldfinch" => "goldfinch",
        "europa" => "europa",
        "cadmus" => "cadmus",
        "condor" => "condor",
        "monza" | "monzaKobo" | "monzaTolino" => "monza",
        "spaBW" | "spaKoboBW" | "spaTolinoBW" | "spaBWTPV" => "spaBW",
        "spaColour" | "spaKoboColour" | "spaTolinoColour" => "spaColour",
        other => Box::leak(other.to_string().into_boxed_str()),
    }
}

/// FBInk's product-id table (`fbink.h` DEVICE_KOBO_*), the offline
/// detection path when no codename probe answers.
fn from_pid_table(pid: u16) -> Option<Kobo> {
    let (codename, model, flavor) = match pid {
        300 | 310 | 320 => ("trilogy", "Kobo Touch", Flavor::V1Ntx),
        340 => ("pixie", "Kobo Mini", Flavor::V1Ntx),
        330 => ("kraken", "Kobo Glo", Flavor::V1Ntx),
        371 => ("alyssum", "Kobo Glo HD", Flavor::V1Ntx),
        372 => ("pika", "Kobo Touch 2.0", Flavor::V1Ntx),
        360 => ("phoenix", "Kobo Aura", Flavor::V1Ntx),
        350 => ("dragon", "Kobo Aura HD", Flavor::V1Ntx),
        370 => ("dahlia", "Kobo Aura H2O", Flavor::V1Ntx),
        374 => ("snow", "Kobo Aura H2O2", Flavor::V1Ntx),
        378 => ("snow", "Kobo Aura H2O2 (r2)", Flavor::Mk7),
        373 | 381 => ("daylight", "Kobo Aura One", Flavor::V1Ntx),
        375 => ("star", "Kobo Aura SE", Flavor::V1Ntx),
        379 => ("star", "Kobo Aura SE (r2)", Flavor::Mk7),
        376 => ("nova", "Kobo Clara HD", Flavor::Mk7),
        377 | 380 => ("frost", "Kobo Forma", Flavor::Mk7),
        382 => ("luna", "Kobo Nia", Flavor::Mk7),
        384 => ("storm", "Kobo Libra H2O", Flavor::Mk7),
        386 => ("goldfinch", "Kobo Clara 2E", Flavor::Mk7),
        388 => ("io", "Kobo Libra 2", Flavor::Mk7),
        387 => ("europa", "Kobo Elipsa", Flavor::Sunxi),
        383 => ("cadmus", "Kobo Sage", Flavor::Sunxi),
        389 => ("condor", "Kobo Elipsa 2E", Flavor::Mtk),
        390 => ("monza", "Kobo Libra Colour", Flavor::Mtk),
        391 | 395 => ("spaBW", "Kobo Clara B&W", Flavor::Mtk),
        393 => ("spaColour", "Kobo Clara Colour", Flavor::Mtk),
        _ => return None,
    };
    // The r2 revisions carry their own quirks in KOReader (snow r2 keeps
    // the snow protocol; star r2 the star one); the coordinate quirks are
    // the family defaults either way.
    let quirks = match codename {
        "snow" => TouchQuirks {
            mirrored_x: false,
            ..TouchQuirks::default()
        },
        "dahlia" => TouchQuirks {
            main_slot: 1,
            ..TouchQuirks::default()
        },
        _ => TouchQuirks::default(),
    };
    Some(Kobo {
        codename: leak_name(codename),
        model,
        pid,
        flavor,
        quirks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_table_covers_every_fblink_id() {
        // The FBInk DEVICE_KOBO_* ids; every one must resolve.
        for pid in [
            300u16, 310, 320, 330, 340, 350, 360, 370, 371, 372, 373, 374, 375, 376, 377, 378, 379,
            380, 381, 382, 383, 384, 386, 387, 388, 389, 390, 391, 393, 395,
        ] {
            assert!(from_pid_table(pid).is_some(), "pid {pid} missing");
        }
        assert!(from_pid_table(321).is_none());
        assert!(from_pid_table(999).is_none());
    }

    #[test]
    fn pid_flavors_match_koreader() {
        // Pre-Mk7 NTX boards.
        assert_eq!(from_pid_table(373).unwrap().flavor, Flavor::V1Ntx);
        // Mk7 V2 boards.
        for pid in [376, 377, 378, 379, 380, 382, 384, 386, 388] {
            assert_eq!(
                from_pid_table(pid).unwrap().flavor,
                Flavor::Mk7,
                "pid {pid}"
            );
        }
        // MTK boards.
        for pid in [389, 390, 391, 393, 395] {
            assert_eq!(
                from_pid_table(pid).unwrap().flavor,
                Flavor::Mtk,
                "pid {pid}"
            );
        }
        // Sunxi: detected, unsupported at open().
        for pid in [387, 383] {
            assert_eq!(
                from_pid_table(pid).unwrap().flavor,
                Flavor::Sunxi,
                "pid {pid}"
            );
        }
    }

    #[test]
    fn sunxi_open_reports_unsupported() {
        let elipsa = from_pid_table(387).unwrap();
        let err = elipsa.open("/nonexistent-fb").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    }

    #[test]
    fn codename_dispatch_matches_koreader() {
        let clara = from_codename_table("nova", 376).unwrap();
        assert_eq!(clara.model, "Kobo Clara HD");
        assert_eq!(clara.flavor, Flavor::Mk7);

        // Aura H2O keeps the first finger in slot 1 (KOReader
        // main_finger_slot = 1).
        let h2o = from_codename_table("dahlia", 370).unwrap();
        assert_eq!(h2o.quirks.main_slot, 1);

        // Snow panels are not x-mirrored (KOReader touch_mirrored_x = no).
        let snow = from_codename_table("snow", 374).unwrap();
        assert!(!snow.quirks.mirrored_x);
        assert!(snow.quirks.switch_xy);

        // Unknown codenames are rejected, not guessed.
        assert!(from_codename_table("krypton", 0).is_none());
    }

    #[test]
    fn kobo_defaults_match_koreader() {
        // KOReader Kobo defaults: switch_xy + mirrored_x, slot 0.
        let q = TouchQuirks::default();
        assert!(q.switch_xy);
        assert!(q.mirrored_x);
        assert!(!q.mirrored_y);
        assert_eq!(q.main_slot, 0);
    }
}
