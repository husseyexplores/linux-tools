use std::{collections::HashSet, fs, path::{Path, PathBuf}};

/// Filesystems that hold real data. Everything else (proc, tmpfs, overlay,
/// squashfs, network mounts…) is hidden and skipped while scanning.
pub const REAL_FS: &[&str] = &[
    "ext2", "ext3", "ext4", "xfs", "btrfs", "f2fs", "vfat", "exfat", "ntfs", "ntfs3", "fuseblk",
    "zfs", "jfs", "reiserfs", "bcachefs", "hfsplus", "nilfs2",
];

pub struct RawMount {
    pub source: String,
    pub target: PathBuf,
    pub fstype: String,
}

fn unescape(s: &str) -> String {
    // /proc/mounts escapes space, tab, newline, backslash as \ooo
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 4], 8) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn raw_mounts() -> Vec<RawMount> {
    let Ok(s) = fs::read_to_string("/proc/self/mounts") else { return vec![] };
    s.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some(RawMount {
                source: unescape(it.next()?),
                target: PathBuf::from(unescape(it.next()?)),
                fstype: it.next()?.to_string(),
            })
        })
        .collect()
}

/// Mount points of pseudo/network filesystems: never descend into these.
pub fn pseudo_mounts() -> HashSet<PathBuf> {
    raw_mounts()
        .into_iter()
        .filter(|m| !REAL_FS.contains(&m.fstype.as_str()))
        .map(|m| m.target)
        .collect()
}

pub struct Mount {
    pub source: String,
    pub targets: Vec<PathBuf>,
    pub fstype: String,
    pub total: u64,
    pub used: u64,
    pub avail: u64,
}

impl Mount {
    pub fn frac(&self) -> f64 {
        let d = self.used + self.avail;
        if d == 0 { 0.0 } else { self.used as f64 / d as f64 }
    }
}

pub fn mounts() -> Vec<Mount> {
    let mut out: Vec<Mount> = vec![];
    for m in raw_mounts().into_iter().filter(|m| REAL_FS.contains(&m.fstype.as_str())) {
        // btrfs subvolumes / bind mounts share a source: show once
        if let Some(e) = out.iter_mut().find(|e| e.source == m.source) {
            e.targets.push(m.target);
            continue;
        }
        let Ok(st) = nix::sys::statvfs::statvfs(&m.target) else { continue };
        let fr = st.fragment_size() as u64;
        let total = st.blocks() as u64 * fr;
        let free = st.blocks_free() as u64 * fr;
        let avail = st.blocks_available() as u64 * fr;
        out.push(Mount {
            source: m.source,
            targets: vec![m.target],
            fstype: m.fstype,
            total,
            used: total.saturating_sub(free),
            avail,
        });
    }
    for m in &mut out {
        m.targets.sort_by_key(|t| t.as_os_str().len());
    }
    out.sort_by(|a, b| a.targets[0].cmp(&b.targets[0]));
    out
}

/// Only the filesystem that holds `p` (longest matching mount point).
pub fn containing(ms: Vec<Mount>, p: &Path) -> Vec<Mount> {
    let best = ms
        .iter()
        .enumerate()
        .filter_map(|(i, m)| m.targets.iter().filter(|t| p.starts_with(t)).map(|t| t.as_os_str().len()).max().map(|l| (i, l)))
        .max_by_key(|x| x.1)
        .map(|x| x.0);
    ms.into_iter().enumerate().filter(|(i, _)| Some(*i) == best).map(|(_, m)| m).collect()
}

pub struct Drive {
    pub name: String,
    pub model: String,
    pub size: u64,
    pub kind: &'static str,
}

fn read(p: String) -> String {
    fs::read_to_string(p).map(|s| s.trim().to_string()).unwrap_or_default()
}

pub fn drives() -> Vec<Drive> {
    let Ok(rd) = fs::read_dir("/sys/block") else { return vec![] };
    let mut v: Vec<Drive> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if ["loop", "ram", "zram", "dm-", "nbd", "fd", "sr"].iter().any(|p| name.starts_with(p)) {
                return None;
            }
            let size = read(format!("/sys/block/{name}/size")).parse::<u64>().ok()? * 512;
            if size == 0 {
                return None;
            }
            let vendor = read(format!("/sys/block/{name}/device/vendor"));
            let mut model = read(format!("/sys/block/{name}/device/model"));
            if model.is_empty() {
                model = read(format!("/sys/block/{name}/device/name"));
            }
            if !vendor.is_empty() && !model.starts_with(&vendor) && vendor != "ATA" {
                model = format!("{vendor} {model}");
            }
            let link = fs::read_link(format!("/sys/block/{name}"))
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let rot = read(format!("/sys/block/{name}/queue/rotational")) == "1";
            let kind = if link.contains("/usb") {
                "USB"
            } else if name.starts_with("nvme") {
                "NVMe"
            } else if name.starts_with("mmcblk") {
                "SD/MMC"
            } else if rot {
                "HDD"
            } else {
                "SSD"
            };
            Some(Drive { name, model, size, kind })
        })
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}
