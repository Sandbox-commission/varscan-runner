use chrono::Local;
use clap::Parser;
use crossterm::{cursor, event, execute, style, terminal, tty::IsTty};
use std::collections::VecDeque;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod checkpoint;
mod pipeline;
mod tui;

use checkpoint::{
    invalidate_downstream, is_step_done, sha256_step, step_display, write_checkpoint,
    Dirs, STEPS_ORDERED,
};
use pipeline::{
    make_dirs, run_bam_readcount, run_copycaller, run_copynumber, run_filter_input,
    run_flagstats, run_mpileup, run_process_somatic, run_somatic, Config,
};
use tui::{fmt_duration, render, render_final_frame, JobSlotSnapshot, RenderSnapshot};

static CANCELLED: AtomicBool = AtomicBool::new(false);

// ─── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name    = "varscan-runner",
    version,
    about   = "VarScan2 somatic + CNV pipeline — TUI + SHA256 per-step resume"
)]
struct Cli {
    /// CSV: col1 = normal_final.bam, col2 = tumor_final.bam
    #[arg(short, long, default_value = "sample_pairs.csv")]
    pairs: PathBuf,

    /// Directory containing *_final.bam files
    #[arg(short = 'd', long, default_value = ".")]
    bam_dir: PathBuf,

    /// Output directory
    #[arg(short, long, default_value = ".")]
    output: PathBuf,

    /// Reference genome FASTA
    #[arg(short, long)]
    genome: PathBuf,

    /// Path to VarScan jar
    #[arg(long, default_value = "/home/gifthr/software/VarScan.v2.3.9.jar")]
    varscan: PathBuf,

    #[arg(long, default_value = "java")]        java:          String,
    #[arg(long, default_value = "24g")]         java_mem:      String,
    #[arg(long, default_value = "samtools")]    samtools:      String,
    #[arg(long, default_value = "bam-readcount")] bam_readcount: String,

    /// Parallel pairs
    #[arg(short, long, default_value_t = 8)]
    jobs: usize,

    // ── VarScan somatic ──
    #[arg(long, default_value_t = 10)]    min_coverage:         u32,
    #[arg(long, default_value_t = 10)]    min_coverage_normal:  u32,
    #[arg(long, default_value_t = 15)]    min_coverage_tumor:   u32,
    #[arg(long, default_value_t = 0.08)]  min_var_freq:         f64,
    #[arg(long, default_value_t = 0.75)]  min_freq_for_hom:     f64,
    #[arg(long, default_value_t = 1.0)]   normal_purity:        f64,
    #[arg(long, default_value_t = 1.0)]   tumor_purity:         f64,
    #[arg(long, default_value_t = 0.99)]  p_value:              f64,
    #[arg(long, default_value_t = 0.05)]  somatic_p_value:      f64,
    #[arg(long, default_value_t = 0.10)]  min_tumor_freq:       f64,
    #[arg(long, default_value_t = 0.05)]  max_normal_freq:      f64,
    #[arg(long, default_value_t = 0.07)]  process_p_value:      f64,
    // ── VarScan copynumber ──
    #[arg(long, default_value_t = 10)]    cnv_min_coverage:     u32,
    #[arg(long, default_value_t = 20)]    min_segment_size:     u32,
    #[arg(long, default_value_t = 100)]   max_segment_size:     u32,
    #[arg(long, default_value_t = 0.005)] cnv_p_value:          f64,
    // ── samtools ──
    #[arg(long, default_value_t = 20)]    mpileup_mapq:         u32,
}

// ─── Sample pair ──────────────────────────────────────────────────────────────

#[derive(Clone)]
struct Pair {
    normal:  String,
    tumor:   String,
    pair_id: String,
}

// ─── Shared TUI state ─────────────────────────────────────────────────────────

struct State {
    stages_done:    AtomicUsize,
    stages_total:   usize,
    pairs_done:     AtomicUsize,
    pairs_failed:   AtomicUsize,
    stages_resumed: AtomicUsize,
    slots:          Mutex<Vec<Option<SlotInfo>>>,
    events:         Mutex<VecDeque<String>>,
    pair_durations: Mutex<Vec<f64>>,
    start:          Instant,
    n_pairs:        usize,
}

#[derive(Clone)]
struct SlotInfo {
    pair_id: String,
    stage:   String,
    started: Instant,
}

impl State {
    fn new(n_pairs: usize, n_workers: usize) -> Self {
        State {
            stages_done:    AtomicUsize::new(0),
            stages_total:   n_pairs * STEPS_ORDERED.len(),
            pairs_done:     AtomicUsize::new(0),
            pairs_failed:   AtomicUsize::new(0),
            stages_resumed: AtomicUsize::new(0),
            slots:          Mutex::new(vec![None; n_workers]),
            events:         Mutex::new(VecDeque::with_capacity(64)),
            pair_durations: Mutex::new(Vec::new()),
            start:          Instant::now(),
            n_pairs,
        }
    }
}

fn push_event(state: &Arc<State>, msg: String) {
    let ts   = Local::now().format("%H:%M:%S");
    let line = format!("  [{ts}] {msg}");
    let mut ev = state.events.lock().unwrap_or_else(|e| e.into_inner());
    if ev.len() >= 200 { ev.pop_front(); }
    ev.push_back(line);
}

// ─── Snapshot assembly ────────────────────────────────────────────────────────

fn assemble_snapshot(state: &Arc<State>, n_workers: usize) -> RenderSnapshot {
    let stages_done    = state.stages_done.load(Ordering::Relaxed);
    let stages_total   = state.stages_total;
    let pairs_done     = state.pairs_done.load(Ordering::Relaxed);
    let pairs_failed   = state.pairs_failed.load(Ordering::Relaxed);
    let stages_resumed = state.stages_resumed.load(Ordering::Relaxed);
    let overall_frac   = if stages_total > 0 { stages_done as f64 / stages_total as f64 } else { 0.0 };
    let elapsed        = state.start.elapsed();

    let slots  = state.slots.lock().unwrap_or_else(|e| e.into_inner());
    let events = state.events.lock().unwrap_or_else(|e| e.into_inner());
    let durs   = state.pair_durations.lock().unwrap_or_else(|e| e.into_inner());

    let avg_dur = if durs.is_empty() { 0.0 }
        else { durs.iter().sum::<f64>() / durs.len() as f64 };

    let (overall_phase, phase_label) = {
        let active: Vec<&str> = slots.iter().flatten().map(|s| s.stage.as_str()).collect();
        let idx = if active.is_empty() {
            (stages_done / state.n_pairs.max(1)).min(STEPS_ORDERED.len().saturating_sub(1))
        } else {
            active.iter()
                .filter_map(|s| STEPS_ORDERED.iter().position(|&step| step == *s))
                .max()
                .unwrap_or(0)
        };
        (
            idx + 1,
            format!("Stage {}/{} \u{2014} {}",
                idx + 1, STEPS_ORDERED.len(), step_display(STEPS_ORDERED[idx])),
        )
    };

    let jobs: Vec<Option<JobSlotSnapshot>> = slots.iter().map(|slot| {
        slot.as_ref().map(|s| JobSlotSnapshot {
            sample:       s.pair_id.clone(),
            step:         step_display(&s.stage).to_string(),
            elapsed_secs: s.started.elapsed().as_secs_f64(),
            pct:          0,
        })
    }).collect();

    let _ = n_workers; // used for slot Vec size

    RenderSnapshot {
        done:                 pairs_done,
        total:                state.n_pairs,
        completed:            pairs_done,
        skipped:              stages_resumed,
        failed:               pairs_failed,
        phase_label,
        jobs,
        recent_events:        events.iter().cloned().collect(),
        elapsed,
        avg_dur,
        overall_frac,
        overall_phase:        overall_phase.max(1),
        overall_total_phases: STEPS_ORDERED.len(),
        overall_done:         pairs_done,
        overall_total:        state.n_pairs,
        overall_elapsed:      elapsed,
        p3_frac:              None,
        cancelled:            CANCELLED.load(Ordering::Relaxed),
        resumed:              stages_resumed,
    }
}

// ─── Pair worker ──────────────────────────────────────────────────────────────

fn process_pair(
    pair:       &Pair,
    slot_idx:   usize,
    cfg:        &Config,
    dirs:       &Dirs,
    state:      &Arc<State>,
    pair_start: Instant,
) {
    let pid = &pair.pair_id;
    let n   = &*pair.normal;
    let t   = &*pair.tumor;

    for &step in STEPS_ORDERED {
        if CANCELLED.load(Ordering::Relaxed) { break; }

        // Update active slot
        {
            let mut slots = state.slots.lock().unwrap_or_else(|e| e.into_inner());
            slots[slot_idx] = Some(SlotInfo {
                pair_id: format!("{t} / {n}"),
                stage:   step.to_string(),
                started: Instant::now(),
            });
        }

        // SHA256 resume check
        if is_step_done(&cfg.output_dir, pid, step, dirs, n, t) {
            state.stages_done.fetch_add(1, Ordering::Relaxed);
            state.stages_resumed.fetch_add(1, Ordering::Relaxed);
            push_event(state, format!("SKIP  {pid} — {} (SHA256 match)", step_display(step)));
            continue;
        }

        // Invalidate downstream checkpoints before re-running
        invalidate_downstream(&cfg.output_dir, pid, step);

        let result: Result<(), String> = match step {
            "flagstats"       => run_flagstats(cfg, dirs, n, t).map_err(|e| e.message),
            "mpileup"         => run_mpileup(cfg, dirs, n, t).map_err(|e| e.message),
            "somatic"         => run_somatic(cfg, dirs, n, t).map_err(|e| e.message),
            "process_somatic" => run_process_somatic(cfg, dirs, t).map_err(|e| e.message),
            "copynumber"      => run_copynumber(cfg, dirs, n, t).map_err(|e| e.message),
            "copycaller"      => run_copycaller(cfg, dirs, t).map_err(|e| e.message),
            "filter_input"    => run_filter_input(dirs, t).map_err(|e| e.message),
            "bam_readcount"   => run_bam_readcount(cfg, dirs, t).map_err(|e| e.message),
            _                 => Err("unknown step".to_string()),
        };

        match result {
            Ok(()) => {
                let digest = sha256_step(dirs, n, t, step);
                let _ = write_checkpoint(&cfg.output_dir, pid, step, &digest);
                state.stages_done.fetch_add(1, Ordering::Relaxed);
                push_event(state, format!("DONE  {pid} — {}", step_display(step)));
            }
            Err(msg) => {
                push_event(state, format!("FAIL  {pid} — {}: {msg}", step_display(step)));
                state.pairs_failed.fetch_add(1, Ordering::Relaxed);
                let mut slots = state.slots.lock().unwrap_or_else(|e| e.into_inner());
                slots[slot_idx] = None;
                return;
            }
        }
    }

    let elapsed = pair_start.elapsed().as_secs_f64();
    state.pairs_done.fetch_add(1, Ordering::Relaxed);
    state.pair_durations.lock().unwrap_or_else(|e| e.into_inner()).push(elapsed);
    push_event(state, format!("DONE  {pid} — complete ({:.0}s)", elapsed));

    let mut slots = state.slots.lock().unwrap_or_else(|e| e.into_inner());
    slots[slot_idx] = None;
}

// ─── Parse pairs CSV ──────────────────────────────────────────────────────────

fn parse_pairs(path: &std::path::Path) -> Result<Vec<Pair>, String> {
    let file = fs::File::open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut pairs = Vec::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| format!("read error line {i}: {e}"))?;
        let line = line.trim().to_string();
        if line.is_empty() || line.starts_with('#') { continue; }
        let mut cols = line.splitn(2, ',');
        let col1 = cols.next().unwrap_or("").trim().to_string();
        let col2 = cols.next().unwrap_or("").trim().to_string();
        if col1.is_empty() || col2.is_empty() {
            return Err(format!("line {}: need 2 comma-separated fields: {line}", i + 1));
        }
        let normal  = col1.trim_end_matches("_final.bam").to_string();
        let tumor   = col2.trim_end_matches("_final.bam").to_string();
        let pair_id = format!("{normal}_{tumor}");
        pairs.push(Pair { normal, tumor, pair_id });
    }
    if pairs.is_empty() {
        return Err(format!("no pairs found in {}", path.display()));
    }
    Ok(pairs)
}

// ─── SIGINT → CANCELLED ───────────────────────────────────────────────────────

fn install_sigint_handler() {
    extern "C" fn handler(_: libc::c_int) {
        CANCELLED.store(true, Ordering::Relaxed);
    }
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handler as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut sa.sa_mask);
        sa.sa_flags = 0;
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
    }
}

// ─── Terminal guard (restores on drop) ───────────────────────────────────────

struct TermGuard;
impl Drop for TermGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, terminal::LeaveAlternateScreen, cursor::Show, style::ResetColor);
        let _ = terminal::disable_raw_mode();
    }
}

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() -> ExitCode {
    let cli = Cli::parse();

    let pairs = match parse_pairs(&cli.pairs) {
        Ok(p)  => p,
        Err(e) => { eprintln!("ERROR: {e}"); return ExitCode::FAILURE; }
    };
    let n_pairs = pairs.len();

    if let Err(e) = fs::create_dir_all(&cli.output) {
        eprintln!("ERROR: output dir: {e}"); return ExitCode::FAILURE;
    }
    let dirs = match make_dirs(&cli.output) {
        Ok(d)  => Arc::new(d),
        Err(e) => { eprintln!("ERROR: dir setup: {e}"); return ExitCode::FAILURE; }
    };

    let cfg = Arc::new(Config {
        bam_dir:             cli.bam_dir.canonicalize().unwrap_or(cli.bam_dir),
        output_dir:          cli.output.canonicalize().unwrap_or(cli.output),
        genome:              cli.genome,
        varscan_jar:         cli.varscan,
        java:                cli.java,
        java_mem:            cli.java_mem,
        samtools:            cli.samtools,
        bam_readcount_bin:   cli.bam_readcount,
        min_coverage:        cli.min_coverage,
        min_coverage_normal: cli.min_coverage_normal,
        min_coverage_tumor:  cli.min_coverage_tumor,
        min_var_freq:        cli.min_var_freq,
        min_freq_for_hom:    cli.min_freq_for_hom,
        normal_purity:       cli.normal_purity,
        tumor_purity:        cli.tumor_purity,
        p_value:             cli.p_value,
        somatic_p_value:     cli.somatic_p_value,
        min_tumor_freq:      cli.min_tumor_freq,
        max_normal_freq:     cli.max_normal_freq,
        process_p_value:     cli.process_p_value,
        cnv_min_coverage:    cli.cnv_min_coverage,
        min_segment_size:    cli.min_segment_size,
        max_segment_size:    cli.max_segment_size,
        cnv_p_value:         cli.cnv_p_value,
        mpileup_mapq:        cli.mpileup_mapq,
    });

    let n_workers = cli.jobs.max(1).min(n_pairs);
    let state     = Arc::new(State::new(n_pairs, n_workers));
    let queue     = Arc::new(Mutex::new(pairs));
    let active    = Arc::new(AtomicUsize::new(n_workers));

    // ── TUI + signal setup ──
    install_sigint_handler();
    let is_tty = io::stdout().is_tty();
    let mut stdout = io::stdout();
    let _guard: Option<TermGuard> = if is_tty {
        let _ = terminal::enable_raw_mode();
        let _ = execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide);
        Some(TermGuard)
    } else {
        None
    };

    // ── Spawn workers ──
    let handles: Vec<_> = (0..n_workers).map(|slot_idx| {
        let queue   = Arc::clone(&queue);
        let state   = Arc::clone(&state);
        let cfg     = Arc::clone(&cfg);
        let dirs    = Arc::clone(&dirs);
        let active  = Arc::clone(&active);
        std::thread::spawn(move || {
            loop {
                if CANCELLED.load(Ordering::Relaxed) { break; }
                let pair = {
                    let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                    if q.is_empty() { break; }
                    q.remove(0)
                };
                let t0 = Instant::now();
                process_pair(&pair, slot_idx, &cfg, &dirs, &state, t0);
            }
            active.fetch_sub(1, Ordering::Relaxed);
        })
    }).collect();

    // ── Render loop ──
    let refresh    = Duration::from_millis(100);
    let mut blink  = false;
    let mut last_b = Instant::now();

    while active.load(Ordering::Relaxed) > 0 {
        if last_b.elapsed() >= Duration::from_millis(500) { blink = !blink; last_b = Instant::now(); }

        if is_tty {
            let snap = assemble_snapshot(&state, n_workers);
            render(&mut stdout, &snap, n_workers, blink);

            if event::poll(Duration::ZERO).unwrap_or(false) {
                if let Ok(event::Event::Key(k)) = event::read() {
                    match k.code {
                        event::KeyCode::Char('q') | event::KeyCode::Char('Q') => {
                            CANCELLED.store(true, Ordering::Relaxed);
                        }
                        event::KeyCode::Char('c')
                            if k.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                            CANCELLED.store(true, Ordering::Relaxed);
                        }
                        _ => {}
                    }
                }
            }
        }
        std::thread::sleep(refresh);
    }

    for h in handles { h.join().ok(); }

    // ── Final frame ──
    let elapsed      = state.start.elapsed();
    let pairs_done   = state.pairs_done.load(Ordering::Relaxed);
    let pairs_failed = state.pairs_failed.load(Ordering::Relaxed);
    let resumed      = state.stages_resumed.load(Ordering::Relaxed);

    if is_tty {
        let msg = format!(
            "{pairs_done}/{n_pairs} pairs  |  {pairs_failed} failed  |  {resumed} stages resumed  |  {}",
            fmt_duration(elapsed)
        );
        render_final_frame(&mut stdout, &msg);
        // Wait up to 10s for a keypress before restoring terminal
        let _ = event::poll(Duration::from_secs(10));
        // _guard Drop restores terminal here
    }

    eprintln!();
    eprintln!("══════════════════════════════════════════════════════════════");
    eprintln!(" VarScan2 Runner — finished");
    eprintln!("══════════════════════════════════════════════════════════════");
    eprintln!(" Pairs completed : {pairs_done}/{n_pairs}");
    eprintln!(" Pairs failed    : {pairs_failed}");
    eprintln!(" Stages resumed  : {resumed} (SHA256 match → skipped)");
    eprintln!(" Total elapsed   : {}", fmt_duration(elapsed));
    eprintln!(" Output          : {}", cfg.output_dir.display());
    eprintln!("══════════════════════════════════════════════════════════════");

    if pairs_failed > 0 || CANCELLED.load(Ordering::Relaxed) { ExitCode::FAILURE }
    else { ExitCode::SUCCESS }
}
