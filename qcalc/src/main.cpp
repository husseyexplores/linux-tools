// qcalcd: the calculator window. Stays resident with the window hidden, so showing it is instant.
#include "common.h"
#include "engine.h"
#include "store.h"

#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xresource.h>
#include <X11/Xutil.h>
#include <X11/cursorfont.h>
#include <X11/extensions/Xrandr.h>
#include <X11/extensions/shape.h>
#include <X11/keysym.h>
#include <cairo-xlib.h>
#include <cairo.h>
#include <pango/pangocairo.h>

#include <algorithm>
#include <cerrno>
#include <climits>
#include <clocale>
#include <cmath>
#include <csignal>
#include <cstdlib>
#include <ctime>
#include <fcntl.h>
#include <poll.h>
#include <string>
#include <vector>

// ---------------------------------------------------------------- helpers

static double now_ms() {
    timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1e3 + ts.tv_nsec / 1e6;
}

static size_t u8_next(const std::string &s, size_t i) {
    if (i >= s.size()) return s.size();
    i++;
    while (i < s.size() && ((unsigned char)s[i] & 0xC0) == 0x80) i++;
    return i;
}

static size_t u8_prev(const std::string &s, size_t i) {
    if (i == 0) return 0;
    i--;
    while (i > 0 && ((unsigned char)s[i] & 0xC0) == 0x80) i--;
    return i;
}

static size_t u8_fix(const std::string &s, size_t i) {  // clamp to a char boundary
    if (i >= s.size()) return s.size();
    while (i > 0 && ((unsigned char)s[i] & 0xC0) == 0x80) i--;
    return i;
}

// 0 = space, 1 = word (letters, digits, '.', '_', non-ASCII), 2 = operator/punctuation
static int char_class(const std::string &s, size_t i) {
    unsigned char c = (unsigned char)s[i];
    if (c == ' ') return 0;
    if (c >= 0x80 || isalnum(c) || c == '_' || c == '.') return 1;
    return 2;
}

static size_t word_left(const std::string &s, size_t i) {
    while (i > 0 && char_class(s, u8_prev(s, i)) == 0) i = u8_prev(s, i);
    if (i == 0) return 0;
    int c = char_class(s, u8_prev(s, i));
    while (i > 0 && char_class(s, u8_prev(s, i)) == c) i = u8_prev(s, i);
    return i;
}

static size_t word_right(const std::string &s, size_t i) {
    while (i < s.size() && char_class(s, i) == 0) i = u8_next(s, i);
    if (i >= s.size()) return s.size();
    int c = char_class(s, i);
    while (i < s.size() && char_class(s, i) == c) i = u8_next(s, i);
    return i;
}

// Single line only: newlines and tabs become spaces, other control chars are dropped.
static std::string sanitize(const std::string &in) {
    std::string out;
    out.reserve(in.size());
    for (unsigned char c : in) {
        if (c == '\n' || c == '\r' || c == '\t') out += ' ';
        else if (c < 0x20 || c == 0x7f) continue;
        else out += (char)c;
    }
    return out;
}

static bool is_assignment(const std::string &s) { return s.find(":=") != std::string::npos; }

static std::string trim(const std::string &s) {
    size_t a = s.find_first_not_of(' '), b = s.find_last_not_of(' ');
    return a == std::string::npos ? "" : s.substr(a, b - a + 1);
}

// ---------------------------------------------------------------- state

struct Snapshot {
    std::string text;
    size_t cur, anchor;
};

struct Row {
    int uid = 0;
    std::string text;
    size_t cur = 0, anchor = 0;
    double scroll = 0;
    uint64_t gen = 0;
    // result
    std::string res, res_copy;
    bool res_approx = false, res_err = false, has_res = false, res_currency = false;
    uint64_t res_gen = 0;
    bool needs_enter = false;  // an assignment that only runs on Enter
    // history navigation
    int hist = -1;
    std::string draft, prefix;
    // undo
    std::vector<Snapshot> undo, redo;
    int last_kind = 0;
    double last_edit = 0;
    // pango
    PangoLayout *lay = nullptr, *rlay = nullptr;
    bool lay_dirty = true;
};

enum EditKind { EK_NONE, EK_TYPE, EK_DELETE, EK_OTHER };

static Config cfg;
static std::vector<Row> rows;
static int focus = 0, next_uid = 1;
static std::vector<std::string> history;
static bool state_dirty = false;

// X / drawing
static Display *dpy;
static int scr;
static Window root, win;
static Visual *visual;
static XIM xim;
static XIC xic;
static cairo_surface_t *surf;
static PangoContext *pctx;
static PangoFontDescription *fdesc;
static PangoLayout *placeholder;
static Cursor cur_text, cur_arrow, cur_hand, cur_move;
static Cursor cur_current = 0;

static Atom A_WM_PROTOCOLS, A_WM_DELETE, A_NET_STATE, A_NET_ABOVE, A_NET_SKIP_TASKBAR, A_NET_SKIP_PAGER,
    A_NET_STICKY, A_NET_TYPE, A_NET_TYPE_DIALOG, A_NET_ACTIVE, A_NET_NAME, A_UTF8, A_MOTIF, A_CLIPBOARD, A_TARGETS,
    A_TEXT, A_QC_SEL, A_NET_PID, A_NET_DESKTOP;

static double dpi = 96, scale = 1;
static int W = 560, H = 60;
static int win_x = 0, win_y = 0;
static double pad, gap, sp, hpad, vpad, row_h, line_h, bsz;
static double notch_h, notch_w;  // tab above the top-right edge holding the overlay buttons (0 = none)
static const double OB_D = 14, OB_GAP = 0, OB_PADX = 0, OB_PADY = 0;  // overlay button size/spacing, px at 96 dpi
static bool have_shape = false;

static bool visible = false, mapped = false, has_focus = false, need_redraw = true;
static double show_time = 0;
static bool blink_on = true;
static double next_blink = 0;
static int flash_uid = -1;
static double flash_until = 0;

// mouse
static bool dragging = false;
// window drag (by the background); the position is kept until the daemon exits
static bool win_dragging = false, user_pos = false, move_pending = false;
static int drag_root_x, drag_root_y, drag_win_x, drag_win_y;
static int press_btn = -1, hover_btn = -1;
static int click_count = 0, last_click_row = -1;
static double last_click_t = 0, last_click_x = 0;
static Time last_time = CurrentTime;

// clipboard
static std::string clip_text, prim_text;
static int paste_uid = -1;
static bool paste_tried_string = false;
static int copy_pending_uid = -1;
static uint64_t copy_pending_gen = 0;

static volatile sig_atomic_t quit_flag = 0;

// ---------------------------------------------------------------- layout

static void compute_metrics() {
    scale = dpi / 96.0;
    PangoLayout *l = pango_layout_new(pctx);
    pango_layout_set_font_description(l, fdesc);
    pango_layout_set_text(l, "Ag8()", -1);
    PangoRectangle lr;
    pango_layout_get_pixel_extents(l, nullptr, &lr);
    g_object_unref(l);
    line_h = lr.height;
    pad = std::round(cfg.window_padding * scale);
    gap = std::round(cfg.row_gap * scale);
    sp = std::round(6 * scale);  // text <-> result <-> button
    hpad = std::round(cfg.box_padding_x * scale);
    vpad = std::round(cfg.box_padding_y * scale);
    row_h = std::round(line_h + 2 * vpad);
    bsz = std::round(std::min(row_h, std::max(14.0 * scale, line_h * 0.85)));
    int nb = (int)cfg.close_button + (int)cfg.center_button;
    double d = std::round(OB_D * scale);
    notch_h = nb ? d + 2 * std::round(OB_PADY * scale) : 0;
    notch_w = nb ? nb * d + (nb - 1) * std::round(OB_GAP * scale) + 2 * std::round(OB_PADX * scale) : 0;
    W = (int)std::round(cfg.width * scale);
}

// Footer: a small line under the boxes showing how old the exchange rates are, while a visible
// result uses a currency.
static PangoLayout *footer_lay;
static double footer_h, footer_text_h;

static bool want_footer() {
    for (auto &r : rows)
        if (r.has_res && !r.res_err && r.res_currency && !r.needs_enter &&
            r.text.find_first_not_of(' ') != std::string::npos)
            return true;
    return false;
}

static std::string footer_text() {
    RatesState st = engine::rates_state();
    if (st == RATES_UPDATING) return "Updating exchange rates\u2026";
    time_t t = engine::rates_time();
    std::string s;
    if (t <= 0) s = "No cached exchange rates";
    else {
        struct tm tmv;
        char date[32];
        localtime_r(&t, &tmv);
        strftime(date, sizeof date, "%Y-%m-%d %H:%M", &tmv);
        long ago = std::max(0L, (long)(time(nullptr) - t));
        long d = ago / 86400, h = ago % 86400 / 3600, m = ago % 3600 / 60;
        char buf[64];
        if (d) snprintf(buf, sizeof buf, "%ldd %ldh %ldm ago", d, h, m);
        else if (h) snprintf(buf, sizeof buf, "%ldh %ldm ago", h, m);
        else if (m) snprintf(buf, sizeof buf, "%ldm ago", m);
        else snprintf(buf, sizeof buf, "just now");
        s = std::string("Rates cached on ") + date + " (" + buf + ")";
    }
    if (st == RATES_FAILED) s += " \u00b7 update failed";
    return s;
}

static double rows_bottom() {
    double n = (double)std::max<size_t>(1, rows.size());
    return notch_h + pad + n * row_h + (n - 1) * gap;
}

static int window_height() { return (int)(rows_bottom() + pad + (want_footer() ? footer_h : 0)); }

struct Geo {
    double x, y, w, h;  // field
    double tx, ty, tw;  // text area
    double rr, rmax;    // result right edge, max width
    double bx, by;      // +/x button
};

// Close button: a small x drawn on top of everything in the window's top-right corner. Takes no layout space.
struct CloseRect {
    double x, y, s;
};
static CloseRect close_rect();
static CloseRect center_rect();
static const int CENTER_BTN = -3;  // hover_btn / press_btn value for the center button
// Overlay buttons sit in the notch at the top right: slot 0 = close, slot 1 = center (left of it).
static CloseRect overlay_rect(int slot) {
    double d = std::round(OB_D * scale);
    double x = W - std::round(OB_PADX * scale) - d - slot * (d + std::round(OB_GAP * scale));
    return {x, std::round((notch_h - d) / 2), d};
}
static CloseRect close_rect() {
    if (!cfg.close_button) return {0, 0, 0};
    return overlay_rect(0);
}
static CloseRect center_rect() {
    if (!cfg.center_button) return {0, 0, 0};
    return overlay_rect(cfg.close_button ? 1 : 0);
}
static const int CLOSE_BTN = -2;  // hover_btn / press_btn value for the close button

static Geo geo(size_t i) {
    Geo g;
    g.x = pad;
    g.y = notch_h + pad + i * (row_h + gap);
    g.w = W - 2 * pad;
    g.h = row_h;
    double m = std::round((row_h - bsz) / 2);
    double mh = std::max(std::round(4 * scale), std::min(m, hpad));
    g.bx = g.x + g.w - mh - bsz;
    g.by = g.y + m;
    g.tx = g.x + hpad;
    g.ty = g.y + std::round((row_h - line_h) / 2);
    g.rr = g.bx - sp;
    double avail = g.rr - g.tx;
    g.rmax = std::round(avail * 0.5);
    g.tw = avail;  // shrunk by the result width in layout_row()
    return g;
}

static void ensure_layouts(Row &r) {
    if (!r.lay) {
        r.lay = pango_layout_new(pctx);
        r.rlay = pango_layout_new(pctx);
        pango_layout_set_font_description(r.lay, fdesc);
        pango_layout_set_font_description(r.rlay, fdesc);
        pango_layout_set_single_paragraph_mode(r.lay, TRUE);
        pango_layout_set_single_paragraph_mode(r.rlay, TRUE);
        r.lay_dirty = true;
    }
    if (r.lay_dirty) {
        pango_layout_set_text(r.lay, r.text.data(), (int)r.text.size());
        r.lay_dirty = false;
    }
}

static std::string result_display(const Row &r) {
    if (r.text.find_first_not_of(' ') == std::string::npos) return "";
    if (r.needs_enter) return "↵ to assign";
    if (!r.has_res || r.res_err) return "";
    return std::string(r.res_approx ? "≈ " : "= ") + r.res;
}

// Sets the result layout and returns the text area width left for the input.
static double layout_row(Row &r, Geo &g, double *res_w) {
    ensure_layouts(r);
    std::string rd = result_display(r);
    pango_layout_set_text(r.rlay, rd.data(), (int)rd.size());
    pango_layout_set_width(r.rlay, -1);
    pango_layout_set_ellipsize(r.rlay, PANGO_ELLIPSIZE_NONE);
    int pw = 0, ph = 0;
    pango_layout_get_pixel_size(r.rlay, &pw, &ph);
    if (pw > g.rmax) {
        pango_layout_set_width(r.rlay, (int)(g.rmax * PANGO_SCALE));
        pango_layout_set_ellipsize(r.rlay, PANGO_ELLIPSIZE_END);
        pango_layout_get_pixel_size(r.rlay, &pw, &ph);
    }
    *res_w = rd.empty() ? 0 : pw;
    g.tw = (g.rr - g.tx) - (*res_w > 0 ? *res_w + 2 * sp : 0);
    return g.tw;
}

static double cursor_x(Row &r, size_t idx) {
    ensure_layouts(r);
    PangoRectangle strong;
    pango_layout_get_cursor_pos(r.lay, (int)idx, &strong, nullptr);
    return strong.x / (double)PANGO_SCALE;
}

static void fix_scroll(Row &r, double tw) {
    int tw_px = 0, th = 0;
    pango_layout_get_pixel_size(r.lay, &tw_px, &th);
    double cx = cursor_x(r, r.cur);
    double margin = 2 * scale;
    if (cx - r.scroll > tw - margin) r.scroll = cx - tw + margin;
    if (cx - r.scroll < 0) r.scroll = cx;
    double maxs = std::max(0.0, tw_px - tw + margin);
    r.scroll = std::clamp(r.scroll, 0.0, maxs);
}

static size_t index_at(Row &r, size_t i, double x) {
    Geo g = geo(i);
    double rw;
    layout_row(r, g, &rw);
    ensure_layouts(r);
    int idx = 0, trailing = 0;
    pango_layout_xy_to_index(r.lay, (int)((x - g.tx + r.scroll) * PANGO_SCALE), (int)(line_h / 2 * PANGO_SCALE),
                             &idx, &trailing);
    size_t p = u8_fix(r.text, (size_t)std::max(0, idx));
    while (trailing-- > 0) p = u8_next(r.text, p);
    return p;
}

// ---------------------------------------------------------------- engine glue

static Row *row_by_uid(int uid) {
    for (auto &r : rows)
        if (r.uid == uid) return &r;
    return nullptr;
}

static void submit_row(Row &r, bool allow_assign) {
    r.needs_enter = false;
    if (r.text.find_first_not_of(' ') == std::string::npos) {
        engine::cancel(r.uid);
        r.has_res = false;
        return;
    }
    if (is_assignment(r.text) && !allow_assign) {
        engine::cancel(r.uid);
        r.needs_enter = true;
        return;
    }
    engine::submit(r.uid, r.gen, r.text);
}

static void resubmit_all_except(int uid) {
    for (auto &r : rows)
        if (r.uid != uid && !is_assignment(r.text)) {
            r.gen++;
            submit_row(r, false);
        }
}

// ---------------------------------------------------------------- clipboard

static bool own_selection(Atom sel, const std::string &text) {
    if (text.empty()) return false;
    (sel == A_CLIPBOARD ? clip_text : prim_text) = text;
    XSetSelectionOwner(dpy, sel, win, last_time);
    return XGetSelectionOwner(dpy, sel) == win;
}

static void request_paste(Atom sel, int uid) {
    if (XGetSelectionOwner(dpy, sel) == win) {  // our own text: no round trip
        Row *r = row_by_uid(uid);
        if (r) {
            extern void insert_text(Row &, const std::string &, int);
            insert_text(*r, sel == A_CLIPBOARD ? clip_text : prim_text, EK_OTHER);
        }
        return;
    }
    paste_uid = uid;
    paste_tried_string = false;
    XConvertSelection(dpy, sel, A_UTF8, A_QC_SEL, win, last_time);
}

static void handle_selection_request(XSelectionRequestEvent &e) {
    XSelectionEvent ev{};
    ev.type = SelectionNotify;
    ev.display = e.display;
    ev.requestor = e.requestor;
    ev.selection = e.selection;
    ev.target = e.target;
    ev.time = e.time;
    ev.property = None;
    Atom prop = e.property == None ? e.target : e.property;
    const std::string &text = e.selection == A_CLIPBOARD ? clip_text : prim_text;
    if (e.target == A_TARGETS) {
        Atom t[] = {A_TARGETS, A_UTF8, XA_STRING, A_TEXT};
        XChangeProperty(dpy, e.requestor, prop, XA_ATOM, 32, PropModeReplace, (unsigned char *)t, 4);
        ev.property = prop;
    } else if (e.target == A_UTF8 || e.target == XA_STRING || e.target == A_TEXT) {
        XChangeProperty(dpy, e.requestor, prop, e.target == XA_STRING ? XA_STRING : A_UTF8, 8, PropModeReplace,
                        (const unsigned char *)text.data(), (int)text.size());
        ev.property = prop;
    }
    XSendEvent(dpy, e.requestor, False, 0, (XEvent *)&ev);
}

// ---------------------------------------------------------------- editing

static void mark_changed(Row &r, bool keep_hist = false) {
    r.lay_dirty = true;
    r.gen++;
    if (!keep_hist) r.hist = -1;
    submit_row(r, false);
    state_dirty = true;
    need_redraw = true;
}

static void push_undo(Row &r, int kind) {
    double t = now_ms();
    if (kind != r.last_kind || kind == EK_OTHER || t - r.last_edit > 1000 || r.undo.empty()) {
        r.undo.push_back({r.text, r.cur, r.anchor});
        if (r.undo.size() > 200) r.undo.erase(r.undo.begin());
    }
    r.redo.clear();
    r.last_kind = kind;
    r.last_edit = t;
}

static bool has_sel(const Row &r) { return r.cur != r.anchor; }

static std::string sel_text(const Row &r) {
    size_t a = std::min(r.cur, r.anchor), b = std::max(r.cur, r.anchor);
    return r.text.substr(a, b - a);
}

static void delete_sel(Row &r) {
    size_t a = std::min(r.cur, r.anchor), b = std::max(r.cur, r.anchor);
    r.text.erase(a, b - a);
    r.cur = r.anchor = a;
}

void insert_text(Row &r, const std::string &raw, int kind) {
    std::string s = sanitize(raw);
    if (s.empty() && !has_sel(r)) return;
    push_undo(r, kind);
    if (has_sel(r)) delete_sel(r);
    r.text.insert(r.cur, s);
    r.cur += s.size();
    r.anchor = r.cur;
    mark_changed(r);
}

static void delete_range(Row &r, size_t a, size_t b) {
    if (a == b) return;
    push_undo(r, EK_DELETE);
    r.text.erase(a, b - a);
    r.cur = r.anchor = a;
    mark_changed(r);
}

static void move_cursor(Row &r, size_t to, bool extend) {
    r.cur = to;
    if (!extend) r.anchor = to;
    r.last_kind = EK_NONE;
    need_redraw = true;
}

static void set_text(Row &r, const std::string &t, bool keep_hist) {
    r.text = t;
    r.cur = r.anchor = t.size();
    r.scroll = 0;
    mark_changed(r, keep_hist);
}

static void undo(Row &r, bool redo) {
    auto &from = redo ? r.redo : r.undo;
    auto &to = redo ? r.undo : r.redo;
    if (from.empty()) return;
    to.push_back({r.text, r.cur, r.anchor});
    Snapshot s = from.back();
    from.pop_back();
    r.text = s.text;
    r.cur = std::min(s.cur, r.text.size());
    r.anchor = std::min(s.anchor, r.text.size());
    r.last_kind = EK_NONE;
    mark_changed(r);
}

static void history_nav(Row &r, int dir) {
    if (history.empty()) return;
    if (dir < 0) {
        int start = r.hist;
        if (r.hist == -1) {
            r.draft = r.text;
            r.prefix = trim(r.text);
            start = (int)history.size();
        }
        for (int i = start - 1; i >= 0; i--) {
            if (history[i].rfind(r.prefix, 0) == 0 && history[i] != r.text) {
                r.hist = i;
                set_text(r, history[i], true);
                return;
            }
        }
    } else {
        if (r.hist == -1) return;
        for (int i = r.hist + 1; i < (int)history.size(); i++) {
            if (history[i].rfind(r.prefix, 0) == 0 && history[i] != r.text) {
                r.hist = i;
                set_text(r, history[i], true);
                return;
            }
        }
        r.hist = -1;
        set_text(r, r.draft, false);
    }
}

static void add_history(const std::string &line) {
    std::string t = trim(line);
    if (t.empty() || cfg.max_history == 0) return;
    if (!history.empty() && history.back() == t) return;
    auto it = std::find(history.begin(), history.end(), t);
    if (it != history.end()) history.erase(it);
    history.push_back(t);
    if ((int)history.size() > cfg.max_history) history.erase(history.begin());
    append_history(t);
}

static void flash(int uid) {
    flash_uid = uid;
    flash_until = now_ms() + 350;
    need_redraw = true;
}

static void copy_result(Row &r) {
    if (r.has_res && !r.res_err && r.res_gen == r.gen && own_selection(A_CLIPBOARD, r.res_copy)) flash(r.uid);
}

static void commit(Row &r) {
    add_history(r.text);
    r.hist = -1;
    if (is_assignment(r.text)) {
        r.gen++;
        submit_row(r, true);
        resubmit_all_except(r.uid);
    }
    if (cfg.copy_on_enter) {
        if (r.has_res && r.res_gen == r.gen) copy_result(r);
        else {
            copy_pending_uid = r.uid;
            copy_pending_gen = r.gen;
        }
    }
    need_redraw = true;
}

// ---------------------------------------------------------------- rows & window geometry

static void free_row(Row &r) {
    if (r.lay) g_object_unref(r.lay);
    if (r.rlay) g_object_unref(r.rlay);
    r.lay = r.rlay = nullptr;
}

static void set_size_hints() {
    XSizeHints *h = XAllocSizeHints();
    h->flags = PPosition | USPosition | PMinSize | PMaxSize;
    h->x = win_x;
    h->y = win_y;
    h->min_width = h->max_width = W;
    h->min_height = h->max_height = H;
    XSetWMNormalHints(dpy, win, h);
    XFree(h);
}

// Rounded window corners via the X Shape extension (works without a compositor).
static void rounded(cairo_t *cr, double x, double y, double w, double h, double r);

// Window outline: the main body (rounded by window_radius) plus the notch tab at the top right.
static void window_outline(cairo_t *cr, double inset) {
    double r = std::max(0.0, std::min(cfg.window_radius * scale, std::min((double)W, H - notch_h) / 2) - inset);
    double x0 = inset, x1 = W - inset, y0 = notch_h + inset, yb = H - inset;
    if (notch_h <= 0) {
        rounded(cr, x0, y0, x1 - x0, yb - y0, r);
        return;
    }
    cairo_new_sub_path(cr);
    // Notch corners follow window_radius, a bit smaller since the tab is small.
    double nr = std::max(0.0, std::min(cfg.window_radius * scale * 0.75, notch_h / 2) - inset), nx = W - notch_w + inset, ny = inset;
    cairo_arc(cr, x0 + r, y0 + r, r, M_PI, 1.5 * M_PI);
    cairo_line_to(cr, nx, y0);
    cairo_arc(cr, nx + nr, ny + nr, nr, M_PI, 1.5 * M_PI);
    cairo_arc(cr, x1 - nr, ny + nr, nr, 1.5 * M_PI, 2 * M_PI);
    cairo_arc(cr, x1 - r, yb - r, r, 0, 0.5 * M_PI);
    cairo_arc(cr, x0 + r, yb - r, r, 0.5 * M_PI, M_PI);
    cairo_close_path(cr);
}

// Cut the window to its outline with the X Shape extension (works without a compositor).
static void apply_shape() {
    if (!have_shape) return;
    if (notch_h <= 0 && cfg.window_radius <= 0) {
        XShapeCombineMask(dpy, win, ShapeBounding, 0, 0, None, ShapeSet);
        return;
    }
    Pixmap pm = XCreatePixmap(dpy, win, W, H, 1);
    cairo_surface_t *ms = cairo_xlib_surface_create_for_bitmap(dpy, pm, ScreenOfDisplay(dpy, scr), W, H);
    cairo_t *cr = cairo_create(ms);
    cairo_set_operator(cr, CAIRO_OPERATOR_CLEAR);
    cairo_paint(cr);
    cairo_set_operator(cr, CAIRO_OPERATOR_OVER);
    cairo_set_antialias(cr, CAIRO_ANTIALIAS_NONE);
    cairo_set_source_rgba(cr, 1, 1, 1, 1);
    window_outline(cr, 0);
    cairo_fill(cr);
    cairo_destroy(cr);
    cairo_surface_flush(ms);
    cairo_surface_destroy(ms);
    XShapeCombineMask(dpy, win, ShapeBounding, 0, 0, pm, ShapeSet);
    XFreePixmap(dpy, pm);
}

static void apply_size() {
    int nh = window_height();
    if (nh == H && surf && cairo_xlib_surface_get_width(surf) == W) return;
    H = nh;
    set_size_hints();
    XResizeWindow(dpy, win, W, H);
    if (surf) cairo_xlib_surface_set_size(surf, W, H);
    apply_shape();
    need_redraw = true;
}

static void add_row(size_t at) {
    Row r;
    r.uid = next_uid++;
    rows.insert(rows.begin() + at, std::move(r));
    focus = (int)at;
    state_dirty = true;
    apply_size();
}

static void remove_row(size_t i) {
    if (rows.size() <= 1) return;
    engine::cancel(rows[i].uid);
    free_row(rows[i]);
    rows.erase(rows.begin() + i);
    if (focus > (int)i || focus >= (int)rows.size()) focus = std::max(0, focus - 1);
    hover_btn = press_btn = -1;
    state_dirty = true;
    apply_size();
}

static SavedState snapshot() {
    SavedState s;
    s.focus = focus;
    for (auto &r : rows) s.rows.push_back({r.text, r.cur});
    return s;
}

// ---------------------------------------------------------------- drawing

static void set_color(cairo_t *cr, uint32_t c, double a = 1) {
    cairo_set_source_rgba(cr, ((c >> 16) & 255) / 255.0, ((c >> 8) & 255) / 255.0, (c & 255) / 255.0, a);
}

static void rounded(cairo_t *cr, double x, double y, double w, double h, double r) {
    if (r <= 0.5) {
        cairo_rectangle(cr, x, y, w, h);
        return;
    }
    cairo_new_sub_path(cr);
    cairo_arc(cr, x + w - r, y + r, r, -M_PI / 2, 0);
    cairo_arc(cr, x + w - r, y + h - r, r, 0, M_PI / 2);
    cairo_arc(cr, x + r, y + h - r, r, M_PI / 2, M_PI);
    cairo_arc(cr, x + r, y + r, r, M_PI, 3 * M_PI / 2);
    cairo_close_path(cr);
}

static void redraw() {
    if (!surf || !mapped) return;
    need_redraw = false;
    cairo_t *cr = cairo_create(surf);
    cairo_push_group(cr);

    set_color(cr, cfg.bg);
    cairo_paint(cr);
    double wb = cfg.window_border > 0 ? std::max(1.0, std::round(cfg.window_border * scale)) : 0;
    if (wb > 0) {
        window_outline(cr, wb / 2);
        set_color(cr, cfg.border);
        cairo_set_line_width(cr, wb);
        cairo_stroke(cr);
    }

    double t = now_ms();
    for (size_t i = 0; i < rows.size(); i++) {
        Row &r = rows[i];
        Geo g = geo(i);
        double rw;
        layout_row(r, g, &rw);
        bool foc = (int)i == focus;
        if (foc) fix_scroll(r, g.tw);

        // field
        double br = std::min(cfg.radius * scale, row_h / 2);
        double bw = cfg.border_width > 0 ? std::max(1.0, cfg.border_width * scale) : 0;
        if (bw > 0 && foc && has_focus) bw = std::max(bw, 1.5 * scale);
        rounded(cr, g.x + bw / 2, g.y + bw / 2, g.w - bw, g.h - bw, std::max(0.0, br - bw / 2));
        set_color(cr, cfg.field);
        cairo_fill_preserve(cr);
        if (bw > 0) {
            set_color(cr, foc && has_focus ? cfg.accent : cfg.border, foc && has_focus ? 0.9 : 1);
            cairo_set_line_width(cr, bw);
            cairo_stroke(cr);
        } else cairo_new_path(cr);

        // input text, clipped to its area
        cairo_save(cr);
        cairo_rectangle(cr, g.tx - 2 * scale, g.y, g.tw + 4 * scale, g.h);
        cairo_clip(cr);
        double ox = g.tx - r.scroll;
        if (has_sel(r)) {
            size_t a = std::min(r.cur, r.anchor), b = std::max(r.cur, r.anchor);
            double xa = cursor_x(r, a), xb = cursor_x(r, b);
            set_color(cr, cfg.sel, foc && has_focus ? 1 : 0.6);
            cairo_rectangle(cr, ox + xa, g.ty, xb - xa, line_h);
            cairo_fill(cr);
        }
        if (r.text.empty() && rows.size() == 1) {
            set_color(cr, cfg.dim, 0.55);
            cairo_move_to(cr, g.tx, g.ty);
            pango_cairo_show_layout(cr, placeholder);
        }
        set_color(cr, cfg.fg);
        cairo_move_to(cr, ox, g.ty);
        pango_cairo_show_layout(cr, r.lay);
        if (foc && has_focus && blink_on) {
            double cx = std::round(ox + cursor_x(r, r.cur));
            set_color(cr, cfg.accent);
            cairo_rectangle(cr, cx, g.ty, std::max(1.0, std::round(1.5 * scale)), line_h);
            cairo_fill(cr);
        }
        cairo_restore(cr);

        // result
        if (rw > 0) {
            bool fl = flash_uid == r.uid && t < flash_until;
            set_color(cr, fl ? cfg.accent : cfg.dim, r.needs_enter ? 0.7 : 1);
            cairo_move_to(cr, g.rr - rw, g.ty);
            pango_cairo_show_layout(cr, r.rlay);
        }

        // + / x button
        double cx = g.bx + bsz / 2, cy = g.by + bsz / 2, k = bsz * 0.22;
        if (hover_btn == (int)i || press_btn == (int)i) {
            cairo_arc(cr, cx, cy, bsz / 2, 0, 2 * M_PI);
            set_color(cr, cfg.border, press_btn == (int)i ? 1 : 0.7);
            cairo_fill(cr);
        }
        set_color(cr, hover_btn == (int)i ? cfg.fg : cfg.dim);
        cairo_set_line_width(cr, std::max(1.2, 1.5 * scale));
        cairo_set_line_cap(cr, CAIRO_LINE_CAP_ROUND);
        if (i == 0) {
            k *= 1.15;
            cairo_move_to(cr, cx - k, cy);
            cairo_line_to(cr, cx + k, cy);
            cairo_move_to(cr, cx, cy - k);
            cairo_line_to(cr, cx, cy + k);
        } else {
            cairo_move_to(cr, cx - k, cy - k);
            cairo_line_to(cr, cx + k, cy + k);
            cairo_move_to(cr, cx + k, cy - k);
            cairo_line_to(cr, cx - k, cy + k);
        }
        cairo_stroke(cr);

    }

    // overlay buttons: solid round background, then the icon
    if (want_footer()) {
        std::string ft = footer_text();
        pango_layout_set_text(footer_lay, ft.data(), (int)ft.size());
        double fx = pad + std::round(cfg.footer_margin_left * scale);
        pango_layout_set_width(footer_lay, (int)(std::max(1.0, W - fx - pad) * PANGO_SCALE));
        pango_layout_set_ellipsize(footer_lay, PANGO_ELLIPSIZE_END);
        set_color(cr, engine::rates_state() == RATES_FAILED ? cfg.error : cfg.dim, 0.85);
        cairo_move_to(cr, fx, rows_bottom() + std::round(cfg.footer_margin_top * scale));
        pango_cairo_show_layout(cr, footer_lay);
    }

    // overlay buttons: just the icon on the window background; hover changes its color
    if (cfg.close_button) {
        CloseRect c = close_rect();
        bool hov = hover_btn == CLOSE_BTN || press_btn == CLOSE_BTN;
        double ccx = c.x + c.s / 2, ccy = c.y + c.s / 2, ck = c.s * 0.17;
        set_color(cr, hov ? cfg.error : cfg.dim, press_btn == CLOSE_BTN ? 0.7 : 1);
        cairo_set_line_width(cr, std::max(1.0, 1.2 * scale));
        cairo_set_line_cap(cr, CAIRO_LINE_CAP_ROUND);
        cairo_move_to(cr, ccx - ck, ccy - ck);
        cairo_line_to(cr, ccx + ck, ccy + ck);
        cairo_move_to(cr, ccx + ck, ccy - ck);
        cairo_line_to(cr, ccx - ck, ccy + ck);
        cairo_stroke(cr);
    }
    if (cfg.center_button) {  // a small target
        CloseRect c = center_rect();
        bool hov = hover_btn == CENTER_BTN || press_btn == CENTER_BTN;
        double ccx = c.x + c.s / 2, ccy = c.y + c.s / 2;
        set_color(cr, hov ? cfg.fg : cfg.dim, press_btn == CENTER_BTN ? 0.7 : 1);
        cairo_set_line_width(cr, std::max(1.0, 1.1 * scale));
        cairo_arc(cr, ccx, ccy, c.s * 0.22, 0, 2 * M_PI);
        cairo_stroke(cr);
        cairo_arc(cr, ccx, ccy, std::max(0.9, c.s * 0.07), 0, 2 * M_PI);
        cairo_fill(cr);
    }

    cairo_pop_group_to_source(cr);
    cairo_set_operator(cr, CAIRO_OPERATOR_SOURCE);
    cairo_paint(cr);
    cairo_destroy(cr);
    cairo_surface_flush(surf);
}

// ---------------------------------------------------------------- show / hide

static void set_wm_state() {
    Atom st[] = {A_NET_ABOVE, A_NET_SKIP_TASKBAR, A_NET_SKIP_PAGER, A_NET_STICKY};
    XChangeProperty(dpy, win, A_NET_STATE, XA_ATOM, 32, PropModeReplace, (unsigned char *)st, 4);
    long desk = 0xFFFFFFFF;
    XChangeProperty(dpy, win, A_NET_DESKTOP, XA_CARDINAL, 32, PropModeReplace, (unsigned char *)&desk, 1);
}

static void monitor_under_pointer(int &mx, int &my, int &mw, int &mh) {
    mx = 0, my = 0, mw = DisplayWidth(dpy, scr), mh = DisplayHeight(dpy, scr);
    Window rr, cr;
    int px = mw / 2, py = mh / 2, wx, wy;
    unsigned mask;
    XQueryPointer(dpy, root, &rr, &cr, &px, &py, &wx, &wy, &mask);
    int n = 0;
    XRRMonitorInfo *mons = XRRGetMonitors(dpy, root, True, &n);
    if (!mons) return;
    int pick = -1;
    for (int i = 0; i < n; i++) {
        if (px >= mons[i].x && px < mons[i].x + mons[i].width && py >= mons[i].y && py < mons[i].y + mons[i].height)
            pick = i;
        if (pick < 0 && mons[i].primary) pick = i;
    }
    if (pick < 0 && n > 0) pick = 0;
    if (pick >= 0) mx = mons[pick].x, my = mons[pick].y, mw = mons[pick].width, mh = mons[pick].height;
    XRRFreeMonitors(mons);
}

static void activate() {
    XEvent e{};
    e.xclient.type = ClientMessage;
    e.xclient.window = win;
    e.xclient.message_type = A_NET_ACTIVE;
    e.xclient.format = 32;
    e.xclient.data.l[0] = 2;  // source: pager, so focus-stealing prevention lets it through
    e.xclient.data.l[1] = CurrentTime;
    XSendEvent(dpy, root, False, SubstructureRedirectMask | SubstructureNotifyMask, &e);
    XRaiseWindow(dpy, win);
    XSetInputFocus(dpy, win, RevertToParent, CurrentTime);
}

static void show() {
    if (visible) {
        activate();
        return;
    }
    H = window_height();
    if (!user_pos) {
        int mx, my, mw, mh;
        monitor_under_pointer(mx, my, mw, mh);
        win_x = mx + (mw - W) / 2;
        win_y = my + (mh - H) / 2;
    }
    set_size_hints();
    set_wm_state();
    XMoveResizeWindow(dpy, win, win_x, win_y, W, H);
    if (surf) cairo_xlib_surface_set_size(surf, W, H);
    apply_shape();
    XMapRaised(dpy, win);
    visible = true;
    show_time = now_ms();
    blink_on = true;
    next_blink = show_time + 530;
    need_redraw = true;
}

// Forget a dragged position and center on the monitor under the pointer.
static void center_window() {
    user_pos = false;
    if (!visible) return;
    int mx, my, mw, mh;
    monitor_under_pointer(mx, my, mw, mh);
    win_x = mx + (mw - W) / 2;
    win_y = my + (mh - H) / 2;
    set_size_hints();
    XMoveWindow(dpy, win, win_x, win_y);
}

static void menu_close();

static void hide() {
    menu_close();
    if (!visible) return;
    XWithdrawWindow(dpy, win, scr);
    visible = mapped = has_focus = false;
    dragging = false;
    press_btn = hover_btn = -1;
    save_state(snapshot(), true);
    state_dirty = false;
}

// ---------------------------------------------------------------- input

// zone: 0 background (drags the window), 1 text, 2 +/x button, 3 close, 4 center
static void hit(int x, int y, int &row, int &zone) {
    row = -1;
    zone = 0;
    if (cfg.close_button) {
        CloseRect c = close_rect();
        if (x >= c.x - 3 && x <= c.x + c.s + 3 && y >= c.y - 3 && y <= c.y + c.s + 3) {
            zone = 3;
            return;
        }
    }
    if (cfg.center_button) {
        CloseRect c = center_rect();
        if (x >= c.x - 3 && x <= c.x + c.s + 3 && y >= c.y - 3 && y <= c.y + c.s + 3) {
            zone = 4;
            return;
        }
    }
    for (size_t i = 0; i < rows.size(); i++) {
        Geo g = geo(i);
        if (y < g.y - gap / 2 || y >= g.y + g.h + gap / 2) continue;
        row = (int)i;
        if (x >= g.bx - 2 && x <= g.bx + bsz + 2 && y >= g.by - 2 && y <= g.by + bsz + 2) zone = 2;
        else if (x >= g.x && x < g.x + g.w && y >= g.y && y < g.y + g.h) zone = 1;
        return;
    }
}

static void set_cursor_shape(Cursor c) {
    if (c == cur_current) return;
    XDefineCursor(dpy, win, c);
    cur_current = c;
}

static void clear_history() {
    history.clear();
    for (auto &r : rows) r.hist = -1;
    clear_history_file();
}

// ---------------------------------------------------------------- shortcuts

enum Action {
    ACT_HIDE, ACT_COMMIT, ACT_NEW_ROW, ACT_REMOVE_ROW, ACT_NEXT_ROW, ACT_PREV_ROW, ACT_HIST_PREV, ACT_HIST_NEXT,
    ACT_COPY, ACT_CUT, ACT_PASTE, ACT_UNDO, ACT_REDO, ACT_SELECT_ALL, ACT_DEL_START, ACT_DEL_END, ACT_RELOAD,
    ACT_CENTER, ACT_CLEAR_HISTORY, ACT_QUIT, ACT_UPDATE_RATES, ACT_COUNT
};

// Config name (key_<name>) and default shortcuts for each action.
static const struct {
    const char *name, *defaults;
} ACTIONS[ACT_COUNT] = {
    {"hide", "Escape"},
    {"commit", "Return, KP_Enter"},
    {"new_row", "ctrl+n, ctrl+Return, ctrl+KP_Enter"},
    {"remove_row", "ctrl+w"},
    {"next_row", "Tab, ctrl+Down, alt+Down"},
    {"prev_row", "shift+Tab, ctrl+Up, alt+Up"},
    {"history_prev", "Up"},
    {"history_next", "Down"},
    {"copy", "ctrl+c, ctrl+Insert"},
    {"cut", "ctrl+x, shift+Delete"},
    {"paste", "ctrl+v, shift+Insert"},
    {"undo", "ctrl+z"},
    {"redo", "ctrl+shift+z, ctrl+y"},
    {"select_all", "ctrl+a"},
    {"delete_to_start", "ctrl+u"},
    {"delete_to_end", "ctrl+k"},
    {"reload", "ctrl+shift+r"},
    {"center", "ctrl+shift+0"},
    {"clear_history", ""},
    {"quit", ""},
    {"update_rates", ""},
};

struct Binding {
    unsigned mods;
    KeySym ks;
    int action;
};
static std::vector<Binding> bindings;
static const unsigned MOD_MASK = ControlMask | ShiftMask | Mod1Mask | Mod4Mask;

// Same key regardless of case, keypad vs. main block, or Shift+Tab's special keysym.
static KeySym norm_keysym(KeySym k) {
    switch (k) {
    case XK_ISO_Left_Tab: return XK_Tab;
    case XK_KP_Up: return XK_Up;
    case XK_KP_Down: return XK_Down;
    case XK_KP_Left: return XK_Left;
    case XK_KP_Right: return XK_Right;
    case XK_KP_Home: return XK_Home;
    case XK_KP_End: return XK_End;
    case XK_KP_Delete: return XK_Delete;
    case XK_KP_Insert: return XK_Insert;
    case XK_KP_Page_Up: return XK_Page_Up;
    case XK_KP_Page_Down: return XK_Page_Down;
    }
    KeySym lo, up;
    XConvertCase(k, &lo, &up);
    return lo;
}

static KeySym parse_keysym(std::string name) {
    KeySym k = XStringToKeysym(name.c_str());
    if (k == NoSymbol && !name.empty()) {  // accept "escape", "return", "f5", ...
        std::string c = name;
        for (auto &ch : c) ch = (char)tolower((unsigned char)ch);
        c[0] = (char)toupper((unsigned char)c[0]);
        k = XStringToKeysym(c.c_str());
        if (k == NoSymbol) {
            for (auto &ch : c) ch = (char)toupper((unsigned char)ch);
            k = XStringToKeysym(c.c_str());
        }
    }
    return k;
}

// "ctrl+shift+r, F5" -> bindings for one action. Returns false if any part is invalid.
static bool parse_shortcuts(const std::string &spec, int action, std::vector<Binding> &out) {
    bool ok = true;
    size_t pos = 0;
    while (pos <= spec.size()) {
        size_t comma = spec.find(',', pos);
        if (comma == std::string::npos) comma = spec.size();
        std::string combo = trim(spec.substr(pos, comma - pos));
        pos = comma + 1;
        if (combo.empty()) continue;
        unsigned mods = 0;
        KeySym ks = NoSymbol;
        size_t p = 0;
        while (p <= combo.size()) {
            size_t plus = combo.find('+', p);
            if (plus == p && plus + 1 == combo.size()) plus = std::string::npos;  // "ctrl++" -> key '+'
            if (plus == std::string::npos) plus = combo.size();
            std::string tok = trim(combo.substr(p, plus - p));
            p = plus + 1;
            std::string low = tok;
            for (auto &ch : low) ch = (char)tolower((unsigned char)ch);
            if (low == "ctrl" || low == "control") mods |= ControlMask;
            else if (low == "shift") mods |= ShiftMask;
            else if (low == "alt" || low == "mod1") mods |= Mod1Mask;
            else if (low == "super" || low == "mod4" || low == "win" || low == "meta") mods |= Mod4Mask;
            else ks = parse_keysym(tok);
        }
        if (ks == NoSymbol) ok = false;
        else out.push_back({mods, norm_keysym(ks), action});
    }
    return ok;
}

static void build_bindings() {
    bindings.clear();
    for (int a = 0; a < ACT_COUNT; a++) {
        std::string spec = ACTIONS[a].defaults;
        for (auto &kv : cfg.keys)  // last setting for this action wins
            if (kv.first == ACTIONS[a].name) spec = kv.second;
        if (spec == "none") continue;
        if (!parse_shortcuts(spec, a, bindings) && getenv("QCALC_DEBUG"))
            fprintf(stderr, "qcalc: bad shortcut in key_%s = %s\n", ACTIONS[a].name, spec.c_str());
    }
    if (getenv("QCALC_DEBUG"))
        for (auto &kv : cfg.keys) {
            bool known = false;
            for (auto &d : ACTIONS) known |= kv.first == d.name;
            if (!known) fprintf(stderr, "qcalc: unknown action key_%s\n", kv.first.c_str());
        }
}

static void reload_config();

static void run_action(int a, Row &r) {
    size_t n = rows.size();
    switch (a) {
    case ACT_HIDE: hide(); break;
    case ACT_COMMIT: commit(r); break;
    case ACT_NEW_ROW: add_row(focus + 1); break;
    case ACT_REMOVE_ROW:  // on the last row it hides the window, like Esc
        if (rows.size() > 1) remove_row(focus);
        else hide();
        break;
    case ACT_NEXT_ROW: focus = (int)((focus + 1) % n); break;
    case ACT_PREV_ROW: focus = (int)((focus + n - 1) % n); break;
    case ACT_HIST_PREV: history_nav(r, -1); break;
    case ACT_HIST_NEXT: history_nav(r, 1); break;
    case ACT_COPY:
        if (has_sel(r)) own_selection(A_CLIPBOARD, sel_text(r));
        else copy_result(r);
        break;
    case ACT_CUT:
        if (has_sel(r)) {
            own_selection(A_CLIPBOARD, sel_text(r));
            delete_range(r, std::min(r.cur, r.anchor), std::max(r.cur, r.anchor));
        }
        break;
    case ACT_PASTE: request_paste(A_CLIPBOARD, r.uid); break;
    case ACT_UNDO: undo(r, false); break;
    case ACT_REDO: undo(r, true); break;
    case ACT_SELECT_ALL:
        r.anchor = 0;
        r.cur = r.text.size();
        break;
    case ACT_DEL_START: delete_range(r, 0, r.cur); break;
    case ACT_DEL_END: delete_range(r, r.cur, r.text.size()); break;
    case ACT_RELOAD: reload_config(); break;
    case ACT_CENTER: center_window(); break;
    case ACT_CLEAR_HISTORY: clear_history(); break;
    case ACT_QUIT: quit_flag = 1; break;
    case ACT_UPDATE_RATES:
        if (engine::rates_state() != RATES_UPDATING) engine::update_rates();
        break;
    }
}

// ---------------------------------------------------------------- context menu
// A small popup (override-redirect window) opened by right-clicking the background.

static const struct {
    const char *label;
    int action;
    bool sep_after;
    bool danger;
} MENU[] = {
    {"Close", ACT_HIDE, false, false},
    {"Center", ACT_CENTER, true, false},
    {"Reload config", ACT_RELOAD, false, false},
    {"Update exchange rates", ACT_UPDATE_RATES, false, false},
    {"Clear history", ACT_CLEAR_HISTORY, true, false},
    {"Quit qcalc", ACT_QUIT, false, true},
};
static const int MENU_N = sizeof MENU / sizeof MENU[0];

static Window menu_win = 0;
static cairo_surface_t *menu_surf;
static PangoFontDescription *menu_font;
static bool menu_open = false, menu_moved = false;
static int menu_x, menu_y, menu_w, menu_h, menu_hover = -1, menu_press = -1, menu_ox, menu_oy;
static double m_item_h, m_sep_h, m_padx, m_pady, m_text_h;

static std::string shortcut_hint(int action) {
    for (auto &b : bindings) {
        if (b.action != action) continue;
        std::string s;
        if (b.mods & ControlMask) s += "Ctrl+";
        if (b.mods & ShiftMask) s += "Shift+";
        if (b.mods & Mod1Mask) s += "Alt+";
        if (b.mods & Mod4Mask) s += "Super+";
        const char *k = XKeysymToString(b.ks);
        std::string key = k ? k : "?";
        if (key.size() == 1) key[0] = (char)toupper((unsigned char)key[0]);
        else if (key == "Escape") key = "Esc";
        else if (key == "Return") key = "Enter";
        return s + key;
    }
    return "";
}

static bool menu_disabled(int i) {
    return MENU[i].action == ACT_UPDATE_RATES && engine::rates_state() == RATES_UPDATING;
}

static const char *menu_label(int i) { return menu_disabled(i) ? "Updating exchange rates\u2026" : MENU[i].label; }

static double menu_item_y(int i) {
    double y = m_pady;
    for (int k = 0; k < i; k++) y += m_item_h + (MENU[k].sep_after ? m_sep_h : 0);
    return y;
}

static int menu_item_at(int x, int y) {
    if (x < 0 || x >= menu_w) return -1;
    for (int i = 0; i < MENU_N; i++) {
        double iy = menu_item_y(i);
        if (y >= iy && y < iy + m_item_h) return i;
    }
    return -1;
}

static PangoLayout *menu_layout(const std::string &text) {
    PangoLayout *l = pango_layout_new(pctx);
    pango_layout_set_font_description(l, menu_font);
    pango_layout_set_text(l, text.data(), (int)text.size());
    return l;
}

static void shape_rounded(Window w, int ww, int hh, double r) {
    if (!have_shape) return;
    if (r <= 0.5) {
        XShapeCombineMask(dpy, w, ShapeBounding, 0, 0, None, ShapeSet);
        return;
    }
    Pixmap pm = XCreatePixmap(dpy, w, ww, hh, 1);
    cairo_surface_t *ms = cairo_xlib_surface_create_for_bitmap(dpy, pm, ScreenOfDisplay(dpy, scr), ww, hh);
    cairo_t *cr = cairo_create(ms);
    cairo_set_operator(cr, CAIRO_OPERATOR_CLEAR);
    cairo_paint(cr);
    cairo_set_operator(cr, CAIRO_OPERATOR_OVER);
    cairo_set_antialias(cr, CAIRO_ANTIALIAS_NONE);
    cairo_set_source_rgba(cr, 1, 1, 1, 1);
    rounded(cr, 0, 0, ww, hh, r);
    cairo_fill(cr);
    cairo_destroy(cr);
    cairo_surface_destroy(ms);
    XShapeCombineMask(dpy, w, ShapeBounding, 0, 0, pm, ShapeSet);
    XFreePixmap(dpy, pm);
}

static void menu_redraw() {
    if (!menu_open || !menu_surf) return;
    cairo_t *cr = cairo_create(menu_surf);
    cairo_push_group(cr);
    double r = std::min(cfg.radius * scale, 8 * scale);
    set_color(cr, cfg.bg);
    cairo_paint(cr);
    rounded(cr, 0.5, 0.5, menu_w - 1, menu_h - 1, std::max(0.0, r - 0.5));
    set_color(cr, cfg.field);
    cairo_fill_preserve(cr);
    set_color(cr, cfg.border);
    cairo_set_line_width(cr, 1);
    cairo_stroke(cr);
    double inset = std::round(4 * scale);
    for (int i = 0; i < MENU_N; i++) {
        double y = menu_item_y(i);
        bool hov = i == menu_hover && !menu_disabled(i);
        if (hov) {
            rounded(cr, inset, y, menu_w - 2 * inset, m_item_h, std::min(r, m_item_h / 2) * 0.7);
            if (MENU[i].danger) set_color(cr, cfg.error, 0.22);
            else set_color(cr, cfg.sel);
            cairo_fill(cr);
        }
        double ty = y + std::round((m_item_h - m_text_h) / 2);
        PangoLayout *l = menu_layout(menu_label(i));
        if (menu_disabled(i)) set_color(cr, cfg.dim);
        else set_color(cr, MENU[i].danger && hov ? cfg.error : cfg.fg);
        cairo_move_to(cr, m_padx, ty);
        pango_cairo_show_layout(cr, l);
        g_object_unref(l);
        std::string hint = shortcut_hint(MENU[i].action);
        if (!hint.empty()) {
            PangoLayout *hl = menu_layout(hint);
            int hw, hh;
            pango_layout_get_pixel_size(hl, &hw, &hh);
            set_color(cr, cfg.dim);
            cairo_move_to(cr, menu_w - m_padx - hw, ty);
            pango_cairo_show_layout(cr, hl);
            g_object_unref(hl);
        }
        if (MENU[i].sep_after && i < MENU_N - 1) {
            double sy = std::round(y + m_item_h + m_sep_h / 2) + 0.5;
            set_color(cr, cfg.border);
            cairo_move_to(cr, inset * 2, sy);
            cairo_line_to(cr, menu_w - inset * 2, sy);
            cairo_stroke(cr);
        }
    }
    cairo_pop_group_to_source(cr);
    cairo_set_operator(cr, CAIRO_OPERATOR_SOURCE);
    cairo_paint(cr);
    cairo_destroy(cr);
    cairo_surface_flush(menu_surf);
}

static void menu_close() {
    if (!menu_open) return;
    menu_open = false;
    XUngrabPointer(dpy, CurrentTime);
    XUnmapWindow(dpy, menu_win);
    menu_hover = menu_press = -1;
}

static void menu_open_at(int rx, int ry) {
    if (!menu_win) {
        XSetWindowAttributes wa{};
        wa.override_redirect = True;
        wa.save_under = True;
        wa.background_pixel = cfg.field;
        wa.event_mask = ExposureMask | ButtonPressMask | ButtonReleaseMask | PointerMotionMask | LeaveWindowMask;
        menu_win = XCreateWindow(dpy, root, 0, 0, 10, 10, 0, CopyFromParent, InputOutput, visual,
                                 CWOverrideRedirect | CWSaveUnder | CWBackPixel | CWEventMask, &wa);
        Atom popup = XInternAtom(dpy, "_NET_WM_WINDOW_TYPE_POPUP_MENU", False);
        XChangeProperty(dpy, menu_win, A_NET_TYPE, XA_ATOM, 32, PropModeReplace, (unsigned char *)&popup, 1);
        menu_surf = cairo_xlib_surface_create(dpy, menu_win, visual, 10, 10);
    }
    XSetWindowBackground(dpy, menu_win, cfg.field);
    if (menu_font) pango_font_description_free(menu_font);
    menu_font = pango_font_description_copy(fdesc);
    pango_font_description_set_size(menu_font, (int)(pango_font_description_get_size(fdesc) * 0.92));

    // measure
    double maxw = 0;
    PangoLayout *l = menu_layout("Ag");
    int tw, th;
    pango_layout_get_pixel_size(l, &tw, &th);
    g_object_unref(l);
    m_text_h = th;
    m_padx = std::round(12 * scale);
    m_pady = std::round(4 * scale);
    m_item_h = std::round(th + 2 * 5 * scale);
    m_sep_h = std::round(7 * scale);
    for (int i = 0; i < MENU_N; i++) {
        int aw = 0, ah;
        for (const char *lab : {MENU[i].label, "Updating exchange rates\u2026"}) {  // widest label it can show
            if (lab != MENU[i].label && MENU[i].action != ACT_UPDATE_RATES) continue;
            PangoLayout *a = menu_layout(lab);
            int w2;
            pango_layout_get_pixel_size(a, &w2, &ah);
            aw = std::max(aw, w2);
            g_object_unref(a);
        }
        double w = aw;
        std::string hint = shortcut_hint(MENU[i].action);
        if (!hint.empty()) {
            PangoLayout *b = menu_layout(hint);
            int bw, bh;
            pango_layout_get_pixel_size(b, &bw, &bh);
            g_object_unref(b);
            w += std::round(28 * scale) + bw;
        }
        maxw = std::max(maxw, w);
    }
    menu_w = (int)std::max(std::round(150 * scale), maxw + 2 * m_padx);
    menu_h = (int)std::round(menu_item_y(MENU_N) + m_pady);

    // place at the pointer, flipped to stay on the monitor
    int mx = 0, my = 0, mw = DisplayWidth(dpy, scr), mh = DisplayHeight(dpy, scr);
    monitor_under_pointer(mx, my, mw, mh);
    menu_x = rx + 1;
    menu_y = ry + 1;
    if (menu_x + menu_w > mx + mw) menu_x = rx - menu_w;
    if (menu_y + menu_h > my + mh) menu_y = ry - menu_h;
    menu_x = std::max(mx, menu_x);
    menu_y = std::max(my, menu_y);
    menu_ox = rx;
    menu_oy = ry;

    XMoveResizeWindow(dpy, menu_win, menu_x, menu_y, menu_w, menu_h);
    cairo_xlib_surface_set_size(menu_surf, menu_w, menu_h);
    shape_rounded(menu_win, menu_w, menu_h, std::min(cfg.radius * scale, 8 * scale));
    XMapRaised(dpy, menu_win);
    menu_open = true;
    menu_moved = false;
    menu_hover = menu_press = -1;
    XGrabPointer(dpy, menu_win, False, ButtonPressMask | ButtonReleaseMask | PointerMotionMask, GrabModeAsync,
                 GrabModeAsync, None, cur_arrow, last_time);
}

static void menu_activate(int i) {
    menu_close();
    if (i < 0 || i >= MENU_N || menu_disabled(i)) return;
    Row &r = rows[std::min(focus, (int)rows.size() - 1)];
    run_action(MENU[i].action, r);
    need_redraw = true;
}

// Pointer events while the menu is open (the menu holds a pointer grab, so they all come here).
static void menu_pointer(XEvent &ev) {
    int rx = ev.type == MotionNotify ? ev.xmotion.x_root : ev.xbutton.x_root;
    int ry = ev.type == MotionNotify ? ev.xmotion.y_root : ev.xbutton.y_root;
    int idx = menu_item_at(rx - menu_x, ry - menu_y);
    if (ev.type == MotionNotify) {
        if (std::abs(rx - menu_ox) > 3 || std::abs(ry - menu_oy) > 3) menu_moved = true;
        if (idx != menu_hover) {
            menu_hover = idx;
            menu_redraw();
        }
    } else if (ev.type == ButtonPress) {
        bool inside = rx >= menu_x && rx < menu_x + menu_w && ry >= menu_y && ry < menu_y + menu_h;
        if (!inside) menu_close();
        else menu_press = idx;
    } else if (ev.type == ButtonRelease) {
        // Click on an item, or press-drag-release from the right-click that opened the menu.
        if (idx >= 0 && (idx == menu_press || (menu_press == -1 && menu_moved))) menu_activate(idx);
        menu_press = -1;
    }
}

static void menu_key(KeySym ks) {
    switch (norm_keysym(ks)) {
    case XK_Escape: menu_close(); break;
    case XK_Up: menu_hover = menu_hover <= 0 ? MENU_N - 1 : menu_hover - 1; menu_redraw(); break;
    case XK_Down: menu_hover = (menu_hover + 1) % MENU_N; menu_redraw(); break;
    case XK_Return:
    case XK_KP_Enter:
    case XK_space:
        if (menu_hover >= 0) menu_activate(menu_hover);
        break;
    }
}

static void on_key(XKeyEvent &ev) {
    char buf[128];
    KeySym ks = NoSymbol;
    Status st = 0;
    int len;
    if (xic) len = Xutf8LookupString(xic, &ev, buf, sizeof buf - 1, &ks, &st);
    else len = XLookupString(&ev, buf, sizeof buf - 1, &ks, nullptr);
    if (xic && st == XBufferOverflow) return;
    if (len < 0) len = 0;
    buf[len] = 0;
    if (menu_open) {
        menu_key(ks);
        return;
    }

    bool ctrl = ev.state & ControlMask, shift = ev.state & ShiftMask;
    if (rows.empty()) return;
    Row &r = rows[focus];
    blink_on = true;
    next_blink = now_ms() + 530;
    need_redraw = true;

    unsigned mods = ev.state & MOD_MASK;
    KeySym nk = norm_keysym(ks);
    KeySym base = norm_keysym(XLookupKeysym(&ev, 0));  // unshifted key: "ctrl+shift+0" arrives as ')'
    for (auto &b : bindings)
        if (b.ks == base && b.mods == mods && base != nk) {
            run_action(b.action, r);
            return;
        }
    if (getenv("QCALC_DEBUG")) fprintf(stderr, "key %s mods=0x%x\n", XKeysymToString(nk), mods);
    for (auto &b : bindings)
        if (b.ks == nk && b.mods == mods) {
            run_action(b.action, r);
            return;
        }

    // Cursor movement and deletion (fixed).
    switch (nk) {
    case XK_Left:
        if (has_sel(r) && !shift) move_cursor(r, std::min(r.cur, r.anchor), false);
        else move_cursor(r, ctrl ? word_left(r.text, r.cur) : u8_prev(r.text, r.cur), shift);
        return;
    case XK_Right:
        if (has_sel(r) && !shift) move_cursor(r, std::max(r.cur, r.anchor), false);
        else move_cursor(r, ctrl ? word_right(r.text, r.cur) : u8_next(r.text, r.cur), shift);
        return;
    case XK_Home: move_cursor(r, 0, shift); return;
    case XK_End: move_cursor(r, r.text.size(), shift); return;
    case XK_BackSpace:
        if (has_sel(r)) delete_range(r, std::min(r.cur, r.anchor), std::max(r.cur, r.anchor));
        else delete_range(r, ctrl ? word_left(r.text, r.cur) : u8_prev(r.text, r.cur), r.cur);
        return;
    case XK_Delete:
        if (has_sel(r)) delete_range(r, std::min(r.cur, r.anchor), std::max(r.cur, r.anchor));
        else delete_range(r, r.cur, ctrl ? word_right(r.text, r.cur) : u8_next(r.text, r.cur));
        return;
    }

    if (ctrl || (ev.state & Mod4Mask)) return;  // unbound shortcut: don't type it
    if (len > 0 && (unsigned char)buf[0] >= 0x20 && buf[0] != 0x7f) insert_text(r, buf, EK_TYPE);
}

static void on_button_press(XButtonEvent &e) {
    int ri, zone;
    hit(e.x, e.y, ri, zone);
    if (e.button == 1 && (zone == 3 || zone == 4)) {
        press_btn = zone == 3 ? CLOSE_BTN : CENTER_BTN;
        need_redraw = true;
        return;
    }
    if (e.button == 3 && zone == 0 && cfg.context_menu) {
        menu_open_at(e.x_root, e.y_root);
        return;
    }
    if (e.button == 1 && zone == 0) {  // background: start moving the window
        Window child;
        XTranslateCoordinates(dpy, win, root, 0, 0, &drag_win_x, &drag_win_y, &child);
        drag_root_x = e.x_root;
        drag_root_y = e.y_root;
        win_dragging = true;
        set_cursor_shape(cur_move);
        return;
    }
    if (ri < 0) return;
    need_redraw = true;
    blink_on = true;
    next_blink = now_ms() + 530;
    if (e.button == 1 && zone == 2) {
        press_btn = ri;
        return;
    }
    if (zone != 1) return;
    focus = ri;
    Row &r = rows[ri];
    size_t idx = index_at(r, ri, e.x);
    if (e.button == 1) {
        double t = now_ms();
        if (ri == last_click_row && t - last_click_t < 400 && std::fabs(e.x - last_click_x) < 5)
            click_count = click_count % 3 + 1;
        else click_count = 1;
        last_click_t = t;
        last_click_x = e.x;
        last_click_row = ri;
        if (click_count == 1) {
            r.cur = idx;
            if (!(e.state & ShiftMask)) r.anchor = idx;
            dragging = true;
        } else if (click_count == 2) {
            size_t a = idx, b = idx;
            size_t probe = idx < r.text.size() ? idx : u8_prev(r.text, idx);
            if (!r.text.empty()) {
                int c = char_class(r.text, probe);
                a = probe;
                b = u8_next(r.text, probe);
                while (a > 0 && char_class(r.text, u8_prev(r.text, a)) == c) a = u8_prev(r.text, a);
                while (b < r.text.size() && char_class(r.text, b) == c) b = u8_next(r.text, b);
            }
            r.anchor = a;
            r.cur = b;
        } else {
            r.anchor = 0;
            r.cur = r.text.size();
        }
        r.last_kind = EK_NONE;
    } else if (e.button == 2) {
        r.cur = r.anchor = idx;
        request_paste(XA_PRIMARY, r.uid);
    }
}

static void on_motion(XMotionEvent &e) {
    if (win_dragging) {  // applied once per event batch, see main loop
        win_x = drag_win_x + (e.x_root - drag_root_x);
        win_y = drag_win_y + (e.y_root - drag_root_y);
        move_pending = true;
        return;
    }
    int ri, zone;
    hit(e.x, e.y, ri, zone);
    int hb = zone == 3 ? CLOSE_BTN : zone == 4 ? CENTER_BTN : zone == 2 ? ri : -1;
    if (hb != hover_btn) {
        hover_btn = hb;
        need_redraw = true;
    }
    set_cursor_shape(zone >= 2 ? cur_hand : zone == 1 ? cur_text : cur_move);
    if (dragging && focus < (int)rows.size()) {
        Row &r = rows[focus];
        size_t idx = index_at(r, focus, e.x);
        if (idx != r.cur) {
            r.cur = idx;
            need_redraw = true;
        }
    }
}

static void on_button_release(XButtonEvent &e) {
    if (e.button != 1) return;
    dragging = false;
    if (win_dragging) {
        win_dragging = false;
        if (win_x != drag_win_x || win_y != drag_win_y) user_pos = true;
        return;
    }
    if (press_btn != -1) {
        int ri, zone;
        hit(e.x, e.y, ri, zone);
        int b = press_btn;
        press_btn = -1;
        need_redraw = true;
        if (b == CLOSE_BTN) {
            if (zone == 3) hide();
        } else if (b == CENTER_BTN) {
            if (zone == 4) center_window();
        } else if (ri == b && zone == 2) {
            if (b == 0) add_row(rows.size());
            else remove_row(b);
        }
        return;
    }
    if (focus < (int)rows.size() && has_sel(rows[focus])) own_selection(XA_PRIMARY, sel_text(rows[focus]));
}

static void on_selection_notify(XSelectionEvent &e) {
    if (e.property != A_QC_SEL || paste_uid < 0) {
        if (e.property == None && e.target == A_UTF8 && !paste_tried_string && paste_uid >= 0) {
            paste_tried_string = true;  // owner does not speak UTF8_STRING: try plain STRING
            XConvertSelection(dpy, e.selection, XA_STRING, A_QC_SEL, win, e.time);
        } else paste_uid = -1;
        return;
    }
    Atom type;
    int fmt;
    unsigned long n, after;
    unsigned char *data = nullptr;
    if (XGetWindowProperty(dpy, win, A_QC_SEL, 0, 1 << 20, True, AnyPropertyType, &type, &fmt, &n, &after, &data) ==
            Success &&
        data && fmt == 8) {
        Row *r = row_by_uid(paste_uid);
        if (r) insert_text(*r, std::string((char *)data, n), EK_OTHER);
    }
    if (data) XFree(data);
    paste_uid = -1;
}

// ---------------------------------------------------------------- setup

static int x_error(Display *, XErrorEvent *e) {
    if (getenv("QCALC_DEBUG")) fprintf(stderr, "X error %d req %d.%d res 0x%lx\n", e->error_code, e->request_code, e->minor_code, e->resourceid);
    return 0;
}

static int x_io_error(Display *) {
    if (getenv("QCALC_DEBUG")) fprintf(stderr, "exit: X connection lost\n");
    save_state(snapshot(), true);
    _exit(0);
}

static volatile sig_atomic_t last_signal = 0;
static void on_signal(int sig) {
    last_signal = sig;
    quit_flag = 1;
}

static void read_xresources(cairo_font_options_t *fo) {
    char *rm = XResourceManagerString(dpy);
    if (!rm) return;
    XrmInitialize();
    XrmDatabase db = XrmGetStringDatabase(rm);
    if (!db) return;
    char *type;
    XrmValue v;
    if (XrmGetResource(db, "Xft.dpi", "Xft.Dpi", &type, &v) && v.addr) {
        double d = atof(v.addr);
        if (d > 30 && d < 1000) dpi = d;
    }
    if (XrmGetResource(db, "Xft.antialias", "Xft.Antialias", &type, &v) && v.addr)
        cairo_font_options_set_antialias(fo, atoi(v.addr) ? CAIRO_ANTIALIAS_GRAY : CAIRO_ANTIALIAS_NONE);
    if (XrmGetResource(db, "Xft.rgba", "Xft.Rgba", &type, &v) && v.addr) {
        std::string s = v.addr;
        if (s == "rgb" || s == "bgr" || s == "vrgb" || s == "vbgr") {
            cairo_font_options_set_antialias(fo, CAIRO_ANTIALIAS_SUBPIXEL);
            cairo_font_options_set_subpixel_order(fo, s == "rgb"    ? CAIRO_SUBPIXEL_ORDER_RGB
                                                      : s == "bgr"  ? CAIRO_SUBPIXEL_ORDER_BGR
                                                      : s == "vrgb" ? CAIRO_SUBPIXEL_ORDER_VRGB
                                                                    : CAIRO_SUBPIXEL_ORDER_VBGR);
        }
    }
    if (XrmGetResource(db, "Xft.hintstyle", "Xft.Hintstyle", &type, &v) && v.addr) {
        std::string s = v.addr;
        cairo_font_options_set_hint_style(fo, s == "hintnone"     ? CAIRO_HINT_STYLE_NONE
                                              : s == "hintslight" ? CAIRO_HINT_STYLE_SLIGHT
                                              : s == "hintmedium" ? CAIRO_HINT_STYLE_MEDIUM
                                              : s == "hintfull"   ? CAIRO_HINT_STYLE_FULL
                                                                  : CAIRO_HINT_STYLE_DEFAULT);
    }
    XrmDestroyDatabase(db);
}

static void setup_fonts() {
    if (fdesc) pango_font_description_free(fdesc);
    fdesc = pango_font_description_from_string(cfg.font.c_str());
    if (pango_font_description_get_size(fdesc) <= 0) pango_font_description_set_size(fdesc, 15 * PANGO_SCALE);
    for (auto &r : rows) free_row(r);
    if (placeholder) g_object_unref(placeholder);
    placeholder = pango_layout_new(pctx);
    pango_layout_set_font_description(placeholder, fdesc);
    pango_layout_set_text(placeholder, "Calculate\u2026", -1);
    compute_metrics();
    if (footer_lay) g_object_unref(footer_lay);
    footer_lay = pango_layout_new(pctx);
    PangoFontDescription *ff = pango_font_description_copy(fdesc);
    pango_font_description_set_size(ff, (int)(pango_font_description_get_size(fdesc) * 0.72));
    pango_layout_set_font_description(footer_lay, ff);
    pango_font_description_free(ff);
    pango_layout_set_text(footer_lay, "Ag", -1);
    int fw, fh;
    pango_layout_get_pixel_size(footer_lay, &fw, &fh);
    footer_text_h = fh;
    footer_h = fh + std::round(cfg.footer_margin_top * scale);
}

static void setup_x() {
    dpy = XOpenDisplay(nullptr);
    if (!dpy) {
        fprintf(stderr, "qcalcd: cannot open display\n");
        exit(1);
    }
    XSetErrorHandler(x_error);
    XSetIOErrorHandler(x_io_error);
    scr = DefaultScreen(dpy);
    root = RootWindow(dpy, scr);
    visual = DefaultVisual(dpy, scr);

#define ATOM(v, n) v = XInternAtom(dpy, n, False)
    ATOM(A_WM_PROTOCOLS, "WM_PROTOCOLS");
    ATOM(A_WM_DELETE, "WM_DELETE_WINDOW");
    ATOM(A_NET_STATE, "_NET_WM_STATE");
    ATOM(A_NET_ABOVE, "_NET_WM_STATE_ABOVE");
    ATOM(A_NET_SKIP_TASKBAR, "_NET_WM_STATE_SKIP_TASKBAR");
    ATOM(A_NET_SKIP_PAGER, "_NET_WM_STATE_SKIP_PAGER");
    ATOM(A_NET_STICKY, "_NET_WM_STATE_STICKY");
    ATOM(A_NET_TYPE, "_NET_WM_WINDOW_TYPE");
    ATOM(A_NET_TYPE_DIALOG, "_NET_WM_WINDOW_TYPE_DIALOG");
    ATOM(A_NET_ACTIVE, "_NET_ACTIVE_WINDOW");
    ATOM(A_NET_NAME, "_NET_WM_NAME");
    ATOM(A_UTF8, "UTF8_STRING");
    ATOM(A_MOTIF, "_MOTIF_WM_HINTS");
    ATOM(A_CLIPBOARD, "CLIPBOARD");
    ATOM(A_TARGETS, "TARGETS");
    ATOM(A_TEXT, "TEXT");
    ATOM(A_QC_SEL, "QCALC_SELECTION");
    ATOM(A_NET_PID, "_NET_WM_PID");
    ATOM(A_NET_DESKTOP, "_NET_WM_DESKTOP");
#undef ATOM

    cairo_font_options_t *fo = cairo_font_options_create();
    read_xresources(fo);
    PangoFontMap *fm = pango_cairo_font_map_get_default();
    pctx = pango_font_map_create_context(fm);
    pango_cairo_context_set_resolution(pctx, dpi);
    pango_cairo_context_set_font_options(pctx, fo);
    cairo_font_options_destroy(fo);
    setup_fonts();
    H = window_height();

    XSetWindowAttributes wa{};
    wa.background_pixel = cfg.bg;
    wa.border_pixel = 0;
    wa.event_mask = KeyPressMask | ButtonPressMask | ButtonReleaseMask | PointerMotionMask | ExposureMask |
                    StructureNotifyMask | FocusChangeMask | LeaveWindowMask;
    win = XCreateWindow(dpy, root, 0, 0, W, H, 0, CopyFromParent, InputOutput, visual,
                        CWBackPixel | CWBorderPixel | CWEventMask, &wa);

    XClassHint ch{(char *)"qcalc", (char *)"QCalc"};
    XWMHints wmh{};
    wmh.flags = InputHint | StateHint;
    wmh.input = True;
    wmh.initial_state = NormalState;
    XSetWMProperties(dpy, win, nullptr, nullptr, nullptr, 0, nullptr, &wmh, &ch);
    XStoreName(dpy, win, "qcalc");
    XChangeProperty(dpy, win, A_NET_NAME, A_UTF8, 8, PropModeReplace, (const unsigned char *)"qcalc", 5);
    XSetWMProtocols(dpy, win, &A_WM_DELETE, 1);
    XChangeProperty(dpy, win, A_NET_TYPE, XA_ATOM, 32, PropModeReplace, (unsigned char *)&A_NET_TYPE_DIALOG, 1);
    long motif[5] = {2, 0, 0, 0, 0};  // flags = decorations, decorations = none
    XChangeProperty(dpy, win, A_MOTIF, A_MOTIF, 32, PropModeReplace, (unsigned char *)motif, 5);
    long pid = getpid();
    XChangeProperty(dpy, win, A_NET_PID, XA_CARDINAL, 32, PropModeReplace, (unsigned char *)&pid, 1);

    cur_text = XCreateFontCursor(dpy, XC_xterm);
    cur_arrow = XCreateFontCursor(dpy, XC_left_ptr);
    cur_hand = XCreateFontCursor(dpy, XC_hand2);
    cur_move = XCreateFontCursor(dpy, XC_fleur);
    set_cursor_shape(cur_text);

    // Input method, for dead keys and compose.
    XSetLocaleModifiers("");
    xim = XOpenIM(dpy, nullptr, nullptr, nullptr);
    if (!xim) {
        XSetLocaleModifiers("@im=none");
        xim = XOpenIM(dpy, nullptr, nullptr, nullptr);
    }
    if (xim)
        xic = XCreateIC(xim, XNInputStyle, XIMPreeditNothing | XIMStatusNothing, XNClientWindow, win, XNFocusWindow,
                        win, nullptr);
    if (xic) {
        long fev = 0;
        XGetICValues(xic, XNFilterEvents, &fev, nullptr);
        XSelectInput(dpy, win, wa.event_mask | fev);
    }

    int sev, serr;
    have_shape = XShapeQueryExtension(dpy, &sev, &serr);
    surf = cairo_xlib_surface_create(dpy, win, visual, W, H);
}

static void restore_state() {
    SavedState s;
    if (load_state(s))
        for (auto &sr : s.rows) {
            Row r;
            r.uid = next_uid++;
            r.text = sanitize(sr.text);
            r.cur = r.anchor = u8_fix(r.text, std::min(sr.cursor, r.text.size()));
            rows.push_back(std::move(r));
        }
    if (rows.empty()) {
        Row r;
        r.uid = next_uid++;
        rows.push_back(std::move(r));
    }
    focus = std::clamp(s.focus, 0, (int)rows.size() - 1);
    // Assignments confirmed with Enter (so they are in history) run again, in row order before the rest.
    for (auto &r : rows)
        submit_row(r, std::find(history.begin(), history.end(), trim(r.text)) != history.end());
}

static void reload_config() {
    int cx = win_x + W / 2, cy = win_y + H / 2;
    cfg = load_config();
    build_bindings();
    engine::set_rates_max_age(cfg.exchange_rates_max_age);
    setup_fonts();
    XSetWindowBackground(dpy, win, cfg.bg);
    H = window_height();
    if (visible) {  // keep it centered where it was
        win_x = cx - W / 2;
        win_y = cy - H / 2;
        set_size_hints();
        XMoveResizeWindow(dpy, win, win_x, win_y, W, H);
    }
    if (surf) cairo_xlib_surface_set_size(surf, W, H);
    apply_shape();
    need_redraw = true;
}

static void handle_command(const std::string &c) {
    if (c == "show") show();
    else if (c == "hide") hide();
    else if (c == "toggle") {
        if (visible && has_focus) hide();
        else show();
    } else if (c == "quit") quit_flag = 1;
    else if (c == "reload") reload_config();
    else if (c == "clear-history") clear_history();
    else if (c == "update-rates") engine::update_rates();
}

int main(int argc, char **argv) {
    bool start_shown = false;
    for (int i = 1; i < argc; i++) {
        std::string a = argv[i];
        if (a == "--show") start_shown = true;
        else if (a == "-h" || a == "--help") {
            puts("usage: qcalcd [--show]\nThe qcalc background process. Use the `qcalc` command to show/hide it.");
            return 0;
        }
    }

    // One daemon per user. If one is already running, hand the command over.
    int sock = socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
    sockaddr_un addr;
    socklen_t alen = qc_sock_addr(&addr);
    if (sock < 0 || bind(sock, (sockaddr *)&addr, alen) != 0) {
        if (start_shown) {
            int c = socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0);
            sendto(c, "show", 4, 0, (sockaddr *)&addr, alen);
        }
        return 0;
    }

    setlocale(LC_ALL, "");
    signal(SIGPIPE, SIG_IGN);
    struct sigaction sa {};
    sa.sa_handler = on_signal;
    sigaction(SIGTERM, &sa, nullptr);
    sigaction(SIGINT, &sa, nullptr);
    sigaction(SIGHUP, &sa, nullptr);

    cfg = load_config();
    build_bindings();
    history = load_history(cfg.max_history);
    setup_x();
    engine::start(cfg.timeout_ms, cfg.angle, cfg.fractions, cfg.exchange_rates_max_age);
    restore_state();
    if (start_shown) show();
    XFlush(dpy);

    int xfd = ConnectionNumber(dpy);
    while (!quit_flag) {
        while (XPending(dpy)) {
            XEvent ev;
            XNextEvent(dpy, &ev);
            if (XFilterEvent(&ev, None)) continue;
            if (menu_win && ev.xany.window == menu_win && ev.type == Expose) {
                if (ev.xexpose.count == 0) menu_redraw();
                continue;
            }
            if (menu_open && (ev.type == MotionNotify || ev.type == ButtonPress || ev.type == ButtonRelease)) {
                menu_pointer(ev);
                continue;
            }
            switch (ev.type) {
            case Expose:
                if (ev.xexpose.count == 0) need_redraw = true;
                break;
            case MapNotify:
                mapped = true;
                need_redraw = true;
                if (visible) activate();
                break;
            case UnmapNotify: mapped = false; break;
            case ConfigureNotify:
                if (ev.xconfigure.width != W || ev.xconfigure.height != H) need_redraw = true;
                break;
            case FocusIn:
                if (ev.xfocus.mode == NotifyGrab || ev.xfocus.mode == NotifyUngrab) break;
                has_focus = true;
                if (xic) XSetICFocus(xic);
                blink_on = true;
                next_blink = now_ms() + 530;
                need_redraw = true;
                break;
            case FocusOut:
                if (ev.xfocus.mode == NotifyGrab || ev.xfocus.detail == NotifyInferior ||
                    ev.xfocus.detail == NotifyPointer)
                    break;
                has_focus = false;
                menu_close();
                if (xic) XUnsetICFocus(xic);
                need_redraw = true;
                if (cfg.hide_on_focus_loss && visible && now_ms() - show_time > 250) hide();
                break;
            case KeyPress:
                last_time = ev.xkey.time;
                on_key(ev.xkey);
                break;
            case ButtonPress:
                last_time = ev.xbutton.time;
                on_button_press(ev.xbutton);
                break;
            case ButtonRelease:
                last_time = ev.xbutton.time;
                on_button_release(ev.xbutton);
                break;
            case MotionNotify: on_motion(ev.xmotion); break;
            case LeaveNotify:
                if (hover_btn != -1) {
                    hover_btn = -1;
                    need_redraw = true;
                }
                break;
            case ClientMessage:
                if (ev.xclient.message_type == A_WM_PROTOCOLS && (Atom)ev.xclient.data.l[0] == A_WM_DELETE) hide();
                break;
            case SelectionRequest: handle_selection_request(ev.xselectionrequest); break;
            case SelectionClear: break;
            case SelectionNotify: on_selection_notify(ev.xselection); break;
            case MappingNotify: XRefreshKeyboardMapping(&ev.xmapping); break;
            }
        }

        if (move_pending) {
            move_pending = false;
            set_size_hints();
            XMoveWindow(dpy, win, win_x, win_y);
        }
        if (state_dirty) {
            save_state(snapshot(), false);
            state_dirty = false;
        }

        double t = now_ms();
        if (has_focus && visible && t >= next_blink) {
            blink_on = !blink_on;
            next_blink = t + 530;
            need_redraw = true;
        }
        if (flash_uid >= 0 && t >= flash_until) {
            flash_uid = -1;
            need_redraw = true;
        }
        if (visible && window_height() != H) apply_size();  // footer appeared or went away
        static double next_footer_tick = 0;
        bool footer_on = visible && want_footer();
        if (footer_on && t >= next_footer_tick) {  // keep "... ago" current
            next_footer_tick = t + 30000;
            need_redraw = true;
        }
        if (need_redraw && visible && mapped) redraw();
        XFlush(dpy);

        int timeout = -1;
        if (has_focus && visible) timeout = std::max(0, (int)(next_blink - t) + 1);
        if (footer_on) timeout = timeout < 0 ? 30000 : std::min(timeout, 30000);
        if (flash_uid >= 0) {
            int ft = std::max(0, (int)(flash_until - t) + 1);
            timeout = timeout < 0 ? ft : std::min(timeout, ft);
        }

        pollfd fds[3] = {{xfd, POLLIN, 0}, {sock, POLLIN, 0}, {engine::fd(), POLLIN, 0}};
        int n = poll(fds, 3, timeout);
        if (n < 0 && errno != EINTR) {
            if (getenv("QCALC_DEBUG")) perror("exit: poll");
            break;
        }

        if (fds[1].revents & POLLIN) {
            char buf[64];
            ssize_t k;
            while ((k = recv(sock, buf, sizeof buf - 1, 0)) > 0) handle_command(std::string(buf, (size_t)k));
        }
        if (fds[2].revents & POLLIN) {
            for (auto &res : engine::take()) {
                if (res.uid == ENGINE_RATES_RELOADED) {  // new rates: recalculate rows that use them
                    for (auto &row : rows)
                        if (row.res_currency && !is_assignment(row.text)) {
                            row.gen++;
                            submit_row(row, false);
                        }
                    need_redraw = true;
                    if (menu_open) menu_redraw();
                    continue;
                }
                if (res.uid == ENGINE_RATES_STATE) {
                    need_redraw = true;
                    if (menu_open) menu_redraw();
                    continue;
                }
                Row *r = row_by_uid(res.uid);
                if (!r || res.gen != r->gen) continue;
                r->res = res.text;
                r->res_copy = res.copy;
                r->res_approx = res.approx;
                r->res_err = res.error;
                r->res_currency = res.currency;
                r->res_gen = res.gen;
                r->has_res = true;
                need_redraw = true;
                if (copy_pending_uid == r->uid) {
                    if (copy_pending_gen == r->gen) copy_result(*r);
                    copy_pending_uid = -1;
                }
            }
        }
    }

    if (getenv("QCALC_DEBUG")) fprintf(stderr, "exit: quit (signal %d)\n", (int)last_signal);
    save_state(snapshot(), true);
    _exit(0);  // skip libqalculate teardown: nothing left to flush
}
