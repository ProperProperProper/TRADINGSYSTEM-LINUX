//! Test helpers: build a synthetic lab with ingested data in a temp folder.

use crate::config::Config;
use crate::synth::{self, SynthSpec};
use crate::{db, ingest};
use rusqlite::Connection;

pub struct Lab {
    pub _dir: tempfile::TempDir,
    pub cfg: Config,
    pub mem: Connection,
    pub data: Connection,
}

/// A test lab in a temporary folder filled with synthetic exports and ingested.
pub fn lab(spec: &SynthSpec) -> Lab {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::for_root(dir.path(), "synthetic");
    let mem = db::open(&cfg.memory_db(), true).unwrap();
    let mut data = db::open(&cfg.data_db(), false).unwrap();
    db::migrate_memory(&mem).unwrap();
    db::migrate_data(&data).unwrap();
    std::fs::create_dir_all(cfg.exports_dir()).unwrap();
    synth::generate(&cfg.exports_dir(), spec).unwrap();
    let r = ingest::ingest(&cfg, &mem, &mut data).unwrap();
    assert_eq!(r.quarantined, 0, "synthetic data must ingest cleanly");
    Lab { _dir: dir, cfg, mem, data }
}

