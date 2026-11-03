use globset::{Glob, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use std::{
    collections::HashSet,
    ffi::OsStr,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
        Mutex,
    },
};

pub struct FileEnt {
    pub name: Box<str>,
    pub size: u64,
}

pub struct DirNode {
    pub name: Box<str>,
    /// Disk usage (allocated blocks) incl. everything below.
    pub size: u64,
    /// Number of entries below.
    pub count: u64,
    /// Newest file mtime below, ignoring build/dependency dirs (used for project age).
    pub src_mtime: i64,
    /// Directory could not be read.
    pub err: bool,
    pub dirs: Vec<DirNode>,
    pub files: Vec<FileEnt>,
}

impl DirNode {
    /// Remove an entry by relative path components. Returns (size, count) removed.
    pub fn remove(&mut self, comps: &[&str]) -> Option<(u64, u64)> {
        let (first, rest) = comps.split_first()?;
        let r = if rest.is_empty() {
            if let Some(i) = self.dirs.iter().position(|d| &*d.name == *first) {
                let d = self.dirs.remove(i);
                (d.size, d.count + 1)
            } else {
                let i = self.files.iter().position(|f| &*f.name == *first)?;
                (self.files.remove(i).size, 1)
            }
        } else {
            self.dirs.iter_mut().find(|d| &*d.name == *first)?.remove(rest)?
        };
        self.size = self.size.saturating_sub(r.0);
        self.count = self.count.saturating_sub(r.1);
        Some(r)
    }
}

#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub bytes: AtomicU64,
    pub errors: AtomicU64,
    pub cancel: AtomicBool,
    pub current: Mutex<String>,
}

pub struct Excludes {
    full: GlobSet,
    base: GlobSet,
    has_full: bool,
    has_base: bool,
}

impl Excludes {
    pub fn new(pats: &[String], home: &Path) -> Self {
        let (mut full, mut base) = (GlobSetBuilder::new(), GlobSetBuilder::new());
        let (mut has_full, mut has_base) = (false, false);
        for p in pats {
            let p = p.trim().trim_end_matches('/');
            if p.is_empty() {
                continue;
            }
            let p = match p.strip_prefix("~/") {
                Some(r) => home.join(r).to_string_lossy().into_owned(),
                None => p.to_string(),
            };
            let Ok(g) = Glob::new(&p) else { continue };
            if p.contains('/') {
                full.add(g);
                has_full = true;
            } else {
                base.add(g);
                has_base = true;
            }
        }
        Self {
            full: full.build().unwrap_or_else(|_| GlobSet::empty()),
            base: base.build().unwrap_or_else(|_| GlobSet::empty()),
            has_full,
            has_base,
        }
    }

    #[inline]
    fn skip(&self, dir: &Path, name: &OsStr) -> bool {
        (self.has_base && self.base.is_match(name)) || (self.has_full && self.full.is_match(dir.join(name)))
    }
}

pub struct ScanOpts {
    pub one_fs: bool,
    pub skip: HashSet<PathBuf>,
    pub ex: Excludes,
}

impl ScanOpts {
    pub fn new(root: &Path, pats: &[String], home: &Path, one_fs: bool) -> Self {
        let mut skip = crate::disks::pseudo_mounts();
        skip.remove(root);
        if root == Path::new("/") {
            for p in ["/proc", "/sys", "/dev", "/run"] {
                skip.insert(p.into());
            }
        }
        Self { one_fs, skip, ex: Excludes::new(pats, home) }
    }
}

/// Dirs whose contents don't count towards a project's "last touched" time.
pub fn is_artifact_name(n: &str) -> bool {
    matches!(
        n,
        "node_modules" | "target" | ".git" | "dist" | "build" | "out" | ".next" | ".nuxt" | ".svelte-kit"
            | ".turbo" | ".parcel-cache" | ".angular" | ".venv" | "venv" | "__pycache__" | ".gradle"
            | "coverage" | ".cache" | ".pytest_cache" | ".mypy_cache" | ".ruff_cache" | ".tox"
            | "zig-cache" | ".zig-cache" | "zig-out" | "_build" | "deps" | ".dart_tool"
    )
}

struct Ctx<'a> {
    opts: &'a ScanOpts,
    prog: &'a Progress,
    hard: Mutex<HashSet<(u64, u64)>>,
    dev: u64,
}

pub fn scan(root: &Path, opts: &ScanOpts, prog: &Progress) -> DirNode {
    let md = fs::metadata(root).ok();
    let ctx = Ctx {
        opts,
        prog,
        hard: Mutex::new(HashSet::new()),
        dev: md.as_ref().map(|m| m.dev()).unwrap_or(0),
    };
    let own = md.map(|m| m.blocks() * 512).unwrap_or(0);
    scan_dir(&ctx, root.to_path_buf(), root.to_string_lossy().into(), own)
}

fn scan_dir(ctx: &Ctx, path: PathBuf, name: Box<str>, own: u64) -> DirNode {
    let mut node = DirNode { name, size: own, count: 0, src_mtime: 0, err: false, dirs: vec![], files: vec![] };
    if ctx.prog.cancel.load(Relaxed) {
        return node;
    }
    let rd = match fs::read_dir(&path) {
        Ok(r) => r,
        Err(_) => {
            node.err = true;
            ctx.prog.errors.fetch_add(1, Relaxed);
            return node;
        }
    };
    let mut subs: Vec<(PathBuf, Box<str>, u64)> = vec![];
    let mut fbytes = 0u64;
    for ent in rd {
        let Ok(ent) = ent else {
            ctx.prog.errors.fetch_add(1, Relaxed);
            continue;
        };
        let fname = ent.file_name();
        let Ok(md) = ent.metadata() else {
            ctx.prog.errors.fetch_add(1, Relaxed);
            continue;
        };
        if ctx.opts.ex.skip(&path, &fname) {
            continue;
        }
        let name: Box<str> = fname.to_string_lossy().into();
        if md.is_dir() {
            let p = path.join(&fname);
            if (ctx.opts.one_fs && md.dev() != ctx.dev) || ctx.opts.skip.contains(&p) {
                continue;
            }
            subs.push((p, name, md.blocks() * 512));
        } else {
            let mut size = md.blocks() * 512;
            if md.nlink() > 1 && !ctx.hard.lock().unwrap().insert((md.dev(), md.ino())) {
                size = 0; // hardlink already counted
            }
            node.src_mtime = node.src_mtime.max(md.mtime());
            fbytes += size;
            node.files.push(FileEnt { name, size });
        }
    }
    ctx.prog.files.fetch_add(node.files.len() as u64, Relaxed);
    ctx.prog.bytes.fetch_add(fbytes, Relaxed);
    if ctx.prog.dirs.fetch_add(1, Relaxed) % 128 == 0 {
        if let Ok(mut c) = ctx.prog.current.try_lock() {
            *c = path.to_string_lossy().into_owned();
        }
    }

    node.dirs = subs.into_par_iter().map(|(p, n, s)| scan_dir(ctx, p, n, s)).collect();

    node.size += fbytes;
    node.count += node.files.len() as u64;
    for d in &node.dirs {
        node.size += d.size;
        node.count += d.count + 1;
        if !is_artifact_name(&d.name) {
            node.src_mtime = node.src_mtime.max(d.src_mtime);
        }
    }
    node.dirs.sort_unstable_by(|a, b| b.size.cmp(&a.size));
    node.files.sort_unstable_by(|a, b| b.size.cmp(&a.size));
    node.dirs.shrink_to_fit();
    node.files.shrink_to_fit();
    node
}

/// Bytes that deleting `path` (or files under it accepted by `keep`) would free.
/// Hardlinked files (nlink > 1) are not counted since removing one link frees nothing.
pub fn du_filtered(path: &Path, keep: &(dyn Fn(&str) -> bool + Sync)) -> u64 {
    let Ok(md) = fs::symlink_metadata(path) else { return 0 };
    if !md.is_dir() {
        let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
        return if md.nlink() == 1 && keep(&name) { md.blocks() * 512 } else { 0 };
    }
    let Ok(rd) = fs::read_dir(path) else { return 0 };
    let mut subs = vec![];
    let mut total = 0;
    for e in rd.flatten() {
        let Ok(m) = e.metadata() else { continue };
        if m.is_dir() {
            subs.push(e.path());
        } else if m.nlink() == 1 && keep(&e.file_name().to_string_lossy()) {
            total += m.blocks() * 512;
        }
    }
    total + subs.par_iter().map(|p| du_filtered(p, keep)).sum::<u64>()
}

pub fn du(path: &Path) -> u64 {
    du_filtered(path, &|_| true)
}
