//! Build script: compile the Slint UI (src/ui/*.slint) into the crate.
//!
//! The software renderer is the only renderer — the app paints straight
//! into the backend framebuffer (see src/ui/mod.rs); there is no windowing
//! system on any target.

fn main() {
    slint_build::compile("src/ui/main.slint").unwrap();
}
