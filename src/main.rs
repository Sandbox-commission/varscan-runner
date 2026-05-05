use chrono::Local;
use clap::Parser;
use crossterm::{cursor, event, execute, style, terminal, tty::IsTty};
use std::collections::VecDeque;
use std::fs;
use std::io::{self, BufRead, BufReader, Write as IoWrite};
use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod checkpoint;
mod dirs;
mod pipeline;
mod tui;

use checkpoint::{
    check_resume_all_parallel, compute_checkpoint, invalidate_downstream,
    pending_checkpoint, pre_run_cleanup, step_display, step_output_size, upgrade_checkpoint,
    write_checkpoint, ResumeStatus, STEPS_ORDERED,
};
use dirs::Dirs;
use pipeline::{
    run_bam_readcount, run_copycaller, run_copynumber, run_filter_input,
    run_flagstats, run_mpileup, run_process_somatic, run_somatic, Config,
};
use tui::{fmt_duration, render, render_final_frame, JobSlotSnapshot, RenderSnapshot};

static CANCELLED: AtomicBool = AtomicBool::new(false);

// ─── SHA worker pool ──────────────────────────────────────────────────────────

// Steps whose total output exceeds this threshold get deferred SHA computation.
const SHA_INLINE_THRESHOLD: u64 = 100 * 1024 * 1024; // 100 MB

struct ShaJob {
    output_dir: PathBuf,
    pair_id:    String,
    step:       &'static str,
    dirs:       Arc<Dirs>,
    normal:     String,
    tumor:      String,
}

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
    #[arg(long)]
    varscan: PathBuf,

    /// java binary path
    #[arg(long, default_value = "java")]
    java: String,

    /// Java heap size for VarScan (e.g. 24g, 48g)
    #[arg(long, default_value = "24g")]
    java_mem: String,

    /// samtools binary path
    #[arg(long, default_value = "samtools")]
    samtools: String,

    /// bam-readcount binary path
    #[arg(long, default_value = "bam-readcount")]
    bam_readcount: String,

    /// Number of tumor–normal pairs to process in parallel
    #[arg(short, long, default_value_t = 8)]
    jobs: usize,

    /// List pairs and resume status without running anything
    #[arg(long)]
    dry_run: bool,

    // ── VarScan somatic ─────────────────────────────────────────────────────────

    /// Minimum read depth across both samples to call a variant
    #[arg(long, default_value_t = 10)]
    min_coverage: u32,

    /// Minimum read depth in the normal sample
    #[arg(long, default_value_t = 10)]
    min_coverage_normal: u32,

    /// Minimum read depth in the tumor sample
    #[arg(long, default_value_t = 15)]
    min_coverage_tumor: u32,

    /// Minimum variant allele frequency to report a variant
    #[arg(long, default_value_t = 0.08)]
    min_var_freq: f64,

    /// Minimum allele frequency to call homozygous
    #[arg(long, default_value_t = 0.75)]
    min_freq_for_hom: f64,

    /// Estimated normal sample purity (1.0 = pure normal)
    #[arg(long, default_value_t = 1.0)]
    normal_purity: f64,

    /// Estimated tumor sample purity (1.0 = pure tumor)
    #[arg(long, default_value_t = 1.0)]
    tumor_purity: f64,

    /// p-value threshold for calling a variant (somatic/germline/LOH classification)
    #[arg(long, default_value_t = 0.99)]
    p_value: f64,

    /// p-value threshold for somatic classification (TCGA recommendation: 0.05)
    #[arg(long, default_value_t = 0.05)]
    somatic_p_value: f64,

    /// Minimum tumor VAF to retain in processSomatic high-confidence filter
    #[arg(long, default_value_t = 0.10)]
    min_tumor_freq: f64,

    /// Maximum normal VAF allowed in processSomatic high-confidence filter
    #[arg(long, default_value_t = 0.05)]
    max_normal_freq: f64,

    /// p-value threshold for processSomatic high-confidence filter
    #[arg(long, default_value_t = 0.07)]
    process_p_value: f64,

    // ── VarScan copynumber ───────────────────────────────────────────────────────

    /// Minimum read depth for copy number analysis
    #[arg(long, default_value_t = 10)]
    cnv_min_coverage: u32,

    /// Minimum segment size (windows) for copy number calls
    #[arg(long, default_value_t = 20)]
    min_segment_size: u32,

    /// Maximum segment size (windows) for copy number calls
    #[arg(long, default_value_t = 100)]
    max_segment_size: u32,

    /// p-value threshold for copy number segment calling
    #[arg(long, default_value_t = 0.005)]
    cnv_p_value: f64,

    // ── samtools mpileup ─────────────────────────────────────────────────────────

    /// Minimum mapping quality for samtools mpileup (-q)
    #[arg(long, default_value_t = 20)]
    mpileup_mapq: u32,
}

// ─── Sample pair ──────────────────────────────────────────────────────────────

#[derive(Clone)]
struct Pair {
    normal:  String,
    tumor:   String,
    pair_id: String,
}

// ─── Per-pair run summary ─────────────────────────────────────────────────────

#[derive(Clone)]
struct PairSummary {
    pair_id:      String,
    normal:       String,
    tumor:        String,
    status:       &'static str,   // "complete" | "failed" | "cancelled" | "skipped"
    stages_run:   usize,
    stages_cached: usize,
    duration_s:   f64,
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
    pair_results:   Mutex<Vec<PairSummary>>,
    start:          Instant,
    n_pairs:        usize,
    global_log:     Option<Mutex<fs::File>>,
}

#[derive(Clone)]
struct SlotInfo {
    pair_id: String,
    stage:   String,
    started: Instant,
}

impl State {
    fn new(n_pairs: usize, n_workers: usize, global_log: Option<fs::File>) -> Self {
        State {
            stages_done:    AtomicUsize::new(0),
            stages_total:   n_pairs * STEPS_ORDERED.len(),
            pairs_done:     AtomicUsize::new(0),
            pairs_failed:   AtomicUsize::new(0),
            stages_resumed: AtomicUsize::new(0),
            slots:          Mutex::new(vec![None; n_workers]),
            events:         Mutex::new(VecDeque::with_capacity(64)),
            pair_durations: Mutex::new(Vec::new()),
            pair_results:   Mutex::new(Vec::with_capacity(n_pairs)),
            start:          Instant::now(),
            n_pairs,
            global_log:     global_log.map(Mutex::new),
        }
    }
}

fn push_event(state: &Arc<State>, msg: String) {
    let ts   = Local::now().format("%H:%M:%S");
    let line = format!("  [{ts}] {msg}");
    let mut ev = state.events.lock().unwrap_or_else(|e| e.into_inner());
    if ev.len() >= 200 { ev.pop_front(); }
    ev.push_back(line.clone());
    if let Some(ref mtx) = state.global_log {
        if let Ok(mut f) = mtx.lock() {
            let _ = f.write_all(line.as_bytes());
            let _ = f.write_all(b"\n");
        }
    }
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

    let _ = n_workers;

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
    pair:        &Pair,
    slot_idx:    usize,
    cfg:         &Config,
    dirs:        &Dirs,
    state:       &Arc<State>,
    sha_queue:   &Arc<Mutex<VecDeque<ShaJob>>>,
    resume_from: usize,
    pair_start:  Instant,
) {
    let pid = &pair.pair_id;
    let n   = &*pair.normal;
    let t   = &*pair.tumor;

    let mut local_run    = 0usize;
    let local_cached = resume_from;   // pre-verified stages counted upfront

    // Per-sample log: append so reruns accumulate history
    let sample_log_path = dirs.logs.join(format!("{pid}.log"));
    let mut sample_log = fs::OpenOptions::new()
        .create(true).append(true)
        .open(&sample_log_path)
        .ok();

    let mut log_sample = |msg: &str| {
        if let Some(ref mut f) = sample_log {
            let ts = Local::now().format("%H:%M:%S");
            let _ = f.write_all(format!("[{ts}] {msg}\n").as_bytes());
        }
    };

    // Credit pre-verified stages to global counters immediately
    for i in 0..resume_from {
        let step = STEPS_ORDERED[i];
        state.stages_done.fetch_add(1, Ordering::Relaxed);
        state.stages_resumed.fetch_add(1, Ordering::Relaxed);
        let msg = format!("SKIP  {pid} — {} (pre-verified)", step_display(step));
        log_sample(&msg);
        push_event(state, msg);
    }

    for (i, &step) in STEPS_ORDERED.iter().enumerate() {
        if i < resume_from { continue; }

        if CANCELLED.load(Ordering::Relaxed) {
            let elapsed = pair_start.elapsed().as_secs_f64();
            let msg = format!("CANCEL {pid} — interrupted at {}", step_display(step));
            log_sample(&msg);
            push_event(state, msg);
            state.pair_results.lock().unwrap_or_else(|e| e.into_inner())
                .push(PairSummary {
                    pair_id: pid.clone(), normal: n.to_string(), tumor: t.to_string(),
                    status: "cancelled", stages_run: local_run, stages_cached: local_cached,
                    duration_s: elapsed,
                });
            let mut slots = state.slots.lock().unwrap_or_else(|e| e.into_inner());
            slots[slot_idx] = None;
            return;
        }

        {
            let mut slots = state.slots.lock().unwrap_or_else(|e| e.into_inner());
            slots[slot_idx] = Some(SlotInfo {
                pair_id: format!("{t} / {n}"),
                stage:   step.to_string(),
                started: Instant::now(),
            });
        }

        invalidate_downstream(&cfg.output_dir, pid, step);

        let step_log = dirs.logs.join(format!("{pid}.{step}.log"));
        let result: Result<(), String> = match step {
            "flagstats"       => run_flagstats(cfg, dirs, n, t, &step_log).map_err(|e| e.message),
            "mpileup"         => run_mpileup(cfg, dirs, n, t, &step_log).map_err(|e| e.message),
            "somatic"         => run_somatic(cfg, dirs, n, t, &step_log).map_err(|e| e.message),
            "process_somatic" => run_process_somatic(cfg, dirs, t, &step_log).map_err(|e| e.message),
            "copynumber"      => run_copynumber(cfg, dirs, n, t, &step_log).map_err(|e| e.message),
            "copycaller"      => run_copycaller(cfg, dirs, t, &step_log).map_err(|e| e.message),
            "filter_input"    => run_filter_input(dirs, t).map_err(|e| e.message),
            "bam_readcount"   => run_bam_readcount(cfg, dirs, t, &step_log).map_err(|e| e.message),
            _                 => Err("unknown step".to_string()),
        };

        match result {
            Ok(()) => {
                let out_size = step_output_size(dirs, n, t, step);
                if out_size <= SHA_INLINE_THRESHOLD {
                    let ckpt = compute_checkpoint(dirs, n, t, step);
                    let _ = write_checkpoint(&cfg.output_dir, pid, step, &ckpt);
                } else {
                    let _ = write_checkpoint(&cfg.output_dir, pid, step,
                        &pending_checkpoint(dirs, n, t, step));
                    sha_queue.lock().unwrap_or_else(|e| e.into_inner())
                        .push_back(ShaJob {
                            output_dir: cfg.output_dir.clone(),
                            pair_id:    pid.clone(),
                            step,
                            dirs:       Arc::new(dirs.clone()),
                            normal:     n.to_string(),
                            tumor:      t.to_string(),
                        });
                }
                state.stages_done.fetch_add(1, Ordering::Relaxed);
                local_run += 1;
                let msg = format!("DONE  {pid} — {}", step_display(step));
                log_sample(&msg);
                push_event(state, msg);
            }
            Err(msg) => {
                let ev = format!("FAIL  {pid} — {}: {msg}", step_display(step));
                log_sample(&ev);
                push_event(state, ev);
                state.pairs_failed.fetch_add(1, Ordering::Relaxed);
                let elapsed = pair_start.elapsed().as_secs_f64();
                state.pair_results.lock().unwrap_or_else(|e| e.into_inner())
                    .push(PairSummary {
                        pair_id: pid.clone(), normal: n.to_string(), tumor: t.to_string(),
                        status: "failed", stages_run: local_run, stages_cached: local_cached,
                        duration_s: elapsed,
                    });
                let mut slots = state.slots.lock().unwrap_or_else(|e| e.into_inner());
                slots[slot_idx] = None;
                return;
            }
        }
    }

    let elapsed = pair_start.elapsed().as_secs_f64();
    state.pairs_done.fetch_add(1, Ordering::Relaxed);
    state.pair_durations.lock().unwrap_or_else(|e| e.into_inner()).push(elapsed);

    let status = if local_run == 0 { "skipped" } else { "complete" };
    state.pair_results.lock().unwrap_or_else(|e| e.into_inner())
        .push(PairSummary {
            pair_id: pid.clone(), normal: n.to_string(), tumor: t.to_string(),
            status, stages_run: local_run, stages_cached: local_cached,
            duration_s: elapsed,
        });

    let msg = format!("DONE  {pid} — complete ({:.0}s)", elapsed);
    log_sample(&msg);
    push_event(state, msg);

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

// ─── Preflight ────────────────────────────────────────────────────────────────

fn can_spawn(bin: &str, args: &[&str]) -> bool {
    std::process::Command::new(bin)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn preflight(cfg: &Config) -> Vec<String> {
    let mut errs = Vec::new();

    if !can_spawn(&cfg.samtools, &["--version"]) {
        errs.push(format!("samtools not found or not executable: {:?}\n  Fix: apt install samtools  OR  --samtools /path/to/samtools", cfg.samtools));
    }
    if !can_spawn(&cfg.java, &["-version"]) {
        errs.push(format!("java not found or not executable: {:?}\n  Fix: install Java ≥11  OR  --java /path/to/java", cfg.java));
    }
    if !cfg.varscan_jar.exists() {
        errs.push(format!("VarScan jar not found: {}\n  Fix: download VarScan2  OR  --varscan /path/to/VarScan.jar", cfg.varscan_jar.display()));
    }
    if !can_spawn(&cfg.bam_readcount_bin, &[]) {
        errs.push(format!("bam-readcount not found or not executable: {:?}\n  Fix: conda install -c bioconda bam-readcount  OR  --bam-readcount /path/to/bam-readcount", cfg.bam_readcount_bin));
    }
    if !cfg.genome.exists() {
        errs.push(format!("Reference genome not found: {}", cfg.genome.display()));
    }
    if !cfg.bam_dir.exists() {
        errs.push(format!("BAM directory not found: {}", cfg.bam_dir.display()));
    }
    errs
}

// ─── Summary TSV ─────────────────────────────────────────────────────────────

fn write_summary(results: &[PairSummary], output_dir: &std::path::Path) {
    let path = output_dir.join("varscan_pipeline_summary.tsv");
    let mut buf = String::from("pair_id\tnormal\ttumor\tstatus\tstages_run\tstages_cached\tduration_s\n");
    for r in results {
        buf.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{:.1}\n",
            r.pair_id, r.normal, r.tumor,
            r.status, r.stages_run, r.stages_cached, r.duration_s,
        ));
    }
    if let Ok(mut f) = fs::File::create(&path) {
        let _ = f.write_all(buf.as_bytes());
    }
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

// ─── Terminal guard ───────────────────────────────────────────────────────────

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

    // Create base output dir then canonicalize for stable absolute paths
    if let Err(e) = fs::create_dir_all(&cli.output) {
        eprintln!("ERROR: output dir: {e}"); return ExitCode::FAILURE;
    }
    let output_canon = cli.output.canonicalize().unwrap_or_else(|_| cli.output.clone());

    let dirs = Arc::new(Dirs::new(&output_canon));
    if let Err(e) = dirs.create_all() {
        eprintln!("ERROR: dir setup: {e}"); return ExitCode::FAILURE;
    }

    let bam_dir_canon = cli.bam_dir.canonicalize().unwrap_or_else(|_| cli.bam_dir.clone());
    let cfg = Arc::new(Config {
        bam_dir:             bam_dir_canon,
        output_dir:          output_canon.clone(),
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

    // ── Pre-run resume scan ──
    let pair_triples: Vec<(String, String, String)> = pairs.iter()
        .map(|p| (p.pair_id.clone(), p.normal.clone(), p.tumor.clone()))
        .collect();
    let resume_results = check_resume_all_parallel(&output_canon, &pair_triples, &dirs);
    let resume_map: std::collections::HashMap<String, ResumeStatus> =
        resume_results.into_iter().collect();

    let (n_complete, n_partial, n_fresh) = pairs.iter().fold((0usize, 0usize, 0usize),
        |acc, p| match resume_map.get(&p.pair_id) {
            Some(r) if r.is_all_done()  => (acc.0 + 1, acc.1,     acc.2    ),
            Some(r) if r.is_fresh()     => (acc.0,     acc.1,     acc.2 + 1),
            None                        => (acc.0,     acc.1,     acc.2 + 1),
            _                           => (acc.0,     acc.1 + 1, acc.2    ),
        });
    eprintln!("Resume scan: {n_pairs} pairs — {n_complete} complete, {n_partial} partial, {n_fresh} fresh");

    // ── Dry-run ──
    if cli.dry_run {
        let col_pair = 42usize;
        let col_sta  = 10usize;
        let col_stg  = 8usize;
        eprintln!();
        eprintln!("  {:<col_pair$}  {:<col_sta$}  {:<col_stg$}  Note",
            "Pair", "Status", "Stages");
        eprintln!("  {}", "─".repeat(col_pair + col_sta + col_stg + 30));
        for pair in &pairs {
            let rs = resume_map.get(&pair.pair_id)
                .cloned()
                .unwrap_or(ResumeStatus::NotDone);
            let cached = rs.cached_count();
            let total  = STEPS_ORDERED.len();
            let note   = match &rs {
                ResumeStatus::AllDone      => "will be skipped".to_string(),
                ResumeStatus::NotDone      => "will run all stages".to_string(),
                ResumeStatus::FromStep(n)  =>
                    format!("resumes from stage {} ({})", n + 1, step_display(STEPS_ORDERED[*n])),
            };
            eprintln!("  {:<col_pair$}  {:<col_sta$}  {}/{:<5}  {}",
                pair.pair_id, rs.display_status(), cached, total, note);
        }
        eprintln!();
        eprintln!("  Use without --dry-run to execute.");
        return ExitCode::SUCCESS;
    }

    // ── Preflight ──
    let pf_errors = preflight(&cfg);
    if !pf_errors.is_empty() {
        eprintln!("Preflight failed:");
        for e in &pf_errors { eprintln!("  ERROR: {e}"); }
        return ExitCode::FAILURE;
    }

    // ── Pre-dispatch routing: pre-emptive cleanup + upfront summary ──
    let annotated_pairs: VecDeque<(Pair, usize)> = pairs.iter().map(|pair| {
        let rs   = resume_map.get(&pair.pair_id).cloned().unwrap_or(ResumeStatus::NotDone);
        let from = rs.resume_from();
        if from < STEPS_ORDERED.len() {
            pre_run_cleanup(&output_canon, &pair.pair_id, from, &dirs, &pair.normal, &pair.tumor);
            match &rs {
                ResumeStatus::NotDone =>
                    eprintln!("  FRESH   {} — all {} stages", pair.pair_id, STEPS_ORDERED.len()),
                ResumeStatus::FromStep(n) =>
                    eprintln!("  RESUME  {} — {} cached, rerunning from {} ({})",
                        pair.pair_id, n, n + 1, step_display(STEPS_ORDERED[*n])),
                ResumeStatus::AllDone => {}
            }
        }
        (pair.clone(), from)
    }).collect();

    let n_workers  = cli.jobs.max(1).min(n_pairs);
    let global_log = fs::OpenOptions::new()
        .create(true).append(true)
        .open(dirs.logs.join("varscan_runner.log"))
        .ok();
    let state = Arc::new(State::new(n_pairs, n_workers, global_log));
    let queue     = Arc::new(Mutex::new(annotated_pairs));
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

    // ── SHA worker pool ──
    let sha_queue: Arc<Mutex<VecDeque<ShaJob>>> = Arc::new(Mutex::new(VecDeque::new()));
    let sha_done  = Arc::new(AtomicBool::new(false));
    let n_sha     = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(4);
    let sha_handles: Vec<_> = (0..n_sha).map(|_| {
        let q    = Arc::clone(&sha_queue);
        let done = Arc::clone(&sha_done);
        std::thread::spawn(move || loop {
            let job = q.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
            match job {
                Some(j) => upgrade_checkpoint(
                    &j.output_dir, &j.pair_id, j.step, &j.dirs, &j.normal, &j.tumor,
                ),
                None => {
                    if done.load(Ordering::Relaxed) { break; }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        })
    }).collect();

    // ── Spawn workers ──
    let handles: Vec<_> = (0..n_workers).map(|slot_idx| {
        let queue     = Arc::clone(&queue);
        let state     = Arc::clone(&state);
        let cfg       = Arc::clone(&cfg);
        let dirs      = Arc::clone(&dirs);
        let active    = Arc::clone(&active);
        let sha_queue = Arc::clone(&sha_queue);
        std::thread::spawn(move || {
            loop {
                if CANCELLED.load(Ordering::Relaxed) { break; }
                let item = {
                    let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                    q.pop_front()
                };
                let Some((pair, resume_from)) = item else { break };
                let t0 = Instant::now();
                process_pair(&pair, slot_idx, &cfg, &dirs, &state, &sha_queue, resume_from, t0);
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
            let mut force_clear = false;
            while event::poll(Duration::ZERO).unwrap_or(false) {
                match event::read() {
                    Ok(event::Event::Key(k)) => match k.code {
                        event::KeyCode::Char('q') | event::KeyCode::Char('Q') => {
                            CANCELLED.store(true, Ordering::Relaxed);
                        }
                        event::KeyCode::Char('c')
                            if k.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                            CANCELLED.store(true, Ordering::Relaxed);
                        }
                        _ => {}
                    },
                    Ok(event::Event::Resize(_, _)) => { force_clear = true; }
                    _ => {}
                }
            }
            if force_clear {
                let mut clr = Vec::new();
                let _ = crossterm::queue!(
                    clr,
                    terminal::Clear(terminal::ClearType::All),
                    cursor::MoveTo(0, 0)
                );
                let _ = stdout.write_all(&clr);
            }
            let snap = assemble_snapshot(&state, n_workers);
            render(&mut stdout, &snap, n_workers, blink);
        }
        std::thread::sleep(refresh);
    }

    for h in handles { h.join().ok(); }

    // ── Drain remaining SHA jobs then shut down SHA pool ──
    sha_done.store(true, Ordering::Relaxed);
    for h in sha_handles { h.join().ok(); }

    // ── Write summary TSV ──
    let results = state.pair_results.lock().unwrap_or_else(|e| e.into_inner()).clone();
    write_summary(&results, &output_canon);

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
        let _ = event::poll(Duration::from_secs(10));
    }

    eprintln!();
    eprintln!("══════════════════════════════════════════════════════════════");
    eprintln!(" VarScan2 Runner — finished");
    eprintln!("══════════════════════════════════════════════════════════════");
    eprintln!(" Pairs completed : {pairs_done}/{n_pairs}");
    eprintln!(" Pairs failed    : {pairs_failed}");
    eprintln!(" Stages resumed  : {resumed} (SHA256 match → skipped)");
    eprintln!(" Total elapsed   : {}", fmt_duration(elapsed));
    eprintln!(" Output          : {}", output_canon.display());
    eprintln!(" Summary         : {}", output_canon.join("varscan_pipeline_summary.tsv").display());
    eprintln!("══════════════════════════════════════════════════════════════");

    if pairs_failed > 0 || CANCELLED.load(Ordering::Relaxed) { ExitCode::FAILURE }
    else { ExitCode::SUCCESS }
}
