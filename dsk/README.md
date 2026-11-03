# dsk

Fast, pretty terminal disk-usage explorer and cache cleaner for Linux (Debian/Ubuntu, Arch, Fedora).

```sh
./install.sh            # builds + installs to ~/.local/bin/dsk
dsk                     # scan $HOME
dsk /                   # whole system (offers to re-run with sudo)
dsk ~/dev -e '*.iso'    # scan a dir, exclude a glob
dsk -p                  # plain-text summary   (--json for scripts)
```

## Tabs
| # | Tab | What |
|---|-----|------|
| 1 | Overview | Filesystems, drives, biggest dirs, quick wins |
| 2 | Explorer | ncdu-style tree; mark, trash (`d`), delete (`D`), exclude (`x`) |
| 3 | Types | Space by category/extension + largest files per extension |
| 4 | Dev | `node_modules`, `target/`, `.next`, venvs, `.git`… with project age; filter by age (`a`) / kind (`f`), `git gc` (`c`) |
| 5 | Clean | Safe caches only: package managers (apt/pacman/dnf), journal, rotated logs, npm/yarn/pnpm/bun/pip/cargo/go/gradle…, thumbnails, Trash, Docker build cache, unused Flatpak runtimes. Browser caches and app data are never touched. |

`?` shows all keys. Root-owned cleaners call `sudo` themselves.

**Mouse:** click tabs, rows, footer hints and dialog buttons; double-click opens; right-click goes up; wheel scrolls the list under the cursor. Click the mark/checkbox column to toggle.

**Scoped mode:** `dsk <path>` stays inside that path everywhere: no navigating to parents, Overview shows only its filesystem, Clean lists only caches located inside it. Plain `dsk` (no path) is the full-system view.

## Notes
- Sizes are real disk usage (allocated blocks); hardlinks are counted once.
- Pseudo/network filesystems (proc, tmpfs, overlay, squashfs, nfs…) are skipped. `-x` stays on one filesystem.
- Dev artifacts inside hidden dirs (`~/.cache`, `~/.local`, …) are ignored: they belong to apps, not your projects.
- Config: `~/.config/dsk/config.toml` (`exclude`, `one_fs`, `hidden_cleaners`, `dev_min_mb`).
- `NO_COLOR` / `--no-color` for monochrome; `--no-mouse` keeps terminal text selection; `DSK_THREADS=n` to tune.
