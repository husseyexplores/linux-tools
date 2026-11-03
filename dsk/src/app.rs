use crate::{
    analyze::{self, Analysis},
    clean::{self, Target},
    config::Config,
    disks::{self, Drive, Mount},
    fmt, is_root,
    scan::{self, DirNode, Progress, ScanOpts},
    sudo_user,
    theme::Theme,
    Cli,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use rayon::prelude::*;
use std::{
    collections::HashSet,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::Ordering::Relaxed,
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Disks,
    Explorer,
    Types,
    Dev,
    Clean,
}

impl Tab {
    pub const ALL: [Tab; 5] = [Tab::Disks, Tab::Explorer, Tab::Types, Tab::Dev, Tab::Clean];
    pub fn title(self) -> &'static str {
        match self {
            Tab::Disks => "Overview",
            Tab::Explorer => "Explorer",
            Tab::Types => "Types",
            Tab::Dev => "Dev",
            Tab::Clean => "Clean",
        }
    }
}

/// Cursor + scroll offset of a list. `h` is the last rendered height.
#[derive(Default, Clone, Copy)]
pub struct ListSt {
    pub sel: usize,
    pub off: usize,
    pub h: usize,
}

impl ListSt {
    pub fn clamp(&mut self, len: usize) {
        self.sel = if len == 0 { 0 } else { self.sel.min(len - 1) };
    }
    pub fn window(&mut self, h: usize, len: usize) -> usize {
        self.h = h.max(1);
        self.clamp(len);
        if self.sel < self.off {
            self.off = self.sel;
        }
        if self.sel >= self.off + self.h {
            self.off = self.sel + 1 - self.h;
        }
        self.off = self.off.min(len.saturating_sub(self.h));
        self.off
    }
    pub fn mv(&mut self, d: isize, len: usize) {
        if len > 0 {
            self.sel = (self.sel as isize + d).clamp(0, len as isize - 1) as usize;
        }
    }
    pub fn nav(&mut self, code: KeyCode, len: usize) -> bool {
        let page = self.h.max(2) as isize - 1;
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.mv(-1, len),
            KeyCode::Down | KeyCode::Char('j') => self.mv(1, len),
            KeyCode::PageUp => self.mv(-page, len),
            KeyCode::PageDown => self.mv(page, len),
            KeyCode::Home | KeyCode::Char('g') => self.sel = 0,
            KeyCode::End | KeyCode::Char('G') => self.sel = len.saturating_sub(1),
            _ => return false,
        }
        true
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Size,
    Name,
    Items,
}

pub const AGES: [(i64, &str); 5] = [(0, "any age"), (30, "≥30d old"), (90, "≥90d old"), (180, "≥6mo old"), (365, "≥1y old")];

pub enum Pending {
    Delete { paths: Vec<PathBuf>, permanent: bool, bytes: u64 },
    Clean(Vec<&'static str>),
    GitGc(Vec<PathBuf>),
}

pub struct Dialog {
    pub title: String,
    pub body: Vec<String>,
    pub danger: bool,
    pub action: Pending,
}

pub enum External {
    Clean(Vec<&'static str>),
    GitGc(Vec<PathBuf>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ListId {
    Disks,
    Explorer,
    TyExt,
    TyFiles,
    Dev,
    Clean,
}

/// Clickable regions recorded while drawing.
#[derive(Clone, Copy)]
pub enum Hit {
    Tab(Tab),
    Key(KeyCode),
    Dialog(KeyCode),
    List { id: ListId, off: usize, len: usize },
}

pub enum Msg {
    Deleted { n: usize, bytes: u64, errs: Vec<String> },
    Sized(&'static str, Option<u64>),
}

pub struct ScanJob {
    pub prog: Arc<Progress>,
    rx: Receiver<(DirNode, Analysis)>,
    pub started: Instant,
}

pub struct App {
    pub tab: Tab,
    pub theme: Theme,
    pub home: PathBuf,
    pub root: PathBuf,
    pub cfg: Config,
    pub cfg_path: PathBuf,
    cli_excludes: Vec<String>,
    one_fs: bool,
    pub is_root: bool,
    /// Started on an explicit path: never show or navigate outside it.
    pub scoped: bool,
    pub hits: Vec<(Rect, Hit)>,
    last_click: Option<(ListId, usize, Instant)>,

    pub mounts: Vec<Mount>,
    pub drives: Vec<Drive>,
    pub disks_ls: ListSt,

    pub scan: Option<ScanJob>,
    pub tree: Option<DirNode>,
    pub analysis: Option<Analysis>,
    pub scan_secs: f64,
    pub scan_errors: u64,

    pub ex_stack: Vec<Box<str>>,
    ex_cursors: Vec<usize>,
    pub ex_rows: Vec<(bool, usize)>,
    pub ex_ls: ListSt,
    pub ex_marks: HashSet<Box<str>>,
    pub ex_sort: Sort,

    pub ty_ls: ListSt,
    pub ty_files_ls: ListSt,
    pub ty_focus_files: bool,

    pub dev_rows: Vec<usize>,
    pub dev_ls: ListSt,
    pub dev_marks: HashSet<PathBuf>,
    pub dev_age: usize,
    pub dev_kind: Option<&'static str>,
    pub dev_sort_age: bool,

    pub targets: Vec<Target>,
    pub cl_ls: ListSt,

    msg_tx: Sender<Msg>,
    msg_rx: Receiver<Msg>,
    pub dialog: Option<Dialog>,
    pub help: bool,
    pub toast: Option<(String, Instant, bool)>,
    pub external: Option<External>,
    pub quit: bool,
    pub spin: usize,
}

/// Resolve a directory from the tree by name stack.
pub fn resolve<'a>(tree: &'a DirNode, stack: &[Box<str>]) -> Option<&'a DirNode> {
    let mut n = tree;
    for name in stack {
        n = n.dirs.iter().find(|d| d.name == *name)?;
    }
    Some(n)
}

/// (name, size, items, is_dir, unreadable)
pub fn row_meta(d: &DirNode, r: (bool, usize)) -> (&str, u64, u64, bool, bool) {
    if r.0 {
        let x = &d.dirs[r.1];
        (&x.name, x.size, x.count, true, x.err)
    } else {
        let x = &d.files[r.1];
        (&x.name, x.size, 0, false, false)
    }
}

fn open_path(p: &Path) {
    let _ = Command::new("xdg-open")
        .arg(p)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

impl App {
    pub fn new(root: PathBuf, home: PathBuf, cfg: Config, cfg_path: PathBuf, cli: &Cli, mono: bool) -> Self {
        let (msg_tx, msg_rx) = mpsc::channel();
        let scoped = cli.path.is_some();
        let mut targets = clean::detect(&home, &cfg.hidden_cleaners);
        if scoped {
            clean::restrict(&mut targets, &root);
        }
        let (mounts, drives) = if scoped { (disks::containing(disks::mounts(), &root), vec![]) } else { (disks::mounts(), disks::drives()) };
        let mut app = App {
            tab: Tab::Disks,
            theme: Theme::new(mono),
            one_fs: cli.one_fs || cfg.one_fs,
            cli_excludes: cli.exclude.clone(),
            home,
            root,
            cfg,
            cfg_path,
            is_root: is_root(),
            scoped,
            hits: vec![],
            last_click: None,
            mounts,
            drives,
            disks_ls: ListSt::default(),
            scan: None,
            tree: None,
            analysis: None,
            scan_secs: 0.0,
            scan_errors: 0,
            ex_stack: vec![],
            ex_cursors: vec![],
            ex_rows: vec![],
            ex_ls: ListSt::default(),
            ex_marks: HashSet::new(),
            ex_sort: Sort::Size,
            ty_ls: ListSt::default(),
            ty_files_ls: ListSt::default(),
            ty_focus_files: false,
            dev_rows: vec![],
            dev_ls: ListSt::default(),
            dev_marks: HashSet::new(),
            dev_age: 0,
            dev_kind: None,
            dev_sort_age: false,
            targets,
            cl_ls: ListSt::default(),
            msg_tx,
            msg_rx,
            dialog: None,
            help: false,
            toast: None,
            external: None,
            quit: false,
            spin: 0,
        };
        app.start_scan();
        app.size_targets(None);
        app
    }

    pub fn toast(&mut self, s: impl Into<String>, err: bool) {
        self.toast = Some((s.into(), Instant::now(), err));
    }

    pub fn busy(&self) -> bool {
        self.scan.is_some() || self.targets.iter().any(|t| t.pending)
    }

    fn patterns(&self) -> Vec<String> {
        self.cfg.exclude.iter().chain(&self.cli_excludes).cloned().collect()
    }

    pub fn start_scan(&mut self) {
        if let Some(j) = &self.scan {
            j.prog.cancel.store(true, Relaxed);
        }
        let opts = ScanOpts::new(&self.root, &self.patterns(), &self.home, self.one_fs);
        let prog = Arc::new(Progress::default());
        let (tx, rx) = mpsc::channel();
        let (root, p2) = (self.root.clone(), prog.clone());
        thread::spawn(move || {
            let tree = scan::scan(&root, &opts, &p2);
            if p2.cancel.load(Relaxed) {
                return;
            }
            let an = analyze::analyze(&tree, &root);
            let _ = tx.send((tree, an));
        });
        self.scan = Some(ScanJob { prog, rx, started: Instant::now() });
    }

    fn set_root(&mut self, p: PathBuf) {
        if p == Path::new("/") && !self.is_root {
            self.toast("Not root: protected dirs are skipped. Run `sudo dsk /` for a full scan", true);
        }
        self.root = p;
        self.tree = None;
        self.analysis = None;
        self.ex_stack.clear();
        self.ex_cursors.clear();
        self.ex_ls = ListSt::default();
        self.dev_marks.clear();
        self.start_scan();
        self.tab = Tab::Explorer;
    }

    pub fn size_targets(&mut self, ids: Option<Vec<&'static str>>) {
        let jobs: Vec<(&'static str, clean::Sizer)> = self
            .targets
            .iter_mut()
            .filter(|t| ids.as_ref().map_or(true, |ids| ids.contains(&t.id)))
            .map(|t| {
                t.pending = true;
                (t.id, t.sizer.clone())
            })
            .collect();
        let tx = self.msg_tx.clone();
        thread::spawn(move || {
            jobs.into_par_iter().for_each_with(tx, |tx, (id, s)| {
                let _ = tx.send(Msg::Sized(id, clean::measure(&s)));
            });
        });
    }

    pub fn tick(&mut self) {
        self.spin = self.spin.wrapping_add(1);
        let done = self.scan.as_ref().and_then(|j| {
            j.rx.try_recv().ok().map(|r| (r, j.started.elapsed().as_secs_f64(), j.prog.errors.load(Relaxed)))
        });
        if let Some(((tree, an), secs, errs)) = done {
            self.scan = None;
            self.scan_secs = secs;
            self.scan_errors = errs;
            self.tree = Some(tree);
            self.analysis = Some(an);
            self.rebuild_ex();
            self.rebuild_dev();
        }
        while let Ok(m) = self.msg_rx.try_recv() {
            match m {
                Msg::Sized(id, size) => {
                    if let Some(t) = self.targets.iter_mut().find(|t| t.id == id) {
                        t.size = size;
                        t.pending = false;
                    }
                }
                Msg::Deleted { n, bytes, errs } => {
                    if errs.is_empty() {
                        self.toast(format!("✓ Removed {n} item(s) · {}", fmt::size(bytes)), false);
                    } else {
                        self.toast(format!("✗ {} failed: {} (press r to rescan)", errs.len(), errs[0]), true);
                    }
                }
            }
        }
        if self.toast.as_ref().is_some_and(|t| t.1.elapsed() > Duration::from_secs(5)) {
            self.toast = None;
        }
    }

    // ── explorer ──────────────────────────────────────────────

    pub fn cur_path(&self) -> PathBuf {
        let mut p = self.root.clone();
        for n in &self.ex_stack {
            p.push(&**n);
        }
        p
    }

    pub fn rebuild_ex(&mut self) {
        let Some(tree) = &self.tree else {
            self.ex_rows.clear();
            return;
        };
        while resolve(tree, &self.ex_stack).is_none() {
            self.ex_stack.pop();
            self.ex_cursors.pop();
        }
        let d = resolve(tree, &self.ex_stack).unwrap();
        let mut rows: Vec<(bool, usize)> =
            (0..d.dirs.len()).map(|i| (true, i)).chain((0..d.files.len()).map(|i| (false, i))).collect();
        match self.ex_sort {
            Sort::Size => rows.sort_by(|a, b| row_meta(d, *b).1.cmp(&row_meta(d, *a).1)),
            Sort::Items => rows.sort_by(|a, b| row_meta(d, *b).2.cmp(&row_meta(d, *a).2)),
            Sort::Name => rows.sort_by_cached_key(|r| (!r.0, row_meta(d, *r).0.to_lowercase())),
        }
        self.ex_rows = rows;
        self.ex_ls.clamp(self.ex_rows.len());
    }

    fn ex_cur(&self) -> Option<(&DirNode, (bool, usize))> {
        let d = resolve(self.tree.as_ref()?, &self.ex_stack)?;
        Some((d, *self.ex_rows.get(self.ex_ls.sel)?))
    }

    fn ex_enter(&mut self) {
        let Some((d, r)) = self.ex_cur() else { return };
        if !r.0 {
            return;
        }
        let name = d.dirs[r.1].name.clone();
        self.ex_stack.push(name);
        self.ex_cursors.push(self.ex_ls.sel);
        self.ex_ls = ListSt::default();
        self.ex_marks.clear();
        self.rebuild_ex();
    }

    fn ex_back(&mut self) {
        if self.ex_stack.pop().is_some() {
            self.ex_ls = ListSt { sel: self.ex_cursors.pop().unwrap_or(0), ..Default::default() };
            self.ex_marks.clear();
            self.rebuild_ex();
        }
    }

    /// Jump the explorer to `path` (dir) and put the cursor on `select`.
    fn goto(&mut self, dir: &Path, select: Option<&str>) {
        let Ok(rel) = dir.strip_prefix(&self.root) else { return };
        self.ex_stack = rel.iter().map(|c| c.to_string_lossy().into()).collect();
        self.ex_cursors = vec![0; self.ex_stack.len()];
        self.ex_ls = ListSt::default();
        self.ex_marks.clear();
        self.rebuild_ex();
        if let (Some(name), Some(tree)) = (select, &self.tree) {
            if let Some(d) = resolve(tree, &self.ex_stack) {
                if let Some(i) = self.ex_rows.iter().position(|r| row_meta(d, *r).0 == name) {
                    self.ex_ls.sel = i;
                }
            }
        }
        self.tab = Tab::Explorer;
    }

    /// Marked entries in the current dir, or the one under the cursor.
    fn ex_targets(&self) -> Vec<(PathBuf, u64)> {
        let Some(d) = self.tree.as_ref().and_then(|t| resolve(t, &self.ex_stack)) else { return vec![] };
        let base = self.cur_path();
        let rows: Vec<(bool, usize)> = if self.ex_marks.is_empty() {
            self.ex_rows.get(self.ex_ls.sel).copied().into_iter().collect()
        } else {
            self.ex_rows.iter().copied().filter(|r| self.ex_marks.contains(row_meta(d, *r).0)).collect()
        };
        rows.into_iter()
            .map(|r| {
                let (n, s, ..) = row_meta(d, r);
                (base.join(n), s)
            })
            .collect()
    }

    fn after_mutation(&mut self) {
        if let Some(t) = &self.tree {
            self.analysis = Some(analyze::analyze(t, &self.root));
        }
        self.rebuild_ex();
        self.rebuild_dev();
    }

    fn tree_remove(&mut self, p: &Path) {
        let (Some(tree), Ok(rel)) = (self.tree.as_mut(), p.strip_prefix(&self.root)) else { return };
        let comps: Vec<String> = rel.iter().map(|c| c.to_string_lossy().into_owned()).collect();
        let comps: Vec<&str> = comps.iter().map(|s| s.as_str()).collect();
        if !comps.is_empty() {
            tree.remove(&comps);
        }
    }

    fn ask_delete(&mut self, items: Vec<(PathBuf, u64)>, permanent: bool) {
        let items: Vec<_> = items.into_iter().filter(|(p, _)| *p != self.root).collect();
        if items.is_empty() {
            return;
        }
        let bytes: u64 = items.iter().map(|x| x.1).sum();
        let mut body: Vec<String> =
            items.iter().take(8).map(|(p, s)| format!("{:>10}  {}", fmt::size(*s), fmt::tilde(p, &self.home))).collect();
        if items.len() > 8 {
            body.push(format!("{:>10}  … and {} more", "", items.len() - 8));
        }
        body.push(String::new());
        body.push(if permanent {
            "Permanently delete. This cannot be undone.".into()
        } else {
            "Move to Trash. Space is freed once Trash is emptied.".into()
        });
        self.dialog = Some(Dialog {
            title: format!("{} {} item(s) · {}", if permanent { "Delete" } else { "Trash" }, items.len(), fmt::size(bytes)),
            body,
            danger: permanent,
            action: Pending::Delete { paths: items.into_iter().map(|x| x.0).collect(), permanent, bytes },
        });
    }

    fn do_delete(&mut self, paths: Vec<PathBuf>, permanent: bool, bytes: u64) {
        for p in &paths {
            self.tree_remove(p);
            self.dev_marks.remove(p);
        }
        self.ex_marks.clear();
        self.after_mutation();
        self.toast(format!("{} {} item(s)…", if permanent { "Deleting" } else { "Trashing" }, paths.len()), false);
        let tx = self.msg_tx.clone();
        thread::spawn(move || {
            let n = paths.len();
            let errs: Vec<String> = if permanent {
                paths
                    .par_iter()
                    .filter_map(|p| {
                        let r = match std::fs::symlink_metadata(p) {
                            Ok(m) if m.is_dir() => std::fs::remove_dir_all(p),
                            _ => std::fs::remove_file(p),
                        };
                        r.err().map(|e| format!("{}: {e}", p.display()))
                    })
                    .collect()
            } else {
                trash::delete_all(&paths).err().map(|e| vec![e.to_string()]).unwrap_or_default()
            };
            let _ = tx.send(Msg::Deleted { n, bytes, errs });
        });
    }

    fn exclude(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        for p in &paths {
            let pat = fmt::tilde(p, &self.home);
            if !self.cfg.exclude.contains(&pat) {
                self.cfg.exclude.push(pat);
            }
            self.tree_remove(p);
        }
        self.ex_marks.clear();
        self.after_mutation();
        match self.cfg.save(&self.cfg_path) {
            Ok(()) => {
                let msg = format!("Excluded {} path(s) · saved to {}", paths.len(), fmt::tilde(&self.cfg_path, &self.home));
                self.toast(msg, false)
            }
            Err(e) => self.toast(format!("Excluded, but config save failed: {e}"), true),
        }
    }

    // ── dev ───────────────────────────────────────────────────

    pub fn dev_kinds(&self) -> Vec<&'static str> {
        let mut k: Vec<&'static str> = vec![];
        if let Some(a) = &self.analysis {
            for x in &a.artifacts {
                if !k.contains(&x.kind) {
                    k.push(x.kind);
                }
            }
        }
        k
    }

    pub fn rebuild_dev(&mut self) {
        let Some(a) = &self.analysis else {
            self.dev_rows.clear();
            return;
        };
        let min = self.cfg.dev_min_mb * fmt::MIB;
        let days = AGES[self.dev_age].0;
        let cutoff = fmt::now() - days * 86400;
        let mut rows: Vec<usize> = a
            .artifacts
            .iter()
            .enumerate()
            .filter(|(_, x)| x.size >= min && (days == 0 || x.mtime <= cutoff) && self.dev_kind.map_or(true, |k| k == x.kind))
            .map(|(i, _)| i)
            .collect();
        if self.dev_sort_age {
            rows.sort_by_key(|&i| a.artifacts[i].mtime);
        }
        let live: HashSet<&PathBuf> = a.artifacts.iter().map(|x| &x.path).collect();
        self.dev_marks.retain(|p| live.contains(p));
        self.dev_rows = rows;
        self.dev_ls.clamp(self.dev_rows.len());
    }

    fn dev_targets(&self) -> Vec<(PathBuf, u64, &'static str)> {
        let Some(a) = &self.analysis else { return vec![] };
        if self.dev_marks.is_empty() {
            self.dev_rows
                .get(self.dev_ls.sel)
                .map(|&i| &a.artifacts[i])
                .map(|x| vec![(x.path.clone(), x.size, x.kind)])
                .unwrap_or_default()
        } else {
            a.artifacts.iter().filter(|x| self.dev_marks.contains(&x.path)).map(|x| (x.path.clone(), x.size, x.kind)).collect()
        }
    }

    // ── keys ──────────────────────────────────────────────────

    pub fn key(&mut self, k: KeyEvent) {
        if self.help {
            self.help = false;
            return;
        }
        if let Some(d) = self.dialog.take() {
            let yes = matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) || (k.code == KeyCode::Enter && !d.danger);
            if yes {
                self.confirm(d.action);
            }
            return;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap();
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char(c @ '1'..='5') => self.tab = Tab::ALL[c as usize - '1' as usize],
            KeyCode::Tab => self.tab = Tab::ALL[(i + 1) % 5],
            KeyCode::BackTab => self.tab = Tab::ALL[(i + 4) % 5],
            code => match self.tab {
                Tab::Disks => self.key_disks(code),
                Tab::Explorer => self.key_explorer(code),
                Tab::Types => self.key_types(code),
                Tab::Dev => self.key_dev(code),
                Tab::Clean => self.key_clean(code),
            },
        }
    }

    fn refresh_mounts(&mut self) {
        self.mounts = if self.scoped { disks::containing(disks::mounts(), &self.root) } else { disks::mounts() };
        if !self.scoped {
            self.drives = disks::drives();
        }
    }

    fn list_mut(&mut self, id: ListId) -> &mut ListSt {
        match id {
            ListId::Disks => &mut self.disks_ls,
            ListId::Explorer => &mut self.ex_ls,
            ListId::TyExt => &mut self.ty_ls,
            ListId::TyFiles => &mut self.ty_files_ls,
            ListId::Dev => &mut self.dev_ls,
            ListId::Clean => &mut self.cl_ls,
        }
    }

    pub fn mouse(&mut self, m: MouseEvent) {
        let pos = Position { x: m.column, y: m.row };
        let hit = self.hits.iter().rev().find(|(r, _)| r.contains(pos)).copied();
        let key = |c: KeyCode| KeyEvent::new(c, KeyModifiers::NONE);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.help {
                    self.help = false;
                    return;
                }
                if self.dialog.is_some() {
                    if let Some((_, Hit::Dialog(c))) = hit {
                        self.key(key(c));
                    }
                    return;
                }
                match hit {
                    Some((_, Hit::Tab(t))) => self.tab = t,
                    Some((_, Hit::Key(c))) => self.key(key(c)),
                    Some((r, Hit::List { id, off, len })) => {
                        let idx = off + (m.row - r.y) as usize;
                        if idx >= len {
                            return;
                        }
                        match id {
                            ListId::TyExt => {
                                if self.ty_ls.sel != idx {
                                    self.ty_files_ls = ListSt::default();
                                }
                                self.ty_focus_files = false;
                            }
                            ListId::TyFiles => self.ty_focus_files = true,
                            _ => {}
                        }
                        self.list_mut(id).sel = idx;
                        // clicking the checkbox / mark column toggles directly
                        let col = m.column - r.x;
                        match id {
                            ListId::Clean if col < 3 => {
                                self.targets[idx].selected ^= true;
                                return;
                            }
                            ListId::Explorer | ListId::Dev if col < 1 => {
                                self.key(key(KeyCode::Char(' ')));
                                self.list_mut(id).sel = idx;
                                return;
                            }
                            _ => {}
                        }
                        let double = self
                            .last_click
                            .is_some_and(|(i, n, t)| i == id && n == idx && t.elapsed() < Duration::from_millis(400));
                        if double {
                            self.last_click = None;
                            match id {
                                ListId::Clean => self.targets[idx].selected ^= true,
                                ListId::TyExt => self.ty_focus_files = true,
                                _ => self.key(key(KeyCode::Enter)),
                            }
                        } else {
                            self.last_click = Some((id, idx, Instant::now()));
                        }
                    }
                    _ => {}
                }
            }
            MouseEventKind::Down(MouseButton::Right) if self.dialog.is_none() && !self.help => match self.tab {
                Tab::Explorer => self.key(key(KeyCode::Left)),
                Tab::Types => self.ty_focus_files = false,
                _ => {}
            },
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let d = if m.kind == MouseEventKind::ScrollDown { 3 } else { -3 };
                if let Some((_, Hit::List { id, len, .. })) = hit {
                    if id == ListId::TyExt {
                        self.ty_files_ls = ListSt::default();
                    }
                    self.list_mut(id).mv(d, len);
                } else {
                    self.scroll_current(d);
                }
            }
            _ => {}
        }
    }

    fn scroll_current(&mut self, d: isize) {
        let (ls, len) = match self.tab {
            Tab::Disks => (&mut self.disks_ls, self.mounts.len()),
            Tab::Explorer => (&mut self.ex_ls, self.ex_rows.len()),
            Tab::Types => {
                let a = self.analysis.as_ref();
                if self.ty_focus_files {
                    let n = a.map_or(0, |a| files_for(a, self.ty_ls.sel).len());
                    (&mut self.ty_files_ls, n)
                } else {
                    (&mut self.ty_ls, a.map_or(0, |a| a.exts.len() + 1))
                }
            }
            Tab::Dev => (&mut self.dev_ls, self.dev_rows.len()),
            Tab::Clean => (&mut self.cl_ls, self.targets.len()),
        };
        ls.mv(d, len);
    }

    fn confirm(&mut self, p: Pending) {
        match p {
            Pending::Delete { paths, permanent, bytes } => self.do_delete(paths, permanent, bytes),
            Pending::Clean(ids) => self.external = Some(External::Clean(ids)),
            Pending::GitGc(dirs) => self.external = Some(External::GitGc(dirs)),
        }
    }

    fn key_disks(&mut self, code: KeyCode) {
        if self.disks_ls.nav(code, self.mounts.len()) {
            return;
        }
        if self.scoped && matches!(code, KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('h') | KeyCode::Char('o')) {
            let msg = format!("Scoped to {} — run `dsk` without a path for all disks", fmt::tilde(&self.root, &self.home));
            self.toast(msg, true);
            return;
        }
        match code {
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if let Some(m) = self.mounts.get(self.disks_ls.sel) {
                    let p = m.targets[0].clone();
                    self.set_root(p);
                }
            }
            KeyCode::Char('h') => {
                let h = self.home.clone();
                self.set_root(h);
            }
            KeyCode::Char('o') => {
                if let Some(m) = self.mounts.get(self.disks_ls.sel) {
                    open_path(&m.targets[0]);
                }
            }
            KeyCode::Char('r') => {
                self.refresh_mounts();
                self.toast("Refreshed", false);
            }
            _ => {}
        }
    }

    fn key_explorer(&mut self, code: KeyCode) {
        if self.ex_ls.nav(code, self.ex_rows.len()) {
            return;
        }
        match code {
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.ex_enter(),
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => self.ex_back(),
            KeyCode::Char(' ') => {
                if let Some((d, r)) = self.ex_cur() {
                    let name: Box<str> = row_meta(d, r).0.into();
                    if !self.ex_marks.remove(&name) {
                        self.ex_marks.insert(name);
                    }
                    self.ex_ls.mv(1, self.ex_rows.len());
                }
            }
            KeyCode::Esc => self.ex_marks.clear(),
            KeyCode::Char('d') => self.ask_delete(self.ex_targets(), false),
            KeyCode::Char('D') => self.ask_delete(self.ex_targets(), true),
            KeyCode::Char('x') => {
                let p = self.ex_targets().into_iter().map(|x| x.0).collect();
                self.exclude(p);
            }
            KeyCode::Char('s') => {
                self.ex_sort = match self.ex_sort {
                    Sort::Size => Sort::Name,
                    Sort::Name => Sort::Items,
                    Sort::Items => Sort::Size,
                };
                self.rebuild_ex();
            }
            KeyCode::Char('o') => {
                if let Some((p, _)) = self.ex_targets().into_iter().next() {
                    open_path(if p.is_dir() { &p } else { p.parent().unwrap_or(&p) });
                }
            }
            KeyCode::Char('r') => self.start_scan(),
            _ => {}
        }
    }

    fn key_types(&mut self, code: KeyCode) {
        let Some(a) = &self.analysis else { return };
        let n_ext = a.exts.len() + 1;
        let files = files_for(a, self.ty_ls.sel);
        let cur_file = files.get(self.ty_files_ls.sel).cloned();
        let n_files = files.len();
        if self.ty_focus_files {
            if self.ty_files_ls.nav(code, n_files) {
                return;
            }
        } else {
            let before = self.ty_ls.sel;
            if self.ty_ls.nav(code, n_ext) {
                if before != self.ty_ls.sel {
                    self.ty_files_ls = ListSt::default();
                }
                return;
            }
        }
        match code {
            KeyCode::Right | KeyCode::Char('l') if n_files > 0 => self.ty_focus_files = true,
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc => self.ty_focus_files = false,
            KeyCode::Enter if !self.ty_focus_files && n_files > 0 => self.ty_focus_files = true,
            KeyCode::Enter => {
                if let Some((_, p)) = cur_file {
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
                    if let Some(dir) = p.parent() {
                        self.goto(dir, name.as_deref());
                    }
                }
            }
            KeyCode::Char('d') | KeyCode::Char('D') if self.ty_focus_files => {
                if let Some((s, p)) = cur_file {
                    self.ask_delete(vec![(p, s)], code == KeyCode::Char('D'));
                }
            }
            KeyCode::Char('o') => {
                if let Some(dir) = cur_file.as_ref().and_then(|f| f.1.parent()) {
                    open_path(dir);
                }
            }
            KeyCode::Char('r') => self.start_scan(),
            _ => {}
        }
    }

    fn key_dev(&mut self, code: KeyCode) {
        if self.dev_ls.nav(code, self.dev_rows.len()) {
            return;
        }
        let cur = self.analysis.as_ref().and_then(|a| self.dev_rows.get(self.dev_ls.sel).map(|&i| &a.artifacts[i]));
        let cur = cur.map(|x| (x.path.clone(), x.kind));
        match code {
            KeyCode::Char(' ') => {
                if let Some((p, _)) = cur {
                    if !self.dev_marks.remove(&p) {
                        self.dev_marks.insert(p);
                    }
                    self.dev_ls.mv(1, self.dev_rows.len());
                }
            }
            KeyCode::Char('A') => {
                let Some(a) = &self.analysis else { return };
                let vis: Vec<PathBuf> = self.dev_rows.iter().map(|&i| a.artifacts[i].path.clone()).collect();
                if vis.iter().all(|p| self.dev_marks.contains(p)) {
                    self.dev_marks.clear();
                } else {
                    self.dev_marks.extend(vis);
                }
            }
            KeyCode::Esc => self.dev_marks.clear(),
            KeyCode::Char('a') => {
                self.dev_age = (self.dev_age + 1) % AGES.len();
                self.rebuild_dev();
            }
            KeyCode::Char('f') => {
                let kinds = self.dev_kinds();
                self.dev_kind = match self.dev_kind.and_then(|k| kinds.iter().position(|x| *x == k)) {
                    None => kinds.first().copied(),
                    Some(i) => kinds.get(i + 1).copied(),
                };
                self.rebuild_dev();
            }
            KeyCode::Char('s') => {
                self.dev_sort_age = !self.dev_sort_age;
                self.rebuild_dev();
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                let t = self.dev_targets();
                let git = t.iter().filter(|x| x.2 == ".git").count();
                let items: Vec<_> = t.into_iter().filter(|x| x.2 != ".git").map(|x| (x.0, x.1)).collect();
                if git > 0 {
                    self.toast(format!("Skipped {git} .git dir(s) — use c to compact them instead"), true);
                }
                self.ask_delete(items, code == KeyCode::Char('D'));
            }
            KeyCode::Char('c') => {
                let dirs: Vec<PathBuf> = self
                    .dev_targets()
                    .into_iter()
                    .filter(|x| x.2 == ".git")
                    .filter_map(|x| x.0.parent().map(Path::to_path_buf))
                    .collect();
                if dirs.is_empty() {
                    self.toast("Select .git rows to compact (git gc)", true);
                    return;
                }
                let body = dirs.iter().take(10).map(|d| fmt::tilde(d, &self.home)).collect();
                self.dialog = Some(Dialog {
                    title: format!("git gc --prune=now in {} repo(s)", dirs.len()),
                    body,
                    danger: false,
                    action: Pending::GitGc(dirs),
                });
            }
            KeyCode::Char('x') => {
                let p = self.dev_targets().into_iter().map(|x| x.0).collect();
                self.dev_marks.clear();
                self.exclude(p);
            }
            KeyCode::Enter => {
                if let Some((p, _)) = cur {
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
                    if let Some(dir) = p.parent() {
                        self.goto(dir, name.as_deref());
                    }
                }
            }
            KeyCode::Char('o') => {
                if let Some(dir) = cur.as_ref().and_then(|c| c.0.parent()) {
                    open_path(dir);
                }
            }
            KeyCode::Char('r') => self.start_scan(),
            _ => {}
        }
    }

    fn key_clean(&mut self, code: KeyCode) {
        let len = self.targets.len();
        if self.cl_ls.nav(code, len) {
            return;
        }
        match code {
            KeyCode::Char(' ') => {
                if let Some(t) = self.targets.get_mut(self.cl_ls.sel) {
                    t.selected = !t.selected;
                    self.cl_ls.mv(1, len);
                }
            }
            KeyCode::Char('a') => {
                let all = self.targets.iter().all(|t| t.selected);
                self.targets.iter_mut().for_each(|t| t.selected = !all);
            }
            KeyCode::Enter | KeyCode::Char('c') => {
                let mut sel: Vec<&Target> = self.targets.iter().filter(|t| t.selected).collect();
                if sel.is_empty() {
                    sel = self.targets.get(self.cl_ls.sel).into_iter().collect();
                }
                if sel.is_empty() {
                    return;
                }
                let total: u64 = sel.iter().filter_map(|t| t.size).sum();
                let mut body: Vec<String> = sel
                    .iter()
                    .map(|t| {
                        let sz = t.size.map(fmt::size).unwrap_or_else(|| "?".into());
                        format!("{sz:>10}  {}{}", t.name, if t.root && !self.is_root { "  (sudo)" } else { "" })
                    })
                    .collect();
                body.push(String::new());
                if sel.iter().any(|t| t.root) && !self.is_root {
                    body.push("sudo will ask for your password.".into());
                }
                body.push("Output is shown in the terminal; press Enter to return.".into());
                let ids = sel.iter().map(|t| t.id).collect();
                self.dialog = Some(Dialog {
                    title: format!("Clean {} target(s) · ~{}", sel.len(), fmt::size(total)),
                    body,
                    danger: false,
                    action: Pending::Clean(ids),
                });
            }
            KeyCode::Char('x') => {
                if self.cl_ls.sel < len {
                    let t = self.targets.remove(self.cl_ls.sel);
                    self.cfg.hidden_cleaners.push(t.id.to_string());
                    let _ = self.cfg.save(&self.cfg_path);
                    self.cl_ls.clamp(self.targets.len());
                    self.toast(format!("Hidden \"{}\" (edit config to restore)", t.name), false);
                }
            }
            KeyCode::Char('r') => self.size_targets(None),
            _ => {}
        }
    }

    /// Runs with the terminal restored to normal mode.
    pub fn run_external(&mut self, job: External) {
        let su = sudo_user();
        println!("\n\x1b[1;34mdsk\x1b[0m\n");
        match job {
            External::Clean(ids) => {
                let mut freed = 0u64;
                for id in &ids {
                    let Some(t) = self.targets.iter().find(|t| t.id == *id) else { continue };
                    println!("\x1b[1;34m▸\x1b[0m \x1b[1m{}\x1b[0m", t.name);
                    let before = clean::measure(&t.sizer);
                    match clean::run(t, self.is_root, su.as_deref()) {
                        Ok(()) => {
                            let after = clean::measure(&t.sizer);
                            let f = before.unwrap_or(0).saturating_sub(after.unwrap_or(0));
                            freed += f;
                            println!("  \x1b[32m✓ freed {}\x1b[0m", fmt::size(f));
                        }
                        Err(e) => println!("  \x1b[31m✗ {e}\x1b[0m"),
                    }
                }
                println!("\n\x1b[1mTotal freed: {}\x1b[0m", fmt::size(freed));
                for t in &mut self.targets {
                    t.selected = false;
                }
                self.size_targets(Some(ids));
                self.refresh_mounts();
            }
            External::GitGc(dirs) => {
                let mut freed = 0u64;
                for d in &dirs {
                    println!("\x1b[1;34m▸\x1b[0m \x1b[1m{}\x1b[0m", fmt::tilde(d, &self.home));
                    let g = d.join(".git");
                    let before = scan::du(&g);
                    match clean::git_gc(d, self.is_root, su.as_deref()) {
                        Ok(()) => {
                            let f = before.saturating_sub(scan::du(&g));
                            freed += f;
                            println!("  \x1b[32m✓ freed {}\x1b[0m", fmt::size(f));
                        }
                        Err(e) => println!("  \x1b[31m✗ {e}\x1b[0m"),
                    }
                }
                println!("\n\x1b[1mTotal freed: {}\x1b[0m", fmt::size(freed));
                self.dev_marks.clear();
                self.toast("Press r to rescan for updated sizes", false);
            }
        }
        print!("\nPress Enter to return to dsk…");
        let _ = io::stdout().flush();
        let mut s = String::new();
        let _ = io::stdin().read_line(&mut s);
    }
}

pub fn files_for(a: &Analysis, sel: usize) -> &[(u64, PathBuf)] {
    if sel == 0 {
        &a.largest
    } else {
        a.exts.get(sel - 1).map(|e| e.top.as_slice()).unwrap_or(&[])
    }
}
