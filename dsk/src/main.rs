mod analyze;
mod app;
mod clean;
mod config;
mod disks;
mod fmt;
mod report;
mod scan;
mod theme;
mod ui;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{
    io::{self, IsTerminal, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

/// Fast disk usage explorer & cache cleaner.
#[derive(Parser, Clone)]
#[command(name = "dsk", version)]
pub struct Cli {
    /// Directory to scan [default: $HOME]
    pub path: Option<PathBuf>,
    /// Exclude glob (repeatable). Without '/' matches names anywhere (e.g. node_modules);
    /// with '/' matches full paths (e.g. ~/vms, /mnt/**)
    #[arg(short, long, value_name = "GLOB")]
    pub exclude: Vec<String>,
    /// Stay on the filesystem of PATH
    #[arg(short = 'x', long)]
    pub one_fs: bool,
    /// Print a summary instead of the TUI
    #[arg(short, long)]
    pub print: bool,
    /// Print a summary as JSON
    #[arg(long)]
    pub json: bool,
    /// Number of rows per section in --print
    #[arg(short = 'n', long, default_value_t = 12)]
    pub top: usize,
    /// Disable colors (also honors NO_COLOR)
    #[arg(long)]
    pub no_color: bool,
    /// Don't capture the mouse (keeps terminal text selection)
    #[arg(long)]
    pub no_mouse: bool,
    /// Never offer to re-run with sudo
    #[arg(long)]
    pub no_sudo: bool,
}

pub fn is_root() -> bool {
    nix::unistd::geteuid().is_root()
}

/// The real user when running via sudo.
pub fn sudo_user() -> Option<String> {
    if is_root() { std::env::var("SUDO_USER").ok().filter(|u| u != "root") } else { None }
}

fn user_home() -> PathBuf {
    if let Some(u) = sudo_user() {
        if let Ok(Some(user)) = nix::unistd::User::from_name(&u) {
            return user.dir;
        }
    }
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into())
}

fn config_path(home: &Path) -> PathBuf {
    if !is_root() {
        if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
            return PathBuf::from(x).join("dsk/config.toml");
        }
    }
    home.join(".config/dsk/config.toml")
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let home = user_home();
    let root = cli.path.clone().unwrap_or_else(|| home.clone());
    let root = std::fs::canonicalize(&root).with_context(|| format!("cannot access {}", root.display()))?;

    if root == Path::new("/") && !is_root() && !cli.no_sudo && io::stdin().is_terminal() {
        eprint!("Scanning / without root skips protected dirs. Re-run with sudo? [Y/n] ");
        io::stderr().flush()?;
        let mut s = String::new();
        io::stdin().read_line(&mut s)?;
        if !s.trim().eq_ignore_ascii_case("n") {
            let err = Command::new("sudo").arg("--").arg(std::env::current_exe()?).args(std::env::args_os().skip(1)).exec();
            return Err(err).context("failed to run sudo");
        }
    }

    // Threads default to core count (DSK_THREADS overrides); scanning is kernel-bound.
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    rayon::ThreadPoolBuilder::new()
        .num_threads(
            std::env::var("DSK_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(cores.clamp(4, 32)),
        )
        .stack_size(16 << 20)
        .build_global()
        .ok();

    let cfg_path = config_path(&home);
    let cfg = config::Config::load(&cfg_path);
    let mono = cli.no_color || std::env::var_os("NO_COLOR").is_some();

    if cli.print || cli.json {
        return report::run(&root, &home, &cfg, &cli, mono);
    }
    let mut app = app::App::new(root, home, cfg, cfg_path, &cli, mono);
    run_tui(&mut app, !cli.no_mouse)
}

fn setup(mouse: bool) -> io::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    if mouse {
        execute!(io::stdout(), EnableMouseCapture)?;
    }
    Ok(())
}

fn restore(mouse: bool) {
    if mouse {
        let _ = execute!(io::stdout(), DisableMouseCapture);
    }
    let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    let _ = disable_raw_mode();
}

fn run_tui(app: &mut app::App, mouse: bool) -> Result<()> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore(true);
        hook(info)
    }));
    setup(mouse)?;
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    term.clear()?;
    let res = (|| -> Result<()> {
        loop {
            app.tick();
            term.draw(|f| ui::draw(f, app))?;
            if let Some(job) = app.external.take() {
                restore(mouse);
                app.run_external(job);
                setup(mouse)?;
                term.clear()?;
                continue;
            }
            let wait = Duration::from_millis(if app.busy() || app.toast.is_some() { 80 } else { 500 });
            if event::poll(wait)? {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => app.key(k),
                    Event::Mouse(m) => app.mouse(m),
                    _ => {}
                }
            }
            if app.quit {
                return Ok(());
            }
        }
    })();
    restore(mouse);
    res
}
