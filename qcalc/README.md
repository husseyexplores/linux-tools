# qcalc

A tiny popup calculator for Linux (X11) built on libqalculate, the engine behind qalculate-gtk. Press a hotkey and a text box appears in the center of the screen with live results. Press Esc and it's gone.

## Build and install

### 1. Install the build dependencies
```sh
# Debian / Ubuntu
sudo apt install build-essential pkg-config libx11-dev libxext-dev libxrandr-dev libcairo2-dev libpango1.0-dev libqalculate-dev

# Arch / Manjaro / EndeavourOS
sudo pacman -S --needed base-devel pkgconf libx11 libxext libxrandr cairo pango libqalculate

# Fedora
sudo dnf install gcc-c++ make pkgconf libX11-devel libXext-devel libXrandr-devel cairo-devel pango-devel libqalculate-devel
```

### 2. Build and install
The easy way: `install.sh` checks dependencies (and offers to install missing ones), builds, installs to `~/.local/bin`, adds an autostart entry, and starts the background process.
```sh
cd qcalc
./install.sh                       # build + install + autostart + start now
./install.sh --hotkey '<Super>c'   # same, and bind a hotkey (XFCE or GNOME only)
./install.sh --uninstall           # remove binaries + autostart (keeps your saved input)
```

Or by hand:
```sh
cd qcalc
make                               # outputs build/qcalc and build/qcalcd
qcalc quit                         # stop the old daemon if one is running
make install                       # PREFIX defaults to ~/.local; e.g. make install PREFIX=/usr/local
qcalcd &                           # start it hidden (or log out and back in, see autostart below)
```
`make clean` removes `build/`. A manual install doesn't copy the config; do it yourself if you want it: `install -Dm644 config ~/.config/qcalc/config`. After a rebuild, always run `qcalc quit` first. The running daemon keeps the old binary until it restarts.

What gets installed:
| Path | What |
|---|---|
| `~/.local/bin/qcalc` | the hotkey command (tiny, libc only) |
| `~/.local/bin/qcalcd` | the background process with the window and libqalculate |
| `~/.config/autostart/qcalc.desktop` | starts `qcalcd` hidden at login (written by `install.sh`) |
| `~/.config/qcalc/config` | copied from [`config`](config) by `install.sh`, only if you don't have one yet (an existing file or stow symlink is never touched) |

If you install by hand, create the autostart entry yourself so the first hotkey press after login is instant too. It's optional: without it, the first `qcalc` starts the daemon, which shows the window in under ~100 ms.
```ini
# ~/.config/autostart/qcalc.desktop
[Desktop Entry]
Type=Application
Name=qcalc
Exec=/home/YOU/.local/bin/qcalcd
NoDisplay=true
```
With a bare WM that doesn't read XDG autostart, add `~/.local/bin/qcalcd &` to `~/.xinitrc`, or the WM's startup file, instead.

### 3. Bind a hotkey to `~/.local/bin/qcalc`
Use the full path, since hotkey daemons often don't have `~/.local/bin` in PATH.
```sh
# sxhkd (~/.config/sxhkd/sxhkdrc), then reload: pkill -USR1 -x sxhkd
super + c
    ~/.local/bin/qcalc

# i3 (~/.config/i3/config)
bindsym $mod+c exec --no-startup-id ~/.local/bin/qcalc

# XFCE from the command line
xfconf-query -c xfce4-keyboard-shortcuts -p '/commands/custom/<Super>c' -n -t string -s ~/.local/bin/qcalc
```
KDE: System Settings > Shortcuts > Add New > Command. GNOME: Settings > Keyboard > Custom Shortcuts.

### Troubleshooting
- Window doesn't appear: run `qcalc quit`, then `QCALC_DEBUG=1 qcalcd --show` in a terminal. X errors and the exit reason are printed there.
- `pkg-config ... not found` during `make`: a dev package from step 1 is missing.
- Check whether it's running: `pgrep -a qcalcd`.

## Why it's fast
- `qcalcd` stays running in the background with the window hidden. Showing it is a single window-map call, with nothing to load.
- `qcalc` (the hotkey command) is a 15 KB C program that only sends "toggle" over a unix socket. It starts `qcalcd` if it isn't running.
- Calculations run on a worker thread with a timeout, so a heavy expression never freezes typing.

## Use
| Key | Action |
|---|---|
| Esc, small **×** in the tab at the top right | hide (your input is kept) |
| Enter | save the line to history and copy the result to the clipboard |
| Up / Down | history, filtered by what's already typed (like fish shell) |
| Ctrl+N, Ctrl+Enter, **+** | new row |
| Ctrl+W, **×** | remove row |
| Tab / Shift+Tab, Ctrl+Up/Down | move between rows |
| Ctrl+C | copy the selection, or the result if nothing is selected |
| Ctrl+A / X / V / Z / Shift+Z | select all, cut, paste, undo, redo |
| Ctrl+Backspace / Ctrl+Delete, Ctrl+U / Ctrl+K | delete a word, delete to start/end |
| Ctrl+Shift+R | reload the config |
| Ctrl+Shift+0, small ◎ left of the × | center the window again |
| Mouse | click to place the cursor, drag to select, double-click selects a word, triple-click selects all, middle-click pastes |
| Drag the background or the top-right tab | move the window anywhere |
| Right-click the background | menu: Close, Center, Reload config, Update exchange rates, Clear history, Quit qcalc (Up/Down + Enter work too, Esc closes it) |

A dragged position is reused every time the window opens, until you center it again or the daemon restarts. It's never saved to disk.

All shortcuts except cursor movement and Backspace/Delete can be changed in the config (see below).

Anything qalculate understands works: `5 m to ft`, `100 USD to EUR`, `0xff to bin`, `sqrt(2)*pi`, `3 km/h * 2h`, `x := 7`. Assignments (`:=`) only run when you press Enter, so half-typed names never get defined.

## Your input is never lost
All rows and cursor positions are saved to `~/.local/state/qcalc/state` after every edit. The write is atomic (temp file plus rename) and is synced to disk on hide and on exit. History is kept in `~/.local/state/qcalc/history`.

## Commands
`qcalc [toggle|show|hide|quit|reload|clear-history|update-rates]`. `toggle` is the default: it shows the window, or hides it if it's already focused. `clear-history` works whether or not qcalc is running. `update-rates` downloads fresh exchange rates (needs qcalc running).

## Config: `~/.config/qcalc/config`
Every option is optional; leave out what you don't change. `#` starts a comment, either on its own line or after a value (`font = Sans 12  # note`).
```ini
# Look
font = Sans 15              # font name + size in points
width = 560                 # px at 96 dpi; all sizes scale with Xft.dpi

# Layout (px at 96 dpi)
radius = 6                  # input box corner radius (also the tab buttons)
window_radius = 0           # window corner radius, 0 = square (the tab uses 75% of it)
window_padding = 6          # window edge <-> input boxes
row_gap = 6                 # between input boxes
box_padding_x = 12          # inside an input box, left/right
box_padding_y = 9           # inside an input box, top/bottom
border_width = 1            # input box border (0 = none)
window_border = 1           # window border (0 = none)
footer_margin_left = 0      # exchange-rate footer: offset from the input box edge
footer_margin_top = 5       # exchange-rate footer: space above it

# Colors (#rrggbb)
color_bg = #1b1d23          # window background
color_field = #262931       # input box background
color_border = #3a3f4b      # borders, button hover
color_text = #e8e8ea        # typed text
color_result = #8b919c      # result, placeholder
color_accent = #6ea8ff      # cursor, focused outline, "copied" flash
color_selection = #2f4f7f   # selected text
color_error = #e0707a       # close button hover

# Behavior
hide_on_focus_loss = false  # hide when another window gets focus
close_button = true         # small x in a tab above the top-right corner
center_button = true        # re-center button next to it (both off = no tab)
context_menu = true         # right-click menu on the background
copy_on_enter = true        # Enter copies the result
max_history = 1000

# Math (need `qcalc quit` to apply)
fractions = false           # true: show "1/3 ≈ 0.333" style exact fractions too
angle =                     # rad | deg | gra (empty = qalculate's own setting)
timeout_ms = 2000           # give up on a calculation after this long

# Exchange rates (applies on reload)
exchange_rates_max_age = 1  # days; refresh in the background when older (0 = only manually)
```

### Shortcuts
Each action takes a comma-separated list of combos. Modifiers are `ctrl`, `shift`, `alt` and `super`. Keys use X11 key names: `a`–`z`, `0`–`9`, `Return`, `Escape`, `Tab`, `Up`, `Down`, `F1`–`F12`, `Insert`, `Delete`, `space`, `KP_Enter` and so on. Setting an action replaces its defaults, and `none` disables it. Shortcuts are parsed once when the config loads, so they cost nothing per key press.
```ini
key_hide = Escape
key_commit = Return, KP_Enter
key_new_row = ctrl+n, ctrl+Return, ctrl+KP_Enter
key_remove_row = ctrl+w
key_next_row = Tab, ctrl+Down, alt+Down
key_prev_row = shift+Tab, ctrl+Up, alt+Up
key_history_prev = Up
key_history_next = Down
key_copy = ctrl+c, ctrl+Insert       # copies the result if nothing is selected
key_cut = ctrl+x, shift+Delete
key_paste = ctrl+v, shift+Insert
key_undo = ctrl+z
key_redo = ctrl+shift+z, ctrl+y
key_select_all = ctrl+a
key_delete_to_start = ctrl+u
key_delete_to_end = ctrl+k
key_reload = ctrl+shift+r
key_center = ctrl+shift+0
key_clear_history =                  # unbound by default
key_quit =                           # unbound by default (stops the daemon)
key_update_rates =                   # unbound by default
```
Shortcuts with Shift match the unshifted key too, so `ctrl+shift+0` works even though Shift+0 types `)`. Cursor keys, Home/End and Backspace/Delete (with their Ctrl/Shift variants) are fixed. To see typos in shortcut names, run `qcalc quit; QCALC_DEBUG=1 qcalcd --show`.

### Applying changes
Press **Ctrl+Shift+R** in qcalc, or run `qcalc reload`. That covers everything except `fractions`, `angle` and `timeout_ms`, which need `qcalc quit` (the next hotkey press restarts it with your input intact).

## Currency exchange rates
qcalc uses qalculate's rate cache in `~/.local/share/qalculate/`, which it shares with `qalc` and qalculate-gtk. It works stale-while-revalidate:
- Results always come from the cache straight away. Nothing waits on the network, and opening the window never touches it.
- When a calculation actually uses a currency and the cache is older than `exchange_rates_max_age` days, qcalc downloads all rates on a background thread. Then it reloads them and recalculates the currency rows, so results update by themselves a moment later. If the download fails (offline, for example), it keeps the old rates and tries again at most every 15 minutes.
- You can force an update with **Update exchange rates** in the right-click menu, or with `qcalc update-rates`.
- While a visible result uses a currency, a small line under the boxes shows the cache age, e.g. `Rates cached on 2026-10-26 00:21 (3h 12m ago)`. During a download it reads `Updating exchange rates…`, and after a failed one it adds `· update failed`.

## Platforms
Debian/Ubuntu, Arch, Fedora. X11 window managers that follow the standard EWMH hints (XFCE, KDE, GNOME on X11, Cinnamon, MATE, i3, Openbox and others) all handle centering, always-on-top and skip-taskbar. On Wayland it runs through XWayland. KDE and GNOME behave normally there, while Hyprland and sway need a float/center rule for the `qcalc` window class.

Build dependencies: X11, Xext, Xrandr, cairo, pango, libqalculate (dev packages). `install.sh` installs them if they're missing.
