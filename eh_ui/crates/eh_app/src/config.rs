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
    pub source: Option<String>,
    /// Lowercased first 3 chars of the config's `language=`/`lang=` value
    /// (C cfg_set_language stores into eh_g_lang); wave-2 i18n consumes it.
    pub language: Option<String>,
    /// Persisted grouping preset (`group=`, lowercase preset name —
    /// "none"/"author_series"/"author"/"year"/"genre"/"series"); parsed by
    /// `crate::menu::group_from_config` at boot so the shelf regroups
    /// across restarts.  Absent in older cfg files → no grouping.
    pub group: Option<String>,
}

impl Config {
    /// Per-key overlay (C eh_cfg_set_kv writes every present key): values
    /// the later layer actually carries win; absent keys keep the earlier
    /// layer's value.
    fn merge(&mut self, over: Config) {
        if !over.api_url.is_empty() {
            self.api_url = over.api_url;
        }
        if !over.api_token.is_empty() {
            self.api_token = over.api_token;
        }
        if over.reader.is_some() {
            self.reader = over.reader;
        }
        if over.downloads_dir.is_some() {
            self.downloads_dir = over.downloads_dir;
        }
        if over.source.is_some() {
            self.source = over.source;
        }
        if over.language.is_some() {
            self.language = over.language;
        }
        if over.group.is_some() {
            self.group = over.group;
        }
    }

    fn finish(mut self) -> Config {
        if self.api_token.is_empty() {
            self.api_token = EH_TOKEN_DEFAULT.to_string();
        }
        self
    }
}

impl Config {
    /// Parse a `key=value` config file (blank lines and `#` comments skipped).
    /// `api_token` defaults to [`EH_TOKEN_DEFAULT`] when absent (the C app's
    /// fallback).  Returns an empty Config when the file is unreadable.
    pub fn load(path: &Path) -> std::io::Result<Config> {
        let mut cfg = parse_kv_file(path)?;
        // The scratch-root override (C eh_load_config_file reads
        // /tmp/bookshelf.cfg LAST so it wins): the emulator guest's app
        // dir is read-only, so settings saves land in /tmp and are
        // re-applied on top of the base config every launch.  The e2e
        // suite also uses it to point the app at a dead/delayed API.
        let tmp = Path::new("/tmp/bookshelf.cfg");
        if tmp != path && tmp.is_file() {
            cfg.merge(parse_kv_file(tmp).unwrap_or_default());
        }
        Ok(cfg.finish())
    }

    /// The full device discovery (C eh_load_config_file): three layers
    /// read in order — the config next to the executable (argv0 dir),
    /// then the platform base dir (`/etc/pbemu`), then the write-root
    /// (`/tmp`) override — each later layer winning per key.  The
    /// /tmp/bookshelf.cfg scratch file doubles as the write-root layer.
    /// Missing layers are skipped; the token defaults when no layer set it.
    pub fn discover(argv0: Option<&str>) -> Config {
        let mut layers: Vec<PathBuf> = Vec::new();
        if let Some(a) = argv0 {
            if a.contains('/') {
                let dir = Path::new(a).parent().unwrap_or(Path::new(""));
                if !dir.as_os_str().is_empty() {
                    layers.push(dir.join(CONFIG_FILENAME));
                }
            }
        }
        layers.push(Path::new(CONFIG_BASE_DIR).join(CONFIG_FILENAME));

        let mut cfg = Config::default();
        for path in &layers {
            if let Ok(part) = parse_kv_file(path) {
                crate::logger::log(&format!("[bookshelf] config: {}", path.display()));
                cfg.merge(part);
            }
        }
        // The write-root override is re-applied last so a settings save
        // that had to fall back to the scratch root beats the read-only
        // base config on the next launch (C eh_load_config_file's third
        // pass, "(override)" marker).
        let tmp = Path::new(CONFIG_WRITE_ROOT).join(CONFIG_FILENAME);
        if let Ok(part) = parse_kv_file(&tmp) {
            crate::logger::log(&format!("[bookshelf] config: {} (override)", tmp.display()));
            cfg.merge(part);
        }
        cfg.finish()
    }

    /// Host/SDL launch order: [`Config::discover`] (argv0-dir → base →
    /// write-root), then the run-dir config last so a harness that writes
    /// `./bookshelf.cfg` into its working directory (eh_host's contract)
    /// wins per key.  `run-visible-sdl.sh` writes the config next to the
    /// binary instead, which discover's argv0 layer picks up.
    pub fn load_for_run(run_dir: &Path, argv0: Option<&str>) -> Config {
        let mut cfg = Self::discover(argv0);
        let run_cfg = run_dir.join(CONFIG_FILENAME);
        if run_cfg.exists() {
            if let Ok(part) = parse_kv_file(&run_cfg) {
                crate::logger::log(&format!("[bookshelf] config: {}", run_cfg.display()));
                cfg.merge(part);
            }
        }
        cfg.finish()
    }

    /// Path settings saves go to (C eh_resolve_config_path): the argv0-dir
    /// config, else the base-dir config — but only when its DIRECTORY is
    /// writable (settings and the library store are created next to the
    /// config file, so a writable file alone is not enough); otherwise the
    /// guest-writable write root, which [`Config::discover`] re-reads as
    /// the override layer on the next launch.
    pub fn resolve_config_path(argv0: Option<&str>) -> PathBuf {
        let primary = argv0
            .filter(|a| a.contains('/'))
            .and_then(|a| {
                let dir = Path::new(a).parent().unwrap_or(Path::new(""));
                if dir.as_os_str().is_empty() {
                    None
                } else {
                    Some(dir.join(CONFIG_FILENAME))
                }
            })
            .unwrap_or_else(|| Path::new(CONFIG_BASE_DIR).join(CONFIG_FILENAME));
        if primary.parent().map(dir_writable).unwrap_or(false) {
            return primary;
        }
        Path::new(CONFIG_WRITE_ROOT).join(CONFIG_FILENAME)
    }

    /// Persist the config as a plain `key=value` list (C
    /// eh_write_config_file): api_url, api_token, downloads_dir, source,
    /// group, reader (path, or `auto` for the firmware reader).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut text = format!("api_url={}\n", self.api_url);
        text.push_str(&format!("api_token={}\n", self.api_token));
        if let Some(d) = &self.downloads_dir {
            if !d.is_empty() {
                text.push_str(&format!("downloads_dir={d}\n"));
            }
        }
        text.push_str(&format!(
            "source={}\n",
            self.source.as_deref().unwrap_or("kavita")
        ));
        text.push_str(&format!(
            "group={}\n",
            self.group.as_deref().unwrap_or("none")
        ));
        text.push_str(&format!(
            "reader={}\n",
            self.reader.as_deref().unwrap_or("auto")
        ));
        std::fs::write(path, text)
    }
}

/// Parse one `key=value` config file (blank lines + `#` comments skipped),
/// shared by `load`, the /tmp override pass, and the boot-save (which must
/// persist the base values, not the override).
pub(crate) fn parse_kv_file(path: &Path) -> std::io::Result<Config> {
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
            "reader" => cfg.reader = Some(value.to_string()),
            "downloads_dir" | "download_dir" => cfg.downloads_dir = Some(value.to_string()),
            "source" => cfg.source = Some(value.to_string()),
            "group" => cfg.group = Some(value.to_string()),
            // C cfg_set_language: trimmed value, lowercased, truncated to
            // 3 chars (en/de/fr/it…).  Stored now; i18n consumes it in
            // wave 2.
            "language" | "lang" => {
                let lang: String = value
                    .chars()
                    .take(3)
                    .map(|c| c.to_ascii_lowercase())
                    .collect();
                cfg.language = Some(lang);
            }
            _ => {}
        }
    }
    Ok(cfg)
}

/// Platform base dir for the config file (C eh_plat_config_base_dir).
const CONFIG_BASE_DIR: &str = "/etc/pbemu";
/// Guest-writable scratch root (C eh_plat_write_root).
const CONFIG_WRITE_ROOT: &str = "/tmp";
/// Config file name (C EH_CONFIG_FILENAME).
const CONFIG_FILENAME: &str = "bookshelf.cfg";

/// `access(dir, W_OK)` equivalent: a directory is writable when a probe
/// file can be created in it (permissions + a real write, not just a mode
/// bit read).
fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(".bookshelf_cfg_probe");
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
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

    #[test]
    fn layers_later_win_per_key() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base.cfg");
        let over = dir.path().join("over.cfg");
        std::fs::write(
            &base,
            "api_url=http://first:1\napi_token=tok1\nreader=/a/reader.app\n",
        )
        .unwrap();
        std::fs::write(&over, "url=http://second:2\nlang=de\n").unwrap();
        // Layered load: the later layer wins only the keys it carries.
        let mut cfg = Config::default();
        for p in [&base, &over] {
            cfg.merge(parse_kv_file(p).unwrap());
        }
        let cfg = cfg.finish();
        assert_eq!(cfg.api_url, "http://second:2");
        assert_eq!(cfg.api_token, "tok1");
        assert_eq!(cfg.reader.as_deref(), Some("/a/reader.app"));
        // language=/lang= is stored lowercased + truncated (C
        // cfg_set_language); i18n::init consumes it.
        assert_eq!(cfg.language.as_deref(), Some("de"));

        assert_eq!(parse_kv_file(&base).unwrap().language, None);
    }

    #[test]
    fn run_layer_beats_argv0_layer() {
        // run-visible-sdl.sh writes build/bookshelf.cfg (argv0 layer);
        // a harness-written ./bookshelf.cfg must still win per key.
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("build");
        std::fs::create_dir(&bin_dir).unwrap();
        std::fs::write(
            bin_dir.join("bookshelf.cfg"),
            "api_url=http://argv0:1\napi_token=argv0tok\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("bookshelf.cfg"), "api_url=http://run:2\n").unwrap();
        let cfg = Config::load_for_run(
            dir.path(),
            Some(bin_dir.join("bookshelf.pc").to_str().unwrap()),
        );
        assert_eq!(cfg.api_url, "http://run:2");
        // Keys the run layer does not carry keep the argv0 value.
        assert_eq!(cfg.api_token, "argv0tok");
    }

    #[test]
    fn resolve_prefers_writable_dir_else_write_root() {
        let dir = tempfile::tempdir().unwrap();
        // A writable argv0 dir keeps the config next to the binary.
        let argv0 = dir.path().join("bookshelf.app");
        let argv0 = argv0.to_str().unwrap();
        assert_eq!(
            Config::resolve_config_path(Some(argv0)),
            dir.path().join("bookshelf.cfg")
        );
        // A directory that cannot take new files (mode 0o555) pushes the
        // save target to the write root (C access(dir, W_OK) check) —
        // skipped when running as root, which ignores the mode bits.
        let ro = tempfile::tempdir().unwrap();
        let path = ro.path();
        if dir_writable(path) {
            return;
        }
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o555);
        std::fs::set_permissions(path, perms).unwrap();
        let argv0 = path.join("bookshelf.app");
        let argv0 = argv0.to_str().unwrap();
        assert_eq!(
            Config::resolve_config_path(Some(argv0)),
            Path::new("/tmp/bookshelf.cfg")
        );
    }
}
