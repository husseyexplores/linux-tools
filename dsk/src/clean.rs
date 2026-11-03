use crate::scan::{du, du_filtered};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Clone)]
pub enum Action {
    /// Delete everything inside these dirs (dirs themselves are kept).
    Contents(Vec<PathBuf>),
    /// Run commands in order.
    Cmds(Vec<Vec<String>>),
}

#[derive(Clone)]
pub enum Sizer {
    Paths(Vec<PathBuf>),
    /// systemd journal minus the size we keep.
    Journal(u64),
    RotatedLogs,
    Docker,
    Unknown,
}

pub struct Target {
    pub id: &'static str,
    pub name: &'static str,
    pub group: &'static str,
    pub root: bool,
    pub action: Action,
    pub sizer: Sizer,
    pub note: &'static str,
    pub size: Option<u64>,
    pub pending: bool,
    pub selected: bool,
}

pub fn which(cmd: &str) -> bool {
    env::var_os("PATH")
        .map(|p| env::split_paths(&p).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false)
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

const ROTATED_FIND: &[&str] = &[
    "find", "/var/log", "-type", "f", "(", "-name", "*.gz", "-o", "-name", "*.xz", "-o", "-name", "*.zst", "-o",
    "-name", "*.bz2", "-o", "-name", "*.old", "-o", "-regex", ".*\\.[0-9]+", ")", "-delete",
];

fn is_rotated(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("");
    name.contains('.')
        && (matches!(ext, "gz" | "xz" | "zst" | "bz2" | "old") || (!ext.is_empty() && ext.bytes().all(|b| b.is_ascii_digit())))
}

pub fn detect(home: &Path, hidden: &[String]) -> Vec<Target> {
    let h = |p: &str| home.join(p);
    let cargo = env::var_os("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|| h(".cargo"));
    let gopath = env::var_os("GOPATH").map(PathBuf::from).unwrap_or_else(|| h("go"));
    let mut v: Vec<Target> = vec![];

    let mut dirs = |id, name, group, root, paths: Vec<PathBuf>, note| {
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| p.is_dir()).collect();
        if !paths.is_empty() {
            v.push(Target {
                id, name, group, root, note,
                action: Action::Contents(paths.clone()),
                sizer: Sizer::Paths(paths),
                size: None, pending: false, selected: false,
            });
        }
    };

    // ── user ──
    dirs("trash", "Trash", "User", false,
        vec![h(".local/share/Trash/files"), h(".local/share/Trash/info"), h(".local/share/Trash/expunged")],
        "Empty the Trash");
    dirs("thumbnails", "Thumbnails", "User", false, vec![h(".cache/thumbnails")], "Image previews, regenerated on demand");
    dirs("aur", "AUR helper cache", "User", false, vec![h(".cache/yay"), h(".cache/paru")], "yay/paru build dirs");

    // ── dev caches ──
    dirs("npm", "npm cache", "Dev", false, vec![h(".npm/_cacache"), h(".npm/_npx"), h(".npm/_logs")], "Re-downloaded on install");
    dirs("yarn", "Yarn cache", "Dev", false, vec![h(".cache/yarn"), h(".yarn/berry/cache")], "Re-downloaded on install");
    dirs("bun", "Bun cache", "Dev", false, vec![h(".bun/install/cache")], "Re-downloaded on install");
    dirs("deno", "Deno cache", "Dev", false, vec![h(".cache/deno")], "Re-downloaded on run");
    dirs("pip", "Python caches", "Dev", false, vec![h(".cache/pip"), h(".cache/uv"), h(".cache/pypoetry/cache"), h(".cache/pypoetry/artifacts")], "pip / uv / poetry downloads");
    dirs("gobuild", "Go build cache", "Dev", false, vec![h(".cache/go-build")], "Rebuilt on next build");
    dirs("cargo", "Cargo registry", "Dev", false, vec![cargo.join("registry/cache"), cargo.join("registry/src"), cargo.join("git/checkouts")], "Crate sources, re-downloaded");
    dirs("gradle", "Gradle caches", "Dev", false, vec![h(".gradle/caches"), h(".gradle/wrapper/dists")], "Re-downloaded on build");
    dirs("maven", "Maven repository", "Dev", false, vec![h(".m2/repository")], "Re-downloaded on build");
    dirs("composer", "Composer cache", "Dev", false, vec![h(".cache/composer")], "Re-downloaded on install");
    dirs("nodegyp", "node-gyp headers", "Dev", false, vec![h(".cache/node-gyp")], "Re-downloaded on native build");
    dirs("browsers", "Test browsers", "Dev", false, vec![h(".cache/ms-playwright"), h(".cache/puppeteer"), h(".cache/Cypress")], "Playwright/Puppeteer/Cypress, re-installed");
    dirs("electron", "Electron cache", "Dev", false, vec![h(".cache/electron"), h(".cache/electron-builder")], "Re-downloaded on build");
    dirs("ccache", "ccache", "Dev", false, vec![h(".cache/ccache"), h(".ccache")], "Compiler cache");
    dirs("misc-dev", "Misc dev caches", "Dev", false, vec![h(".cache/typescript"), h(".cache/prisma"), h(".cache/pre-commit"), h(".cache/zig"), h(".nvm/.cache")], "typescript, prisma, pre-commit, zig, nvm");

    // ── system (root) ──
    dirs("coredumps", "Core dumps", "System", true, vec!["/var/lib/systemd/coredump".into(), "/var/crash".into()], "Crash dumps");

    let mut cmd = |id, name, group, root, cmds: Vec<Vec<String>>, sizer, note| {
        v.push(Target {
            id, name, group, root, note, sizer,
            action: Action::Cmds(cmds),
            size: None, pending: false, selected: false,
        });
    };

    let go_mod = gopath.join("pkg/mod");
    if which("go") && go_mod.is_dir() {
        cmd("gomod", "Go module cache", "Dev", false, vec![s(&["go", "clean", "-modcache"])], Sizer::Paths(vec![go_mod]), "Re-downloaded on build");
    }
    let pnpm = [h(".local/share/pnpm/store"), h(".cache/pnpm")];
    if which("pnpm") && pnpm.iter().any(|p| p.is_dir()) {
        cmd("pnpm", "pnpm store (unused)", "Dev", false, vec![s(&["pnpm", "store", "prune"])],
            Sizer::Paths(pnpm.into_iter().filter(|p| p.is_dir()).collect()), "Packages no project links to");
    }
    if which("apt-get") && Path::new("/var/cache/apt/archives").is_dir() {
        cmd("apt", "APT package cache", "System", true, vec![s(&["apt-get", "clean"])],
            Sizer::Paths(vec!["/var/cache/apt/archives".into()]), "Downloaded .deb files");
    }
    if which("pacman") && Path::new("/var/cache/pacman/pkg").is_dir() {
        let cmds = if which("paccache") {
            vec![s(&["paccache", "-rk1"]), s(&["paccache", "-ruk0"])]
        } else {
            vec![s(&["pacman", "-Sc", "--noconfirm"])]
        };
        cmd("pacman", "Pacman package cache", "System", true, cmds,
            Sizer::Paths(vec!["/var/cache/pacman/pkg".into()]), "Keeps newest version of installed pkgs");
    }
    if which("dnf") {
        let p: Vec<PathBuf> = ["/var/cache/dnf", "/var/cache/libdnf5"].iter().map(PathBuf::from).filter(|p| p.is_dir()).collect();
        if !p.is_empty() {
            cmd("dnf", "DNF package cache", "System", true, vec![s(&["dnf", "clean", "all"])], Sizer::Paths(p), "Downloaded packages & metadata");
        }
    }
    if which("journalctl") && Path::new("/var/log/journal").is_dir() {
        cmd("journal", "systemd journal", "System", true, vec![s(&["journalctl", "--vacuum-size=200M"])],
            Sizer::Journal(200 * 1024 * 1024), "Shrink logs to 200 MiB");
    }
    if Path::new("/var/log").is_dir() {
        cmd("logs", "Rotated logs", "System", true, vec![s(ROTATED_FIND)], Sizer::RotatedLogs, "Old *.gz / *.1 logs in /var/log");
    }
    if which("flatpak") {
        cmd("flatpak", "Flatpak unused runtimes", "System", false, vec![s(&["flatpak", "uninstall", "--unused", "-y"])],
            Sizer::Unknown, "Runtimes no app uses");
    }
    if which("docker") {
        let root = nix::unistd::access("/var/run/docker.sock", nix::unistd::AccessFlags::W_OK).is_err();
        cmd("docker", "Docker build cache", "System", root,
            vec![s(&["docker", "builder", "prune", "-f"]), s(&["docker", "image", "prune", "-f"])],
            Sizer::Docker, "Build cache + dangling images");
    }

    v.retain(|t| !hidden.iter().any(|x| x == t.id));
    let order = |g: &str| match g { "User" => 0, "Dev" => 1, _ => 2 };
    v.sort_by_key(|t| order(t.group));
    v
}

/// Keep only cleaners whose files all live inside `root` (narrowing their paths to it).
pub fn restrict(v: &mut Vec<Target>, root: &Path) {
    let inside = |p: &PathBuf| p.starts_with(root);
    v.retain_mut(|t| match (&mut t.action, &mut t.sizer) {
        (Action::Contents(ps), Sizer::Paths(sp)) => {
            ps.retain(inside);
            sp.retain(inside);
            !ps.is_empty()
        }
        (Action::Cmds(_), Sizer::Paths(sp)) => !sp.is_empty() && sp.iter().all(inside),
        _ => false,
    });
}

fn parse_docker_size(s: &str) -> Option<u64> {
    let s = s.split_whitespace().next()?;
    let i = s.find(|c: char| c.is_ascii_alphabetic())?;
    let n: f64 = s[..i].parse().ok()?;
    let m = match &s[i..] {
        "B" => 1.0,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => return None,
    };
    Some((n * m) as u64)
}

fn docker_reclaimable() -> Option<u64> {
    let out = Command::new("docker")
        .args(["system", "df", "--format", "{{.Type}}|{{.Reclaimable}}"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with("Build Cache"))
        .filter_map(|l| parse_docker_size(l.split('|').nth(1)?))
        .next()
}

pub fn measure(s: &Sizer) -> Option<u64> {
    match s {
        Sizer::Paths(ps) => Some(ps.iter().map(|p| du(p)).sum()),
        Sizer::Journal(keep) => Some(du(Path::new("/var/log/journal")).saturating_sub(*keep)),
        Sizer::RotatedLogs => Some(du_filtered(Path::new("/var/log"), &is_rotated)),
        Sizer::Docker => docker_reclaimable(),
        Sizer::Unknown => None,
    }
}

/// argv adjusted for privileges: root targets get `sudo` when we aren't root;
/// user targets run as the invoking user when we are root via sudo.
fn wrap(argv: &[String], root: bool, is_root: bool, sudo_user: Option<&str>) -> Vec<String> {
    let mut v = vec![];
    if root && !is_root {
        v.push("sudo".into());
    } else if !root && is_root {
        if let Some(u) = sudo_user {
            v.extend(["sudo".into(), "-u".into(), u.to_string()]);
        }
    }
    v.extend(argv.iter().cloned());
    v
}

pub fn exec(argv: &[String]) -> Result<(), String> {
    println!("  \x1b[2m$ {}\x1b[0m", argv.join(" "));
    let st = Command::new(&argv[0]).args(&argv[1..]).status().map_err(|e| format!("{}: {e}", argv[0]))?;
    if st.success() { Ok(()) } else { Err(format!("{} exited with {st}", argv[0])) }
}

fn remove_contents(dir: &Path) -> Result<(), String> {
    let rd = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut err = None;
    for e in rd.flatten() {
        let p = e.path();
        let r = match e.file_type() {
            Ok(t) if t.is_dir() => fs::remove_dir_all(&p),
            _ => fs::remove_file(&p),
        };
        if let Err(e) = r {
            err.get_or_insert(format!("{}: {e}", p.display()));
        }
    }
    err.map_or(Ok(()), Err)
}

pub fn run(t: &Target, is_root: bool, sudo_user: Option<&str>) -> Result<(), String> {
    match &t.action {
        Action::Contents(ps) => {
            for p in ps {
                if t.root && !is_root {
                    let ps = p.to_string_lossy();
                    exec(&wrap(&s(&["find", &ps, "-mindepth", "1", "-delete"]), true, false, None))?;
                } else {
                    println!("  \x1b[2mrm -rf {}/*\x1b[0m", p.display());
                    remove_contents(p)?;
                }
            }
        }
        Action::Cmds(cmds) => {
            for c in cmds {
                if !which(&c[0]) {
                    return Err(format!("{} not found", c[0]));
                }
                exec(&wrap(c, t.root, is_root, sudo_user))?;
            }
        }
    }
    Ok(())
}

pub fn git_gc(dir: &Path, is_root: bool, sudo_user: Option<&str>) -> Result<(), String> {
    let d = dir.to_string_lossy();
    exec(&wrap(&s(&["git", "-C", &d, "gc", "--prune=now", "--quiet"]), false, is_root, sudo_user))
}
