use crate::{
    app::{files_for, resolve, row_meta, App, Hit, ListId, ListSt, Sort, Tab, AGES},
    fmt,
    theme::Theme,
};
use crossterm::event::KeyCode;
use ratatui::{
    layout::{Constraint, Constraint::*, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Cell, Clear, Paragraph, Wrap, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table, TableState,
    },
    Frame,
};
use std::sync::atomic::Ordering::Relaxed;

const PARTIAL: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn bar(t: &Theme, frac: f64, w: usize, color: Color) -> Vec<Span<'static>> {
    if w == 0 {
        return vec![];
    }
    let units = (frac.clamp(0.0, 1.0) * (w * 8) as f64).round() as usize;
    let (full, rem) = (units / 8, units % 8);
    let mut s = "█".repeat(full);
    let mut used = full;
    if rem > 0 && full < w {
        s.push_str(PARTIAL[rem]);
        used += 1;
    }
    if t.mono {
        vec![Span::raw(s), Span::raw("░".repeat(w - used))]
    } else {
        vec![
            Span::styled(s, Style::new().fg(color).bg(t.track)),
            Span::styled(" ".repeat(w - used), Style::new().bg(t.track)),
        ]
    }
}

fn panel<'a>(t: &Theme, title: &str, focused: bool) -> Block<'a> {
    let bc = if focused { t.accent } else { t.border };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(bc))
        .title(Span::styled(format!(" {title} "), Style::new().fg(t.accent).add_modifier(Modifier::BOLD)))
}

fn right_title<'a>(t: &Theme, s: String) -> Line<'a> {
    Line::from(Span::styled(format!(" {s} "), t.fg(t.dim))).right_aligned()
}

fn hdr<'a>(t: &Theme, cols: &[&'a str]) -> Row<'a> {
    Row::new(cols.iter().map(|c| Cell::from(*c))).style(Style::new().fg(t.dim).add_modifier(Modifier::BOLD))
}

fn rsize(t: &Theme, b: u64) -> Cell<'static> {
    Cell::from(Line::from(Span::styled(fmt::size(b), t.fg(t.size))).right_aligned())
}

fn rtext(s: String, st: Style) -> Cell<'static> {
    Cell::from(Line::from(Span::styled(s, st)).right_aligned())
}

/// Render only the visible slice of a list (scales to 100k+ rows).
fn list<'a>(
    f: &mut Frame,
    area: Rect,
    t: &Theme,
    ls: &mut ListSt,
    hits: &mut Vec<(Rect, Hit)>,
    id: ListId,
    len: usize,
    widths: Vec<Constraint>,
    header: Option<Row<'a>>,
    focused: bool,
    mut make: impl FnMut(usize) -> Row<'a>,
) {
    let h = (area.height as usize).saturating_sub(header.is_some() as usize);
    let off = ls.window(h, len);
    let hy = area.y + header.is_some() as u16;
    hits.push((Rect { y: hy, height: h as u16, ..area }, Hit::List { id, off, len }));
    let rows: Vec<Row> = (off..(off + h).min(len)).map(&mut make).collect();
    let mut table = Table::new(rows, widths).column_spacing(1);
    if focused {
        table = table.row_highlight_style(t.sel_style());
    }
    if let Some(hd) = header {
        table = table.header(hd);
    }
    let mut st = TableState::default();
    if len > 0 {
        st.select(Some(ls.sel - off));
    }
    f.render_stateful_widget(table, area, &mut st);
    if len > h && area.width > 0 {
        let sb = Rect { x: area.x + area.width, y: area.y, width: 1, height: area.height };
        let mut sst = ScrollbarState::new(len.saturating_sub(h)).position(off);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("│"))
                .thumb_symbol("┃")
                .style(Style::new().fg(t.border))
                .thumb_style(Style::new().fg(t.accent)),
            sb,
            &mut sst,
        );
    }
}

fn empty(f: &mut Frame, area: Rect, t: &Theme, msg: &str) {
    let y = area.y + area.height / 2;
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(msg.to_string(), t.fg(t.dim))).centered()),
        Rect { y, height: 1, ..area },
    );
}

pub fn draw(f: &mut Frame, app: &mut App) {
    app.hits.clear();
    let [head, body, foot] = Layout::vertical([Length(1), Min(0), Length(1)]).areas(f.area());
    header(f, head, app);
    match app.tab {
        Tab::Disks => disks(f, body, app),
        Tab::Explorer | Tab::Types | Tab::Dev if app.tree.is_none() => progress(f, body, app),
        Tab::Explorer => explorer(f, body, app),
        Tab::Types => types(f, body, app),
        Tab::Dev => dev(f, body, app),
        Tab::Clean => clean(f, body, app),
    }
    footer(f, foot, app);
    if app.dialog.is_some() {
        dialog(f, app);
    }
    if app.help {
        help(f, app);
    }
}

fn header(f: &mut Frame, area: Rect, app: &mut App) {
    let t = &app.theme;
    let mut spans = vec![Span::styled(" dsk ", t.badge(t.accent)), Span::raw(" ")];
    let mut x = area.x + 6;
    let mut tab_hits = vec![];
    for (i, tab) in Tab::ALL.iter().enumerate() {
        let st = if *tab == app.tab {
            if t.mono { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new().fg(t.accent).bg(t.sel).add_modifier(Modifier::BOLD) }
        } else {
            t.fg(t.dim)
        };
        let label = format!(" {} {} ", i + 1, tab.title());
        let w = label.chars().count() as u16;
        tab_hits.push((Rect { x, y: area.y, width: w, height: 1 }, Hit::Tab(*tab)));
        x += w;
        spans.push(Span::styled(label, st));
    }
    if app.scoped {
        spans.push(Span::styled("  ⌂ scoped", t.fg(t.warn)));
    }
    let mut right: Vec<Span> = vec![];
    if let Some(j) = &app.scan {
        right.push(Span::styled(format!("{} ", SPIN[app.spin % SPIN.len()]), t.fg(t.accent)));
        right.push(Span::styled(
            format!(
                "scanning {} · {} files · {} ",
                fmt::tilde(&app.root, &app.home),
                fmt::count(j.prog.files.load(Relaxed)),
                fmt::size(j.prog.bytes.load(Relaxed))
            ),
            t.fg(t.dim),
        ));
    } else if let Some(tree) = &app.tree {
        right.push(Span::styled(format!("{} ", fmt::tilde(&app.root, &app.home)), t.fg(t.accent)));
        right.push(Span::styled(fmt::size(tree.size), t.fg(t.size)));
        right.push(Span::styled(format!(" · {} items · {:.2}s ", fmt::count(tree.count), app.scan_secs), t.fg(t.dim)));
        if app.scan_errors > 0 {
            right.push(Span::styled(format!("⚠ {} unreadable ", app.scan_errors), t.fg(t.warn)));
        }
    }
    let right = Line::from(right);
    let w = right.width() as u16;
    let [l, r] = Layout::horizontal([Min(0), Length(w)]).areas(area);
    f.render_widget(Paragraph::new(Line::from(spans)), l);
    f.render_widget(Paragraph::new(right), r);
    app.hits.extend(tab_hits);
}

fn footer(f: &mut Frame, area: Rect, app: &mut App) {
    let t = &app.theme;
    use KeyCode::{Char, Enter, Left};
    let keys: &[(&str, &str, Option<KeyCode>)] = match app.tab {
        Tab::Disks if app.scoped => &[("↑↓", "move", None), ("r", "refresh", Some(Char('r')))],
        Tab::Disks => &[("↑↓", "move", None), ("⏎", "scan mount", Some(Enter)), ("h", "scan ~", Some(Char('h'))), ("o", "open", Some(Char('o'))), ("r", "refresh", Some(Char('r')))],
        Tab::Explorer => &[("⏎→", "open", Some(Enter)), ("←", "up", Some(Left)), ("␣", "mark", Some(Char(' '))), ("d", "trash", Some(Char('d'))), ("D", "delete", Some(Char('D'))), ("x", "exclude", Some(Char('x'))), ("s", "sort", Some(Char('s'))), ("o", "open", Some(Char('o'))), ("r", "rescan", Some(Char('r')))],
        Tab::Types => &[("↑↓", "move", None), ("→←", "pane", None), ("⏎", "show", Some(Enter)), ("d", "trash", Some(Char('d'))), ("D", "delete", Some(Char('D'))), ("o", "open", Some(Char('o')))],
        Tab::Dev => &[("␣", "mark", Some(Char(' '))), ("A", "all", Some(Char('A'))), ("a", "age", Some(Char('a'))), ("f", "kind", Some(Char('f'))), ("s", "sort", Some(Char('s'))), ("d", "trash", Some(Char('d'))), ("D", "delete", Some(Char('D'))), ("c", "git gc", Some(Char('c'))), ("x", "exclude", Some(Char('x'))), ("⏎", "show", Some(Enter))],
        Tab::Clean => &[("␣", "toggle", Some(Char(' '))), ("a", "all", Some(Char('a'))), ("⏎", "clean", Some(Enter)), ("x", "hide", Some(Char('x'))), ("r", "re-measure", Some(Char('r')))],
    };
    let mut spans = vec![Span::raw(" ")];
    let mut x = area.x + 1;
    for (k, l, code) in keys.iter().chain(&[("?", "help", Some(Char('?'))), ("q", "quit", Some(Char('q')))]) {
        let desc = format!(" {l}  ");
        let w = (k.chars().count() + desc.chars().count()) as u16;
        if let Some(c) = code {
            if x + w <= area.x + area.width {
                app.hits.push((Rect { x, y: area.y, width: w - 2, height: 1 }, Hit::Key(*c)));
            }
        }
        x += w;
        spans.push(Span::styled(*k, Style::new().fg(t.accent).add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(desc, t.fg(t.dim)));
    }
    let toast = app.toast.as_ref().map(|(m, _, err)| {
        Line::from(Span::styled(format!(" {m} "), if *err { t.badge(t.bad) } else { t.badge(t.ok) }))
    });
    let tw = toast.as_ref().map_or(0, |l| l.width() as u16).min(area.width.saturating_sub(10));
    let [l, r] = Layout::horizontal([Min(0), Length(tw)]).areas(area);
    f.render_widget(Paragraph::new(Line::from(spans)), l);
    if let Some(tl) = toast {
        f.render_widget(Paragraph::new(tl), r);
    }
}

fn progress(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let Some(j) = &app.scan else { return empty(f, area, t, "Nothing scanned") };
    let b = centered(area, 72, 8);
    let blk = panel(t, "Scanning", true);
    let inner = blk.inner(b);
    f.render_widget(Clear, b);
    f.render_widget(blk, b);
    let secs = j.started.elapsed().as_secs_f64().max(0.001);
    let files = j.prog.files.load(Relaxed);
    let cur = j.prog.current.lock().map(|s| s.clone()).unwrap_or_default();
    let w = inner.width as usize;
    let lines = vec![
        Line::from(vec![
            Span::styled(format!("{} ", SPIN[app.spin % SPIN.len()]), t.fg(t.accent)),
            Span::styled(fmt::tilde(&app.root, &app.home), Style::new().add_modifier(Modifier::BOLD)),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled(format!("{:>10}", fmt::count(files)), t.fg(t.size)),
            Span::styled(" files   ", t.fg(t.dim)),
            Span::styled(format!("{:>10}", fmt::size(j.prog.bytes.load(Relaxed))), t.fg(t.size)),
            Span::styled(format!("   {:.1}s   {}/s", secs, fmt::count((files as f64 / secs) as u64)), t.fg(t.dim)),
        ]),
        Line::from(Span::styled(fmt::trunc_left(&fmt::tilde(std::path::Path::new(&cur), &app.home), w), t.fg(t.dim))),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn disks(f: &mut Frame, area: Rect, app: &mut App) {
    let n = app.mounts.len() as u16;
    let nd = app.drives.len() as u16;
    let t = &app.theme;
    let [top, bot, rest] =
        Layout::vertical([Length((n + 3).min(area.height.saturating_sub(4))), Length(if nd > 0 { nd + 2 } else { 0 }), Min(0)]).areas(area);

    let (tot, used, avail) = app.mounts.iter().fold((0, 0, 0), |a, m| (a.0 + m.total, a.1 + m.used, a.2 + m.avail));
    let blk = panel(t, "Filesystems", true)
        .title(right_title(t, format!("{} total · {} used · {} free", fmt::size(tot), fmt::size(used), fmt::size(avail))));
    let inner = blk.inner(top);
    f.render_widget(blk, top);
    let barw = (inner.width as usize).saturating_sub(70).clamp(10, 40);
    let mounts = &app.mounts;
    list(
        f, inner, t, &mut app.disks_ls, &mut app.hits, ListId::Disks, mounts.len(),
        vec![Min(14), Length(18), Length(7), Length(10), Length(10), Length(10), Length(barw as u16 + 5)],
        Some(hdr(t, &["Mount", "Device", "FS", "     Size", "     Used", "     Free", "Usage"])),
        true,
        |i| {
            let m = &mounts[i];
            let mut name = vec![Span::styled(m.targets[0].display().to_string(), Style::new().add_modifier(Modifier::BOLD))];
            if m.targets.len() > 1 {
                name.push(Span::styled(format!(" +{}", m.targets.len() - 1), t.fg(t.dim)));
            }
            let fr = m.frac();
            let mut u = bar(t, fr, barw, t.usage(fr));
            u.push(Span::styled(format!("{:>4.0}%", fr * 100.0), t.fg(t.usage(fr))));
            Row::new(vec![
                Cell::from(Line::from(name)),
                Cell::from(Span::styled(fmt::trunc_left(&m.source, 18), t.fg(t.dim))),
                Cell::from(Span::styled(m.fstype.clone(), t.fg(t.dim))),
                rsize(t, m.total),
                rsize(t, m.used),
                rtext(fmt::size(m.avail), t.fg(t.usage(fr))),
                Cell::from(Line::from(u)),
            ])
        },
    );

    if nd > 0 {
    let dtot: u64 = app.drives.iter().map(|d| d.size).sum();
    let blk = panel(t, "Drives", false)
        .title(right_title(t, format!("{} drive(s) · {}", app.drives.len(), fmt::size(dtot))));
    let inner = blk.inner(bot);
    f.render_widget(blk, bot);
    let rows: Vec<Row> = app
        .drives
        .iter()
        .map(|d| {
            Row::new(vec![
                Cell::from(Span::styled(d.name.clone(), Style::new().add_modifier(Modifier::BOLD))),
                Cell::from(Span::styled(d.kind, t.fg(t.accent))),
                rsize(t, d.size),
                Cell::from(Span::styled(d.model.clone(), t.fg(t.dim))),
            ])
        })
        .collect();
    f.render_widget(Table::new(rows, [Length(12), Length(7), Length(10), Min(10)]).column_spacing(1), inner);
    }

    if rest.height >= 4 {
        overview(f, rest, app);
    }
}

/// Bottom of the Overview tab: biggest dirs of the scan root + reclaimable space summary.
fn overview(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let [l, r] = Layout::horizontal([Percentage(55), Percentage(45)]).areas(area);

    let blk = panel(t, &format!("Biggest in {}", fmt::tilde(&app.root, &app.home)), false);
    let inner = blk.inner(l);
    f.render_widget(blk, l);
    match &app.tree {
        None => empty(f, inner, t, "scanning…"),
        Some(tree) => {
            let mut top: Vec<(&str, u64, bool)> = tree
                .dirs
                .iter()
                .map(|d| (&*d.name, d.size, true))
                .chain(tree.files.iter().take(inner.height as usize).map(|x| (&*x.name, x.size, false)))
                .collect();
            top.sort_by(|a, b| b.1.cmp(&a.1));
            let barw = (inner.width as usize / 4).clamp(6, 24);
            let total = tree.size.max(1) as f64;
            let rows: Vec<Row> = top
                .into_iter()
                .take(inner.height as usize)
                .map(|(name, size, dir)| {
                    let fr = size as f64 / total;
                    Row::new(vec![
                        rsize(t, size),
                        rtext(format!("{:.1}%", fr * 100.0), t.fg(t.dim)),
                        Cell::from(Line::from(bar(t, fr, barw, t.accent))),
                        Cell::from(if dir {
                            Span::styled(format!("{name}/"), Style::new().fg(t.dir).add_modifier(Modifier::BOLD))
                        } else {
                            Span::raw(name.to_string())
                        }),
                    ])
                })
                .collect();
            f.render_widget(
                Table::new(rows, [Length(10), Length(6), Length(barw as u16), Min(8)]).column_spacing(1),
                inner,
            );
        }
    }

    let blk = panel(t, "Quick wins", false);
    let inner = blk.inner(r);
    f.render_widget(blk, r);
    let k = |s: &str| Span::styled(format!("{s:<16}"), Style::new().add_modifier(Modifier::BOLD));
    let v = |b: u64| Span::styled(format!("{:>10}  ", fmt::size(b)), t.fg(t.size));
    let hint = |s: &str| Span::styled(s.to_string(), t.fg(t.dim));
    let mut lines = vec![];
    let sizing = app.targets.iter().any(|x| x.pending);
    let caches: u64 = app.targets.iter().filter_map(|x| x.size).sum();
    lines.push(Line::from(vec![
        k("Caches"),
        if sizing { Span::styled(format!("{:>10}  ", SPIN[app.spin % SPIN.len()]), t.fg(t.accent)) } else { v(caches) },
        hint(&format!("{} cleaners · tab 5", app.targets.len())),
    ]));
    if let Some(a) = &app.analysis {
        let mut kinds: Vec<(&str, u64, usize)> = vec![];
        for x in &a.artifacts {
            match kinds.iter_mut().find(|k| k.0 == x.kind) {
                Some(k) => {
                    k.1 += x.size;
                    k.2 += 1
                }
                None => kinds.push((x.kind, x.size, 1)),
            }
        }
        kinds.sort_by(|a, b| b.1.cmp(&a.1));
        let old = fmt::now() - 90 * 86400;
        let stale: u64 = a.artifacts.iter().filter(|x| x.kind != ".git" && x.mtime < old).map(|x| x.size).sum();
        lines.push(Line::from(vec![k("Stale dev ≥90d"), v(stale), hint("node_modules, target… · tab 4")]));
        lines.push(Line::raw(""));
        for (kind, s, n) in kinds.iter().take(4) {
            lines.push(Line::from(vec![k(kind), v(*s), hint(&format!("{n} dirs"))]));
        }
        lines.push(Line::raw(""));
        for (c, s) in a.cats.iter().take(inner.height.saturating_sub(lines.len() as u16) as usize) {
            lines.push(Line::from(vec![
                Span::styled("● ", t.fg(t.cat(*c))),
                Span::styled(format!("{:<14}", c.name()), Style::new().add_modifier(Modifier::BOLD)),
                v(*s),
                hint("tab 3"),
            ]));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn explorer(f: &mut Frame, area: Rect, app: &mut App) {
    let t = &app.theme;
    let Some(d) = app.tree.as_ref().and_then(|tr| resolve(tr, &app.ex_stack)) else { return };
    let path = fmt::tilde(&app.cur_path(), &app.home);
    let sort = match app.ex_sort {
        Sort::Size => "size",
        Sort::Name => "name",
        Sort::Items => "items",
    };
    let mut info = format!("{} · {} items · sort: {sort}", fmt::size(d.size), fmt::count(d.count));
    if !app.ex_marks.is_empty() {
        let ms: u64 = app.ex_rows.iter().map(|r| row_meta(d, *r)).filter(|m| app.ex_marks.contains(m.0)).map(|m| m.1).sum();
        info = format!("{} marked · {} │ {info}", app.ex_marks.len(), fmt::size(ms));
    }
    let blk = panel(t, &fmt::trunc_left(&path, (area.width as usize / 2).max(10)), true).title(right_title(t, info));
    let inner = blk.inner(area);
    f.render_widget(blk, area);
    if app.ex_rows.is_empty() {
        return empty(f, inner, t, "empty directory");
    }
    let barw = (inner.width as usize / 5).clamp(6, 30);
    let total = d.size.max(1) as f64;
    let (rows, marks) = (&app.ex_rows, &app.ex_marks);
    let mut ls = app.ex_ls;
    list(
        f, inner, t, &mut ls, &mut app.hits, ListId::Explorer, rows.len(),
        vec![Length(1), Length(10), Length(6), Length(barw as u16), Length(7), Min(10)],
        None,
        true,
        |i| {
            let (name, size, items, is_dir, err) = row_meta(d, rows[i]);
            let frac = size as f64 / total;
            let marked = marks.contains(name);
            let name_span = if is_dir {
                Span::styled(format!("{name}/"), Style::new().fg(t.dir).add_modifier(Modifier::BOLD))
            } else {
                Span::styled(name.to_string(), t.fg(t.fg))
            };
            let mut nl = vec![];
            if err {
                nl.push(Span::styled("! ", t.fg(t.bad)));
            }
            nl.push(name_span);
            Row::new(vec![
                Cell::from(Span::styled(if marked { "●" } else { " " }, t.fg(t.mark))),
                rsize(t, size),
                rtext(format!("{:.1}%", frac * 100.0), t.fg(t.dim)),
                Cell::from(Line::from(bar(t, frac, barw, if is_dir { t.accent } else { t.dim }))),
                rtext(if is_dir { fmt::count(items) } else { String::new() }, t.fg(t.dim)),
                Cell::from(Line::from(nl)),
            ])
            .style(if marked { t.fg(t.mark) } else { Style::new() })
        },
    );
    app.ex_ls = ls;
}

fn types(f: &mut Frame, area: Rect, app: &mut App) {
    let t = &app.theme;
    let Some(a) = &app.analysis else { return };
    let [top, rest] = Layout::vertical([Length(4), Min(0)]).areas(area);

    // category stacked bar + legend
    let total: u64 = a.cats.iter().map(|c| c.1).sum::<u64>().max(1);
    let blk = panel(t, "Categories", false).title(right_title(t, format!("{} in {} files", fmt::size(total), fmt::count(a.files))));
    let inner = blk.inner(top);
    f.render_widget(blk, top);
    let w = inner.width as usize;
    let mut bar_spans = vec![];
    let mut used = 0;
    for (i, (c, s)) in a.cats.iter().enumerate() {
        let mut cw = ((*s as f64 / total as f64) * w as f64).round() as usize;
        if i == a.cats.len() - 1 {
            cw = w.saturating_sub(used);
        }
        cw = cw.min(w.saturating_sub(used));
        used += cw;
        bar_spans.push(Span::styled("█".repeat(cw), t.fg(t.cat(*c))));
    }
    let mut legend = vec![];
    let mut lw = 0;
    for (c, s) in &a.cats {
        let item = format!("● {} {}   ", c.name(), fmt::size(*s));
        lw += item.chars().count();
        if lw > w + 3 {
            break;
        }
        legend.push(Span::styled("● ", t.fg(t.cat(*c))));
        legend.push(Span::styled(format!("{} ", c.name()), Style::new().add_modifier(Modifier::BOLD)));
        legend.push(Span::styled(format!("{}   ", fmt::size(*s)), t.fg(t.dim)));
    }
    f.render_widget(Paragraph::new(vec![Line::from(bar_spans), Line::from(legend)]), inner);

    // full path of the highlighted file, wrapped, under both panes
    let cur = {
        let fs = files_for(a, app.ty_ls.sel);
        fs.get(app.ty_files_ls.sel.min(fs.len().saturating_sub(1))).cloned()
    };
    let cur_path = cur.as_ref().map(|(_, p)| fmt::tilde(p, &app.home)).unwrap_or_default();
    let pw = rest.width.saturating_sub(2).max(1) as usize;
    let ph = (cur_path.chars().count().div_ceil(pw)).clamp(1, 4) as u16 + 2;
    let [rest, path_area] = Layout::vertical([Min(0), Length(ph)]).areas(rest);
    let mut blk = panel(t, "Path", false);
    if let Some((s, _)) = &cur {
        blk = blk.title(right_title(t, fmt::size(*s)));
    }
    let inner = blk.inner(path_area);
    f.render_widget(blk, path_area);
    let (dir, base) = match cur_path.rsplit_once('/') {
        Some((d, b)) => (format!("{d}/"), b.to_string()),
        None => (String::new(), cur_path.clone()),
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(dir, t.fg(t.dim)),
            Span::styled(base, Style::new().fg(t.dir).add_modifier(Modifier::BOLD)),
        ]))
        .wrap(Wrap { trim: false }),
        inner,
    );

    let [l, r] = Layout::horizontal([Percentage(42), Percentage(58)]).areas(rest);
    let focus_files = app.ty_focus_files;

    let blk = panel(t, "Extensions", !focus_files).title(right_title(t, format!("{} kinds", a.exts.len())));
    let inner = blk.inner(l);
    f.render_widget(blk, l);
    let barw = (inner.width as usize).saturating_sub(40).clamp(4, 24);
    let max = a.exts.first().map_or(1, |e| e.size).max(1) as f64;
    let mut ls = app.ty_ls;
    list(
        f, inner, t, &mut ls, &mut app.hits, ListId::TyExt, a.exts.len() + 1,
        vec![Length(2), Min(8), Length(10), Length(7), Length(barw as u16)],
        None,
        true,
        |i| {
            if i == 0 {
                return Row::new(vec![
                    Cell::from(Span::styled("★", t.fg(t.accent))),
                    Cell::from(Span::styled("largest files", Style::new().fg(t.accent).add_modifier(Modifier::BOLD))),
                    rsize(t, total),
                    rtext(fmt::count(a.files), t.fg(t.dim)),
                    Cell::from(""),
                ]);
            }
            let e = &a.exts[i - 1];
            let name = if e.ext.is_empty() { "(none)".to_string() } else { format!(".{}", e.ext) };
            Row::new(vec![
                Cell::from(Span::styled("●", t.fg(t.cat(e.cat)))),
                Cell::from(Span::styled(name, Style::new().add_modifier(Modifier::BOLD))),
                rsize(t, e.size),
                rtext(fmt::count(e.count), t.fg(t.dim)),
                Cell::from(Line::from(bar(t, e.size as f64 / max, barw, t.cat(e.cat)))),
            ])
        },
    );
    app.ty_ls = ls;

    let files = files_for(a, app.ty_ls.sel);
    let title = if app.ty_ls.sel == 0 {
        "Largest files".to_string()
    } else {
        let e = &a.exts[app.ty_ls.sel - 1];
        format!("Largest {}", if e.ext.is_empty() { "(no ext)".into() } else { format!(".{}", e.ext) })
    };
    let blk = panel(t, &title, focus_files).title(right_title(t, format!("top {}", files.len())));
    let inner = blk.inner(r);
    f.render_widget(blk, r);
    let pw = (inner.width as usize).saturating_sub(12);
    let (root, home) = (&app.root, &app.home);
    let mut fls = app.ty_files_ls;
    list(
        f, inner, t, &mut fls, &mut app.hits, ListId::TyFiles, files.len(),
        vec![Length(10), Min(10)],
        None,
        focus_files,
        |i| {
            let (s, p) = &files[i];
            let shown = p.strip_prefix(root).map(|r| r.display().to_string()).unwrap_or_else(|_| fmt::tilde(p, home));
            Row::new(vec![rsize(t, *s), Cell::from(Span::raw(fmt::trunc_left(&shown, pw)))])
        },
    );
    app.ty_files_ls = fls;
}

fn dev(f: &mut Frame, area: Rect, app: &mut App) {
    let t = &app.theme;
    let Some(a) = &app.analysis else { return };
    // per-kind totals
    let mut kinds: Vec<(&str, u64, usize)> = vec![];
    for x in &a.artifacts {
        match kinds.iter_mut().find(|k| k.0 == x.kind) {
            Some(k) => {
                k.1 += x.size;
                k.2 += 1
            }
            None => kinds.push((x.kind, x.size, 1)),
        }
    }
    kinds.sort_by(|a, b| b.1.cmp(&a.1));

    let shown: u64 = app.dev_rows.iter().map(|&i| a.artifacts[i].size).sum();
    let mut info = format!(
        "{} in {} · {} · {} · sort: {}",
        fmt::size(shown),
        app.dev_rows.len(),
        AGES[app.dev_age].1,
        app.dev_kind.unwrap_or("all kinds"),
        if app.dev_sort_age { "oldest" } else { "size" }
    );
    if !app.dev_marks.is_empty() {
        let ms: u64 = a.artifacts.iter().filter(|x| app.dev_marks.contains(&x.path)).map(|x| x.size).sum();
        info = format!("{} marked · {} │ {info}", app.dev_marks.len(), fmt::size(ms));
    }
    let blk = panel(t, "Dev artifacts", true).title(right_title(t, info));
    let inner = blk.inner(area);
    f.render_widget(blk, area);
    let [strip, body] = Layout::vertical([Length(1), Min(0)]).areas(inner);
    let mut ks = vec![];
    for (k, s, n) in &kinds {
        let active = app.dev_kind == Some(*k);
        let st = if active { t.badge(t.accent) } else { Style::new().fg(t.accent).add_modifier(Modifier::BOLD) };
        ks.push(Span::styled(k.to_string(), st));
        ks.push(Span::styled(format!(" {} ({n})   ", fmt::size(*s)), t.fg(t.dim)));
    }
    f.render_widget(Paragraph::new(Line::from(ks)), strip);

    if app.dev_rows.is_empty() {
        return empty(f, body, t, "No matching artifacts (try a / f to change filters)");
    }
    let pw = (body.width as usize).saturating_sub(36);
    let (rows, marks, root, home) = (&app.dev_rows, &app.dev_marks, &app.root, &app.home);
    let mut ls = app.dev_ls;
    list(
        f, body, t, &mut ls, &mut app.hits, ListId::Dev, rows.len(),
        vec![Length(1), Length(10), Length(13), Length(6), Min(10)],
        Some(hdr(t, &["", "      Size", "Kind", "   Age", "Path"])),
        true,
        |i| {
            let x = &a.artifacts[rows[i]];
            let marked = marks.contains(&x.path);
            let shown = x.path.strip_prefix(root).map(|r| r.display().to_string()).unwrap_or_else(|_| fmt::tilde(&x.path, home));
            let (dir, base) = match shown.rsplit_once('/') {
                Some((d, b)) => (format!("{d}/"), b.to_string()),
                None => (String::new(), shown.clone()),
            };
            let dir = fmt::trunc_left(&dir, pw.saturating_sub(base.len()));
            let age_days = (fmt::now() - x.mtime) / 86400;
            let age_c = if age_days > 180 { t.ok } else if age_days > 30 { t.warn } else { t.dim };
            Row::new(vec![
                Cell::from(Span::styled(if marked { "●" } else { " " }, t.fg(t.mark))),
                rsize(t, x.size),
                Cell::from(Span::styled(x.kind, t.fg(if x.kind == ".git" { t.warn } else { t.accent }))),
                rtext(fmt::age(x.mtime), t.fg(age_c)),
                Cell::from(Line::from(vec![
                    Span::styled(dir, t.fg(t.dim)),
                    Span::styled(base, Style::new().fg(t.dir).add_modifier(Modifier::BOLD)),
                ])),
            ])
            .style(if marked { t.fg(t.mark) } else { Style::new() })
        },
    );
    app.dev_ls = ls;
}

fn clean(f: &mut Frame, area: Rect, app: &mut App) {
    let t = &app.theme;
    let [top, detail] = Layout::vertical([Min(5), Length(5)]).areas(area);
    let total: u64 = app.targets.iter().filter_map(|x| x.size).sum();
    let sel: Vec<_> = app.targets.iter().filter(|x| x.selected).collect();
    let sel_total: u64 = sel.iter().filter_map(|x| x.size).sum();
    let blk = panel(t, "Cleanable caches", true).title(right_title(
        t,
        format!("{} selected · {} │ {} reclaimable", sel.len(), fmt::size(sel_total), fmt::size(total)),
    ));
    let inner = blk.inner(top);
    f.render_widget(blk, top);
    let targets = &app.targets;
    let (is_root, spin) = (app.is_root, app.spin);
    let max = targets.iter().filter_map(|x| x.size).max().unwrap_or(1).max(1) as f64;
    let barw = (inner.width as usize).saturating_sub(90).clamp(6, 24);
    if targets.is_empty() {
        let msg = if app.scoped {
            format!("No known caches inside {} (scoped)", fmt::tilde(&app.root, &app.home))
        } else {
            "No known caches found".to_string()
        };
        return empty(f, inner, t, &msg);
    }
    let mut ls = app.cl_ls;
    list(
        f, inner, t, &mut ls, &mut app.hits, ListId::Clean, targets.len(),
        vec![Length(3), Length(7), Length(26), Length(10), Length(barw as u16), Length(4), Min(10)],
        Some(hdr(t, &["", "Group", "Name", "      Size", "", "", "Note"])),
        true,
        |i| {
            let x = &targets[i];
            let first = i == 0 || targets[i - 1].group != x.group;
            let size = if x.pending {
                rtext(SPIN[spin % SPIN.len()].into(), t.fg(t.accent))
            } else {
                match x.size {
                    Some(s) => rsize(t, s),
                    None => rtext("?".into(), t.fg(t.dim)),
                }
            };
            Row::new(vec![
                Cell::from(Span::styled(if x.selected { "[✓]" } else { "[ ]" }, t.fg(if x.selected { t.ok } else { t.dim }))),
                Cell::from(Span::styled(if first { x.group } else { "" }, Style::new().fg(t.accent).add_modifier(Modifier::BOLD))),
                Cell::from(Span::styled(x.name, Style::new().add_modifier(Modifier::BOLD))),
                size,
                Cell::from(Line::from(bar(t, x.size.unwrap_or(0) as f64 / max, barw, t.size))),
                Cell::from(Span::styled(if x.root && !is_root { "sudo" } else { "" }, t.fg(t.warn))),
                Cell::from(Span::styled(x.note, t.fg(t.dim))),
            ])
            .style(if x.size == Some(0) { t.fg(t.dim) } else { Style::new() })
        },
    );
    app.cl_ls = ls;

    let blk = panel(t, "Details", false);
    let inner = blk.inner(detail);
    f.render_widget(blk, detail);
    if let Some(x) = app.targets.get(app.cl_ls.sel) {
        let lines: Vec<Line> = match &x.action {
            crate::clean::Action::Contents(ps) => ps
                .iter()
                .map(|p| Line::from(vec![Span::styled("rm -rf ", t.fg(t.dim)), Span::raw(format!("{}/*", fmt::tilde(p, &app.home)))]))
                .collect(),
            crate::clean::Action::Cmds(cs) => cs
                .iter()
                .map(|c| {
                    let pre = if x.root && !app.is_root { "sudo " } else { "" };
                    Line::from(vec![Span::styled(format!("$ {pre}"), t.fg(t.dim)), Span::raw(c.join(" "))])
                })
                .collect(),
        };
        f.render_widget(Paragraph::new(lines), inner);
    }
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let (w, h) = (w.min(area.width), h.min(area.height));
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn dialog(f: &mut Frame, app: &mut App) {
    let t = &app.theme;
    let Some(d) = &app.dialog else { return };
    let w = d.body.iter().map(|l| l.chars().count()).max().unwrap_or(0).max(d.title.chars().count() + 4).max(40) as u16 + 4;
    let area = centered(f.area(), w, d.body.len() as u16 + 5);
    let color = if d.danger { t.bad } else { t.accent };
    let blk = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color))
        .title(Span::styled(format!(" {} ", d.title), Style::new().fg(color).add_modifier(Modifier::BOLD)));
    let inner = blk.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(blk, area);
    let mut lines: Vec<Line> = d.body.iter().map(|l| Line::raw(l.clone())).collect();
    lines.push(Line::raw(""));
    let yes = if d.danger { " confirm    " } else { " / ⏎ confirm    " };
    let by = inner.y + lines.len() as u16;
    let yw = 3 + yes.chars().count() as u16 - 4;
    lines.push(Line::from(vec![
        Span::styled(" y ", t.badge(color)),
        Span::styled(yes, t.fg(t.dim)),
        Span::styled(" n ", t.badge(t.dim)),
        Span::styled(" / esc cancel", t.fg(t.dim)),
    ]));
    f.render_widget(Paragraph::new(lines), inner);
    app.hits.push((Rect { x: inner.x, y: by, width: yw, height: 1 }, Hit::Dialog(KeyCode::Char('y'))));
    app.hits.push((Rect { x: inner.x + yw + 4, y: by, width: 16, height: 1 }, Hit::Dialog(KeyCode::Char('n'))));
}

fn help(f: &mut Frame, app: &App) {
    let t = &app.theme;
    let rows: &[(&str, &str)] = &[
        ("Global", ""),
        ("1-5 / Tab", "switch tab"),
        ("↑↓ j k  PgUp PgDn  g G", "move"),
        ("q / Ctrl-C", "quit"),
        ("Mouse", "click tab/row/hint · dbl-click open · right-click up"),
        ("Overview", ""),
        ("⏎", "scan selected mount   h: scan home"),
        ("Explorer", ""),
        ("⏎ → l  /  ← h ⌫", "open dir / go up"),
        ("␣  esc", "mark / clear marks"),
        ("d  D", "move to Trash / delete permanently"),
        ("x", "exclude path (saved to config)"),
        ("s  o  r", "sort · open in file manager · rescan"),
        ("Types", ""),
        ("→ ←  ⏎", "switch pane · jump to file in Explorer"),
        ("Dev", ""),
        ("␣  A", "mark / mark all visible"),
        ("a  f  s", "age filter · kind filter · sort size/oldest"),
        ("d D  c", "trash / delete · git gc on .git rows"),
        ("Clean", ""),
        ("␣  a  ⏎", "toggle · all · clean selected"),
        ("x", "hide cleaner (saved to config)"),
    ];
    let area = centered(f.area(), 74, rows.len() as u16 + 4);
    let blk = panel(t, "Help", true).title(right_title(t, fmt::tilde(&app.cfg_path, &app.home)));
    let inner = blk.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(blk, area);
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, v)| {
            if v.is_empty() {
                Line::from(Span::styled(*k, Style::new().fg(t.accent).add_modifier(Modifier::BOLD)))
            } else {
                Line::from(vec![Span::styled(format!("  {k:<24}"), Style::new().add_modifier(Modifier::BOLD)), Span::styled(*v, t.fg(t.dim))])
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}
