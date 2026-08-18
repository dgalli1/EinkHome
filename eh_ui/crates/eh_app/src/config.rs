//! Device configuration — the `bookshelf.cfg` KV file the C app wrote.
//!
//! Format is `key=value` lines, `#` comments.  Keys (matching the C app's
//! eh_config.c): `api_url`/`url`, `api_token`/`token`, `downloads_dir`,
//! `reader`.  Defaults fall back to EH_TOKEN_DEFAULT for the token.
//!
//! Config search mirrors eh_resolve_config_path: the file next to the
//! executable (argv0 dir / bookshelf.cfg), then a system base dir, then the
//! scratch root — first candidate that parses cleanly wins.  For the Rust
//! app the primary is `/mnt/ext1/system/bin/bookshelf.cfg` (device) or a
//! caller-supplied path (host/emulator).

use std::path::{Path, PathBuf};

pub const EH_TOKEN_DEFAULT: &str = "pbemu-dev-token";

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub api_url: String,
    pub api_token: String,
    pub downloads_dir: Option<String>,
    pub reader: Option<String>,
}

impl Config {
    /// Parse a `key=value` config file (blank lines and `#` comments skipped).
    /// `api_token` defaults to [`EH_TOKEN_DEFAULT`] when absent (the C app's
    /// fallback).  Returns an empty Config when the file is unreadable.
    pub fn load(path: &Path) -> std::io::Result<Config> {
        let text = std::fs::read_to_string(path)?;
        let mut cfg = Config::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some(eq) = line.find('=') else {
                continue;
            };
            let key = line[..eq].trim();
            let value = line[eq + 1..].trim();
            match key {
                "api_url" | "url" => cfg.api_url = value.to_string(),
                "api_token" | "token" => cfg.api_token = value.to_string(),
                "downloads_dir" | "download_dir" => cfg.downloads_dir = Some(value.to_string()),
                "reader" => cfg.reader = Some(value.to_string()),
                _ => {}
            }
        }
        if cfg.api_token.is_empty() {
            cfg.api_token = EH_TOKEN_DEFAULT.to_string();
        }
        Ok(cfg)
    }

    /// Search order for the config file (device semantics).
    pub fn locate(candidate: Option<&Path>) -> Option<PathBuf> {
        if let Some(p) = candidate {
            if p.exists() {
                return Some(p.to_path_buf());
            }
        }
        for p in [
            PathBuf::from("/mnt/ext1/system/bin/bookshelf.cfg"),
            PathBuf::from("/etc/pbemu/bookshelf.cfg"),
            PathBuf::from("/tmp/bookshelf.cfg"),
        ] {
            if p.exists() {
                return Some(p);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kv_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bookshelf.cfg");
        std::fs::write(
            &path,
            "# comment\napi_url=http://192.168.1.5:8765\napi_token=sekrit\ndownloads_dir=/mnt/ext1/Books\n",
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.api_url, "http://192.168.1.5:8765");
        assert_eq!(cfg.api_token, "sekrit");
        assert_eq!(cfg.downloads_dir.as_deref(), Some("/mnt/ext1/Books"));
    }

    #[test]
    fn defaults_token_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bookshelf.cfg");
        std::fs::write(&path, "api_url=http://x:1\n").unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.api_token, EH_TOKEN_DEFAULT);
    }
}