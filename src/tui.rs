use chrono::Local;
use crossterm::{
    cursor, execute, queue,
    style::{self, Color},
    terminal::{self, ClearType},
};
use std::io::{self, Write};
use std::time::Duration;

// ─── Public snapshot types ────────────────────────────────────────────────────

#[derive(Clone)]
pub struct JobSlotSnapshot {
    pub sample: String,
    pub step: String,
    pub elapsed_secs: f64,
    pub pct: usize,
}

#[allow(dead_code)]
pub struct RenderSnapshot {
    pub done: usize,
    pub total: usize,
    pub completed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub phase_label: String,
    pub jobs: Vec<Option<JobSlotSnapshot>>,
    pub recent_events: Vec<String>,
    pub elapsed: Duration,
    pub avg_dur: f64,
    pub overall_frac: f64,
    pub overall_phase: usize,
    pub overall_total_phases: usize,
    pub overall_done: usize,
    pub overall_total: usize,
    pub overall_elapsed: Duration,
    pub p3_frac: Option<f64>,
    pub cancelled: bool,
    pub resumed: usize,
}

// ─── Layout ───────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode { Compact, Normal, Wide }

#[allow(dead_code)]
pub struct LayoutMetrics {
    pub mode: LayoutMode,
    pub overall_bar_w: usize,
    pub sample_col_w: usize,
    pub step_col_w: usize,
    pub bar_col_w: usize,
    pub pct_col_w: usize,
    pub stats_rows: usize,
}

pub fn compute_layout(w: usize, _h: usize) -> LayoutMetrics {
    let mode = match w {
        0..=79  => LayoutMode::Compact,
        80..=119 => LayoutMode::Normal,
        _        => LayoutMode::Wide,
    };
    let overall_bar_w = w.saturating_sub(7).max(4);
    let sample_col_w  = (w * 50 / 100).max(10);
    let step_col_w    = (w * 20 / 100).max(8);
    let bar_col_w     = (w * 20 / 100).max(8);
    let used          = sample_col_w + step_col_w + bar_col_w + 5;
    let pct_col_w     = w.saturating_sub(used).max(5);
    let stats_rows    = if mode == LayoutMode::Wide { 1 } else { 2 };
    LayoutMetrics { mode, overall_bar_w, sample_col_w, step_col_w, bar_col_w, pct_col_w, stats_rows }
}

// ─── Duration formatting ──────────────────────────────────────────────────────

pub fn fmt_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 { format!("{h:02}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
}

pub fn fmt_secs(s: f64) -> String {
    if s.is_nan() || s.is_infinite() || s < 0.0 { return "??:??".to_string(); }
    if s >= 359_999.0 { return "99:59:59+".to_string(); }
    fmt_duration(Duration::from_secs_f64(s))
}

pub fn truncate_to(s: &str, n: usize) -> String {
    if s.chars().count() > n { s.chars().take(n).collect() } else { s.to_string() }
}

// ─── Gradient bar ─────────────────────────────────────────────────────────────

fn gradient_bar_string(
    filled: usize, empty: usize,
    dark: (u8, u8, u8), bright: (u8, u8, u8),
    blink_on: bool, blink_color: (u8, u8, u8),
) -> String {
    let mut buf = String::with_capacity((filled + empty) * 20);
    if filled == 0 {
        buf.push_str(&fg_rgb(128, 128, 128));
        for _ in 0..empty { buf.push('\u{2591}'); }
        buf.push_str(RESET);
        return buf;
    }
    for i in 0..filled {
        let t = if filled > 1 { i as f64 / (filled - 1) as f64 } else { 1.0 };
        let is_last = i == filled - 1;
        if is_last && blink_on && empty > 0 {
            let (r, g, b) = blink_color;
            buf.push_str(&fg_rgb(r, g, b));
        } else {
            let r = (dark.0 as f64 + t * (bright.0 as f64 - dark.0 as f64)) as u8;
            let g = (dark.1 as f64 + t * (bright.1 as f64 - dark.1 as f64)) as u8;
            let b = (dark.2 as f64 + t * (bright.2 as f64 - dark.2 as f64)) as u8;
            buf.push_str(&fg_rgb(r, g, b));
        }
        buf.push('\u{2588}');
    }
    buf.push_str(&fg_rgb(128, 128, 128));
    for _ in 0..empty { buf.push('\u{2591}'); }
    buf.push_str(RESET);
    buf
}

fn set_fg(stdout: &mut io::Stdout, r: u8, g: u8, b: u8) {
    if is_truecolor() {
        let _ = execute!(stdout, style::SetForegroundColor(Color::Rgb { r, g, b }));
    } else {
        let _ = execute!(stdout, style::SetForegroundColor(Color::AnsiValue(rgb_to_256(r, g, b))));
    }
}

pub fn print_gradient_bar(
    stdout: &mut io::Stdout,
    filled: usize, empty: usize,
    dark: (u8, u8, u8), bright: (u8, u8, u8),
    blink_on: bool, blink_color: (u8, u8, u8),
) {
    if filled == 0 {
        let _ = execute!(stdout, style::SetForegroundColor(Color::DarkGrey));
        for _ in 0..empty { let _ = write!(stdout, "\u{2591}"); }
        let _ = stdout.flush();
        return;
    }
    for i in 0..filled {
        let t = if filled > 1 { i as f64 / (filled - 1) as f64 } else { 1.0 };
        let is_last = i == filled - 1;
        if is_last && blink_on && empty > 0 {
            let (r, g, b) = blink_color;
            set_fg(stdout, r, g, b);
        } else {
            let r = (dark.0 as f64 + t * (bright.0 as f64 - dark.0 as f64)) as u8;
            let g = (dark.1 as f64 + t * (bright.1 as f64 - dark.1 as f64)) as u8;
            let b = (dark.2 as f64 + t * (bright.2 as f64 - dark.2 as f64)) as u8;
            set_fg(stdout, r, g, b);
        }
        let _ = write!(stdout, "\u{2588}");
    }
    let _ = execute!(stdout, style::SetForegroundColor(Color::DarkGrey));
    for _ in 0..empty { let _ = write!(stdout, "\u{2591}"); }
    let _ = stdout.flush();
}

// ─── Per-job bar color (amber < 25 %, teal 25–59 %, blue ≥ 60 %) ─────────────

fn job_bar_colors(frac: f64) -> ((u8, u8, u8), (u8, u8, u8)) {
    if frac < 0.25      { ((0x85, 0x4f, 0x0b), (0xef, 0x9f, 0x27)) }
    else if frac < 0.60 { ((0x1d, 0x9e, 0x75), (0x5d, 0xca, 0xa5)) }
    else                { ((0x18, 0x5f, 0xa5), (0x37, 0x8a, 0xdd)) }
}

fn job_pct_color(frac: f64) -> String {
    if frac < 0.25      { fg_rgb(0xef, 0x9f, 0x27) }
    else if frac < 0.60 { fg_rgb(0x1d, 0x9e, 0x75) }
    else                { fg_rgb(0x37, 0x8a, 0xdd) }
}

// ─── ANSI color helpers ───────────────────────────────────────────────────────

use std::sync::atomic::{AtomicU8, Ordering as AO};
static COLOR_MODE: AtomicU8 = AtomicU8::new(0);

fn is_truecolor() -> bool {
    let v = COLOR_MODE.load(AO::Relaxed);
    if v != 0 { return v == 1; }
    let tc   = std::env::var("COLORTERM").map(|v| v == "truecolor" || v == "24bit").unwrap_or(false);
    let term = std::env::var("TERM").unwrap_or_default();
    let in_screen = term.starts_with("screen") && std::env::var("TMUX").is_err();
    let result = tc && !in_screen;
    COLOR_MODE.store(if result { 1 } else { 2 }, AO::Relaxed);
    result
}

fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    if r.abs_diff(g) < 10 && g.abs_diff(b) < 10 {
        let avg = ((r as u16 + g as u16 + b as u16) / 3) as u8;
        if avg < 8   { return 16; }
        if avg > 248 { return 231; }
        return 232 + ((avg as u16 - 8) * 24 / 240) as u8;
    }
    let ri = ((r as u16) * 5 / 255) as u8;
    let gi = ((g as u16) * 5 / 255) as u8;
    let bi = ((b as u16) * 5 / 255) as u8;
    16 + 36 * ri + 6 * gi + bi
}

fn fg_rgb(r: u8, g: u8, b: u8) -> String {
    if is_truecolor() { format!("\x1b[38;2;{r};{g};{b}m") }
    else              { format!("\x1b[38;5;{}m", rgb_to_256(r, g, b)) }
}

fn fg_rgb_bold(r: u8, g: u8, b: u8) -> String {
    if is_truecolor() { format!("\x1b[1;38;2;{r};{g};{b}m") }
    else              { format!("\x1b[1;38;5;{}m", rgb_to_256(r, g, b)) }
}

fn fg_named(name: &str) -> &'static str {
    match name {
        "white"     => "\x1b[97m",
        "cyan"      => "\x1b[96m",
        "green"     => "\x1b[92m",
        "yellow"    => "\x1b[93m",
        "red"       => "\x1b[91m",
        "magenta"   => "\x1b[95m",
        "darkgrey"  => "\x1b[90m",
        "darkyellow"=> "\x1b[33m",
        "darkred"   => "\x1b[31m",
        "bold"      => "\x1b[1m",
        _           => "\x1b[0m",
    }
}

const RESET: &str = "\x1b[0m";

// ─── Theme ────────────────────────────────────────────────────────────────────

struct Theme {
    section_label: String,
    hdr_blue:      String,
    stats_grey:    String,
    stats_val:     String,
    badge_blue:    String,
    badge_text:    String,
    sep_dim:       String,
    cnt_done:      String,
    cnt_skip:      String,
    cnt_fail:      String,
    cnt_rem:       String,
    act_done:      String,
    spin_color:    String,
}

impl Theme {
    fn load() -> Self {
        Self {
            section_label: fg_rgb_bold(240, 230, 140),
            hdr_blue:      fg_rgb_bold(88,  166, 255),
            stats_grey:    fg_rgb_bold(139, 148, 158),
            stats_val:     fg_rgb_bold(230, 237, 243),
            badge_blue:    fg_rgb_bold(31,  111, 235),
            badge_text:    fg_rgb_bold(88,  166, 255),
            sep_dim:       fg_rgb_bold(68,  68,  68 ),
            cnt_done:      fg_rgb_bold(63,  185, 80 ),
            cnt_skip:      fg_rgb_bold(139, 148, 158),
            cnt_fail:      fg_rgb_bold(248, 81,  73 ),
            cnt_rem:       fg_rgb_bold(88,  166, 255),
            act_done:      fg_rgb_bold(63,  185, 80 ),
            spin_color:    fg_rgb_bold(239, 159, 39 ),
        }
    }
}

use std::sync::OnceLock;
static THEME: OnceLock<Theme> = OnceLock::new();
fn theme() -> &'static Theme { THEME.get_or_init(Theme::load) }

#[allow(non_snake_case)] fn SECTION_LABEL() -> &'static str { &theme().section_label }
#[allow(non_snake_case)] fn HDR_BLUE()      -> &'static str { &theme().hdr_blue }
#[allow(non_snake_case)] fn STATS_GREY()    -> &'static str { &theme().stats_grey }
#[allow(non_snake_case)] fn STATS_VAL()     -> &'static str { &theme().stats_val }
#[allow(non_snake_case)] fn BADGE_BLUE()    -> &'static str { &theme().badge_blue }
#[allow(non_snake_case)] fn BADGE_TEXT()    -> &'static str { &theme().badge_text }
#[allow(non_snake_case)] fn SEP_DIM()       -> &'static str { &theme().sep_dim }
#[allow(non_snake_case)] fn CNT_DONE()      -> &'static str { &theme().cnt_done }
#[allow(non_snake_case)] fn CNT_SKIP()      -> &'static str { &theme().cnt_skip }
#[allow(non_snake_case)] fn CNT_FAIL()      -> &'static str { &theme().cnt_fail }
#[allow(non_snake_case)] fn CNT_REM()       -> &'static str { &theme().cnt_rem }
#[allow(non_snake_case)] fn ACT_DONE()      -> &'static str { &theme().act_done }
#[allow(non_snake_case)] fn SPIN_COLOR()    -> &'static str { &theme().spin_color }

// ─── Visible-length helpers ───────────────────────────────────────────────────

fn visible_len(s: &str) -> usize {
    let mut n = 0;
    let mut esc = false;
    for c in s.chars() {
        if esc      { if c == 'm' { esc = false; } }
        else if c == '\x1b' { esc = true; }
        else                { n += 1; }
    }
    n
}

fn pad_to_width(s: &str, w: usize) -> String {
    let vis = visible_len(s);
    if vis >= w { s.to_string() } else { format!("{}{}", s, " ".repeat(w - vis)) }
}

fn truncate_ansi(s: &str, max_vis: usize) -> String {
    let mut out = String::with_capacity(s.len());
    let mut vis = 0;
    let mut esc = false;
    for c in s.chars() {
        if esc { out.push(c); if c == 'm' { esc = false; } }
        else if c == '\x1b' { esc = true; out.push(c); }
        else {
            if vis >= max_vis { break; }
            out.push(c);
            vis += 1;
        }
    }
    out
}

// ─── Frame clipping ───────────────────────────────────────────────────────────

const HEADER_ROWS: usize = 4;
const FOOTER_ROWS: usize = 3;

fn clip_frame(frame: &[String], h: usize) -> Vec<String> {
    if frame.len() <= h { return frame.to_vec(); }
    let header_take = HEADER_ROWS.min(frame.len()).min(h);
    let footer_take = FOOTER_ROWS
        .min(frame.len().saturating_sub(header_take))
        .min(h.saturating_sub(header_take));
    let middle_budget = h.saturating_sub(header_take + footer_take);
    let mut out = Vec::with_capacity(h);
    for row in frame.iter().take(header_take)           { out.push(row.clone()); }
    let footer_start = frame.len().saturating_sub(footer_take);
    let mid_avail    = footer_start.saturating_sub(header_take);
    if middle_budget > 0 && mid_avail > 0 {
        let take = middle_budget.min(mid_avail);
        for row in frame[header_take..header_take + take].iter() { out.push(row.clone()); }
    }
    for row in frame[footer_start..].iter().take(footer_take) { out.push(row.clone()); }
    out.truncate(h);
    out
}

// ─── Braille spinner ──────────────────────────────────────────────────────────

const BRAILLE: [char; 10] = [
    '\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283C}',
    '\u{2834}', '\u{2826}', '\u{2827}', '\u{2807}', '\u{280F}',
];

// ─── Main render ──────────────────────────────────────────────────────────────

pub fn render(stdout: &mut io::Stdout, snap: &RenderSnapshot, parallel_jobs: usize, blink_on: bool) {
    let (build_w, build_h) = terminal::size().unwrap_or((80, 24));
    let w = build_w as usize;
    let h = build_h as usize;
    if w < 20 || h < 10 { return; }

    let lm = compute_layout(w, h);

    #[allow(non_snake_case)] let SECTION_LABEL = SECTION_LABEL();
    #[allow(non_snake_case)] let HDR_BLUE      = HDR_BLUE();
    #[allow(non_snake_case)] let STATS_GREY    = STATS_GREY();
    #[allow(non_snake_case)] let STATS_VAL     = STATS_VAL();
    #[allow(non_snake_case)] let BADGE_BLUE    = BADGE_BLUE();
    #[allow(non_snake_case)] let BADGE_TEXT    = BADGE_TEXT();
    #[allow(non_snake_case)] let SEP_DIM       = SEP_DIM();
    #[allow(non_snake_case)] let CNT_DONE      = CNT_DONE();
    #[allow(non_snake_case)] let CNT_SKIP      = CNT_SKIP();
    #[allow(non_snake_case)] let CNT_FAIL      = CNT_FAIL();
    #[allow(non_snake_case)] let CNT_REM       = CNT_REM();
    #[allow(non_snake_case)] let ACT_DONE      = ACT_DONE();
    #[allow(non_snake_case)] let SPIN_COLOR    = SPIN_COLOR();

    let mut frame: Vec<String> = Vec::with_capacity(h);

    // ── Header ──
    frame.push(pad_to_width(&format!("{}{}{RESET}", SEP_DIM, "\u{2500}".repeat(w)), w));
    {
        let title_str = format!("VarScan2 Runner v{}", env!("CARGO_PKG_VERSION"));
        let pad_l = w.saturating_sub(title_str.len()) / 2;
        frame.push(pad_to_width(
            &format!("{}{}{HDR_BLUE}{title_str}{RESET}", " ".repeat(pad_l), fg_named("bold")), w
        ));
    }
    {
        let sub = "Somatic SNV\u{00B7}INDEL + Copy Number Analysis  |  tumor\u{2013}normal pairs";
        let pad_l = w.saturating_sub(sub.len()) / 2;
        frame.push(pad_to_width(&format!("{}{STATS_GREY}{sub}{RESET}", " ".repeat(pad_l)), w));
    }
    frame.push(pad_to_width(&format!("{}{}{RESET}", SEP_DIM, "\u{2500}".repeat(w)), w));

    // ── Overall pipeline section ──
    {
        let phase_name = snap.phase_label.split('\u{2014}')
            .next_back().unwrap_or(&snap.phase_label).trim();
        let label  = format!("  {SECTION_LABEL}OVERALL PIPELINE{RESET}");
        let badge  = format!(
            "{BADGE_BLUE}[{BADGE_TEXT} stage {}/{} \u{2014} {} {BADGE_BLUE}]{RESET}",
            snap.overall_phase, snap.overall_total_phases, phase_name,
        );
        let label_vis = 18;
        let gap = w.saturating_sub(label_vis + visible_len(&badge));
        frame.push(pad_to_width(&format!("{label}{}{badge}", " ".repeat(gap)), w));
    }
    {
        let bar_w  = lm.overall_bar_w;
        let filled = (bar_w as f64 * snap.overall_frac) as usize;
        let empty  = bar_w.saturating_sub(filled);
        let pct    = (snap.overall_frac * 100.0) as usize;
        let bar    = gradient_bar_string(filled, empty, (0x1d,0x9e,0x75), (0x5d,0xca,0xa5), blink_on, (0x5d,0xca,0xa5));
        frame.push(pad_to_width(&format!("  {bar} {STATS_GREY}{pct:>3}%{RESET}"), w));
    }
    {
        let o_eta_str = if snap.overall_frac > 0.0 && snap.overall_frac < 1.0 {
            let eta = Duration::from_secs_f64(
                snap.overall_elapsed.as_secs_f64() / snap.overall_frac * (1.0 - snap.overall_frac)
            );
            fmt_duration(eta)
        } else if snap.overall_frac >= 1.0 { "00:00".to_string() }
        else                               { "--:--".to_string() };
        frame.push(pad_to_width(&format!(
            "    {STATS_GREY}elapsed {STATS_VAL}{}   {STATS_GREY}eta {STATS_VAL}{}   {STATS_GREY}pairs {STATS_VAL}{}/{}{RESET}",
            fmt_duration(snap.overall_elapsed), o_eta_str, snap.overall_done, snap.overall_total,
        ), w));
    }

    // ── Phase progress ──
    frame.push(pad_to_width(&format!("  {SECTION_LABEL}STAGE PROGRESS{RESET}"), w));
    let remaining = snap.total.saturating_sub(snap.done);
    let phase_frac = if let Some(p3f) = snap.p3_frac { p3f }
        else if snap.total > 0 { snap.done.min(snap.total) as f64 / snap.total as f64 }
        else                   { 0.0 };
    let phase_pct = (phase_frac * 100.0) as usize;
    {
        let bar_w  = lm.overall_bar_w;
        let filled = (bar_w as f64 * phase_frac) as usize;
        let empty  = bar_w.saturating_sub(filled);
        let bar    = gradient_bar_string(filled, empty, (0x18,0x5f,0xa5), (0x37,0x8a,0xdd), blink_on, (0x37,0x8a,0xdd));
        frame.push(pad_to_width(&format!("  {bar} {STATS_GREY}{phase_pct:>3}%{RESET}"), w));
    }
    let avg_dur   = snap.avg_dur;
    let elapsed   = snap.elapsed;
    let processed = snap.completed;
    let elapsed_m = elapsed.as_secs_f64() / 60.0;
    let speed_str = if processed == 0 { "calculating...".to_string() }
        else {
            let spm = processed as f64 / elapsed_m.max(0.001);
            if spm < 1.0 { format!("{:.1}/hr", spm * 60.0) } else { format!("{:.1}/min", spm) }
        };
    let (has_eta, eta) = if processed > 0 && remaining > 0 {
        (true, Duration::from_secs_f64(avg_dur * remaining as f64))
    } else if snap.done > 0 && remaining > 0 {
        let per = elapsed.as_secs_f64().max(0.001) / snap.done as f64;
        (true, Duration::from_secs_f64(per * remaining as f64))
    } else {
        (false, Duration::ZERO)
    };
    let eta_str = if has_eta { fmt_duration(eta) } else { "--:--".to_string() };
    let completion_time = if has_eta && eta.as_secs() > 0 {
        let now = Local::now();
        (now + chrono::Duration::seconds(eta.as_secs() as i64)).format("%H:%M:%S").to_string()
    } else {
        "\u{2014}".to_string()
    };
    if lm.stats_rows == 2 {
        frame.push(pad_to_width(&format!(
            "    {STATS_GREY}elapsed {STATS_VAL}{}   {STATS_GREY}eta {STATS_VAL}{}   {STATS_GREY}{}/{} pairs done{RESET}",
            fmt_duration(elapsed), eta_str, snap.done, snap.total,
        ), w));
        frame.push(pad_to_width(&format!(
            "    {STATS_GREY}speed {STATS_VAL}{}   {STATS_GREY}complete {STATS_VAL}{}{RESET}",
            speed_str, completion_time,
        ), w));
    } else {
        frame.push(pad_to_width(&format!(
            "    {STATS_GREY}elapsed {STATS_VAL}{}   {STATS_GREY}eta {STATS_VAL}{}   {STATS_GREY}speed {STATS_VAL}{}   {STATS_GREY}complete {STATS_VAL}{}{RESET}",
            fmt_duration(elapsed), eta_str, speed_str, completion_time,
        ), w));
    }

    // ── Active jobs ──
    let active_count = snap.jobs.iter().filter(|s| s.is_some()).count();
    frame.push(pad_to_width(&format!(
        "  {SECTION_LABEL}ACTIVE JOBS {}{CNT_REM}({}/{}){RESET}",
        RESET, active_count, parallel_jobs,
    ), w));

    let spin_idx = (snap.elapsed.as_millis() / 100) as usize;
    let sc = lm.sample_col_w;
    let stc = lm.step_col_w;
    let bc  = lm.bar_col_w;
    let pc  = lm.pct_col_w;

    frame.push(pad_to_width(&format!(
        "  {SEP_DIM}{}\u{252C}{}\u{252C}{}\u{252C}{}{RESET}",
        "\u{2500}".repeat(sc), "\u{2500}".repeat(stc), "\u{2500}".repeat(bc), "\u{2500}".repeat(pc),
    ), w));
    frame.push(pad_to_width(&format!(
        "  {STATS_GREY}{:<sc$}{SEP_DIM}\u{2502}{STATS_GREY}{:<stc$}{SEP_DIM}\u{2502}{STATS_GREY}{:<bc$}{SEP_DIM}\u{2502}{STATS_GREY}{:>pc$}{RESET}",
        " PAIR", " STAGE", " PROGRESS", " ",
    ), w));
    frame.push(pad_to_width(&format!(
        "  {SEP_DIM}{}\u{253C}{}\u{253C}{}\u{253C}{}{RESET}",
        "\u{2500}".repeat(sc), "\u{2500}".repeat(stc), "\u{2500}".repeat(bc), "\u{2500}".repeat(pc),
    ), w));

    let active_jobs: Vec<(usize, &JobSlotSnapshot)> = snap.jobs.iter().enumerate()
        .filter_map(|(i, s)| s.as_ref().map(|j| (i, j))).collect();

    if active_jobs.is_empty() {
        let cell = format!("{:<width$}", " No active jobs", width = sc + stc + bc + pc + 3);
        frame.push(pad_to_width(&format!("  {STATS_GREY}{cell}{RESET}"), w));
    }

    let rows_below = 7;
    let max_rows   = h.saturating_sub(frame.len() + rows_below) / 2;
    let max_rows   = max_rows.max(1);

    for (shown, (i, job)) in active_jobs.iter().enumerate() {
        if shown >= max_rows {
            let hidden = active_jobs.len().saturating_sub(shown);
            if hidden > 0 {
                let cell = format!("{:<width$}", format!(" ... and {hidden} more"), width = sc + stc + bc + pc + 3);
                frame.push(pad_to_width(&format!("  {STATS_GREY}{cell}{RESET}"), w));
            }
            break;
        }
        let spin = BRAILLE[(spin_idx + i) % BRAILLE.len()];

        let name_max = sc.saturating_sub(4);
        let name_d = if job.sample.chars().count() > name_max {
            format!("{}...", job.sample.chars().take(name_max.saturating_sub(3)).collect::<String>())
        } else { job.sample.clone() };
        let sample_cell = format!("{:<sc$}", format!(" {spin} {name_d}"));

        let step_d    = truncate_to(&job.step, stc.saturating_sub(2));
        let step_cell = format!("{:<stc$}", format!(" {step_d}"));

        let (frac, pct_display, is_real_pct) = if job.pct > 0 {
            let f = job.pct.min(100) as f64 / 100.0;
            (f, format!("{:>3}%", job.pct.min(100)), true)
        } else if avg_dur > 0.0 {
            let f = (job.elapsed_secs / avg_dur).min(1.0);
            (f, format!("~{:>2}%", (f * 100.0) as usize), false)
        } else {
            (0.0, String::new(), false)
        };

        let bar_inner_w = bc.saturating_sub(4);
        let bar_cell = if avg_dur > 0.0 || is_real_pct {
            let b_filled = (bar_inner_w as f64 * frac) as usize;
            let b_empty  = bar_inner_w.saturating_sub(b_filled);
            let (dark, bright) = job_bar_colors(frac);
            let bar = gradient_bar_string(b_filled, b_empty, dark, bright, blink_on, bright);
            format!(" [{bar}] ")
        } else {
            let pulse_pos = (spin_idx + i * 3) % (bar_inner_w + 4);
            let mut pulse = String::new();
            for p in 0..bar_inner_w {
                if p >= pulse_pos.saturating_sub(2) && p <= pulse_pos {
                    pulse.push_str(&format!("{}\u{2588}", fg_rgb(0xef, 0x9f, 0x27)));
                } else {
                    pulse.push_str(&format!("{}\u{2591}", fg_rgb(128, 128, 128)));
                }
            }
            format!(" [{pulse}] ")
        };
        let bar_vis = visible_len(&bar_cell);
        let bar_cell_padded = if bar_vis < bc { format!("{}{}", bar_cell, " ".repeat(bc - bar_vis)) }
            else { bar_cell };

        let pct_color = job_pct_color(frac);
        let pct_cell  = format!("{pct_color}{:>pc$}{RESET}", pct_display);

        frame.push(pad_to_width(&format!(
            "  {SPIN_COLOR}{sample_cell}{SEP_DIM}\u{2502}{HDR_BLUE}{step_cell}{SEP_DIM}\u{2502}{bar_cell_padded}{SEP_DIM}\u{2502}{pct_cell}{RESET}",
        ), w));

        let ela_str = fmt_secs(job.elapsed_secs);
        let eta_part = if job.pct > 0 && job.pct < 100 {
            let total_est = job.elapsed_secs / (job.pct as f64 / 100.0);
            let rem = total_est - job.elapsed_secs;
            format!("{} / ~{}", ela_str, fmt_secs(rem.max(0.0)))
        } else if avg_dur > 0.0 {
            format!("{} / ~{}", ela_str, fmt_secs(avg_dur))
        } else { ela_str };
        let inner_w = sc + stc + bc + pc + 3;
        let ela_cell = format!("{:<inner_w$}", format!("    {eta_part}"));
        frame.push(pad_to_width(&format!("  {STATS_GREY}{ela_cell}{RESET}"), w));
    }

    frame.push(pad_to_width(&format!(
        "  {SEP_DIM}{}\u{2534}{}\u{2534}{}\u{2534}{}{RESET}",
        "\u{2500}".repeat(sc), "\u{2500}".repeat(stc), "\u{2500}".repeat(bc), "\u{2500}".repeat(pc),
    ), w));

    // ── Counters ──
    frame.push(pad_to_width(&format!("{}{}{RESET}", SEP_DIM, "\u{2500}".repeat(w)), w));
    frame.push(pad_to_width(&format!(
        "  {CNT_DONE}\u{2713} completed: {}   {CNT_SKIP}\u{2192} resumed: {}   {CNT_FAIL}\u{2717} failed: {}   {CNT_REM}\u{22C5} remaining: {}{RESET}",
        snap.completed, snap.skipped, snap.failed, remaining,
    ), w));

    // ── Recent activity ──
    frame.push(pad_to_width(&format!("{}{}{RESET}", SEP_DIM, "\u{2500}".repeat(w)), w));
    frame.push(pad_to_width(&format!("  {SECTION_LABEL}RECENT ACTIVITY{RESET}"), w));

    let used_rows      = frame.len();
    let max_event_rows = h.saturating_sub(used_rows + 2);
    let events         = &snap.recent_events;
    let start          = events.len().saturating_sub(max_event_rows);
    for ev_line in events[start..].iter().rev() {
        let ev    = truncate_to(ev_line, w);
        let color = if ev.contains("DONE")   { ACT_DONE }
            else if ev.contains("SKIP") || ev.contains("RESUME") { CNT_SKIP }
            else if ev.contains("FAIL")      { CNT_FAIL }
            else if ev.contains("STOP")      { fg_named("darkred") }
            else if ev.contains("INFO")      { STATS_GREY }
            else                             { STATS_VAL };
        frame.push(pad_to_width(&format!("{color}{ev}{RESET}"), w));
    }

    while frame.len() < h.saturating_sub(2) { frame.push(" ".repeat(w)); }

    frame.push(pad_to_width(&format!("{}{}{RESET}", SEP_DIM, "\u{2500}".repeat(w)), w));
    {
        let quit_hint = if snap.cancelled {
            format!("{CNT_FAIL}  CANCELLING...")
        } else {
            format!("{STATS_GREY}  [q] quit   Ctrl+C cancel")
        };
        let timestamp = format!("Updated: {}", Local::now().format("%H:%M:%S"));
        let quit_vis  = if snap.cancelled { 16 } else { 28 };
        let pad       = w.saturating_sub(quit_vis + timestamp.len());
        frame.push(pad_to_width(&format!("{quit_hint}{}{STATS_GREY}{timestamp}{RESET}", " ".repeat(pad)), w));
    }

    // ── Flush ──
    let (fw, fh) = terminal::size().unwrap_or((build_w, build_h));
    let fw = fw as usize;
    let fh = fh as usize;
    let clipped = if frame.len() > fh { clip_frame(&frame, fh) } else { frame };
    let mut buf = Vec::with_capacity(fw * fh * 4);
    let _ = queue!(buf, cursor::Hide);
    for (row, line) in clipped.iter().enumerate() {
        let _ = queue!(buf, cursor::MoveTo(0, row as u16));
        let truncated = truncate_ansi(line, fw);
        let padded    = pad_to_width(&truncated, fw);
        let _ = write!(buf, "{padded}");
    }
    for row in clipped.len()..fh {
        let _ = queue!(buf, cursor::MoveTo(0, row as u16));
        let _ = queue!(buf, terminal::Clear(ClearType::CurrentLine));
    }
    let _ = queue!(buf, style::ResetColor, cursor::Hide);
    let _ = stdout.write_all(&buf);
    let _ = stdout.flush();
}

// ─── Post-run final frame ─────────────────────────────────────────────────────

pub fn render_final_frame(stdout: &mut io::Stdout, message: &str) {
    let (tw, th) = terminal::size().unwrap_or((80, 24));
    let w = tw as usize;
    let h = th as usize;

    #[allow(non_snake_case)] let CNT_DONE   = CNT_DONE();
    #[allow(non_snake_case)] let STATS_GREY = STATS_GREY();
    #[allow(non_snake_case)] let STATS_VAL  = STATS_VAL();

    let mut frame: Vec<String> = Vec::with_capacity(h);
    frame.push(pad_to_width(&format!("{}{}{RESET}", CNT_DONE, "\u{2550}".repeat(w)), w));
    let mid = h / 2;
    for _ in 1..mid.saturating_sub(1) { frame.push(" ".repeat(w)); }

    let banner = "  \u{2714} PIPELINE COMPLETE  ";
    let pad_l  = w.saturating_sub(banner.chars().count()) / 2;
    frame.push(pad_to_width(&format!("{}{}{CNT_DONE}{banner}{RESET}", " ".repeat(pad_l), fg_named("bold")), w));

    let msg_pad = w.saturating_sub(message.chars().count()) / 2;
    frame.push(pad_to_width(&format!("{}{STATS_VAL}{message}{RESET}", " ".repeat(msg_pad)), w));

    let hint = "Press any key or wait 10s...";
    let hint_pad = w.saturating_sub(hint.len()) / 2;
    frame.push(pad_to_width(&format!("{}{STATS_GREY}{hint}{RESET}", " ".repeat(hint_pad)), w));

    while frame.len() < h.saturating_sub(1) { frame.push(" ".repeat(w)); }
    frame.push(pad_to_width(&format!("{}{}{RESET}", CNT_DONE, "\u{2550}".repeat(w)), w));

    let mut buf = Vec::with_capacity(w * h * 4);
    let _ = queue!(buf, cursor::MoveTo(0, 0));
    for line in frame.iter().take(h) {
        let t = truncate_ansi(line, w);
        let p = pad_to_width(&t, w);
        let _ = write!(buf, "{p}\r\n");
    }
    let _ = queue!(buf, terminal::Clear(ClearType::FromCursorDown));
    let _ = queue!(buf, style::ResetColor);
    let _ = stdout.write_all(&buf);
    let _ = stdout.flush();
}
