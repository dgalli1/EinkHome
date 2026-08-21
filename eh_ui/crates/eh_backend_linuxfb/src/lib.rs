//! eh_backend_linuxfb — a [`Framebuffer`] over `/dev/fb0` + the E-ink refresh
//! ioctls, the direct-framebuffer path KOReader uses on PocketBook/Kobo/
//! Kindle/reMarkable.
//!
//! Design notes (anchored by the pbemu shim at `pbemu/src/shim/src/fake_fb.c`):
//! - The frame is mmap'd from `/dev/fb0`; geometry from `FBIOGET_VSCREENINFO` /
//!   `FBIOGET_FSCREENINFO`.  `refresh()` copies the dirty region into the map
//!   (the surface IS the map, so that's a clamp, not a copy) then issues a
//!   vendor update ioctl.
//! - On **real devices** the pixels land on the panel and are visible (this
//!   is exactly what KOReader's `framebuffer_mxcfb.lua` does on PB/Kobo/Kindle).
//! - In pbemu the fake `fb0` is a private memfd, so the IOCTLs are no-ops and
//!   `frame_dump` cannot see these pixels — that is an emulator observation
//!   gap, not a device problem.  See `eh_backend_inkview` for the path that
//!   pbemu can observe.
//!
//! The status-bar rule is honoured here: `refresh()` clamps every region to
//! `[0, content_bottom)`, never touching the native panel strip.

use std::os::unix::io::RawFd;

use eh_hal::{Framebuffer, InputEvent, PixelFormat, Rect, RefreshMode, Screen};

/// ioctls from `<linux/fb.h>` (value-stable on linux).
const FBIOGET_VSCREENINFO: libc::c_ulong = 0x4600;
const FBIOGET_FSCREENINFO: libc::c_ulong = 0x4602;

/// Vendor EPDC update ioctls.  `0x46...` namespace is shared across the
/// mxcfb (i.MX) / EPDC (sunxi) kernels KOReader drives.  Sending the update
/// request with an UPDATE_MODE is what makes freshly-written framebuffer
/// pixels appear on the panel.  pbemu treats these as no-ops (fake_fb.c).
#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
mod epdc {
    pub const SEND_UPDATE: libc::c_ulong = 0x4040_462e; // _IOW('F', 0x2e, struct)
    // Partial/full modes map to EPDC UPDATE_MODE_* as the C app's eh_flush_*.
    pub const MODE_PARTIAL: u32 = 0x01;
    pub const MODE_FULL: u32 = 0x04;
}
#[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
mod epdc {
    pub const SEND_UPDATE: libc::c_ulong = 0x4040_462e;
    pub const MODE_PARTIAL: u32 = 0x01;
    pub const MODE_FULL: u32 = 0x04;
}

#[repr(C)]
struct FbVarScreeninfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    // rest unused
    _pad: [u8; 96],
}

#[repr(C)]
struct FbFixScreeninfo {
    smem_start: u64,
    smem_len: u32,
    line_length: u32,
    _pad: [u8; 64],
}

/// A Linux framebuffer backend with e-ink refresh.
pub struct LinuxFb {
    fd: RawFd,
    map: &'static mut [u8],
    width: u32,
    height: u32,
    stride: usize,
    /// Rows app-owned; `[content_bottom, height)` reserved for the panel.
    content_bottom: u32,
    format: PixelFormat,
}

impl LinuxFb {
    /// Open and mmap `/dev/fb0`, probing geometry and format.
    ///
    /// `content_bottom` defaults to full height (no native panel strip).  Call
    /// [`set_panel`](Self::set_panel) when the firmware owns a bottom strip.
    pub fn open(path: &str) -> std::io::Result<Self> {
        let cpath = std::ffi::CString::new(path)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has NUL"))?;
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDWR) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }

        let mut var = FbVarScreeninfo { xres: 0, yres: 0, xres_virtual: 0, yres_virtual: 0, xoffset: 0, yoffset: 0, bits_per_pixel: 0, _pad: [0; 96] };
        let mut fix = FbFixScreeninfo { smem_start: 0, smem_len: 0, line_length: 0, _pad: [0; 64] };

        let r = unsafe { libc::ioctl(fd, FBIOGET_VSCREENINFO, &mut var) };
        if r < 0 {
            close_quiet(fd);
            return Err(std::io::Error::last_os_error());
        }
        let r = unsafe { libc::ioctl(fd, FBIOGET_FSCREENINFO, &mut fix) };
        if r < 0 {
            close_quiet(fd);
            return Err(std::io::Error::last_os_error());
        }

        let format = match var.bits_per_pixel {
            8 => PixelFormat::Grayscale8,
            24 => PixelFormat::Rgb24,
            32 => PixelFormat::Rgba32,
            bpp => {
                close_quiet(fd);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    format!("unsupported fb depth: {bpp}bpp"),
                ));
            }
        };

        let stride = fix.line_length as usize;
        let len = fix.smem_len as usize;
        let map = unsafe {
            let p = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
            if p == libc::MAP_FAILED {
                close_quiet(fd);
                return Err(std::io::Error::last_os_error());
            }
            core::slice::from_raw_parts_mut(p as *mut u8, len)
        };

        Ok(Self {
            fd,
            map,
            width: var.xres,
            height: var.yres,
            stride,
            content_bottom: var.yres,
            format,
        })
    }

    /// Reserve the native panel region (pbemu: firmware paints it; device:
    /// app may own it).  `panel_h` rows at the bottom are excluded from all
    /// app refreshes.
    pub fn set_panel(&mut self, panel_h: u32) {
        self.content_bottom = self.height.saturating_sub(panel_h);
    }
}

fn close_quiet(fd: RawFd) {
    unsafe {
        libc::close(fd);
    }
}

impl Drop for LinuxFb {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.map.as_mut_ptr() as *mut libc::c_void, self.map.len());
            libc::close(self.fd);
        }
    }
}

impl Framebuffer for LinuxFb {
    fn screen(&self) -> Screen {
        Screen { width: self.width, height: self.height, content_bottom: self.content_bottom }
    }
    fn format(&self) -> PixelFormat {
        self.format
    }
    fn surface_mut(&mut self) -> &mut [u8] {
        self.map
    }
    fn stride(&self) -> usize {
        self.stride
    }
    fn refresh(&mut self, region: Rect, mode: RefreshMode) {
        // Never touch the native panel strip.
        let limit = Rect { x: 0, y: 0, w: self.width, h: self.content_bottom };
        let region = region.intersect(&limit);
        if region.is_empty() {
            return;
        }
        let umode = if mode.is_partial() { epdc::MODE_PARTIAL } else { epdc::MODE_FULL };
        unsafe {
            // EPDC update request: region + mode; kernel copies from the map.
            #[repr(C)]
            struct UpdateRegion {
                x: u32,
                y: u32,
                w: u32,
                h: u32,
                m: u32,
            }
            let upd = UpdateRegion { x: region.x, y: region.y, w: region.w, h: region.h, m: umode };
            libc::ioctl(self.fd, epdc::SEND_UPDATE, &upd);
        }
    }
    fn mark_dirty(&mut self, _region: Rect) {}
    fn poll_event(&mut self) -> Option<InputEvent> {
        None
    }
    fn wait_for_event(&mut self, _timeout_ms: u32) {}
    fn present(&mut self, mode: RefreshMode) {
        self.refresh(Rect { x: 0, y: 0, w: self.width, h: self.content_bottom }, mode);
    }
}