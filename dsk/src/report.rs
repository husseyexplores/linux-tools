use crate::{
    analyze, clean, config::Config, disks, fmt,
    scan::{self, Progress, ScanOpts},
    Cli,
};
use std::{
    io::{self, IsTerminal},
    path::Path,
    sync::atomic::{AtomicBool, Ordering::Relaxed},
    thread,
    time::{Duration, Instant},
};

struct C(bool);
impl C {
    fn p(&self, code: &str, s: impl std::fmt::Display) -> String {
        if self.0 { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
    }
}

fn bar(frac: f64, w: usize) -> String {
    let n = (frac.clamp(0.0, 1.0) * w as f64).round() as usize;
    format!("{}{}", "█".repeat(n), "░".repeat(w - n))
}

pub fn run(root: &Path, home: &Path, cfg: &Config, cli: &Cli, mono: bool) -> anyhow::Result<()> {
    let mut pats = cfg.exclude.clone();
    pats.extend(cli.exclude.iter().cloned());
    let opts = ScanOpts::new(root, &pats, home, cfg.one_fs || cli.one_fs);
    let prog = Progress::default();
    let done = AtomicBool::new(false);
    let t0 = Instant::now();
    let tree = thread::scope(|s| {
        if io::stderr().is_terminal() {
            s.spawn(|| {
                while !done.load(Relaxed) {
                    eprint!(
                        "\r\x1b[2Kscanning… {} files · {}",
                        fmt::count(prog.files.load(Relaxed)),
                        fmt::size(prog.bytes.load(Relaxed))
                    );
                    thread::sleep(Duration::from_millis(100));
                }
                eprint!("\r\x1b[2K");
            });
        }
        let t = scan::scan(root, &opts, &prog);
        done.store(true, Relaxed);
        t
    });
    let secs = t0.elapsed().as_secs_f64();
    let an = analyze::analyze(&tree, root);
    let scoped = cli.path.is_some();
    let mounts = if scoped { disks::containing(disks::mounts(), root) } else { disks::mounts() };
    let n = cli.top;

    if cli.json {
        use serde_json::json;
        let mut top: Vec<_> = tree
            .dirs
            .iter()
            .map(|d| json!({"name": &*d.name, "size": d.size, "items": d.count, "dir": true}))
            .chain(tree.files.iter().map(|f| json!({"name": &*f.name, "size": f.size, "dir": false})))
            .collect();
        top.sort_by_key(|v| std::cmp::Reverse(v["size"].as_u64().unwrap_or(0)));
        let mut cleaners = clean::detect(home, &cfg.hidden_cleaners);
        if scoped {
            clean::restrict(&mut cleaners, root);
        }
        let cleaners: Vec<_> = cleaners
            .iter()
            .map(|t| json!({"id": t.id, "name": t.name, "size": clean::measure(&t.sizer), "root": t.root}))
            .collect();
        let out = json!({
            "root": root, "size": tree.size, "items": tree.count, "scan_secs": secs,
            "unreadable": prog.errors.load(Relaxed),
            "disks": mounts.iter().map(|m| json!({"mount": m.targets[0], "device": m.source, "fs": m.fstype,
                "total": m.total, "used": m.used, "avail": m.avail})).collect::<Vec<_>>(),
            "top": top.into_iter().take(n).collect::<Vec<_>>(),
            "categories": an.cats.iter().map(|(c, s)| json!({"category": c.name(), "size": s})).collect::<Vec<_>>(),
            "extensions": an.exts.iter().take(n).map(|e| json!({"ext": &*e.ext, "category": e.cat.name(),
                "size": e.size, "count": e.count})).collect::<Vec<_>>(),
            "largest_files": an.largest.iter().take(n).map(|(s, p)| json!({"path": p, "size": s})).collect::<Vec<_>>(),
            "dev_artifacts": an.artifacts.iter().map(|a| json!({"path": a.path, "kind": a.kind, "size": a.size,
                "project_mtime": a.mtime})).collect::<Vec<_>>(),
            "cleanable": cleaners,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    let c = C(!mono && io::stdout().is_terminal());
    let h = |s: &str| println!("\n{}", c.p("1;34", s));

    h("Filesystems");
    for m in &mounts {
        let f = m.frac();
        let col = if f < 0.75 { "32" } else if f < 0.9 { "33" } else { "31" };
        println!(
            "  {:<22} {:>10} {:>10} free  {} {}",
            m.targets[0].display(),
            fmt::size(m.total),
            fmt::size(m.avail),
            c.p(col, bar(f, 24)),
            c.p(col, format!("{:>3.0}%", f * 100.0))
        );
    }

    h(&format!("{} · {} · {} items · {:.2}s", fmt::tilde(root, home), fmt::size(tree.size), fmt::count(tree.count), secs));
    let mut top: Vec<(&str, u64, bool)> = tree
        .dirs
        .iter()
        .map(|d| (&*d.name, d.size, true))
        .chain(tree.files.iter().map(|f| (&*f.name, f.size, false)))
        .collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    let total = tree.size.max(1) as f64;
    for (name, size, dir) in top.into_iter().take(n) {
        let fr = size as f64 / total;
        let name = if dir { c.p("1;36", format!("{name}/")) } else { name.to_string() };
        println!("  {:>10} {:>5.1}% {} {}", c.p("33", fmt::size(size)), fr * 100.0, c.p("34", bar(fr, 20)), name);
    }

    h("File types");
    for e in an.exts.iter().take(n) {
        let ext = if e.ext.is_empty() { "(none)".into() } else { format!(".{}", e.ext) };
        println!("  {:>10}  {:<12} {:<9} {} files", c.p("33", fmt::size(e.size)), ext, e.cat.name(), fmt::count(e.count));
    }

    if !an.artifacts.is_empty() {
        h("Dev artifacts");
        let mut kinds: Vec<(&str, u64, usize)> = vec![];
        for a in &an.artifacts {
            match kinds.iter_mut().find(|k| k.0 == a.kind) {
                Some(k) => {
                    k.1 += a.size;
                    k.2 += 1
                }
                None => kinds.push((a.kind, a.size, 1)),
            }
        }
        kinds.sort_by(|a, b| b.1.cmp(&a.1));
        for (k, s, cnt) in kinds {
            println!("  {:>10}  {:<14} {cnt} dirs", c.p("33", fmt::size(s)), k);
        }
    }

    h("Cleanable caches");
    let mut targets = clean::detect(home, &cfg.hidden_cleaners);
    if scoped {
        clean::restrict(&mut targets, root);
    }
    let sizes: Vec<Option<u64>> = {
        use rayon::prelude::*;
        targets.par_iter().map(|t| clean::measure(&t.sizer)).collect()
    };
    let mut sum = 0;
    for (t, s) in targets.iter().zip(sizes) {
        if s == Some(0) {
            continue;
        }
        sum += s.unwrap_or(0);
        let sz = s.map(fmt::size).unwrap_or_else(|| "?".into());
        println!("  {:>10}  {:<26} {}", c.p("33", sz), t.name, c.p("2", t.note));
    }
    println!("  {:>10}  {}", c.p("1;33", fmt::size(sum)), c.p("1", "total — run `dsk` → Clean tab to free it"));
    if prog.errors.load(Relaxed) > 0 {
        println!("\n{}", c.p("33", format!("⚠ {} unreadable entries (try sudo)", prog.errors.load(Relaxed))));
    }
    Ok(())
}
