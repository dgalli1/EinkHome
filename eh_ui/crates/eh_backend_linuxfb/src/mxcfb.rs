//! mxcfb / HWTCON e-ink update ioctls, ported from KOReader's proven
//! definitions (koreader-base `ffi/mxcfb_kobo_h.lua`, `ffi/mxcfb_remarkable_h.lua`,
//! `ffi/mxcfb_cervantes_h.lua` — the same C structs the vendor kernels ship).
//!
//! Three struct generations share the `'F'`/0x2E ioctl slot; the `_IOW`
//! encoding bakes the struct size into the request number, so picking the
//! wrong layout addresses the wrong kernel:
//!
//! Sizes are the 32-bit ARM layouts — the only ones these kernels run on —
//! verified against KOReader's generated constants:
//!
//! | flavor | struct | bytes | ioctl | devices |
//! |---|---|---|---|---|
//! | `V1Ntx` | `mxcfb_update_data_v1_ntx` | 68 | `0x4044462e` | pre-Mk7 Kobo (Touch…Aura One), Cervantes |
//! | `V2` | `mxcfb_update_data` | 72 | `0x4048462e` | Kobo Mk7 (Clara HD…Libra 2), reMarkable 1 |
//! | `Hwtcon` | `hwtcon_update_data` | 36 | `0x4024462e` | Kobo MTK (Elipsa 2E, Libra/Clara Colour, Clara B&W) |
//!
//! The `V1Ntx` alt-buffer struct carries a pointer, so its size is
//! architecture-dependent (68 on armv7); every other layout is pointer-free
//! and asserted on the host too.  PocketBook's 64-byte `0x4040462e` variant
//! is not reachable from here — those boards go through inkview.

#![allow(non_camel_case_types)]

use eh_hal::{Rect, RefreshMode};

pub const UPDATE_MODE_PARTIAL: u32 = 0;
pub const UPDATE_MODE_FULL: u32 = 1;

/// KOReader Kobo waveforms (framebuffer_mxcfb.lua): fast = DU, partial =
/// AUTO (REAGL devices: REAGL), full/flashing = GC16.
pub const WAVEFORM_MODE_DU: u32 = 1;
pub const WAVEFORM_MODE_GC16: u32 = 2;
pub const WAVEFORM_MODE_REAGL: u32 = 6;
pub const WAVEFORM_MODE_AUTO: u32 = 257;

/// `TEMP_USE_AMBIENT`: let the EPDC measure the panel temperature.
pub const TEMP_USE_AMBIENT: i32 = 4096;

/// `mxcfb_rect` — top/left/width/height, exactly `<linux/mxcfb.h>`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MxcfbRect {
    pub top: u32,
    pub left: u32,
    pub width: u32,
    pub height: u32,
}

impl From<Rect> for MxcfbRect {
    fn from(r: Rect) -> Self {
        Self {
            top: r.y,
            left: r.x,
            width: r.w,
            height: r.h,
        }
    }
}

/// `mxcfb_alt_buffer_data` — pointer-free tail shared by `V1` and `V2`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MxcfbAltBufferData {
    pub phys_addr: u32,
    pub width: u32,
    pub height: u32,
    pub alt_update_region: MxcfbRect,
}

/// `mxcfb_update_data` (V2, 72 bytes on any arch): Kobo Mk7 + reMarkable 1.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MxcfbUpdateData {
    pub update_region: MxcfbRect,
    pub waveform_mode: u32,
    pub update_mode: u32,
    pub update_marker: u32,
    pub temp: i32,
    pub flags: u32,
    pub dither_mode: i32,
    pub quant_bit: i32,
    pub alt_buffer_data: MxcfbAltBufferData,
}
const _: () = assert!(core::mem::size_of::<MxcfbUpdateData>() == 72);

/// `mxcfb_update_data_v1_ntx` (72 bytes on 32-bit ARM): pre-Mk7 Kobo.
/// The alt-buffer `virt_addr` is a pointer, so the layout is arch-gated;
/// we never pass alt buffers, but the kernel validates the ioctl size.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MxcfbAltBufferDataNtx {
    pub virt_addr: NtxPtr,
    pub phys_addr: u32,
    pub width: u32,
    pub height: u32,
    pub alt_update_region: MxcfbRect,
}

#[cfg(target_pointer_width = "32")]
pub type NtxPtr = u32;
#[cfg(target_pointer_width = "64")]
pub type NtxPtr = u64;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MxcfbUpdateDataV1Ntx {
    pub update_region: MxcfbRect,
    pub waveform_mode: u32,
    pub update_mode: u32,
    pub update_marker: u32,
    pub temp: i32,
    pub flags: u32,
    pub alt_buffer_data: MxcfbAltBufferDataNtx,
}
#[cfg(target_pointer_width = "32")]
#[cfg(target_pointer_width = "32")]
const _: () = assert!(core::mem::size_of::<MxcfbUpdateDataV1Ntx>() == 68);

/// `hwtcon_update_data` (36 bytes): Kobo MTK (HWTCON driver).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct HwtconUpdateData {
    pub update_region: MxcfbRect,
    pub waveform_mode: u32,
    pub update_mode: u32,
    pub update_marker: u32,
    pub flags: u32,
    pub dither_mode: i32,
}
const _: () = assert!(core::mem::size_of::<HwtconUpdateData>() == 36);

/// `_IOW('F', 0x2e, …)` request numbers, per flavor (see the module table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MxcFlavor {
    /// `MXCFB_SEND_UPDATE_V1_NTX` (0x4044462e) — pre-Mk7 Kobo, Cervantes.
    V1Ntx,
    /// `MXCFB_SEND_UPDATE_V2` (0x4048462e) — Kobo Mk7, reMarkable 1.
    V2,
    /// `HWTCON_SEND_UPDATE` (0x4024462e) — Kobo MTK.
    Hwtcon,
}

impl MxcFlavor {
    /// The `MXCFB_SEND_UPDATE` request number for this flavor (KOReader
    /// `mxcfb_kobo_h.lua` constants).
    pub fn send_update_ioctl(self) -> libc::c_ulong {
        match self {
            // _IOW('F', 0x2e, mxcfb_update_data_v1_ntx) == 1078216238
            MxcFlavor::V1Ntx => 0x4044_462e,
            // _IOW('F', 0x2e, mxcfb_update_data) == 1078478382
            MxcFlavor::V2 => 0x4048_462e,
            // _IOW('F', 0x2e, hwtcon_update_data) == 1076119086
            MxcFlavor::Hwtcon => 0x4024_462e,
        }
    }

    /// `(waveform, update_mode)` for a refresh intent, per KOReader's
    /// Kobo mapping (framebuffer_mxcfb.lua): fast = DU, partial = AUTO
    /// (REAGL devices promote partial to REAGL), full = GC16 + FULL.
    pub fn waveforms(self, mode: RefreshMode) -> (u32, u32) {
        let (waveform, update_mode) = match mode {
            RefreshMode::Fast => (WAVEFORM_MODE_DU, UPDATE_MODE_PARTIAL),
            RefreshMode::Partial => (WAVEFORM_MODE_AUTO, UPDATE_MODE_PARTIAL),
            RefreshMode::Full | RefreshMode::FullHq => (WAVEFORM_MODE_GC16, UPDATE_MODE_FULL),
        };
        // REAGL-capable flavors drive partials with the REAGL waveform
        // (KOReader: Mk7 `waveform_partial = REAGL`, MTK `waveform_partial =
        // GLR16`); the kernel ignores REAGL where unsupported.
        let waveform = match (self, mode) {
            (MxcFlavor::V2, RefreshMode::Partial) => WAVEFORM_MODE_REAGL,
            (MxcFlavor::Hwtcon, RefreshMode::Partial) => WAVEFORM_MODE_REAGL,
            _ => waveform,
        };
        (waveform, update_mode)
    }
}

/// Issue one vendor update for `region`.  `marker` cycles so the kernel can
/// track collisions; we never wait on it (the app paces itself).
pub fn send_update(fd: i32, flavor: MxcFlavor, region: Rect, mode: RefreshMode) {
    let (waveform, update_mode) = flavor.waveforms(mode);
    static NEXT_MARKER: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(1);
    let marker = NEXT_MARKER.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let rect = MxcfbRect::from(region);
    // KOReader discards 1px-or-less regions: they can hang some kernels
    // (framebuffer_mxcfb.lua "discarding bogus refresh region").
    if rect.width <= 1 || rect.height <= 1 {
        return;
    }
    let req = flavor.send_update_ioctl();
    let rc = unsafe {
        match flavor {
            MxcFlavor::V1Ntx => {
                // KOReader zeroes the NTX alt buffer and nils virt_addr
                // (framebuffer_mxcfb.lua refresh_kobo).
                let mut data = MxcfbUpdateDataV1Ntx {
                    update_region: rect,
                    waveform_mode: waveform,
                    update_mode,
                    update_marker: marker,
                    temp: TEMP_USE_AMBIENT,
                    flags: 0,
                    alt_buffer_data: MxcfbAltBufferDataNtx::default(),
                };
                libc::ioctl(fd, req, &mut data as *mut _)
            }
            MxcFlavor::V2 => {
                let mut data = MxcfbUpdateData {
                    update_region: rect,
                    waveform_mode: waveform,
                    update_mode,
                    update_marker: marker,
                    temp: TEMP_USE_AMBIENT,
                    flags: 0,
                    dither_mode: 0,
                    quant_bit: 0,
                    alt_buffer_data: MxcfbAltBufferData::default(),
                };
                libc::ioctl(fd, req, &mut data as *mut _)
            }
            MxcFlavor::Hwtcon => {
                let mut data = HwtconUpdateData {
                    update_region: rect,
                    waveform_mode: waveform,
                    update_mode,
                    update_marker: marker,
                    flags: 0,
                    dither_mode: 0,
                };
                libc::ioctl(fd, req, &mut data as *mut _)
            }
        }
    };
    // Update failures are non-fatal (panel keeps the last frame); the
    // emulator's fake fb0 returns ENOTTY for every vendor ioctl.
    let _ = rc;
}
