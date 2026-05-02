#!/bin/bash

# =============================================================================
# Accurate VarScan2 CNV and Somatic Analysis Pipeline
# Based on official VarScan2 documentation and best practices
# =============================================================================

echo "========================================================================"
echo "VarScan2 CNV and Somatic Variant Analysis Pipeline - Stage 02"
echo "========================================================================"

# =============================================================================
# CONFIGURATION SECTION
# =============================================================================

# Directory paths
BASEDIRINDEX="/home/gifthr/reference-database/GRCh38.p14.ensembl113"
GENOMEIDX1="$BASEDIRINDEX/Homo_sapiens.GRCh38.dna.toplevel.fna"
SOFTWAREDIR="/home/gifthr/software"
BASEDIRDATA="$PWD"
VARSCANDIR="$PWD"

# VarScan2 parameters (based on official documentation)
MIN_COVERAGE=10
MIN_COVERAGE_NORMAL=10
MIN_COVERAGE_TUMOR=15
MIN_VAR_FREQ=0.08
MIN_FREQ_FOR_HOM=0.75
NORMAL_PURITY=1.0
TUMOR_PURITY=1.0
P_VALUE=0.99
SOMATIC_P_VALUE=0.05
MIN_TUMOR_FREQ=0.10
MAX_NORMAL_FREQ=0.05
PROCESS_P_VALUE=0.07

# Copy number parameters
CNV_MIN_COVERAGE=10
MIN_SEGMENT_SIZE=20
MAX_SEGMENT_SIZE=100
CNV_P_VALUE=0.005

# Parallel processing
MAX_PARALLEL_JOBS=60

# Input file
FILE_PAIRS_LIST="sample_pairs.csv"

# =============================================================================
# UTILITY FUNCTIONS
# =============================================================================

log_message() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1"
}

create_directory() {
    local dir_name="$1"
    local dir_path="$2"
    
    if [ ! -d "$dir_path" ]; then
        mkdir -p "$dir_path"
        chmod 755 "$dir_path"
        log_message "Created directory: $dir_path"
    else
        log_message "Directory already exists: $dir_path"
    fi
}

check_file_exists() {
    if [ ! -f "$1" ]; then
        log_message "ERROR: Required file not found: $1"
        exit 1
    fi
}

calculate_data_ratio() {
    local normal_flagstats="$1"
    local tumor_flagstats="$2"
    
    if [ -f "$normal_flagstats" ] && [ -f "$tumor_flagstats" ]; then
        local normal_mapped=$(grep -m 1 "mapped (" "$normal_flagstats" | cut -f1 -d' ')
        local tumor_mapped=$(grep -m 1 "mapped (" "$tumor_flagstats" | cut -f1 -d' ')
        
        if [ "$tumor_mapped" -ne 0 ]; then
            echo "scale=6;$normal_mapped/$tumor_mapped" | bc
        else
            echo "1.0"
        fi
    else
        echo "1.0"
    fi
}

# =============================================================================
# DIRECTORY SETUP
# =============================================================================

setup_directories() {
    log_message "Setting up directory structure..."
    
    # Create all required directories
    create_directory "Flagstats" "$PWD/flagstats"
    create_directory "Mpileup Files" "$PWD/mpileup"
    create_directory "Somatic Variants" "$PWD/somatic"
    create_directory "Copy Number" "$PWD/copynumber"
    create_directory "Readcount" "$PWD/readcount"
    create_directory "Filter Input" "$PWD/filter-input"
    create_directory "Filtered Results" "$PWD/filtered"
    
    # Set directory variables
    FLAGSTATDIR="$PWD/flagstats"
    MPILEUPDIR="$PWD/mpileup"
    SOMATICDIR="$PWD/somatic"
    COPYNUMBERDIR="$PWD/copynumber"
    READCOUNTDIR="$PWD/readcount"
    FILTERINPUTDIR="$PWD/filter-input"
    FILTEREDDIR="$PWD/filtered"
    
    log_message "Directory setup completed"
}

# =============================================================================
# STAGE 1: GENERATE FLAGSTATS FOR DATA RATIO CALCULATION
# =============================================================================

generate_flagstats() {
    log_message "=== STAGE 1: Generating BAM flagstats ==="
    
    check_file_exists "$GENOMEIDX1"
    
    cd "$BASEDIRDATA" || exit 1
    
    local job_count=0
    for file in *_final.bam; do
        if [ -f "$file" ]; then
            sample=${file/_final.bam/}
            
            log_message "Generating flagstats for: $sample"
            (samtools flagstat "$file" > "$FLAGSTATDIR/$sample.flagstats") &
            
            ((job_count++))
            if (( job_count % MAX_PARALLEL_JOBS == 0 )); then
                wait
            fi
        fi
    done
    wait
    
    cd "$VARSCANDIR" || exit 1
    log_message "Flagstats generation completed"
}

# =============================================================================
# STAGE 2: GENERATE PAIRED MPILEUP FILES
# =============================================================================

generate_mpileup() {
    log_message "=== STAGE 2: Generating paired mpileup files ==="
    
    check_file_exists "$FILE_PAIRS_LIST"
    check_file_exists "$GENOMEIDX1"
    
    local job_count=0
    
    # Read file pairs and generate paired mpileups (normal first, tumor second as per VarScan requirement)
    while IFS=',' read -r file1 file2; do
        # Remove any whitespace
        file1=$(echo "$file1" | tr -d ' ')
        file2=$(echo "$file2" | tr -d ' ')
        
        # Extract sample names (assuming file1=normal, file2=tumor)
        samplen=${file1/_final.bam/}
        samplet=${file2/_final.bam/}
        
        log_message "Generating mpileup for Normal: $samplen, Tumor: $samplet"
        
        # Generate paired mpileup (normal first, then tumor as required by VarScan)
        (samtools mpileup -B -q 1 -f "$GENOMEIDX1" \
            "$BASEDIRDATA/${samplen}_final.bam" \
            "$BASEDIRDATA/${samplet}_final.bam" > \
            "$MPILEUPDIR/${samplen}_${samplet}.mpileup") &
        
        ((job_count++))
        if (( job_count % 30 == 0 )); then
            wait
        fi
    done < "$FILE_PAIRS_LIST"
    wait
    
    log_message "Mpileup generation completed"
}

# =============================================================================
# STAGE 3: VARSCAN SOMATIC VARIANT CALLING
# =============================================================================

run_varscan_somatic() {
    log_message "=== STAGE 3: Running VarScan somatic variant calling ==="
    
    check_file_exists "$FILE_PAIRS_LIST"
    check_file_exists "$SOFTWAREDIR/VarScan.v2.3.9.jar"
    
    local job_count=0
    
    while IFS=',' read -r file1 file2; do
        # Remove any whitespace
        file1=$(echo "$file1" | tr -d ' ')
        file2=$(echo "$file2" | tr -d ' ')
        
        # Extract sample names
        samplen=${file1/_final.bam/}
        samplet=${file2/_final.bam/}
        
        log_message "Running VarScan somatic calling for Normal: $samplen, Tumor: $samplet"
        
        # Check if mpileup file exists
        mpileup_file="$MPILEUPDIR/${samplen}_${samplet}.mpileup"
        if [ ! -f "$mpileup_file" ]; then
            log_message "WARNING: Mpileup file not found: $mpileup_file"
            continue
        fi
        
        # Run VarScan somatic (following official documentation parameters)
        (java -Xmx24g -jar "$SOFTWAREDIR/VarScan.v2.3.9.jar" somatic \
            "$mpileup_file" \
            "$SOMATICDIR/${samplet}" \
            --mpileup 1 \
            --min-coverage "$MIN_COVERAGE" \
            --min-coverage-normal "$MIN_COVERAGE_NORMAL" \
            --min-coverage-tumor "$MIN_COVERAGE_TUMOR" \
            --min-var-freq "$MIN_VAR_FREQ" \
            --min-freq-for-hom "$MIN_FREQ_FOR_HOM" \
            --normal-purity "$NORMAL_PURITY" \
            --tumor-purity "$TUMOR_PURITY" \
            --p-value "$P_VALUE" \
            --somatic-p-value "$SOMATIC_P_VALUE" \
            --strand-filter 1 \
            --output-vcf 1) &
        
        ((job_count++))
        if (( job_count % MAX_PARALLEL_JOBS == 0 )); then
            wait
        fi
    done < "$FILE_PAIRS_LIST"
    wait
    
    log_message "VarScan somatic calling completed"
}

# =============================================================================
# STAGE 4: PROCESS SOMATIC VARIANTS
# =============================================================================

process_somatic_variants() {
    log_message "=== STAGE 4: Processing somatic variants ==="
    
    local job_count=0
    
    # Process SNP files
    for snp_file in "$SOMATICDIR"/*.snp.vcf; do
        if [ -f "$snp_file" ]; then
            log_message "Processing SNP file: $(basename $snp_file)"
            
            (java -Xmx24g -jar "$SOFTWAREDIR/VarScan.v2.3.9.jar" processSomatic \
                "$snp_file" \
                --min-tumor-freq "$MIN_TUMOR_FREQ" \
                --max-normal-freq "$MAX_NORMAL_FREQ" \
                --p-value "$PROCESS_P_VALUE") &
            
            ((job_count++))
            if (( job_count % MAX_PARALLEL_JOBS == 0 )); then
                wait
            fi
        fi
    done
    
    # Process INDEL files
    for indel_file in "$SOMATICDIR"/*.indel.vcf; do
        if [ -f "$indel_file" ]; then
            log_message "Processing INDEL file: $(basename $indel_file)"
            
            (java -Xmx24g -jar "$SOFTWAREDIR/VarScan.v2.3.9.jar" processSomatic \
                "$indel_file" \
                --min-tumor-freq "$MIN_TUMOR_FREQ" \
                --max-normal-freq "$MAX_NORMAL_FREQ" \
                --p-value "$PROCESS_P_VALUE") &
            
            ((job_count++))
            if (( job_count % MAX_PARALLEL_JOBS == 0 )); then
                wait
            fi
        fi
    done
    wait
    
    log_message "Somatic variant processing completed"
}

# =============================================================================
# STAGE 5: VARSCAN COPY NUMBER ANALYSIS
# =============================================================================

run_varscan_copynumber() {
    log_message "=== STAGE 5: Running VarScan copy number analysis ==="
    
    check_file_exists "$FILE_PAIRS_LIST"
    
    local job_count=0
    
    while IFS=',' read -r file1 file2; do
        # Remove any whitespace
        file1=$(echo "$file1" | tr -d ' ')
        file2=$(echo "$file2" | tr -d ' ')
        
        # Extract sample names
        samplen=${file1/_final.bam/}
        samplet=${file2/_final.bam/}
        
        log_message "Running VarScan copy number analysis for Normal: $samplen, Tumor: $samplet"
        
        # Calculate data ratio from flagstats
        normal_flagstats="$FLAGSTATDIR/$samplen.flagstats"
        tumor_flagstats="$FLAGSTATDIR/$samplet.flagstats"
        dataratio=$(calculate_data_ratio "$normal_flagstats" "$tumor_flagstats")
        
        log_message "Data ratio for $samplen vs $samplet: $dataratio"
        
        # Check if mpileup file exists
        mpileup_file="$MPILEUPDIR/${samplen}_${samplet}.mpileup"
        if [ ! -f "$mpileup_file" ]; then
            log_message "WARNING: Mpileup file not found: $mpileup_file"
            continue
        fi
        
        # Run VarScan copynumber (following official documentation)
        (java -Xmx24g -jar "$SOFTWAREDIR/VarScan.v2.3.9.jar" copynumber \
            "$mpileup_file" \
            "$COPYNUMBERDIR/${samplet}" \
            --mpileup 1 \
            --min-coverage "$CNV_MIN_COVERAGE" \
            --min-segment-size "$MIN_SEGMENT_SIZE" \
            --max-segment-size "$MAX_SEGMENT_SIZE" \
            --p-value "$CNV_P_VALUE" \
            --data-ratio "$dataratio") &
        
        ((job_count++))
        if (( job_count % MAX_PARALLEL_JOBS == 0 )); then
            wait
        fi
    done < "$FILE_PAIRS_LIST"
    wait
    
    log_message "VarScan copy number analysis completed"
}

# =============================================================================
# STAGE 6: COPY NUMBER CALLER AND GC ADJUSTMENT
# =============================================================================

run_copy_caller() {
    log_message "=== STAGE 6: Running VarScan copyCaller ==="
    
    local job_count=0
    
    # Run copyCaller on generated copynumber files
    for cnv_file in "$COPYNUMBERDIR"/*.copynumber; do
        if [ -f "$cnv_file" ]; then
            base_name=$(basename "$cnv_file" .copynumber)
            
            log_message "Running copyCaller for: $base_name"
            
            (java -Xmx24g -jar "$SOFTWAREDIR/VarScan.v2.3.9.jar" copyCaller \
                "$cnv_file" \
                --output-file "$COPYNUMBERDIR/$base_name.copynumber.called" \
                --output-homdel-file "$COPYNUMBERDIR/$base_name.copynumber.homdel") &
            
            ((job_count++))
            if (( job_count % MAX_PARALLEL_JOBS == 0 )); then
                wait
            fi
        fi
    done
    wait
    
    log_message "VarScan copyCaller completed"
}

# =============================================================================
# STAGE 7: PREPARE FILTER INPUT FILES
# =============================================================================

prepare_filter_input() {
    log_message "=== STAGE 7: Preparing filter input files ==="
    
    local job_count=0
    
    # Convert high confidence somatic VCF files to position format for bam-readcount
    for hc_file in "$SOMATICDIR"/*.Somatic.hc.vcf; do
        if [ -f "$hc_file" ]; then
            base_name=$(basename "$hc_file" .vcf)
            output_var="$FILTERINPUTDIR/${base_name}.var"
            
            log_message "Converting $hc_file to VAR format"
            
            (awk 'BEGIN {OFS="\t"} {
                if (!/^#/) {
                    print $1, $2, $2
                }
            }' "$hc_file" > "$output_var") &
            
            ((job_count++))
            if (( job_count % MAX_PARALLEL_JOBS == 0 )); then
                wait
            fi
        fi
    done
    wait
    
    log_message "Filter input preparation completed"
}

# =============================================================================
# STAGE 8: RUN BAM-READCOUNT FOR FALSE POSITIVE FILTERING
# =============================================================================

run_bam_readcount() {
    log_message "=== STAGE 8: Running bam-readcount for false positive filtering ==="
    
    check_file_exists "$FILE_PAIRS_LIST"
    
    local job_count=0
    
    while IFS=',' read -r file1 file2; do
        # Remove any whitespace
        file1=$(echo "$file1" | tr -d ' ')
        file2=$(echo "$file2" | tr -d ' ')
        
        # Extract sample names
        samplen=${file1/_final.bam/}
        samplet=${file2/_final.bam/}
        
        log_message "Running bam-readcount for: $samplet"
        
        # Run bam-readcount for somatic mutations (use tumor BAM as per documentation)
        somatic_var_file="$FILTERINPUTDIR/${samplet}.snp.Somatic.hc.var"
        if [ -f "$somatic_var_file" ]; then
            (bam-readcount -q 1 -b 20 -f "$GENOMEIDX1" \
                -l "$somatic_var_file" \
                "$BASEDIRDATA/${samplet}_final.bam" > \
                "$READCOUNTDIR/${samplet}.snp.Somatic.hc.readcount") &
            
            ((job_count++))
            if (( job_count % MAX_PARALLEL_JOBS == 0 )); then
                wait
            fi
        fi
        
    done < "$FILE_PAIRS_LIST"
    wait
    
    log_message "bam-readcount analysis completed"
}

# =============================================================================
# STAGE 9: GENERATE SUMMARY REPORT
# =============================================================================

generate_summary() {
    log_message "=== STAGE 9: Generating summary report ==="
    
    local summary_file="$VARSCANDIR/varscan_analysis_summary.txt"
    
    {
        echo "========================================================================"
        echo "VarScan2 CNV and Somatic Analysis Pipeline Summary"
        echo "Generated on: $(date)"
        echo "========================================================================"
        echo ""
        
        echo "ANALYSIS PARAMETERS:"
        echo "- Minimum coverage: $MIN_COVERAGE"
        echo "- Minimum variant frequency: $MIN_VAR_FREQ"
        echo "- Somatic p-value: $SOMATIC_P_VALUE"
        echo "- Copy number p-value: $CNV_P_VALUE"
        echo ""
        
        echo "OUTPUT DIRECTORIES:"
        echo "- Somatic variants: $SOMATICDIR"
        echo "- Copy number results: $COPYNUMBERDIR"
        echo "- BAM readcount: $READCOUNTDIR"
        echo "- Mpileup files: $MPILEUPDIR"
        echo ""
        
        echo "RESULTS SUMMARY:"
        if [ -d "$SOMATICDIR" ]; then
            snp_somatic_hc=$(find "$SOMATICDIR" -name "*.snp.Somatic.hc.vcf" -type f | wc -l)
            indel_somatic_hc=$(find "$SOMATICDIR" -name "*.indel.Somatic.hc.vcf" -type f | wc -l)
            germline_hc=$(find "$SOMATICDIR" -name "*.Germline.hc.vcf" -type f | wc -l)
            loh_hc=$(find "$SOMATICDIR" -name "*.LOH.hc.vcf" -type f | wc -l)
            
            echo "- High-confidence somatic SNP files: $snp_somatic_hc"
            echo "- High-confidence somatic INDEL files: $indel_somatic_hc"
            echo "- High-confidence germline files: $germline_hc"
            echo "- High-confidence LOH files: $loh_hc"
        fi
        
        if [ -d "$COPYNUMBERDIR" ]; then
            cnv_count=$(find "$COPYNUMBERDIR" -name "*.copynumber" -type f | wc -l)
            called_count=$(find "$COPYNUMBERDIR" -name "*.copynumber.called" -type f | wc -l)
            homdel_count=$(find "$COPYNUMBERDIR" -name "*.copynumber.homdel" -type f | wc -l)
            
            echo "- Raw copy number files: $cnv_count"
            echo "- Called copy number files: $called_count"
            echo "- Homozygous deletion files: $homdel_count"
        fi
        
        echo ""
        echo "Next Steps:"
        echo "1. Apply false positive filtering using fpfilter.pl script"
        echo "2. Perform circular binary segmentation (CBS) on copy number data"
        echo "3. Annotate variants with functional consequences"
        echo "4. Validate high-impact somatic mutations"
        echo ""
        echo "Pipeline completed successfully!"
        echo "========================================================================"
    } > "$summary_file"
    
    log_message "Summary report generated: $summary_file"
    cat "$summary_file"
}

# =============================================================================
# MAIN PIPELINE EXECUTION
# =============================================================================

main() {
    log_message "Starting VarScan2 CNV and Somatic Analysis Pipeline"
    
    # Check prerequisites
    check_file_exists "$FILE_PAIRS_LIST"
    check_file_exists "$GENOMEIDX1"
    check_file_exists "$SOFTWAREDIR/VarScan.v2.3.9.jar"
    
    # Execute pipeline stages in correct sequence
    setup_directories
    generate_flagstats
    generate_mpileup
    run_varscan_somatic
    process_somatic_variants
    run_varscan_copynumber
    run_copy_caller
    prepare_filter_input
    run_bam_readcount
    generate_summary
    
    log_message "VarScan2 Analysis Pipeline completed successfully!"
}

# =============================================================================
# SCRIPT EXECUTION
# =============================================================================

# Execute main function
main "$@"
