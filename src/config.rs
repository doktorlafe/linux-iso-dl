//! Settings: `key=value` lines, one per line.

use std::env;
use std::fs;
use std::io;
use std::path::{self, Path, PathBuf};

use crate::NAME;

pub struct Config {
    /// Where downloaded images are stored.
    pub dir: PathBuf,
}

fn home() -> Result<PathBuf, String> {
    Ok(PathBuf::from(
        env::var_os("HOME").ok_or("není nastaveno HOME")?,
    ))
}

/// Turns what the user typed into an absolute path, with `~` standing for the home directory.
pub fn expand(text: &str) -> Result<PathBuf, String> {
    let text = text.trim();
    if text.is_empty() || text.contains('\n') {
        return Err(format!("neplatná složka {text:?}"));
    }
    let dir = match text.strip_prefix('~') {
        Some("") => home()?,
        Some(rest) if rest.starts_with('/') => home()?.join(rest.trim_start_matches('/')),
        _ => PathBuf::from(text),
    };
    path::absolute(&dir).map_err(|e| format!("neplatná složka {text:?}: {e}"))
}

pub fn path() -> Result<PathBuf, String> {
    if let Some(p) = env::var_os("LINUX_ISO_DL_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    let dir = match env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => home()?.join(".config"),
    };
    Ok(dir.join(NAME).join("config"))
}

pub fn load(path: &Path) -> Result<Config, String> {
    let mut dir = None;
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("nelze číst {}: {e}", path.display())),
    };
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = |why: &str| format!("{}:{}: {why}", path.display(), i + 1);
        let (key, value) = line.split_once('=').ok_or_else(|| bad("chybí '='"))?;
        match key {
            "dir" => dir = Some(PathBuf::from(value)),
            _ => return Err(bad(&format!("neznámý klíč {key:?}"))),
        }
    }
    let dir = match dir {
        Some(dir) => dir,
        None => home()?.join("Downloads").join(NAME),
    };
    Ok(Config { dir })
}

pub fn save(path: &Path, cfg: &Config) -> Result<(), String> {
    let text = format!("dir={}\n", cfg.dir.display());
    let write = || -> io::Result<()> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        // Renamed into place so a crash never leaves half a config behind.
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, &text)?;
        fs::rename(&tmp, path)
    };
    write().map_err(|e| format!("nelze zapsat {}: {e}", path.display()))
}
