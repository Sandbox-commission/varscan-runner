use crate::checkpoint::Dirs;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;

// ─── Config (pipeline parameters) ────────────────────────────────────────────

pub struct Config {
    pub bam_dir:          PathBuf,
    pub output_dir:       PathBuf,
    pub genome:           PathBuf,
    pub varscan_jar:      PathBuf,
    pub java:             String,
    pub java_mem:         String,
    pub samtools:         String,
    pub bam_readcount_bin: String,
    // VarScan somatic parameters
    pub min_coverage:         u32,
    pub min_coverage_normal:  u32,
    pub min_coverage_tumor:   u32,
    pub min_var_freq:         f64,
    pub min_freq_for_hom:     f64,
    pub normal_purity:        f64,
    pub tumor_purity:         f64,
    pub p_value:              f64,
    pub somatic_p_value:      f64,
    pub min_tumor_freq:       f64,
    pub max_normal_freq:      f64,
    pub process_p_value:      f64,
    // VarScan copynumber parameters
    pub cnv_min_coverage:  u32,
    pub min_segment_size:  u32,
    pub max_segment_size:  u32,
    pub cnv_p_value:       f64,
    // samtools mpileup
    pub mpileup_mapq: u32,
}

// ─── Error type ───────────────────────────────────────────────────────────────

pub struct StepError {
    pub step: String,
    pub message: String,
}

impl StepError {
    fn new(step: &str, msg: impl Into<String>) -> Self {
        Self { step: step.to_string(), message: msg.into() }
    }
}

// ─── Directory setup ──────────────────────────────────────────────────────────

pub fn make_dirs(output_dir: &Path) -> std::io::Result<Dirs> {
    let dirs = Dirs {
        flagstats:    output_dir.join("flagstats"),
        mpileup:      output_dir.join("mpileup"),
        somatic:      output_dir.join("somatic"),
        copynumber:   output_dir.join("copynumber"),
        readcount:    output_dir.join("readcount"),
        filter_input: output_dir.join("filter-input"),
    };
    for d in [
        &dirs.flagstats, &dirs.mpileup, &dirs.somatic,
        &dirs.copynumber, &dirs.readcount, &dirs.filter_input,
    ] {
        fs::create_dir_all(d)?;
    }
    fs::create_dir_all(output_dir.join("filtered"))?;
    Ok(dirs)
}

// ─── Stage 1: samtools flagstat ────────────────────────────────────────────────

pub fn run_flagstats(cfg: &Config, dirs: &Dirs, normal: &str, tumor: &str) -> Result<(), StepError> {
    const STEP: &str = "flagstats";
    for (sample, out_path) in [
        (normal, dirs.flagstats.join(format!("{normal}.flagstats"))),
        (tumor,  dirs.flagstats.join(format!("{tumor}.flagstats"))),
    ] {
        let bam = cfg.bam_dir.join(format!("{sample}_final.bam"));
        let out = std::process::Command::new(&cfg.samtools)
            .arg("flagstat")
            .arg(&bam)
            .output()
            .map_err(|e| StepError::new(STEP, format!("spawn samtools: {e}")))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(StepError::new(STEP, format!(
                "samtools flagstat {sample}: exit={} — {}",
                out.status.code().unwrap_or(-1),
                stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("(no stderr)")
            )));
        }
        fs::write(&out_path, &out.stdout)
            .map_err(|e| StepError::new(STEP, format!("write {}: {e}", out_path.display())))?;
    }
    Ok(())
}

// ─── Stage 2: samtools mpileup ────────────────────────────────────────────────

pub fn run_mpileup(cfg: &Config, dirs: &Dirs, normal: &str, tumor: &str) -> Result<(), StepError> {
    const STEP: &str = "mpileup";
    let out_path    = dirs.mpileup.join(format!("{normal}_{tumor}.mpileup"));
    let normal_bam  = cfg.bam_dir.join(format!("{normal}_final.bam"));
    let tumor_bam   = cfg.bam_dir.join(format!("{tumor}_final.bam"));
    let out_file    = fs::File::create(&out_path)
        .map_err(|e| StepError::new(STEP, format!("create {}: {e}", out_path.display())))?;

    let status = std::process::Command::new(&cfg.samtools)
        .args(["mpileup", "-B"])
        .arg("-q").arg(cfg.mpileup_mapq.to_string())
        .arg("-f").arg(&cfg.genome)
        .arg(&normal_bam)
        .arg(&tumor_bam)
        .stdout(out_file)
        .stderr(Stdio::null())
        .status()
        .map_err(|e| StepError::new(STEP, format!("spawn samtools: {e}")))?;

    if !status.success() {
        let _ = fs::remove_file(&out_path);
        return Err(StepError::new(STEP, format!("samtools mpileup exit={}", status.code().unwrap_or(-1))));
    }
    Ok(())
}

// ─── Stage 3: VarScan somatic ─────────────────────────────────────────────────

pub fn run_somatic(cfg: &Config, dirs: &Dirs, normal: &str, tumor: &str) -> Result<(), StepError> {
    const STEP: &str = "somatic";
    let mpileup    = dirs.mpileup.join(format!("{normal}_{tumor}.mpileup"));
    let out_prefix = dirs.somatic.join(tumor);

    let status = std::process::Command::new(&cfg.java)
        .arg(format!("-Xmx{}", cfg.java_mem))
        .arg("-jar").arg(&cfg.varscan_jar)
        .arg("somatic")
        .arg(&mpileup)
        .arg(&out_prefix)
        .arg("--mpileup").arg("1")
        .arg("--min-coverage").arg(cfg.min_coverage.to_string())
        .arg("--min-coverage-normal").arg(cfg.min_coverage_normal.to_string())
        .arg("--min-coverage-tumor").arg(cfg.min_coverage_tumor.to_string())
        .arg("--min-var-freq").arg(cfg.min_var_freq.to_string())
        .arg("--min-freq-for-hom").arg(cfg.min_freq_for_hom.to_string())
        .arg("--normal-purity").arg(cfg.normal_purity.to_string())
        .arg("--tumor-purity").arg(cfg.tumor_purity.to_string())
        .arg("--p-value").arg(cfg.p_value.to_string())
        .arg("--somatic-p-value").arg(cfg.somatic_p_value.to_string())
        .arg("--strand-filter").arg("1")
        .arg("--output-vcf").arg("1")
        .stderr(Stdio::null())
        .status()
        .map_err(|e| StepError::new(STEP, format!("spawn java: {e}")))?;

    if !status.success() {
        return Err(StepError::new(STEP, format!("VarScan somatic exit={}", status.code().unwrap_or(-1))));
    }
    Ok(())
}

// ─── Stage 4: VarScan processSomatic ─────────────────────────────────────────

pub fn run_process_somatic(cfg: &Config, dirs: &Dirs, tumor: &str) -> Result<(), StepError> {
    const STEP: &str = "process_somatic";
    for suffix in ["snp.vcf", "indel.vcf"] {
        let vcf = dirs.somatic.join(format!("{tumor}.{suffix}"));
        if !vcf.exists() { continue; }

        let status = std::process::Command::new(&cfg.java)
            .arg(format!("-Xmx{}", cfg.java_mem))
            .arg("-jar").arg(&cfg.varscan_jar)
            .arg("processSomatic")
            .arg(&vcf)
            .arg("--min-tumor-freq").arg(cfg.min_tumor_freq.to_string())
            .arg("--max-normal-freq").arg(cfg.max_normal_freq.to_string())
            .arg("--p-value").arg(cfg.process_p_value.to_string())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| StepError::new(STEP, format!("spawn java: {e}")))?;

        if !status.success() {
            return Err(StepError::new(STEP, format!(
                "processSomatic ({suffix}) exit={}", status.code().unwrap_or(-1)
            )));
        }
    }
    Ok(())
}

// ─── Data ratio helper for copynumber ────────────────────────────────────────

fn parse_mapped(flagstats: &Path) -> Option<u64> {
    let f = fs::File::open(flagstats).ok()?;
    BufReader::new(f).lines().flatten().find_map(|line| {
        // "12345 + 0 mapped (98.54% : N/A)"
        if line.contains("mapped (") {
            line.split_whitespace().next()?.parse().ok()
        } else {
            None
        }
    })
}

pub fn compute_data_ratio(dirs: &Dirs, normal: &str, tumor: &str) -> f64 {
    let n_path = dirs.flagstats.join(format!("{normal}.flagstats"));
    let t_path = dirs.flagstats.join(format!("{tumor}.flagstats"));
    match (parse_mapped(&n_path), parse_mapped(&t_path)) {
        (Some(n), Some(t)) if t > 0 => n as f64 / t as f64,
        _ => 1.0,
    }
}

// ─── Stage 5: VarScan copynumber ─────────────────────────────────────────────

pub fn run_copynumber(cfg: &Config, dirs: &Dirs, normal: &str, tumor: &str) -> Result<(), StepError> {
    const STEP: &str = "copynumber";
    let mpileup    = dirs.mpileup.join(format!("{normal}_{tumor}.mpileup"));
    let out_prefix = dirs.copynumber.join(tumor);
    let ratio      = compute_data_ratio(dirs, normal, tumor);

    let status = std::process::Command::new(&cfg.java)
        .arg(format!("-Xmx{}", cfg.java_mem))
        .arg("-jar").arg(&cfg.varscan_jar)
        .arg("copynumber")
        .arg(&mpileup)
        .arg(&out_prefix)
        .arg("--mpileup").arg("1")
        .arg("--min-coverage").arg(cfg.cnv_min_coverage.to_string())
        .arg("--min-segment-size").arg(cfg.min_segment_size.to_string())
        .arg("--max-segment-size").arg(cfg.max_segment_size.to_string())
        .arg("--p-value").arg(cfg.cnv_p_value.to_string())
        .arg("--data-ratio").arg(format!("{ratio:.6}"))
        .stderr(Stdio::null())
        .status()
        .map_err(|e| StepError::new(STEP, format!("spawn java: {e}")))?;

    if !status.success() {
        return Err(StepError::new(STEP, format!("VarScan copynumber exit={}", status.code().unwrap_or(-1))));
    }
    Ok(())
}

// ─── Stage 6: VarScan copyCaller ─────────────────────────────────────────────

pub fn run_copycaller(cfg: &Config, dirs: &Dirs, tumor: &str) -> Result<(), StepError> {
    const STEP: &str = "copycaller";
    let cnv = dirs.copynumber.join(format!("{tumor}.copynumber"));
    if !cnv.exists() {
        return Err(StepError::new(STEP, format!("{} not found — copynumber step may have failed", cnv.display())));
    }

    let status = std::process::Command::new(&cfg.java)
        .arg(format!("-Xmx{}", cfg.java_mem))
        .arg("-jar").arg(&cfg.varscan_jar)
        .arg("copyCaller")
        .arg(&cnv)
        .arg("--output-file")
            .arg(dirs.copynumber.join(format!("{tumor}.copynumber.called")))
        .arg("--output-homdel-file")
            .arg(dirs.copynumber.join(format!("{tumor}.copynumber.homdel")))
        .stderr(Stdio::null())
        .status()
        .map_err(|e| StepError::new(STEP, format!("spawn java: {e}")))?;

    if !status.success() {
        return Err(StepError::new(STEP, format!("copyCaller exit={}", status.code().unwrap_or(-1))));
    }
    Ok(())
}

// ─── Stage 7: convert HC somatic VCF → VAR (native, no awk) ─────────────────

pub fn run_filter_input(dirs: &Dirs, tumor: &str) -> Result<(), StepError> {
    const STEP: &str = "filter_input";
    let hc_vcf  = dirs.somatic.join(format!("{tumor}.snp.Somatic.hc.vcf"));
    let out_var = dirs.filter_input.join(format!("{tumor}.snp.Somatic.hc.var"));

    if !hc_vcf.exists() {
        // No HC somatic SNPs — empty var file is valid
        fs::write(&out_var, b"")
            .map_err(|e| StepError::new(STEP, e.to_string()))?;
        return Ok(());
    }

    let content = fs::read_to_string(&hc_vcf)
        .map_err(|e| StepError::new(STEP, format!("read HC VCF: {e}")))?;

    let mut out = String::new();
    for line in content.lines() {
        if line.starts_with('#') { continue; }
        let mut cols = line.splitn(3, '\t');
        let chrom = match cols.next() { Some(v) => v, None => continue };
        let pos   = match cols.next() { Some(v) => v, None => continue };
        out.push_str(chrom);
        out.push('\t');
        out.push_str(pos);
        out.push('\t');
        out.push_str(pos);
        out.push('\n');
    }

    fs::write(&out_var, out.as_bytes())
        .map_err(|e| StepError::new(STEP, format!("write VAR file: {e}")))?;
    Ok(())
}

// ─── Stage 8: bam-readcount ───────────────────────────────────────────────────

pub fn run_bam_readcount(cfg: &Config, dirs: &Dirs, tumor: &str) -> Result<(), StepError> {
    const STEP: &str = "bam_readcount";
    let var_file = dirs.filter_input.join(format!("{tumor}.snp.Somatic.hc.var"));
    let out_path = dirs.readcount.join(format!("{tumor}.snp.Somatic.hc.readcount"));

    // Empty var file → empty readcount (no HC somatic SNPs to process)
    let var_size = fs::metadata(&var_file).map(|m| m.len()).unwrap_or(0);
    if !var_file.exists() || var_size == 0 {
        fs::write(&out_path, b"")
            .map_err(|e| StepError::new(STEP, e.to_string()))?;
        return Ok(());
    }

    let tumor_bam = cfg.bam_dir.join(format!("{tumor}_final.bam"));
    let out_file  = fs::File::create(&out_path)
        .map_err(|e| StepError::new(STEP, format!("create readcount output: {e}")))?;

    let status = std::process::Command::new(&cfg.bam_readcount_bin)
        .arg("-q").arg("1")
        .arg("-b").arg("20")
        .arg("-f").arg(&cfg.genome)
        .arg("-l").arg(&var_file)
        .arg(&tumor_bam)
        .stdout(out_file)
        .stderr(Stdio::null())
        .status()
        .map_err(|e| StepError::new(STEP, format!("spawn bam-readcount: {e}")))?;

    if !status.success() {
        return Err(StepError::new(STEP, format!(
            "bam-readcount exit={}", status.code().unwrap_or(-1)
        )));
    }
    Ok(())
}
