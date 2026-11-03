use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    /// Glob patterns. No '/' → matches a name anywhere (e.g. "node_modules").
    /// With '/' → matches a full path (e.g. "~/vms", "/mnt/backup/**").
    pub exclude: Vec<String>,
    /// Do not cross filesystem boundaries while scanning.
    pub one_fs: bool,
    /// Cleaner ids hidden from the Clean tab.
    pub hidden_cleaners: Vec<String>,
    /// Hide dev artifacts smaller than this (MiB).
    pub dev_min_mb: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self { exclude: vec![], one_fs: false, hidden_cleaners: vec![], dev_min_mb: 1 }
    }
}

impl Config {
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
            give_back(dir);
        }
        let body = toml::to_string_pretty(self).unwrap_or_default();
        fs::write(path, format!("# dsk config\n{body}"))?;
        give_back(path);
        Ok(())
    }
}

/// When running under sudo, keep files in the user's home owned by the user.
fn give_back(p: &Path) {
    let (Ok(uid), Ok(gid)) = (std::env::var("SUDO_UID"), std::env::var("SUDO_GID")) else { return };
    let (Ok(uid), Ok(gid)) = (uid.parse(), gid.parse()) else { return };
    let _ = nix::unistd::chown(
        p,
        Some(nix::unistd::Uid::from_raw(uid)),
        Some(nix::unistd::Gid::from_raw(gid)),
    );
}
