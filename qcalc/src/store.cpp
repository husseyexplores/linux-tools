#include "store.h"

#include <algorithm>
#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>
#include <unordered_set>

static std::string home() {
    const char *h = getenv("HOME");
    return h && *h ? h : "/tmp";
}

static std::string xdg(const char *var, const char *fallback) {
    const char *v = getenv(var);
    return (v && *v == '/') ? std::string(v) : home() + fallback;
}

static void mkdirs(const std::string &path) {
    for (size_t i = 1; i <= path.size(); i++)
        if (i == path.size() || path[i] == '/') mkdir(path.substr(0, i).c_str(), 0700);
}

std::string config_path() { return xdg("XDG_CONFIG_HOME", "/.config") + "/qcalc/config"; }

std::string state_dir() {
    static std::string d;
    if (d.empty()) {
        d = xdg("XDG_STATE_HOME", "/.local/state") + "/qcalc";
        mkdirs(d);
    }
    return d;
}

static bool read_file(const std::string &path, std::string &out) {
    FILE *f = fopen(path.c_str(), "rb");
    if (!f) return false;
    char buf[65536];
    size_t n;
    out.clear();
    while ((n = fread(buf, 1, sizeof buf, f)) > 0) out.append(buf, n);
    fclose(f);
    return true;
}

static std::string trim(const std::string &s) {
    size_t a = s.find_first_not_of(" \t\r"), b = s.find_last_not_of(" \t\r");
    return a == std::string::npos ? "" : s.substr(a, b - a + 1);
}

static bool parse_bool(const std::string &v) { return v == "true" || v == "1" || v == "yes" || v == "on"; }

static void parse_color(const std::string &v, uint32_t &out) {
    std::string s = v[0] == '#' ? v.substr(1) : v;
    if (s.size() != 6) return;
    char *end;
    unsigned long c = strtoul(s.c_str(), &end, 16);
    if (*end == 0) out = (uint32_t)c;
}

Config load_config() {
    Config c;
    std::string text;
    if (!read_file(config_path(), text)) return c;
    size_t pos = 0;
    while (pos < text.size()) {
        size_t nl = text.find('\n', pos);
        if (nl == std::string::npos) nl = text.size();
        std::string line = trim(text.substr(pos, nl - pos));
        pos = nl + 1;
        if (line.empty() || line[0] == '#') continue;
        size_t eq = line.find('=');
        if (eq == std::string::npos) continue;
        std::string k = trim(line.substr(0, eq)), v = trim(line.substr(eq + 1));
        // Trailing comment: "# " at the start, or whitespace before '#' (colors like #aabbcc stay intact).
        if (v.rfind("# ", 0) == 0 || v == "#") v.clear();
        size_t hash = v.find_first_of("#", 1);
        while (hash != std::string::npos && v[hash - 1] != ' ' && v[hash - 1] != '\t')
            hash = v.find('#', hash + 1);
        if (hash != std::string::npos) v = trim(v.substr(0, hash));
        if (v.size() >= 2 && v.front() == '"' && v.back() == '"') v = v.substr(1, v.size() - 2);
        if (v.empty()) continue;
        auto num = [&](double &out, double lo, double hi) {
            char *end;
            double d = strtod(v.c_str(), &end);
            if (end != v.c_str()) out = std::clamp(d, lo, hi);
        };
        if (k.rfind("key_", 0) == 0) c.keys.emplace_back(k.substr(4), v);
        else if (k == "radius") num(c.radius, 0, 200);
        else if (k == "exchange_rates_max_age") num(c.exchange_rates_max_age, 0, 3650);
        else if (k == "window_radius") num(c.window_radius, 0, 200);
        else if (k == "window_padding") num(c.window_padding, 0, 200);
        else if (k == "row_gap") num(c.row_gap, 0, 200);
        else if (k == "box_padding_x") num(c.box_padding_x, 0, 200);
        else if (k == "box_padding_y") num(c.box_padding_y, 0, 200);
        else if (k == "border_width") num(c.border_width, 0, 20);
        else if (k == "window_border") num(c.window_border, 0, 20);
        else if (k == "footer_margin_left") num(c.footer_margin_left, 0, 500);
        else if (k == "footer_margin_top") num(c.footer_margin_top, 0, 200);
        else if (k == "font") c.font = v;
        else if (k == "width") c.width = std::max(200, atoi(v.c_str()));
        else if (k == "hide_on_focus_loss") c.hide_on_focus_loss = parse_bool(v);
        else if (k == "close_button") c.close_button = parse_bool(v);
        else if (k == "center_button") c.center_button = parse_bool(v);
        else if (k == "context_menu") c.context_menu = parse_bool(v);
        else if (k == "copy_on_enter") c.copy_on_enter = parse_bool(v);
        else if (k == "max_history") c.max_history = std::max(0, atoi(v.c_str()));
        else if (k == "timeout_ms") c.timeout_ms = std::max(50, atoi(v.c_str()));
        else if (k == "angle") c.angle = v;
        else if (k == "fractions") c.fractions = parse_bool(v);
        else if (k == "color_bg") parse_color(v, c.bg);
        else if (k == "color_field") parse_color(v, c.field);
        else if (k == "color_border") parse_color(v, c.border);
        else if (k == "color_text") parse_color(v, c.fg);
        else if (k == "color_result") parse_color(v, c.dim);
        else if (k == "color_accent") parse_color(v, c.accent);
        else if (k == "color_selection") parse_color(v, c.sel);
        else if (k == "color_error") parse_color(v, c.error);
    }
    return c;
}

// State file: "qcalc-state 1", "focus <n>", then one "<cursor>\t<text>" line per row.
// Row text never contains newlines or tabs (the editor replaces them with spaces).
bool load_state(SavedState &s) {
    std::string text;
    if (!read_file(state_dir() + "/state", text)) return false;
    s = SavedState{};
    size_t pos = 0;
    int lineno = 0;
    while (pos < text.size()) {
        size_t nl = text.find('\n', pos);
        if (nl == std::string::npos) nl = text.size();
        std::string line = text.substr(pos, nl - pos);
        pos = nl + 1;
        if (lineno++ == 0) {
            if (line.rfind("qcalc-state", 0) != 0) return false;
            continue;
        }
        if (line.rfind("focus ", 0) == 0) {
            s.focus = atoi(line.c_str() + 6);
            continue;
        }
        size_t tab = line.find('\t');
        if (tab == std::string::npos) continue;
        SavedRow r;
        r.cursor = strtoul(line.substr(0, tab).c_str(), nullptr, 10);
        r.text = line.substr(tab + 1);
        if (r.cursor > r.text.size()) r.cursor = r.text.size();
        s.rows.push_back(std::move(r));
    }
    return true;
}

static bool write_all(int fd, const std::string &data) {
    size_t off = 0;
    while (off < data.size()) {
        ssize_t w = write(fd, data.data() + off, data.size() - off);
        if (w < 0) {
            if (errno == EINTR) continue;
            return false;
        }
        off += (size_t)w;
    }
    return true;
}

bool save_state(const SavedState &s, bool sync) {
    std::string out = "qcalc-state 1\nfocus " + std::to_string(s.focus) + "\n";
    for (auto &r : s.rows) out += std::to_string(r.cursor) + "\t" + r.text + "\n";
    std::string path = state_dir() + "/state", tmp = path + ".tmp";
    int fd = open(tmp.c_str(), O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
    if (fd < 0) return false;
    bool ok = write_all(fd, out);
    if (ok && sync) ok = fdatasync(fd) == 0;
    close(fd);
    if (!ok || rename(tmp.c_str(), path.c_str()) != 0) {
        unlink(tmp.c_str());
        return false;
    }
    return true;
}

std::vector<std::string> load_history(int max) {
    std::string path = state_dir() + "/history", text;
    std::vector<std::string> lines;
    if (!read_file(path, text)) return lines;
    size_t pos = 0;
    while (pos < text.size()) {
        size_t nl = text.find('\n', pos);
        if (nl == std::string::npos) nl = text.size();
        if (nl > pos) lines.push_back(text.substr(pos, nl - pos));
        pos = nl + 1;
    }
    size_t raw = lines.size();
    // Keep the newest copy of each entry, oldest first.
    std::vector<std::string> out;
    std::unordered_set<std::string> seen;
    for (size_t i = lines.size(); i-- > 0;) {
        if (seen.insert(lines[i]).second) out.push_back(std::move(lines[i]));
        if ((int)out.size() >= max) break;
    }
    std::vector<std::string> res(out.rbegin(), out.rend());
    // Compact the file when it has grown well past what we keep.
    if (raw > (size_t)max + max / 2 + 50) {
        std::string data;
        for (auto &l : res) data += l + "\n";
        std::string tmp = path + ".tmp";
        int fd = open(tmp.c_str(), O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
        if (fd >= 0) {
            bool ok = write_all(fd, data);
            close(fd);
            if (!ok || rename(tmp.c_str(), path.c_str()) != 0) unlink(tmp.c_str());
        }
    }
    return res;
}

void append_history(const std::string &line) {
    int fd = open((state_dir() + "/history").c_str(), O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
    if (fd < 0) return;
    write_all(fd, line + "\n");
    close(fd);
}

void clear_history_file() {
    int fd = open((state_dir() + "/history").c_str(), O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
    if (fd >= 0) close(fd);
}
