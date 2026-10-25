#!/usr/bin/env sh
# Build qcalc and install it to ~/.local/bin, with an autostart entry.
# Works on Debian/Ubuntu, Arch/Manjaro and Fedora.
#   ./install.sh                         build + install + start in background
#   ./install.sh --hotkey '<Super>c'     also bind a hotkey (XFCE, GNOME)
#   ./install.sh --uninstall
set -e
cd "$(dirname "$0")"
PREFIX="$HOME/.local"
AUTOSTART="${XDG_CONFIG_HOME:-$HOME/.config}/autostart/qcalc.desktop"
CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/qcalc/config"
HOTKEY=""

while [ $# -gt 0 ]; do
  case "$1" in
    --hotkey) HOTKEY="$2"; shift 2 ;;
    --uninstall)
      "$PREFIX/bin/qcalc" quit 2>/dev/null || true
      rm -f "$PREFIX/bin/qcalc" "$PREFIX/bin/qcalcd" "$AUTOSTART"
      echo "Removed qcalc. Left in place: your config ($CONFIG) and saved input (${XDG_STATE_HOME:-$HOME/.local/state}/qcalc)."
      exit 0 ;;
    *) echo "unknown option: $1"; exit 2 ;;
  esac
done

# ---- build dependencies
missing=""
for p in x11 xext xrandr cairo pangocairo libqalculate; do
  pkg-config --exists "$p" 2>/dev/null || missing="$missing $p"
done
if [ -n "$missing" ] || ! command -v c++ >/dev/null 2>&1 || ! command -v pkg-config >/dev/null 2>&1; then
  if command -v pacman >/dev/null 2>&1; then
    cmd="sudo pacman -S --needed base-devel pkgconf libx11 libxext libxrandr cairo pango libqalculate"
  elif command -v apt-get >/dev/null 2>&1; then
    cmd="sudo apt-get install -y build-essential pkg-config libx11-dev libxext-dev libxrandr-dev libcairo2-dev libpango1.0-dev libqalculate-dev"
  elif command -v dnf >/dev/null 2>&1; then
    cmd="sudo dnf install -y gcc-c++ make pkgconf libX11-devel libXext-devel libXrandr-devel cairo-devel pango-devel libqalculate-devel"
  else
    echo "Missing build dependencies:$missing (install the dev packages for X11, Xrandr, cairo, pango, libqalculate)"; exit 1
  fi
  echo "Missing build dependencies:$missing"
  echo "Will run: $cmd"
  printf "Continue? [Y/n] "; read -r ans
  case "$ans" in n|N) exit 1 ;; esac
  sh -c "$cmd"
fi

# ---- build + install
make -j"$(nproc 2>/dev/null || echo 2)"
"$PREFIX/bin/qcalc" quit 2>/dev/null || true
make install PREFIX="$PREFIX"

# ---- default config (never overwrites an existing file or symlink, e.g. from stow)
if [ -e "$CONFIG" ] || [ -L "$CONFIG" ]; then
  echo "Keeping existing config: $CONFIG"
else
  install -Dm644 config "$CONFIG"
  echo "Installed default config: $CONFIG"
fi

# ---- start with the session (hidden), so the first hotkey press is instant too
mkdir -p "$(dirname "$AUTOSTART")"
cat > "$AUTOSTART" <<EOF
[Desktop Entry]
Type=Application
Name=qcalc
Comment=Fast popup calculator (background process)
Exec=$PREFIX/bin/qcalcd
NoDisplay=true
X-GNOME-Autostart-enabled=true
EOF
sleep 0.2
(setsid "$PREFIX/bin/qcalcd" >/dev/null 2>&1 &)

# ---- hotkey
bind_hotkey() {
  key="$1"; cmd="$PREFIX/bin/qcalc"
  case "$(printf '%s' "${XDG_CURRENT_DESKTOP:-}" | tr '[:upper:]' '[:lower:]')" in
    *xfce*)
      xfconf-query -c xfce4-keyboard-shortcuts -p "/commands/custom/$key" -n -t string -s "$cmd" ;;
    *gnome*|*unity*)
      schema=org.gnome.settings-daemon.plugins.media-keys
      p=/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/qcalc/
      gsettings set "$schema.custom-keybinding:$p" name 'qcalc' &&
      gsettings set "$schema.custom-keybinding:$p" command "$cmd" &&
      gsettings set "$schema.custom-keybinding:$p" binding "$key" || return 1
      list=$(gsettings get $schema custom-keybindings)
      case "$list" in
        *"$p"*) ;;
        "@as []"|"[]") gsettings set $schema custom-keybindings "['$p']" ;;
        *) gsettings set $schema custom-keybindings "$(printf '%s' "$list" | sed "s|]\$|, '$p']|")" ;;
      esac ;;
    *) return 1 ;;
  esac
}

echo
echo "Installed: $PREFIX/bin/qcalc (hotkey command) and $PREFIX/bin/qcalcd (background process)"
if [ -n "$HOTKEY" ] && bind_hotkey "$HOTKEY"; then
  echo "Bound $HOTKEY to qcalc."
else
  [ -n "$HOTKEY" ] && echo "Could not bind the hotkey automatically on '${XDG_CURRENT_DESKTOP:-unknown}'."
  cat <<EOF
Bind a hotkey to: $PREFIX/bin/qcalc
  XFCE:      Settings > Keyboard > Application Shortcuts   (or ./install.sh --hotkey '<Super>c')
  GNOME:     Settings > Keyboard > Custom Shortcuts         (or ./install.sh --hotkey '<Super>c')
  KDE:       System Settings > Shortcuts > Add New > Command
  i3:        bindsym \$mod+c exec --no-startup-id $PREFIX/bin/qcalc
  Openbox/others: bind the command in your WM or sxhkd/xbindkeys.
EOF
fi
case ":$PATH:" in *":$PREFIX/bin:"*) ;; *) echo "Note: add $PREFIX/bin to your PATH" ;; esac
