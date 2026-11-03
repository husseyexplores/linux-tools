use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MIB: u64 = 1024 * 1024;

pub fn size(b: u64) -> String {
    const U: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if b < 1024 {
        return format!("{b} B");
    }
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", U[i])
    } else if v >= 10.0 {
        format!("{v:.1} {}", U[i])
    } else {
        format!("{v:.2} {}", U[i])
    }
}

pub fn count(n: u64) -> String {
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{:.1}K", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn age(mtime: i64) -> String {
    if mtime <= 0 {
        return "—".into();
    }
    let s = (now() - mtime).max(0);
    let d = s / 86400;
    if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h", s / 3600)
    } else if d < 60 {
        format!("{d}d")
    } else if d < 730 {
        format!("{}mo", d / 30)
    } else {
        format!("{}y", d / 365)
    }
}

/// Replace the home prefix with `~`.
pub fn tilde(p: &Path, home: &Path) -> String {
    if home != Path::new("/") {
        if let Ok(r) = p.strip_prefix(home) {
            return if r.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", r.display())
            };
        }
    }
    p.display().to_string()
}

/// Keep the tail of a string, prefixing with an ellipsis.
pub fn trunc_left(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        return s.to_string();
    }
    if w <= 1 {
        return "…".into();
    }
    let tail: String = s.chars().skip(n - (w - 1)).collect();
    format!("…{tail}")
}
