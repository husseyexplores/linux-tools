#include "engine.h"

#include <libqalculate/qalculate.h>

#include <atomic>
#include <cctype>
#include <chrono>
#include <condition_variable>
#include <fcntl.h>
#include <map>
#include <mutex>
#include <thread>
#include <unistd.h>

namespace {
std::mutex mtx;
std::condition_variable cv;
std::map<int, std::pair<uint64_t, std::string>> pending;  // latest request per row
std::vector<CalcResult> done;
int pipefd[2] = {-1, -1};
int busy_uid = -1;
uint64_t busy_gen = 0;
bool calc_ready = false;
int timeout = 2000;
std::string angle_cfg, temp_cfg;
bool local_currency = false;
bool fractions = false;

// exchange rates
std::atomic<long long> g_rates_time{0};
std::atomic<int> g_rates_state{RATES_IDLE};
std::atomic<double> max_age_days{1};
bool fetch_running = false, reload_pending = false, update_requested = false;
double last_auto_try = -1e18;  // steady-clock seconds

double steady_s() {
    return std::chrono::duration<double>(std::chrono::steady_clock::now().time_since_epoch()).count();
}

void wake() {
    char c = 1;
    (void)!write(pipefd[1], &c, 1);
}

// libqalculate applies "to" only to a whole expression: "(1 usd to jpy) * 10" gives a wrong result.
// Rewrite conversions inside parentheses into one at the end: "(1 usd) * 10 to jpy". Left as is when
// the expression already ends with its own "to", or when the groups convert to different targets.
bool word_char(unsigned char c) { return isalnum(c) || c == '_' || c >= 0x80; }

// Length of a conversion operator ("to", "->", "→") at i, or 0.
size_t to_op_at(const std::string &s, size_t i) {
    if (s.compare(i, 2, "->") == 0) return 2;
    if (s.compare(i, 3, "\xE2\x86\x92") == 0) return 3;  // →
    if (i + 2 <= s.size() && (s[i] == 't' || s[i] == 'T') && (s[i + 1] == 'o' || s[i + 1] == 'O') &&
        (i == 0 || !word_char(s[i - 1])) && (i + 2 == s.size() || !word_char(s[i + 2])))
        return 2;
    return 0;
}

// First conversion operator at depth 0 of s[from, to), or npos.
size_t find_top_to(const std::string &s, size_t from, size_t to, size_t *len) {
    int depth = 0;
    for (size_t i = from; i < to; i++) {
        char c = s[i];
        if (c == '"' || c == '\'') {  // skip quoted text
            size_t q = s.find(c, i + 1);
            if (q == std::string::npos || q >= to) return std::string::npos;
            i = q;
            continue;
        }
        if (c == '(' || c == '[') depth++;
        else if (c == ')' || c == ']') depth--;
        else if (depth == 0) {
            size_t l = to_op_at(s, i);
            if (l) {
                *len = l;
                return i;
            }
        }
    }
    return std::string::npos;
}

std::string hoist_conversions(const std::string &expr) {
    if (expr.find("to") == std::string::npos && expr.find("TO") == std::string::npos &&
        expr.find("->") == std::string::npos && expr.find("\xE2\x86\x92") == std::string::npos)
        return expr;
    std::string s = expr, target;
    bool changed = false;
    for (int guard = 0; guard < 64; guard++) {
        // find a "( ... to X )" group that is not a function call
        bool found = false;
        std::vector<size_t> open;
        for (size_t i = 0; i < s.size() && !found; i++) {
            if (s[i] == '"' || s[i] == '\'') {
                size_t q = s.find(s[i], i + 1);
                if (q == std::string::npos) break;
                i = q;
            } else if (s[i] == '(') open.push_back(i);
            else if (s[i] == ')' && !open.empty()) {
                size_t o = open.back();
                open.pop_back();
                size_t k = o;
                while (k > 0 && s[k - 1] == ' ') k--;
                if (k > 0 && word_char((unsigned char)s[k - 1])) continue;  // f(...)
                size_t len = 0, p = find_top_to(s, o + 1, i, &len);
                if (p == std::string::npos) continue;
                std::string t = s.substr(p + len, i - p - len);
                size_t a = t.find_first_not_of(' '), b = t.find_last_not_of(' ');
                t = a == std::string::npos ? "" : t.substr(a, b - a + 1);
                if (t.empty()) return expr;
                if (!target.empty() && target != t) return expr;  // different targets: leave it
                target = t;
                size_t e = p;
                while (e > o + 1 && s[e - 1] == ' ') e--;
                s.erase(e, i - e);  // drop " to X", keep ")"
                changed = found = true;
            }
        }
        if (!found) break;
    }
    if (!changed) return expr;
    size_t len;
    if (find_top_to(s, 0, s.size(), &len) == std::string::npos) s += " to " + target;
    return s;
}

// "x := (1 usd to jpy)": qalculate stores the value before the conversion (1 USD), so "x * 10" shows
// dollars. After qalculate has made the assignment (with its own rules for shadowing built-in names),
// replace the stored value with the converted one.
void store_converted_assignment(const std::string &expr, const EvaluationOptions &eo) {
    size_t asg = expr.find(":=");
    if (asg == std::string::npos) return;
    std::string name = expr.substr(0, asg), rhs = hoist_conversions(expr.substr(asg + 2));
    size_t a = name.find_first_not_of(' '), b = name.find_last_not_of(' '), len;
    if (a == std::string::npos) return;
    name = name.substr(a, b - a + 1);
    if (find_top_to(rhs, 0, rhs.size(), &len) == std::string::npos) return;  // no conversion: nothing to fix
    Variable *v = CALCULATOR->getActiveVariable(name);
    if (!v || !v->isLocal() || !v->isKnown()) return;
    MathStructure m;
    if (CALCULATOR->calculate(&m, CALCULATOR->unlocalizeExpression(rhs, eo.parse_options), timeout, eo))
        ((KnownVariable *)v)->set(m);
    CALCULATOR->clearMessages();
}

void post_event(int kind) {  // mtx held
    CalcResult ev{};
    ev.uid = kind;
    done.push_back(std::move(ev));
    wake();
}

// Download all rates on a separate thread so calculations never wait on the network. mtx held.
void start_fetch() {
    if (fetch_running) return;
    fetch_running = true;
    g_rates_state = RATES_UPDATING;
    post_event(ENGINE_RATES_STATE);
    std::thread([] {
        bool ok = CALCULATOR->canFetch() && CALCULATOR->fetchExchangeRates(15);
        std::lock_guard<std::mutex> lk(mtx);
        fetch_running = false;
        if (ok) {
            reload_pending = true;  // the worker loads them between calculations
            cv.notify_one();
        } else {
            g_rates_state = RATES_FAILED;
            post_event(ENGINE_RATES_STATE);
        }
    }).detach();
}

bool rates_stale() {
    double days = max_age_days;
    if (days <= 0) return false;
    long long t = g_rates_time;
    return t <= 0 || (double)(time(nullptr) - t) > days * 86400;
}

void replace_all(std::string &s, const char *from, const char *to) {
    size_t n = strlen(from), m = strlen(to), p = 0;
    while ((p = s.find(from, p)) != std::string::npos) {
        s.replace(p, n, to);
        p += m;
    }
}

// "1 ∕ 2 = 0.5" -> "0.5", and unicode operators -> ASCII so it pastes anywhere.
std::string copy_text(std::string r) {
    size_t a = r.rfind(" = "), b = r.rfind(" ≈ ");
    size_t cut = std::string::npos;
    if (a != std::string::npos) cut = a + 3;
    if (b != std::string::npos && (cut == std::string::npos || b + 5 > cut)) cut = b + 5;  // "≈" is 3 bytes
    if (cut != std::string::npos) r = r.substr(cut);
    replace_all(r, "−", "-");
    replace_all(r, "×", "*");
    replace_all(r, "⋅", "*");
    replace_all(r, "∕", "/");
    replace_all(r, "÷", "/");
    replace_all(r, " ", " ");  // thin space
    return r;
}

void worker() {
    new Calculator();
    CALCULATOR->loadExchangeRates();
    CALCULATOR->loadGlobalDefinitions();
    CALCULATOR->loadLocalDefinitions();
    g_rates_time = (long long)CALCULATOR->getExchangeRatesTime();
    // "relative" makes mixed °F/°C sums behave like "convert, then add": 100 °F + 5 °C = 42.78 °C.
    // Plain conversions (20 °C to K) are the same in every mode.
    CALCULATOR->setTemperatureCalculationMode(temp_cfg == "hybrid"     ? TEMPERATURE_CALCULATION_HYBRID
                                              : temp_cfg == "absolute" ? TEMPERATURE_CALCULATION_ABSOLUTE
                                                                       : TEMPERATURE_CALCULATION_RELATIVE);

    EvaluationOptions eo = default_user_evaluation_options;
    eo.local_currency_conversion = local_currency;
    if (angle_cfg == "rad") eo.parse_options.angle_unit = ANGLE_UNIT_RADIANS;
    else if (angle_cfg == "deg") eo.parse_options.angle_unit = ANGLE_UNIT_DEGREES;
    else if (angle_cfg == "gra") eo.parse_options.angle_unit = ANGLE_UNIT_GRADIANS;
    PrintOptions po = default_print_options;
    po.use_unicode_signs = true;
    po.interval_display = INTERVAL_DISPLAY_SIGNIFICANT_DIGITS;
    po.lower_case_e = true;

    std::unique_lock<std::mutex> lk(mtx);
    calc_ready = true;
    for (;;) {
        cv.wait(lk, [] { return !pending.empty() || reload_pending || update_requested; });
        if (update_requested) {
            update_requested = false;
            start_fetch();
            continue;
        }
        if (reload_pending) {
            reload_pending = false;
            lk.unlock();
            CALCULATOR->loadExchangeRates();
            CALCULATOR->clearMessages();
            long long t = (long long)CALCULATOR->getExchangeRatesTime();
            lk.lock();
            g_rates_time = t;
            g_rates_state = RATES_IDLE;
            post_event(ENGINE_RATES_RELOADED);
            continue;
        }
        auto it = pending.begin();
        int uid = it->first;
        uint64_t gen = it->second.first;
        std::string expr = std::move(it->second.second);
        pending.erase(it);
        busy_uid = uid;
        busy_gen = gen;
        lk.unlock();

        bool approx = false, cmp = false, err = false;
        po.is_approximate = &approx;
        CALCULATOR->resetExchangeRatesUsed();
        std::string r = CALCULATOR->calculateAndPrint(CALCULATOR->unlocalizeExpression(hoist_conversions(expr), eo.parse_options),
                                                      timeout, eo, po, fractions ? AUTOMATIC_FRACTION_AUTO : AUTOMATIC_FRACTION_OFF,
                                                      AUTOMATIC_APPROXIMATION_AUTO, nullptr, -1, &cmp);
        for (CalculatorMessage *m = CALCULATOR->message(); m; m = CALCULATOR->nextMessage())
            if (m->type() == MESSAGE_ERROR) err = true;
        if (CALCULATOR->aborted()) err = true;
        if (!err) store_converted_assignment(expr, eo);

        bool cur = CALCULATOR->exchangeRatesUsed() != 0;
        CalcResult res{uid, gen, r, copy_text(r), approx, err || r.empty(), cur};
        lk.lock();
        // Stale-while-revalidate: answer from the cache now, refresh in the background. When the update
        // fails (offline), try again at most every 15 minutes.
        if (cur && rates_stale() && !fetch_running && steady_s() - last_auto_try > 15 * 60) {
            last_auto_try = steady_s();
            start_fetch();
        }
        busy_uid = -1;
        done.push_back(std::move(res));
        wake();
    }
}
}  // namespace

namespace engine {
void start(int timeout_ms, const std::string &angle, bool show_fractions, double rates_max_age_days,
           const std::string &temperature, bool local_currency_conversion) {
    timeout = timeout_ms;
    local_currency = local_currency_conversion;
    temp_cfg = temperature;
    max_age_days = rates_max_age_days;
    fractions = show_fractions;
    angle_cfg = angle;
    if (pipe2(pipefd, O_CLOEXEC | O_NONBLOCK) != 0) return;
    std::thread(worker).detach();
}

void submit(int uid, uint64_t gen, const std::string &expr) {
    std::lock_guard<std::mutex> lk(mtx);
    pending[uid] = {gen, expr};
    // The row changed while its old value is still being computed: stop the stale one.
    if (calc_ready && busy_uid == uid && busy_gen < gen) CALCULATOR->abort();
    cv.notify_one();
}

void cancel(int uid) {
    std::lock_guard<std::mutex> lk(mtx);
    pending.erase(uid);
    if (calc_ready && busy_uid == uid) CALCULATOR->abort();
}

int fd() { return pipefd[0]; }

std::vector<CalcResult> take() {
    char buf[256];
    while (read(pipefd[0], buf, sizeof buf) > 0) {}
    std::lock_guard<std::mutex> lk(mtx);
    std::vector<CalcResult> out;
    out.swap(done);
    return out;
}

void update_rates() {
    std::lock_guard<std::mutex> lk(mtx);
    if (calc_ready) start_fetch();
    else update_requested = true;  // still loading: the worker starts it once ready
    cv.notify_one();
}

void set_rates_max_age(double days) { max_age_days = days; }
time_t rates_time() { return (time_t)g_rates_time.load(); }
RatesState rates_state() { return (RatesState)g_rates_state.load(); }
}  // namespace engine
