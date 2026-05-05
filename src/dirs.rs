use std::fs;
use std::io;
use std::path::{Path, PathBuf};

// ─── Output directory set ─────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Dirs {
    pub base:         PathBuf,
    pub flagstats:    PathBuf,
    pub mpileup:      PathBuf,
    pub somatic:      PathBuf,
    pub copynumber:   PathBuf,
    pub readcount:    PathBuf,
    pub filter_input: PathBuf,
    pub filtered:     PathBuf,
    pub logs:         PathBuf,
}

impl Dirs {
    pub fn new(base: &Path) -> Self {
        Dirs {
            base:         base.to_path_buf(),
            flagstats:    base.join("flagstats"),
            mpileup:      base.join("mpileup"),
            somatic:      base.join("somatic"),
            copynumber:   base.join("copynumber"),
            readcount:    base.join("readcount"),
            filter_input: base.join("filter-input"),
            filtered:     base.join("filtered"),
            logs:         base.join("logs"),
        }
    }

    /// Create all output subdirectories.
    pub fn create_all(&self) -> io::Result<()> {
        for dir in [
            &self.flagstats,
            &self.mpileup,
            &self.somatic,
            &self.copynumber,
            &self.readcount,
            &self.filter_input,
            &self.filtered,
            &self.logs,
        ] {
            fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}
