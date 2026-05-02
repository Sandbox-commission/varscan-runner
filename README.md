# varscan-runner

VarScan2 somatic SNV/INDEL + copy number pipeline — resume-aware, parallel, with a live terminal UI.

```
╔════════════════════════════════════════════════════════════╗
  VarScan2 Runner v1.0.0
  Somatic SNV·INDEL + Copy Number Analysis  |  tumor–normal pairs
╠════════════════════════════════════════════════════════════╣
  OVERALL PIPELINE         [stage 3/8 — VS Somatic]
  ████████████████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  38%
  elapsed 04:12   eta 06:50   pairs 0/12

  STAGE PROGRESS
  ████████████████████████░░░░░░░░░░░░░░░░░░░░░  51%

  ACTIVE JOBS  (4/8)
  ┌──────────────────┬────────────┬────────────┬──────┐
  │ PAIR             │ STAGE      │ PROGRESS   │      │
  ├──────────────────┼────────────┼────────────┼──────┤
  │ ⠙ TUMOR1/NORM1  │ VS Somatic │ [████░░░░] │ ~41% │
  │    00:23 / ~56s  │            │            │      │
  │ ⠸ TUMOR2/NORM2  │ Mpileup    │ [██░░░░░░] │ ~22% │
  │    01:12 / ~56s  │            │            │      │
  └──────────────────┴────────────┴────────────┴──────┘

  ✓ completed: 3   → resumed: 12   ✗ failed: 0   · remaining: 9
```

---

## Table of Contents

- [Features](#features)
- [Prerequisites](#prerequisites)
- [Build](#build)
- [Input](#input)
- [BAM Naming Convention](#bam-naming-convention)
- [Quick Start](#quick-start)
- [Usage](#usage)
- [Pipeline Stages](#pipeline-stages)
- [Resume and SHA256 Integrity](#resume-and-sha256-integrity)
- [Terminal UI](#terminal-ui)
- [Output Structure](#output-structure)
- [Troubleshooting](#troubleshooting)
- [Architecture](#architecture)
- [Next Steps After Pipeline](#next-steps-after-pipeline)
- [License](#license)

---

## Features

- **8-stage sequential pipeline per pair**: flagstat → mpileup → VarScan somatic → processSomatic → copynumber → copyCaller → filter prep → bam-readcount
- **Per-step SHA256 checkpoints** — resume from the exact interrupted stage on re-run; downstream stages auto-invalidate when upstream reruns
- **Parallel pairs** (`--jobs N`), sequential stages within each pair
- **Pre-run resume scan** — checks all pairs in parallel before any work starts; reports complete/partial/fresh counts upfront
- **Dry-run mode** (`--dry-run`) — lists all pairs with resume status without executing anything
- **Preflight validation** — verifies all required binaries and reference files exist before starting
- **Partial output cleanup** — failed stages remove their incomplete outputs so subsequent resumes start clean
- **Full-screen TUI** with gradient progress bars (amber→teal→blue), braille spinners, ETA, speed, completion time, and scrolling activity log
- **Pipeline summary TSV** written after every run (`varscan_pipeline_summary.tsv`)
- **Static musl build** option — single binary, no runtime dependencies, runs on any Linux x86_64

---

## Prerequisites

| Tool | Version | Notes |
|------|---------|-------|
| Rust | ≥ 1.70  | `curl https://sh.rustup.rs -sSf \| sh` |
| samtools | ≥ 1.13 | `apt install samtools` or conda |
| VarScan2 | 2.3.9 | [GitHub releases](https://github.com/dkoboldt/varscan) |
| bam-readcount | ≥ 0.8 | [GitHub](https://github.com/genome/bam-readcount) |
| Java | ≥ 11 | required for VarScan jar |

---

## Build

```bash
git clone https://github.com/Sandbox-commission/varscan-runner
cd varscan-runner
cargo build --release
# Binary: ./target/release/varscan-runner
```

### Portable static binary (any Linux x86_64)

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
# Binary: ./target/x86_64-unknown-linux-musl/release/varscan-runner
```

Copy the binary to any Linux x86_64 machine — no Rust or library dependencies needed.

---

## Input

`sample_pairs.csv` — one pair per line, `normal_final.bam,tumor_final.bam`:

```
PATIENT01_N_final.bam,PATIENT01_T_final.bam
PATIENT02_N_final.bam,PATIENT02_T_final.bam
PATIENT03_N_final.bam,PATIENT03_T_final.bam
```

Lines starting with `#` are ignored.

---

## BAM Naming Convention

BAM files must follow this exact pattern:

```
{SAMPLE}_final.bam
{SAMPLE}_final.bam.bai    (index, required)
```

The `_final.bam` suffix is stripped to derive sample names. For a pair:

```
PATIENT01_N_final.bam,PATIENT01_T_final.bam
```

This yields `normal = PATIENT01_N`, `tumor = PATIENT01_T`, pair ID = `PATIENT01_N_PATIENT01_T`. All output files use these derived names.

If your BAMs are named differently (e.g., `sample.bam`), create symlinks before running:
```bash
ln -s sample.bam sample_final.bam
ln -s sample.bam.bai sample_final.bam.bai
```

---

## Quick Start

```bash
# 1. Dry-run: verify pairs and resume status without running
varscan-runner \
  --genome /path/to/GRCh38.p14.fna \
  --varscan /path/to/VarScan.v2.3.9.jar \
  --pairs sample_pairs.csv \
  --dry-run

# 2. First run
varscan-runner \
  --genome /path/to/GRCh38.p14.fna \
  --pairs  sample_pairs.csv \
  --bam-dir /path/to/bams \
  --output /path/to/output \
  --varscan /path/to/VarScan.v2.3.9.jar \
  --jobs 8

# 3. Resume after interruption — same command, picks up where it stopped
varscan-runner --genome ... --pairs ... --output ... --varscan ...
```

---

## Usage

```bash
varscan-runner \
  --genome /path/to/GRCh38.p14.fna \
  --pairs  sample_pairs.csv \
  --bam-dir /path/to/bams \
  --output /path/to/output \
  --varscan /path/to/VarScan.v2.3.9.jar \
  --jobs 16
```

All options:

```
Options:
  -p, --pairs           CSV pairs file              [default: sample_pairs.csv]
  -d, --bam-dir         Directory with *_final.bam  [default: .]
  -o, --output          Output directory            [default: .]
  -g, --genome          Reference FASTA             (required)
      --varscan         VarScan jar path            (required)
      --java            java binary                 [default: java]
      --java-mem        Java heap size              [default: 24g]
      --samtools        samtools binary             [default: samtools]
      --bam-readcount   bam-readcount binary        [default: bam-readcount]
  -j, --jobs            Parallel pairs              [default: 8]
      --dry-run         List pairs + resume status, do not run

VarScan somatic parameters:
      --min-coverage          [default: 10]
      --min-coverage-normal   [default: 10]
      --min-coverage-tumor    [default: 15]
      --min-var-freq          [default: 0.08]
      --min-freq-for-hom      [default: 0.75]
      --normal-purity         [default: 1.0]
      --tumor-purity          [default: 1.0]
      --p-value               [default: 0.99]
      --somatic-p-value       [default: 0.05]
      --min-tumor-freq        [default: 0.10]
      --max-normal-freq       [default: 0.05]
      --process-p-value       [default: 0.07]

VarScan copynumber parameters:
      --cnv-min-coverage      [default: 10]
      --min-segment-size      [default: 20]
      --max-segment-size      [default: 100]
      --cnv-p-value           [default: 0.005]

samtools mpileup:
      --mpileup-mapq          [default: 20]
```

---

## Pipeline Stages

Each pair runs 8 stages sequentially. Pairs run in parallel (`--jobs`).

| # | Stage | Output |
|---|-------|--------|
| 1 | `samtools flagstat` | `flagstats/{sample}.flagstats` |
| 2 | `samtools mpileup` | `mpileup/{normal}_{tumor}.mpileup` |
| 3 | `VarScan somatic` | `somatic/{tumor}.snp.vcf`, `.indel.vcf` |
| 4 | `VarScan processSomatic` | `somatic/{tumor}.snp.Somatic.hc.vcf` etc. |
| 5 | `VarScan copynumber` | `copynumber/{tumor}.copynumber` |
| 6 | `VarScan copyCaller` | `copynumber/{tumor}.copynumber.called`, `.homdel` |
| 7 | Filter prep (VCF→VAR) | `filter-input/{tumor}.snp.Somatic.hc.var` |
| 8 | `bam-readcount` | `readcount/{tumor}.snp.Somatic.hc.readcount` |

---

## Resume and SHA256 Integrity

### How it works

Each completed stage writes a SHA256 checksum of its output files to `.checkpoints/PAIRID.STEP.sha256`. On re-run, the runner recomputes the hash — if it matches the stored value, the stage is skipped. If the checkpoint is missing or the hash mismatches, the stage reruns and all downstream checkpoints for that pair are removed.

Before processing starts, a **pre-run resume scan** checks all pairs in parallel and reports:

```
Resume scan: 12 pairs — 4 complete, 3 partial, 5 fresh
```

### Resume states

| State | Condition | Action |
|-------|-----------|--------|
| Complete | All 8 stage checkpoints valid | Pair skipped entirely |
| Partial | N stages valid from front, stage N+1 invalid | Resume from stage N+1; stages 1–N not rerun |
| Fresh | No checkpoints found | Run all 8 stages |

### Dry-run mode

```bash
varscan-runner --genome ... --varscan ... --dry-run
```

Runs the resume scan and prints each pair's status without executing any pipeline steps:

```
  Pair                                      Status    Stages    Note
  ──────────────────────────────────────────────────────────────────────────────
  PATIENT01_N_PATIENT01_T                   COMPLETE  8/8       will be skipped
  PATIENT02_N_PATIENT02_T                   PARTIAL   5/8       resumes from stage 6
  PATIENT03_N_PATIENT03_T                   FRESH     0/8       will run all stages
```

### Partial output cleanup

When a stage fails, its partial output files are removed automatically. This prevents a subsequent resume from inheriting corrupt or incomplete files.

### Downstream invalidation

Rerunning a stage removes all downstream checkpoints for that pair. For example, if stage 3 (somatic) reruns, checkpoints for stages 4–8 are deleted — they will rerun even if previously complete.

```bash
# Force a single stage to rerun by deleting its checkpoint
rm .checkpoints/PATIENT01_N_PATIENT01_T.somatic.sha256

# Force full rerun of a pair
rm .checkpoints/PATIENT01_N_PATIENT01_T.*.sha256

# Force full rerun of all pairs
rm -rf .checkpoints/
```

---

## Terminal UI

The pipeline features a full-screen alternate-screen terminal interface built with `crossterm`. Press `q` or `Ctrl+C` to cancel.

- Centered title and subtitle
- Overall pipeline bar (teal gradient) + stage badge
- Stage progress bar (blue gradient) with elapsed, ETA, speed, and estimated completion time
- Per-pair job table with braille spinner, elapsed/ETA, and color-coded progress bar (amber < 25%, teal 25–60%, blue ≥ 60%)
- Color-coded activity log: green (DONE), yellow (SKIP/resumed), red (FAIL), dark red (STOP/cancelled)
- Sticky footer with cancel hint and last-updated timestamp
- Resize-safe: clips frame to terminal height preserving header and footer

---

## Output Structure

```
output/
├── flagstats/          samtools flagstat outputs
├── mpileup/            paired mpileup files
├── somatic/            VarScan somatic VCFs (raw + HC-classified)
├── copynumber/         VarScan copy number + called + homdel
├── filter-input/       Position files for bam-readcount
├── readcount/          bam-readcount outputs (input to fpfilter.pl)
├── filtered/           (reserved for fpfilter output)
├── .checkpoints/       SHA256 resume state (safe to delete to force full rerun)
└── varscan_pipeline_summary.tsv   Per-pair status, stage counts, and duration
```

The summary TSV columns: `pair_id`, `normal`, `tumor`, `status`, `stages_run`, `stages_cached`, `duration_s`.

---

## Troubleshooting

### 1. `ERROR: java not found`
```bash
java -version        # check if installed
which java           # check PATH
```
Fix: install Java ≥ 11, or pass `--java /path/to/java`.

### 2. `ERROR: VarScan jar not found`
Fix: download VarScan2 and pass the path:
```bash
varscan-runner --varscan /path/to/VarScan.v2.3.9.jar ...
```

### 3. `ERROR: samtools not found`
```bash
apt install samtools
# or: conda install -c bioconda samtools
# or: varscan-runner --samtools /path/to/samtools ...
```

### 4. `ERROR: bam-readcount not found`
```bash
conda install -c bioconda bam-readcount
# or build from source: https://github.com/genome/bam-readcount
# or: varscan-runner --bam-readcount /path/to/bam-readcount ...
```

### 5. `no pairs found in sample_pairs.csv`
Check file format — one pair per line, comma-separated, no extra spaces:
```
PATIENT01_N_final.bam,PATIENT01_T_final.bam
```
Lines starting with `#` are ignored. Verify the file exists at the path given to `--pairs`.

### 6. `samtools mpileup` fails or produces empty output
- Verify BAMs are coordinate-sorted and indexed (`samtools sort`, `samtools index`)
- Check that BAM filenames match `{SAMPLE}_final.bam` exactly
- Verify `--bam-dir` points to the correct directory
- Confirm `--genome` FASTA chromosome names match BAM headers (`samtools view -H sample.bam | grep ^@SQ`)

### 7. VarScan somatic produces empty or header-only VCF
Check mpileup size — if very small (< 1 MB for a typical WES pair), mpileup likely failed silently. Also:
- Verify `--min-coverage` is not higher than your actual coverage
- Check the mpileup file directly: `wc -l mpileup/NORM_TUM.mpileup`

### 8. Stages rerun despite looking complete
Cause: output files were modified, truncated, or the pipeline crashed mid-write.
```bash
# Check which checkpoints exist
ls .checkpoints/PATIENT01_N_PATIENT01_T.*.sha256

# Force rerun of one stage (e.g. somatic)
rm .checkpoints/PATIENT01_N_PATIENT01_T.somatic.sha256

# Force full rerun of all pairs
rm -rf .checkpoints/
```

### 9. OOM / Java heap error during VarScan
Increase heap: `--java-mem 48g`. Default is 24g.

---

## Architecture

```
main()
 ├── parse_pairs()                 CSV → Vec<Pair>
 ├── Dirs::new().create_all()      Create output subdirectories
 ├── check_resume_all_parallel()   SHA256 pre-scan (up to 16 threads)
 ├── [--dry-run branch]            Print table and exit
 ├── preflight()                   Validate binaries + reference files
 ├── install_sigint_handler()      CANCELLED AtomicBool
 ├── TUI setup (crossterm)         Alternate screen + raw mode
 ├── worker thread pool            --jobs slots
 │    └── process_pair()           8-stage sequential loop per pair
 │         ├── is_step_done()      SHA256 resume check
 │         ├── invalidate_downstream()   Remove downstream checkpoints
 │         ├── run_{stage}()       Execute stage, cleanup outputs on failure
 │         └── write_checkpoint()  Atomic SHA256 write on success
 ├── render loop (100 ms)          TUI frame assembly + single-buffer flush
 ├── write_summary()               varscan_pipeline_summary.tsv
 └── render_final_frame()          Completion screen
```

Key design decisions:
- **No rayon** — scoped thread pool with AtomicUsize work-stealing gives per-slot TUI visibility
- **Single-buffer flush** — full frame written in one `write_all` + `flush` per tick; no scroll artifacts
- **Atomic checkpoint writes** — temp file + `fsync` + rename; never half-written on power loss
- **Cascade invalidation** — rerunning any stage removes all downstream checkpoints for that pair

---

## Next Steps After Pipeline

1. **FP filtering** — run `fpfilter.pl` using the `.readcount` files in `readcount/`
2. **CBS segmentation** — apply circular binary segmentation to `.copynumber.called`
3. **Annotation** — VEP / ANNOVAR on the HC somatic VCFs in `somatic/`
4. **QC** — check `varscan_pipeline_summary.tsv` for per-pair status and stage counts

---

## License

MIT
