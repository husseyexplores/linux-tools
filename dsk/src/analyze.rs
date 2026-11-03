use crate::scan::DirNode;
use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Cat {
    Video,
    Audio,
    Image,
    Archive,
    Document,
    Code,
    Data,
    Binary,
    Other,
}

impl Cat {
    pub fn name(self) -> &'static str {
        match self {
            Cat::Video => "Video",
            Cat::Audio => "Audio",
            Cat::Image => "Images",
            Cat::Archive => "Archives",
            Cat::Document => "Docs",
            Cat::Code => "Code",
            Cat::Data => "Data",
            Cat::Binary => "Binary",
            Cat::Other => "Other",
        }
    }
}

pub fn cat_of(ext: &str) -> Cat {
    use Cat::*;
    match ext {
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "wmv" | "flv" | "m4v" | "mpg" | "mpeg" | "3gp" | "ogv" | "m2ts" => Video,
        "mp3" | "flac" | "wav" | "ogg" | "opus" | "m4a" | "aac" | "wma" | "aiff" | "mid" => Audio,
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "svg" | "bmp" | "tiff" | "tif" | "ico" | "heic" | "avif" | "raw"
        | "cr2" | "nef" | "arw" | "psd" | "xcf" | "kra" => Image,
        "zip" | "tar" | "gz" | "tgz" | "xz" | "zst" | "bz2" | "7z" | "rar" | "lz4" | "deb" | "rpm" | "appimage"
        | "flatpak" | "snap" | "whl" | "jar" | "crate" | "apk" | "dmg" | "cab" => Archive,
        "pdf" | "doc" | "docx" | "odt" | "xls" | "xlsx" | "ods" | "ppt" | "pptx" | "odp" | "txt" | "md" | "epub"
        | "rtf" | "tex" | "mobi" | "djvu" => Document,
        "js" | "mjs" | "cjs" | "ts" | "mts" | "cts" | "tsx" | "jsx" | "map" | "rs" | "go" | "py" | "pyc" | "c" | "h"
        | "cpp" | "hpp" | "cc" | "java" | "kt" | "class" | "rb" | "php" | "lua" | "sh" | "zsh" | "css" | "scss"
        | "less" | "html" | "vue" | "svelte" | "astro" | "swift" | "dart" | "zig" | "ex" | "exs" | "cs" | "d.ts"
        | "flow" | "pyi" | "el" | "elc" => Code,
        "json" | "csv" | "xml" | "yaml" | "yml" | "toml" | "sql" | "db" | "sqlite" | "sqlite3" | "parquet" | "log"
        | "jsonl" | "ndjson" | "lock" | "ldb" | "idx" | "dat" => Data,
        "so" | "o" | "a" | "rlib" | "rmeta" | "bin" | "exe" | "dll" | "iso" | "img" | "qcow2" | "vdi" | "vmdk"
        | "node" | "dylib" | "ko" | "gguf" | "safetensors" | "pt" | "pth" | "onnx" | "ckpt" | "pack" | "wasm"
        | "pdb" | "dwo" | "vhd" | "vhdx" => Binary,
        _ => Other,
    }
}

const TOP_N: usize = 50;

pub struct ExtStat {
    pub ext: Box<str>,
    pub cat: Cat,
    pub size: u64,
    pub count: u64,
    pub top: Vec<(u64, PathBuf)>,
}

pub struct Artifact {
    pub path: PathBuf,
    pub kind: &'static str,
    pub size: u64,
    /// Project's last source modification.
    pub mtime: i64,
}

pub struct Analysis {
    pub exts: Vec<ExtStat>,
    pub cats: Vec<(Cat, u64)>,
    pub largest: Vec<(u64, PathBuf)>,
    pub artifacts: Vec<Artifact>,
    pub files: u64,
}

#[derive(Default)]
struct Acc {
    size: u64,
    count: u64,
    heap: BinaryHeap<Reverse<(u64, PathBuf)>>,
}

impl Acc {
    #[inline]
    fn push(&mut self, size: u64, path: impl FnOnce() -> PathBuf) {
        self.size += size;
        self.count += 1;
        if self.heap.len() < TOP_N {
            self.heap.push(Reverse((size, path())));
        } else if size > self.heap.peek().map_or(0, |r| r.0 .0) {
            self.heap.pop();
            self.heap.push(Reverse((size, path())));
        }
    }
    fn top(self) -> Vec<(u64, PathBuf)> {
        let mut v: Vec<_> = self.heap.into_iter().map(|r| r.0).collect();
        v.sort_by(|a, b| b.0.cmp(&a.0));
        v
    }
}

fn ext_of<'a>(name: &str, buf: &'a mut String) -> &'a str {
    buf.clear();
    if let Some(i) = name.rfind('.') {
        let e = &name[i + 1..];
        if i > 0 && !e.is_empty() && e.len() <= 12 {
            buf.extend(e.chars().flat_map(char::to_lowercase));
        }
    }
    buf.as_str()
}

fn artifact_kind(parent: &DirNode, d: &DirNode) -> Option<&'static str> {
    let has = |f: &str| parent.files.iter().any(|x| &*x.name == f);
    Some(match &*d.name {
        "node_modules" => "node_modules",
        ".git" => ".git",
        "target" if has("Cargo.toml") => "rust target",
        ".next" | ".nuxt" | ".svelte-kit" | ".turbo" | ".parcel-cache" | ".angular" | ".output" | ".vercel" => "js cache",
        "dist" | "build" | "out" | "coverage" | "storybook-static" if has("package.json") => "js build",
        "build" | ".gradle" if has("build.gradle") || has("build.gradle.kts") || has("settings.gradle") => "gradle",
        ".venv" | "venv" | "env" if d.files.iter().any(|f| &*f.name == "pyvenv.cfg") => "python venv",
        "__pycache__" | ".pytest_cache" | ".mypy_cache" | ".ruff_cache" | ".tox" => "python cache",
        ".zig-cache" | "zig-cache" | "zig-out" => "zig",
        "_build" | "deps" if has("mix.exs") => "elixir",
        "build" | ".dart_tool" if has("pubspec.yaml") => "flutter",
        "vendor" if has("composer.json") => "php vendor",
        "cmake-build-debug" | "cmake-build-release" => "cmake",
        _ => return None,
    })
}

struct Walker {
    exts: HashMap<Box<str>, Acc>,
    all: Acc,
    arts: Vec<Artifact>,
    buf: String,
    files: u64,
}

impl Walker {
    fn walk(&mut self, n: &DirNode, p: &mut PathBuf, in_art: bool) {
        self.files += n.files.len() as u64;
        for f in &n.files {
            let ext = ext_of(&f.name, &mut self.buf);
            let acc = match self.exts.get_mut(ext) {
                Some(a) => a,
                None => self.exts.entry(ext.into()).or_default(),
            };
            acc.push(f.size, || p.join(&*f.name));
            self.all.push(f.size, || p.join(&*f.name));
        }
        for d in &n.dirs {
            let mut art = in_art;
            if !in_art {
                if let Some(kind) = artifact_kind(n, d) {
                    self.arts.push(Artifact { path: p.join(&*d.name), kind, size: d.size, mtime: n.src_mtime });
                    art = true;
                }
            }
            // Don't look for project artifacts inside hidden dirs (~/.cache, ~/.local, ~/.cargo…):
            // those belong to apps/tools, not your projects.
            if d.name.starts_with('.') {
                art = true;
            }
            p.push(&*d.name);
            self.walk(d, p, art);
            p.pop();
        }
    }
}

pub fn analyze(root: &DirNode, root_path: &Path) -> Analysis {
    let mut w = Walker { exts: HashMap::new(), all: Acc::default(), arts: vec![], buf: String::new(), files: 0 };
    let mut p = root_path.to_path_buf();
    w.walk(root, &mut p, false);

    let mut cats: HashMap<Cat, u64> = HashMap::new();
    let mut exts: Vec<ExtStat> = w
        .exts
        .into_iter()
        .map(|(ext, acc)| {
            let cat = cat_of(&ext);
            *cats.entry(cat).or_default() += acc.size;
            ExtStat { ext, cat, size: acc.size, count: acc.count, top: acc.top() }
        })
        .collect();
    exts.sort_by(|a, b| b.size.cmp(&a.size));
    let mut cats: Vec<(Cat, u64)> = cats.into_iter().filter(|c| c.1 > 0).collect();
    cats.sort_by(|a, b| b.1.cmp(&a.1));
    w.arts.sort_by(|a, b| b.size.cmp(&a.size));
    Analysis { exts, cats, largest: w.all.top(), artifacts: w.arts, files: w.files }
}
