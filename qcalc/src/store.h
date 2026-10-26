// Config, saved input and history on disk.
#pragma once
#include <cstdint>
#include <string>
#include <vector>

struct Config {
    std::string font = "Sans 15";
    int width = 560;                 // logical px at 96 dpi
    bool hide_on_focus_loss = false;
    bool close_button = true;        // small x at the top right that hides the window
    bool context_menu = true;        // right-click on the background
    bool center_button = true;       // small button left of it that re-centers the window
    bool copy_on_enter = true;
    int max_history = 1000;
    int timeout_ms = 2000;           // per calculation
    bool fractions = false;          // also show exact fractions, e.g. "1/3 ≈ 0.333"
    double exchange_rates_max_age = 1; // days; refresh rates in the background when older (0 = only manually)
    std::string temperature = "relative"; // relative | hybrid | absolute: how °C/°F behave in sums
    std::string angle = "";          // "", "rad", "deg", "gra" ("" = qalculate's own setting)
    // Layout, in px at 96 dpi (scaled with Xft.dpi)
    double radius = 6;               // input box corner radius
    double window_radius = 0;        // window corner radius
    double window_padding = 6;       // window edge <-> input boxes
    double row_gap = 6;              // between input boxes
    double box_padding_x = 12;       // inside an input box, left/right
    double box_padding_y = 9;        // inside an input box, top/bottom
    double border_width = 1;         // input box border (0 = none)
    double window_border = 1;        // window border (0 = none)
    double footer_margin_left = 0;   // exchange-rate footer: from the input box edge
    double footer_margin_top = 5;    // exchange-rate footer: space above it
    // Shortcuts: action -> "ctrl+shift+r, F5" (raw text, parsed by the UI). "none" unbinds.
    std::vector<std::pair<std::string, std::string>> keys;
    uint32_t bg = 0x1b1d23, field = 0x262931, border = 0x3a3f4b, fg = 0xe8e8ea, dim = 0x8b919c,
             accent = 0x6ea8ff, sel = 0x2f4f7f, error = 0xe0707a;
};

struct SavedRow {
    std::string text;
    size_t cursor = 0;
};

struct SavedState {
    std::vector<SavedRow> rows;
    int focus = 0;
};

std::string config_path();
std::string state_dir();
Config load_config();
bool load_state(SavedState &s);
bool save_state(const SavedState &s, bool sync);  // atomic: tmp file + rename
std::vector<std::string> load_history(int max);
void append_history(const std::string &line);
void clear_history_file();
