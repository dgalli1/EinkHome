//! eh_backend_devices — KOReader-style device detection and per-vendor
//! wiring for the direct-fb e-ink targets: Kobo, reMarkable, Cervantes.
//!
//! Each module mirrors the corresponding KOReader `frontend/device/<vendor>/
//! device.lua`: detect the model, then hand back a [`LinuxFb`] configured
//! with the right EPDC flavor ([`MxcFlavor`]), the vendor's evdev nodes and
//! the model's touch quirks.  Detection is layered exactly like KOReader's:
//! vendor env → vendor probe script → a hardware table keyed by product id.
//!
//! What is deliberately **not** here (and why):
//! - Kobo sunxi boards (Elipsa, Sage): KOReader drives them through an
//!   ION-allocated G2D layer (`framebuffer_ion.lua` + `DISP_EINK_UPDATE2`),
//!   a fundamentally different memory path than an fb0 mmap.  Detection
//!   reports them so the caller can say so instead of mis-driving the panel.
//! - Kindle: KOReader renders through FBInk (a C library) on all Kindles;
//!   porting means vendoring FBInk.  Out of scope.
//! - Frontlight / suspend / wifi: device power plumbing the app does not
//!   surface; no hardware here to validate it against.

pub mod cervantes;
pub mod kobo;
pub mod remarkable;

pub use cervantes::Cervantes;
pub use kobo::Kobo;
pub use remarkable::Remarkable;

use eh_backend_linuxfb::{LinuxFb, MxcFlavor, TouchQuirks};

/// A wired-up device: framebuffer + input, ready for the app loop.
pub struct Device {
    pub fb: LinuxFb,
    /// Human-readable model name for the log line.
    pub model: &'static str,
}

impl core::fmt::Debug for Device {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Device")
            .field("model", &self.model)
            .finish()
    }
}

/// Kobo input node layout (device/kobo/device.lua): the NTX button pad on
/// the stable `event0`, the touch panel on `event1`.
const KOBO_INPUT_PATHS: [&str; 3] = [
    "/dev/input/event0",
    "/dev/input/event1",
    "/dev/input/event2",
];

/// reMarkable node layout (`frontend/device/remarkable/device.lua`): pen,
/// buttons, touch — classified here by capability, so order is not
/// load-bearing.
const RM_INPUT_PATHS: [&str; 4] = [
    "/dev/input/event0",
    "/dev/input/event1",
    "/dev/input/event2",
    "/dev/input/event3",
];

/// Cervantes: single-touch panel + button pad.
const CERVANTES_INPUT_PATHS: [&str; 2] = ["/dev/input/event0", "/dev/input/event1"];

/// Wire a probed framebuffer to the vendor's evdev nodes.
fn wire(
    mut fb: LinuxFb,
    flavor: MxcFlavor,
    quirks: TouchQuirks,
    buttons: &'static [(u16, eh_hal::KeyCode)],
    paths: &[&str],
    model: &'static str,
) -> std::io::Result<Device> {
    fb.set_flavor(eh_backend_linuxfb::Flavor::Mxc(flavor));
    // EvDev::open skips unopenable nodes; a hard error means no input at
    // all, which the caller wants to know about.
    fb.attach_input(paths, quirks, buttons)?;
    Ok(Device { fb, model })
}
