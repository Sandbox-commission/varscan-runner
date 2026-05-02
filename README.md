# varscan-runner

VarScan2 somatic SNV/INDEL + copy number pipeline with a live terminal UI and SHA256-based per-step resume.

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

## Prerequisites

| Tool | Version | Notes |
|------|---------|-------|
| Rust | ≥ 1.70  | `curl https://sh.rustup.rs -sSf \| sh` |
| samtools | ≥ 1.13 | `apt install samtools` or conda |
| VarScan2 | 2.3.9 | [GitHub releases](https://github.com/dkoboldt/varscan) |
| bam-readcount | ≥ 0.8 | [GitHub](https://github.com/genome/bam-readcount) |
| Java | ≥ 11 | required for VarScan jar |

BAM files must be **coordinate-sorted** and **indexed** (`*.bam` + `*.bai`), named `{SAMPLE}_final.bam`.

## Build

```bash
git clone https://github.com/YOUR_USERNAME/varscan-runner
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

## Input

`sample_pairs.csv` — one pair per line, `normal_final.bam,tumor_final.bam`:

```
PATIENT01_N_final.bam,PATIENT01_T_final.bam
PATIENT02_N_final.bam,PATIENT02_T_final.bam
PATIENT03_N_final.bam,PATIENT03_T_final.bam
```

Lines starting with `#` are ignored. The `_final.bam` suffix is stripped to derive sample names.

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
      --varscan         VarScan jar path            [default: /home/gifthr/software/VarScan.v2.3.9.jar]
      --java            java binary                 [default: java]
      --java-mem        Java heap size              [default: 24g]
      --samtools        samtools binary             [default: samtools]
      --bam-readcount   bam-readcount binary        [default: bam-readcount]
  -j, --jobs            Parallel pairs              [default: 8]

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

## Pipeline stages

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

## SHA256 resume

Each completed stage writes a SHA256 checksum of its output files to `.checkpoints/PAIRID.STEP.sha256`.

On re-run, the runner recomputes the hash and skips the stage if it matches (logged as `SKIP … SHA256 match`). Re-running a stage automatically invalidates all downstream checkpoints for that pair.

```bash
# Interrupted run — just re-run the same command to resume from where it stopped
varscan-runner --genome ... --pairs sample_pairs.csv
```

## Output structure

```
output/
├── flagstats/          samtools flagstat outputs
├── mpileup/            paired mpileup files
├── somatic/            VarScan somatic VCFs (raw + HC-classified)
├── copynumber/         VarScan copy number + called + homdel
├── filter-input/       Position files for bam-readcount
├── readcount/          bam-readcount outputs (input to fpfilter.pl)
├── filtered/           (reserved for fpfilter output)
└── .checkpoints/       SHA256 resume state (safe to delete to force full rerun)
```

## Next steps after pipeline

1. **FP filtering** — run `fpfilter.pl` using the `.readcount` files
2. **CBS segmentation** — apply circular binary segmentation to `.copynumber.called`
3. **Annotation** — VEP / ANNOVAR on the HC somatic VCFs
4. **QC** — check `varscan_analysis_summary.txt` for per-pair counts

## License

MIT
