//! eh_backend_linuxfb — a [`Framebuffer`] over `/dev/fb0` + the E-ink refresh
//! ioctls + evdev input: the direct-framebuffer path KOReader uses on every
//! non-Android device (Kobo, reMarkable, Cervantes, PocketBook direct-fb,
//! generic boards).
//!
//! Design notes (anchored by the pbemu shim at `pbemu/src/shim/src/fake_fb.c`):
//! - The frame is mmap'd from `/dev/fb0`; geometry from `FBIOGET_VSCREENINFO` /
//!   `FBIOGET_FSCREENINFO`.  `refresh()` clamps the region to the content area
//!   then issues the vendor update ioctl selected by [`Flavor`] (see
//!   `mxcfb`).
//! - On **real devices** the pixels land on the panel and are visible (this
//!   is exactly what KOReader's `framebuffer_mxcfb.lua` does on PB/Kobo/
//!   reMarkable/Cervantes).
//! - In pbemu the fake `fb0` is a private memfd, so the IOCTLs are no-ops and
//!   `frame_dump` cannot see these pixels — that is an emulator observation
//!   gap, not a device problem.  See `eh_backend_inkview` for the path that
//!   pbemu can observe.
//! - Input comes from evdev nodes (`evdev`); without them the backend is
//!   display-only (`poll_event` yields nothing), which is what the plain
//!   `linuxfb` demo and the emulator use.
//!
//! The status-bar rule is honoured here: `refresh()` clamps every region to
//! `[0, content_bottom)`, never touching the native panel strip.

mod evdev;
mod mxcfb;

pub use evdev::{
    Decoder, EvDev, NodeKind, RawEvent, TouchQuirks, CERVANTES_BUTTONS, KOBO_BUTTONS,
    REMARKABLE_BUTTONS,
};
pub use mxcfb::MxcFlavor;

use eh_hal::{Framebuffer, InputEvent, KeyCode, PixelFormat, Rect, RefreshMode, Screen};
use std::os::unix::io::RawFd;

/// ioctls from `<linux/fb.h>` (value-stable on linux).
const FBIOGET_VSCREENINFO: libc::c_ulong = 0x4600;
const FBIOGET_FSCREENINFO: libc::c_ulong = 0x4602;

/// What `refresh()` does after the pixels are in the map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Flavor {
    /// No vendor ioctl: a plain framebuffer (pbemu's fake fb0, generic
    /// boards without an e-ink driver).
    #[default]
    Plain,
    /// mxcfb / HWTCON EPDC update (see [`mxcfb::MxcFlavor`]).
    Mxc(MxcFlavor),
}

/// `struct fb_var_screeninfo` — only the fields we read.
#[repr(C)]
struct FbVarScreeninfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    _pad: [u8; 96],
}

/// `struct fb_fix_screeninfo` — only the fields we read.
#[repr(C)]
struct FbFixScreeninfo {
    smem_start: u64,
    smem_len: u32,
    line_length: u32,
    _pad: [u8; 64],
}

/// A Linux framebuffer backend with e-ink refresh + optional evdev input.
pub struct LinuxFb {
    fd: RawFd,
    map: &'static mut [u8],
    width: u32,
    height: u32,
    stride: usize,
    /// Rows app-owned; `[content_bottom, height)` reserved for the panel.
    content_bottom: u32,
    format: PixelFormat,
    flavor: Flavor,
    input: Option<EvDev>,
}

impl LinuxFb {
    /// Open and mmap `path`, probing geometry and format.  No vendor
    /// refresh ioctls, no input — the plain-framebuffer baseline.
    pub fn open(path: &str) -> std::io::Result<Self> {
        let cpath = std::ffi::CString::new(path)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has NUL"))?;
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDWR) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }

        let mut var = FbVarScreeninfo {
            xres: 0,
            yres: 0,
            xres_virtual: 0,
            yres_virtual: 0,
            xoffset: 0,
            yoffset: 0,
            bits_per_pixel: 0,
            _pad: [0; 96],
        };
        let mut fix = FbFixScreeninfo {
            smem_start: 0,
            smem_len: 0,
            line_length: 0,
            _pad: [0; 64],
        };

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
            let p = libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            );
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
            flavor: Flavor::Plain,
            input: None,
        })
    }

    /// Reserve the native panel region (pbemu: firmware paints it; device:
    /// app may own it).  `panel_h` rows at the bottom are excluded from all
    /// app refreshes.
    pub fn set_panel(&mut self, panel_h: u32) {
        self.content_bottom = self.height.saturating_sub(panel_h);
    }

    /// Select the vendor refresh flavor (see [`mxcfb::MxcFlavor`]).
    pub fn set_flavor(&mut self, flavor: Flavor) {
        self.flavor = flavor;
    }

    /// Attach evdev input: `paths` are classified by capability (touch /
    /// pen / buttons — see [`EvDev::open`]), coordinates land in
    /// `width × height` surface space via `quirks`, buttons map through
    /// `button_map`.
    pub fn attach_input(
        &mut self,
        paths: &[&str],
        quirks: TouchQuirks,
        button_map: &'static [(u16, KeyCode)],
    ) -> std::io::Result<()> {
        self.input = Some(EvDev::open(
            paths,
            quirks,
            (self.width, self.height),
            button_map,
        )?);
        Ok(())
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
        Screen {
            width: self.width,
            height: self.height,
            content_bottom: self.content_bottom,
        }
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
        let limit = Rect {
            x: 0,
            y: 0,
            w: self.width,
            h: self.content_bottom,
        };
        let region = region.intersect(&limit);
        if region.is_empty() {
            return;
        }
        match self.flavor {
            Flavor::Plain => {}
            Flavor::Mxc(f) => mxcfb::send_update(self.fd, f, region, mode),
        }
    }
    fn mark_dirty(&mut self, _region: Rect) {}
    fn poll_event(&mut self) -> Option<InputEvent> {
        self.input.as_mut().and_then(|i| i.poll_event())
    }
    fn wait_for_event(&mut self, timeout_ms: u32) {
        if let Some(input) = self.input.as_ref() {
            input.wait(timeout_ms as i32);
        } else {
            // Display-only: pace the loop for the caller.
            std::thread::sleep(std::time::Duration::from_millis(u64::from(timeout_ms)));
        }
    }
    fn present(&mut self, mode: RefreshMode) {
        self.refresh(
            Rect {
                x: 0,
                y: 0,
                w: self.width,
                h: self.content_bottom,
            },
            mode,
        );
    }
}
