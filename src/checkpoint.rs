use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

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

// ─── Output directories passed to sha256_step ────────────────────────────────

pub struct Dirs {
    pub flagstats:    PathBuf,
    pub mpileup:      PathBuf,
    pub somatic:      PathBuf,
    pub copynumber:   PathBuf,
    pub readcount:    PathBuf,
    pub filter_input: PathBuf,
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

/// Compute the expected SHA256 digest for a step's output files.
pub fn sha256_step(dirs: &Dirs, normal: &str, tumor: &str, step: &str) -> String {
    let files: Vec<PathBuf> = match step {
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
        _ => return "UNKNOWN_STEP".to_string(),
    };
    sha256_file_list(&files)
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

/// True if step output exists and SHA256 matches the stored checkpoint.
pub fn is_step_done(
    base: &Path,
    pair_id: &str,
    step: &str,
    dirs: &Dirs,
    normal: &str,
    tumor: &str,
) -> bool {
    match read_checkpoint(base, pair_id, step) {
        Some(saved) => saved == sha256_step(dirs, normal, tumor, step),
        None => false,
    }
}

/// Remove checkpoints for all steps downstream of `changed_step`.
pub fn invalidate_downstream(base: &Path, pair_id: &str, changed_step: &str) {
    let pos = STEPS_ORDERED.iter().position(|&s| s == changed_step).unwrap_or(0);
    for step in STEPS_ORDERED.iter().skip(pos + 1) {
        let _ = fs::remove_file(checkpoint_path(base, pair_id, step));
    }
}

pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let name   = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp    = parent.join(format!(".{name}.tmp.{}", std::process::id()));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).inspect_err(|_| { let _ = fs::remove_file(&tmp); })
}
