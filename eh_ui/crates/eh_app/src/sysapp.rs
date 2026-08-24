//! Promote/demote bookshelf as the PocketBook home task (the Rust port of
//! `eh_sysapp.c`).
//!
//! "Install as system app" (Settings) copies the RUNNING binary to the
//! firmware's home-task override path — `$EH_SYSAPP_DIR`, else
//! `/mnt/ext1/system/bin` — which monitor.app boots in preference to the
//! stock bookshelf, plus a fresh cfg so the promoted task talks to the
//! same API with the same settings.  Promotion is a raw byte copy (never
//! a wrapper script): a wrapper's exec would break the reader's book-open
//! handshake.  Unpromote removes both files (missing files are a
//! successful unpromote).

use crate::app::App;
use eh_hal::Framebuffer;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Home-task override dir: `$EH_SYSAPP_DIR` (the SDL e2e suite's test
/// hook), else the real device path.
pub fn dir() -> String {
    std::env::var("EH_SYSAPP_DIR").unwrap_or_else(|_| "/mnt/ext1/system/bin".to_string())
}

/// True when a home-task override binary is installed.
pub fn detect() -> bool {
    Path::new(&format!("{}/bookshelf.app", dir())).exists()
}

/// The running binary's path.  `/proc/self/exe` is authoritative (works
/// even if the on-disk file was unlinked); fall back to argv[0].
fn self_bin() -> Option<PathBuf> {
    if let Ok(p) = std::fs::read_link("/proc/self/exe") {
        return Some(p);
    }
    std::env::args().next().map(PathBuf::from)
}

/// Stream-copy `src` → `dst` in bounded chunks (the promoted binary is
/// ~35 MB — buffering it whole risks an OOM on low-RAM readers).  A
/// failure can leave a partial `dst`; the caller removes it (promote
/// does).
fn copy_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    let mut inp = std::fs::File::open(src)?;
    let mut out = std::fs::File::create(dst)?;
    // 1 MiB chunks: large enough to keep syscalls few, far below the
    // allocation that would threaten the device.
    let mut buf = [0u8; 1 << 20];
    loop {
        let n = inp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
    }
    Ok(())
}

/// Copy the running binary + a fresh cfg into the home-task dir.
/// Returns true on success (and logs the C marker line).
pub fn promote<B: Framebuffer>(app: &mut App<B>) -> bool {
    let Some(src) = self_bin() else {
        crate::logger::log("[bookshelf] sysapp: cannot resolve the running binary");
        return false;
    };
    let dir = dir();
    let dst = format!("{dir}/bookshelf.app");
    let cfg = format!("{dir}/bookshelf.cfg");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        crate::logger::log(&format!("[bookshelf] sysapp: mkdir {dir} failed: {e}"));
        return false;
    }
    // Fresh cfg FIRST: a failure leaves no binary installed.
    let cfg_tmp = Path::new(&cfg);
    if let Err(e) = app.config.clone().save(cfg_tmp) {
        crate::logger::log(&format!(
            "[bookshelf] sysapp: promote cfg write {cfg} failed: {e}"
        ));
        return false;
    }
    // Already the home task?  Copying would truncate the running exe.
    let is_home = src == dst;
    if !is_home {
        if let Err(e) = copy_file(&src, Path::new(&dst)) {
            let _ = std::fs::remove_file(&dst);
            crate::logger::log(&format!(
                "[bookshelf] sysapp: promote copy {} -> {dst} failed: {e}",
                src.display()
            ));
            return false;
        }
        let _ = std::fs::set_permissions(&dst, std::os::unix::fs::PermissionsExt::from_mode(0o755));
    }
    crate::logger::log(&format!(
        "[bookshelf] sysapp: {dst} installed as home task ({})",
        if is_home { "was already" } else { "promoted" }
    ));
    true
}

/// Remove the home-task override (binary + cfg).
pub fn unpromote() {
    let dir = dir();
    let _ = std::fs::remove_file(format!("{dir}/bookshelf.app"));
    let _ = std::fs::remove_file(format!("{dir}/bookshelf.cfg"));
    crate::logger::log(
        "[bookshelf] sysapp: home-task override removed; stock home returns on reboot",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_file_roundtrips_across_chunk_boundaries() {
        // Larger than the internal 1 MiB chunk with a partial final
        // chunk, so the streaming loop provably iterates.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        let payload: Vec<u8> = (0..(2 << 20) + 123).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &payload).unwrap();

        copy_file(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), payload);

        // A missing source errors without corrupting the destination.
        let missing = dir.path().join("missing.bin");
        assert!(copy_file(&missing, &dst).is_err());
    }
}
