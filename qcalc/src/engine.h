// libqalculate on a worker thread. The UI submits expressions and polls fd() for results.
#pragma once
#include <cstdint>
#include <ctime>
#include <string>
#include <vector>

// Special uids in results: events rather than calculations.
enum { ENGINE_RATES_RELOADED = -1, ENGINE_RATES_STATE = -2 };

struct CalcResult {
    int uid;
    uint64_t gen;
    std::string text;  // as displayed, e.g. "16 ft + 4.85 in" or "1∕2 = 0.5"
    std::string copy;  // the final value in plain ASCII operators, for the clipboard
    bool approx = false;
    bool error = false;
    bool currency = false;  // used exchange rates
};

enum RatesState { RATES_IDLE, RATES_UPDATING, RATES_FAILED };

namespace engine {
void start(int timeout_ms, const std::string &angle, bool show_fractions, double rates_max_age_days,
           const std::string &temperature);
void submit(int uid, uint64_t gen, const std::string &expr);  // replaces any queued request for uid
void cancel(int uid);
int fd();                          // readable when results are waiting
std::vector<CalcResult> take();

// Exchange rates: served from qalculate's cache; refreshed in the background when a calculation uses
// them and the cache is older than the max age (0 = never automatically), or when asked.
void update_rates();
void set_rates_max_age(double days);
time_t rates_time();   // when the cached rates were fetched (0 = unknown)
RatesState rates_state();
}  // namespace engine
