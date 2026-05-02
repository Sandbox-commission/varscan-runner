use crate::dirs::Dirs;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

// ─── Stage constants ──────────────────────────────────────────────────────────

pub const STEP_FLAGSTATS:       &str = "flagstats";
pub const STEP_MPILEUP:         &str = "mpileup";
pub const STEP_SOMATIC:         &str = "somatic";
pub const STEP_PROCESS_SOMATIC: &str = "process_somatic";
pub const STEP_COPYNUMBER:      &str = "copynumber";
pub const STEP_COPYCALLER:      &str = "copycaller";
pub const STEP_FILTER_INPUT:    &str = "filter_input";
pub const STEP_BAM_READCOUNT:   &str = "bam_readcount";

pub const STEPS_ORDERED: &[&str] = &[
    STEP_FLAGSTATS,
    STEP_MPILEUP,
    STEP_SOMATIC,
    STEP_PROCESS_SOMATIC,
    STEP_COPYNUMBER,
    STEP_COPYCALLER,
    STEP_FILTER_INPUT,
    STEP_BAM_READCOUNT,
];

pub fn step_display(step: &str) -> &'static str {
    match step {
        STEP_FLAGSTATS       => "Flagstats",
        STEP_MPILEUP         => "Mpileup",
        STEP_SOMATIC         => "VS Somatic",
        STEP_PROCESS_SOMATIC => "procSomatic",
        STEP_COPYNUMBER      => "CopyNumber",
        STEP_COPYCALLER      => "CopyCaller",
        STEP_FILTER_INPUT    => "Filter Prep",
        STEP_BAM_READCOUNT   => "BAM RC",
        _                    => "Unknown",
    }
}

// ─── SHA256 helpers ───────────────────────────────────────────────────────────

pub fn sha256_file(path: &Path) -> Result<Vec<u8>, String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut file = File::open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_vec())
}

fn sha256_file_list(files: &[PathBuf]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for path in files {
        hasher.update(path.file_name().unwrap_or_default().to_string_lossy().as_bytes());
        if path.exists() {
            let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            if size > 0 {
                match sha256_file(path) {
                    Ok(bytes) => hasher.update(&bytes),
                    Err(_)    => hasher.update(b"__READ_ERR__"),
                }
            } else {
                hasher.update(b"__ZERO__");
            }
        } else {
            hasher.update(b"__MISSING__");
        }
    }
    format!("{:x}", hasher.finalize())
}

fn step_files(dirs: &Dirs, normal: &str, tumor: &str, step: &str) -> Vec<PathBuf> {
    match step {
        STEP_FLAGSTATS => vec![
            dirs.flagstats.join(format!("{normal}.flagstats")),
            dirs.flagstats.join(format!("{tumor}.flagstats")),
        ],
        STEP_MPILEUP => vec![
            dirs.mpileup.join(format!("{normal}_{tumor}.mpileup")),
        ],
        STEP_SOMATIC => vec![
            dirs.somatic.join(format!("{tumor}.snp.vcf")),
            dirs.somatic.join(format!("{tumor}.indel.vcf")),
        ],
        STEP_PROCESS_SOMATIC => vec![
            dirs.somatic.join(format!("{tumor}.snp.Somatic.hc.vcf")),
            dirs.somatic.join(format!("{tumor}.snp.Germline.hc.vcf")),
            dirs.somatic.join(format!("{tumor}.snp.LOH.hc.vcf")),
            dirs.somatic.join(format!("{tumor}.indel.Somatic.hc.vcf")),
        ],
        STEP_COPYNUMBER => vec![
            dirs.copynumber.join(format!("{tumor}.copynumber")),
        ],
        STEP_COPYCALLER => vec![
            dirs.copynumber.join(format!("{tumor}.copynumber.called")),
            dirs.copynumber.join(format!("{tumor}.copynumber.homdel")),
        ],
        STEP_FILTER_INPUT => vec![
            dirs.filter_input.join(format!("{tumor}.snp.Somatic.hc.var")),
        ],
        STEP_BAM_READCOUNT => vec![
            dirs.readcount.join(format!("{tumor}.snp.Somatic.hc.readcount")),
        ],
        _ => vec![],
    }
}

fn file_mtime_secs(path: &Path) -> u64 {
    fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Checkpoint format: "sha256hex size1:mtime1 size2:mtime2 ..."
/// One size:mtime token per output file, in step_files order.
pub fn compute_checkpoint(dirs: &Dirs, normal: &str, tumor: &str, step: &str) -> String {
    let files = step_files(dirs, normal, tumor, step);
    let sha256 = sha256_file_list(&files);
    let meta: Vec<String> = files.iter().map(|p| {
        let size  = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let mtime = file_mtime_secs(p);
        format!("{size}:{mtime}")
    }).collect();
    format!("{sha256} {}", meta.join(" "))
}

// ─── Checkpoint I/O ───────────────────────────────────────────────────────────

pub fn checkpoint_dir(base: &Path) -> PathBuf {
    base.join(".checkpoints")
}

pub fn checkpoint_path(base: &Path, pair_id: &str, step: &str) -> PathBuf {
    checkpoint_dir(base).join(format!("{pair_id}.{step}.sha256"))
}

pub fn write_checkpoint(base: &Path, pair_id: &str, step: &str, digest: &str) -> io::Result<()> {
    fs::create_dir_all(checkpoint_dir(base))?;
    atomic_write(&checkpoint_path(base, pair_id, step), digest.as_bytes())
}

pub fn read_checkpoint(base: &Path, pair_id: &str, step: &str) -> Option<String> {
    fs::read_to_string(checkpoint_path(base, pair_id, step))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Total byte size of all output files for a step.
/// Used to decide inline vs deferred SHA computation.
pub fn step_output_size(dirs: &Dirs, normal: &str, tumor: &str, step: &str) -> u64 {
    step_files(dirs, normal, tumor, step)
        .iter()
        .map(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .sum()
}

/// Immediate checkpoint — records size+mtime without blocking on SHA.
/// Prefix "PENDING" signals the SHA worker to upgrade this later.
pub fn pending_checkpoint(dirs: &Dirs, normal: &str, tumor: &str, step: &str) -> String {
    let files = step_files(dirs, normal, tumor, step);
    let meta: Vec<String> = files.iter().map(|p| {
        let size  = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let mtime = file_mtime_secs(p);
        format!("{size}:{mtime}")
    }).collect();
    format!("PENDING {}", meta.join(" "))
}

/// Called by the SHA worker pool: recomputes SHA256 and rewrites the checkpoint.
pub fn upgrade_checkpoint(
    base:    &Path,
    pair_id: &str,
    step:    &str,
    dirs:    &Dirs,
    normal:  &str,
    tumor:   &str,
) {
    let files  = step_files(dirs, normal, tumor, step);
    let sha256 = sha256_file_list(&files);
    let meta: Vec<String> = files.iter().map(|p| {
        let size  = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let mtime = file_mtime_secs(p);
        format!("{size}:{mtime}")
    }).collect();
    let _ = write_checkpoint(base, pair_id, step, &format!("{sha256} {}", meta.join(" ")));
}

/// True if step outputs are unchanged since the last checkpoint write.
///
/// Three checkpoint formats handled:
///   PENDING s:m s:m …   — SHA not yet computed; accept via size+mtime fast path only
///   sha256hex s:m s:m … — fully verified; fast path first, SHA fallback
///   sha256hex           — legacy format; SHA slow path only
pub fn is_step_done(
    base:    &Path,
    pair_id: &str,
    step:    &str,
    dirs:    &Dirs,
    normal:  &str,
    tumor:   &str,
) -> bool {
    let stored = match read_checkpoint(base, pair_id, step) {
        Some(s) => s,
        None    => return false,
    };

    let mut tokens    = stored.split_whitespace();
    let first         = match tokens.next() { Some(s) => s, None => return false };
    let stored_meta: Vec<&str> = tokens.collect();
    let files         = step_files(dirs, normal, tumor, step);

    let is_pending    = first == "PENDING";
    let stored_sha    = if is_pending { "" } else { first };

    // Fast path: size+mtime match for all output files
    if !stored_meta.is_empty() && stored_meta.len() == files.len() {
        let all_match = files.iter().zip(stored_meta.iter()).all(|(path, meta)| {
            let mut kv      = meta.split(':');
            let s_size: u64 = kv.next().and_then(|v| v.parse().ok()).unwrap_or(u64::MAX);
            let s_mtime: u64 = kv.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            if s_mtime == 0 { return false; }
            let m       = fs::metadata(path).ok();
            let a_size  = m.as_ref().map(|m| m.len()).unwrap_or(u64::MAX - 1);
            let a_mtime = file_mtime_secs(path);
            a_size == s_size && a_mtime == s_mtime
        });
        if all_match { return true; }
    }

    // PENDING checkpoints have no SHA — cannot fall back; treat as not done
    if is_pending { return false; }

    // Slow path: full SHA256 recompute (legacy format or mtime mismatch)
    sha256_file_list(&files) == stored_sha
}

/// Remove checkpoints for all steps downstream of `changed_step`.
pub fn invalidate_downstream(base: &Path, pair_id: &str, changed_step: &str) {
    let pos = STEPS_ORDERED.iter().position(|&s| s == changed_step).unwrap_or(0);
    for step in STEPS_ORDERED.iter().skip(pos + 1) {
        let _ = fs::remove_file(checkpoint_path(base, pair_id, step));
    }
}

pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let parent = path.parent().unwrap_or(Path::new("."));
    let name   = path.file_name().unwrap_or_default().to_string_lossy();
    let tid    = {
        let mut h = DefaultHasher::new();
        std::thread::current().id().hash(&mut h);
        h.finish()
    };
    let tmp = parent.join(format!(".{name}.tmp{}-{tid:x}", std::process::id()));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).inspect_err(|_| { let _ = fs::remove_file(&tmp); })
}

// ─── Pre-run resume state ─────────────────────────────────────────────────────

/// Per-pair resume decision produced by the pre-run scan.
///
/// `AllDone`     — all 8 stages have valid checkpoints; pair will be skipped entirely.
/// `FromStep(n)` — stages 0..n are valid; rerun starts at stage n.
/// `NotDone`     — no valid checkpoints; all stages run from scratch.
#[derive(Clone)]
pub enum ResumeStatus {
    AllDone,
    FromStep(usize),
    NotDone,
}

impl ResumeStatus {
    pub fn is_all_done(&self) -> bool { matches!(self, Self::AllDone) }
    pub fn is_fresh(&self)    -> bool { matches!(self, Self::NotDone) }

    /// First stage index that needs to run (0-based into STEPS_ORDERED).
    /// Returns STEPS_ORDERED.len() for AllDone.
    pub fn resume_from(&self) -> usize {
        match self {
            Self::AllDone     => STEPS_ORDERED.len(),
            Self::FromStep(n) => *n,
            Self::NotDone     => 0,
        }
    }

    /// Number of stages whose checkpoints are valid.
    pub fn cached_count(&self) -> usize { self.resume_from().min(STEPS_ORDERED.len()) }

    pub fn display_status(&self) -> &'static str {
        match self {
            Self::AllDone    => "COMPLETE",
            Self::FromStep(_) => "PARTIAL",
            Self::NotDone    => "FRESH",
        }
    }
}

pub fn check_resume(
    base:    &Path,
    pair_id: &str,
    normal:  &str,
    tumor:   &str,
    dirs:    &Dirs,
) -> ResumeStatus {
    for (i, &step) in STEPS_ORDERED.iter().enumerate() {
        if !is_step_done(base, pair_id, step, dirs, normal, tumor) {
            return if i == 0 { ResumeStatus::NotDone } else { ResumeStatus::FromStep(i) };
        }
    }
    ResumeStatus::AllDone
}

/// Remove output files and checkpoints for all stages at or after `from_step`.
/// Called before workers start so partial outputs from a prior interrupted run
/// are cleaned before the stages rerun.
pub fn pre_run_cleanup(
    base:      &Path,
    pair_id:   &str,
    from_step: usize,
    dirs:      &Dirs,
    normal:    &str,
    tumor:     &str,
) {
    for (i, &step) in STEPS_ORDERED.iter().enumerate() {
        if i < from_step { continue; }
        for path in step_files(dirs, normal, tumor, step) {
            let _ = fs::remove_file(&path);
        }
        let _ = fs::remove_file(checkpoint_path(base, pair_id, step));
    }
}

/// Check resume state for all pairs in parallel (up to 32 threads).
/// Returns results in the same order as the input slice.
pub fn check_resume_all_parallel(
    base:  &Path,
    pairs: &[(String, String, String)],   // (pair_id, normal, tumor)
    dirs:  &Dirs,
) -> Vec<(String, ResumeStatus)> {
    let n         = pairs.len();
    let n_threads = n.min(32).max(1);
    let results   = Arc::new(Mutex::new(Vec::<(String, ResumeStatus)>::with_capacity(n)));
    let next      = Arc::new(AtomicUsize::new(0));
    let pairs_arc = Arc::new(pairs.to_vec());
    let base_arc  = Arc::new(base.to_path_buf());
    let dirs_arc  = Arc::new(dirs.clone());

    let handles: Vec<_> = (0..n_threads).map(|_| {
        let results = Arc::clone(&results);
        let next    = Arc::clone(&next);
        let pairs   = Arc::clone(&pairs_arc);
        let base    = Arc::clone(&base_arc);
        let dirs    = Arc::clone(&dirs_arc);
        std::thread::spawn(move || loop {
            let idx = next.fetch_add(1, Ordering::Relaxed);
            if idx >= pairs.len() { break; }
            let (ref pid, ref norm, ref tum) = pairs[idx];
            let status = check_resume(&base, pid, norm, tum, &dirs);
            results.lock().unwrap_or_else(|e| e.into_inner())
                .push((pid.clone(), status));
        })
    }).collect();

    for h in handles {
        if h.join().is_err() {
            eprintln!("Warning: resume-check thread panicked — affected pairs will reprocess");
        }
    }

    let mut out: Vec<(String, ResumeStatus)> =
        results.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let order: std::collections::HashMap<&str, usize> =
        pairs.iter().enumerate().map(|(i, (pid, _, _))| (pid.as_str(), i)).collect();
    out.sort_by_key(|(pid, _)| order.get(pid.as_str()).copied().unwrap_or(usize::MAX));
    out
}
