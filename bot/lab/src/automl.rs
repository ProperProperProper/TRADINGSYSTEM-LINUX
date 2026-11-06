//! The self-improving trainer (owner 2026-10-13: "the model needs to train 24x7", "it needs to self improve 24x7"). A worker that never stops searches for better deep
//! architectures with an evolutionary loop (the genetic-algorithm idea of *The Nature of Code*, applied to the model itself): a population of genomes (architecture +
//! training recipe) is kept in SQLite; each turn the worker picks a parent from the best ones (tournament), mutates or crosses it, trains and evaluates the child on real
//! out-of-sample windows, records the result in the trial ledger (paired against the ridge, adjusted for the number of trials), and keeps the best. A genome is a CHAMPION
//! only when its paired gain over the ridge is above zero at the adjusted level; the champion is then retrained on all data and saved for live blending. It learns from the
//! historical candles, the indicators computed from them, and (via `mlmeta`) the bot's backtest trades. Resource-guarded and niced; stops on the plain switch files.

use crate::deepnet::{self, DeepModel, Genome, Kind, Row};
use crate::mlcandle::{Sample, FEATURES};
use crate::mlscores::Scheme;
use crate::{candles, mlbot, mlcandle as m, mlimprove as imp};
use anyhow::Result;
use rusqlite::{params, Connection};
use std::path::Path;

const DAY_MS: i64 = 86_400_000;
pub const MAX_SEQ: usize = 64;
pub const OBJECTIVE: &str = "ridge-blend-15m-v2";

/// A change to the live ridge's training scheme starts a separate population.
pub fn objective_for(scheme: Scheme) -> String {
    match (scheme.window_days, scheme.half_life_days) {
        (Some(days), _) => format!("{OBJECTIVE}/rolling-{days}"),
        (_, Some(days)) => format!("{OBJECTIVE}/half-life-{days:.6}"),
        _ => format!("{OBJECTIVE}/all-history"),
    }
}

pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Rng { Rng(seed.wrapping_mul(2).wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1) }
    pub fn bits(&mut self) -> u64 { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; self.0 }
    pub fn unit(&mut self) -> f64 { (self.bits() >> 11) as f64 / (1u64 << 53) as f64 }
    fn pick<T: Copy>(&mut self, v: &[T]) -> T { v[(self.bits() % v.len() as u64) as usize] }
    fn log_uniform(&mut self, lo: f64, hi: f64) -> f64 { (lo.ln() + self.unit() * (hi.ln() - lo.ln())).exp() }
}

/// Keeps a genome inside what the CPU can train: valid head counts, bounded Transformer size.
pub fn clamp_genome(mut g: Genome) -> Genome {
    g.heads = if !g.width.is_multiple_of(g.heads) { 1 } else { g.heads };
    match g.kind {
        Kind::Res => { g.width = g.width.clamp(16, 64); g.blocks = g.blocks.clamp(1, 3); g.seq_len = 8; g.epochs = g.epochs.clamp(6, 24); }
        Kind::Transformer => { g.width = g.width.clamp(8, 16) / 8 * 8; g.blocks = 1; g.seq_len = g.seq_len.clamp(8, 16); g.heads = if g.width.is_multiple_of(g.heads) { g.heads.min(4) } else { 1 }; g.epochs = g.epochs.clamp(6, 10); g.moments_per_batch = g.moments_per_batch.max(8); }
    }
    g.dropout = g.dropout.clamp(0.0, 0.4); g.lr = g.lr.clamp(2e-4, 8e-3); g.weight_decay = g.weight_decay.clamp(0.0, 0.1);
    g.moments_per_batch = g.moments_per_batch.clamp(4, 24); g.mse_weight = g.mse_weight.clamp(0.0, 0.5);
    g
}

fn random_genome(r: &mut Rng) -> Genome {
    let kind = if r.unit() < 0.4 { Kind::Transformer } else { Kind::Res };
    clamp_genome(Genome { kind, width: if kind == Kind::Transformer { r.pick(&[8, 16]) } else { r.pick(&[16, 24, 32, 48, 64]) }, blocks: r.pick(&[1, 2, 3]), heads: r.pick(&[1, 2, 4]), dropout: r.unit() * 0.3, lr: r.log_uniform(5e-4, 6e-3), weight_decay: r.unit() * 0.05,
        epochs: if kind == Kind::Transformer { r.pick(&[6, 8, 10]) } else { r.pick(&[10, 14, 18, 24]) }, seq_len: if kind == Kind::Transformer { r.pick(&[8, 16]) } else { 8 }, moments_per_batch: r.pick(&[8, 12, 16]), mse_weight: r.unit() * 0.3 })
}

/// Changes 1-3 genes (the mutation step of the genetic algorithm).
pub fn mutate(g: &Genome, r: &mut Rng) -> Genome {
    let mut c = *g;
    for _ in 0..(1 + r.bits() % 3) {
        match r.bits() % 9 {
            0 => c.width = ((c.width as f64) * if r.unit() < 0.5 { 0.75 } else { 1.33 }).round() as usize,
            1 => c.blocks = if r.unit() < 0.5 { c.blocks.saturating_sub(1).max(1) } else { c.blocks + 1 },
            2 => c.heads = r.pick(&[1, 2, 4]),
            3 => c.dropout += (r.unit() - 0.5) * 0.15,
            4 => c.lr *= r.log_uniform(0.5, 2.0),
            5 => c.weight_decay += (r.unit() - 0.5) * 0.03,
            6 => c.epochs = ((c.epochs as f64) * r.log_uniform(0.7, 1.4)).round() as usize,
            7 => c.seq_len = r.pick(&[16, 24, 32, 48]),
            _ => { c.mse_weight += (r.unit() - 0.5) * 0.2; c.moments_per_batch = r.pick(&[4, 8, 12, 16]); }
        }
    }
    clamp_genome(c)
}

/// Crossover: each gene from one of two parents.
pub fn crossover(a: &Genome, b: &Genome, r: &mut Rng) -> Genome {
    let p = |x: f64, y: f64, r: &mut Rng| if r.unit() < 0.5 { x } else { y };
    let kind = if r.unit() < 0.5 { a.kind } else { b.kind };
    clamp_genome(Genome { kind, width: if r.unit() < 0.5 { a.width } else { b.width }, blocks: if r.unit() < 0.5 { a.blocks } else { b.blocks }, heads: if r.unit() < 0.5 { a.heads } else { b.heads },
        dropout: p(a.dropout, b.dropout, r), lr: p(a.lr, b.lr, r), weight_decay: p(a.weight_decay, b.weight_decay, r), epochs: if r.unit() < 0.5 { a.epochs } else { b.epochs },
        seq_len: if r.unit() < 0.5 { a.seq_len } else { b.seq_len }, moments_per_batch: if r.unit() < 0.5 { a.moments_per_batch } else { b.moments_per_batch }, mse_weight: p(a.mse_weight, b.mse_weight, r) })
}

// ------------------------------------------------------------------ the population (everything on disk)
pub fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS automl (id INTEGER PRIMARY KEY, label TEXT NOT NULL UNIQUE, genome TEXT NOT NULL, status TEXT NOT NULL, parent INTEGER, ic REAL, ic_lo REAL, ic_hi REAL,
        d REAL, d_lo REAL, d_hi REAL, secs REAL, created INTEGER NOT NULL, finished INTEGER, champion INTEGER NOT NULL DEFAULT 0, objective TEXT NOT NULL DEFAULT 'standalone');")?;
    let has_objective: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('automl') WHERE name = 'objective')", [], |r| r.get(0))?;
    if !has_objective { db.execute_batch("ALTER TABLE automl ADD COLUMN objective TEXT NOT NULL DEFAULT 'standalone';")?; }
    Ok(())
}

pub fn next_genome(db: &Connection, seed: u64, objective: &str) -> Result<(Genome, Option<i64>)> {
    init(db)?;
    let mut r = Rng::new(seed);
    while let Ok((id, g)) = db.query_row("SELECT id, genome FROM automl WHERE status = 'pending' AND objective = ?1 ORDER BY id LIMIT 1", [objective], |x| Ok((x.get::<_, i64>(0)?, x.get::<_, String>(1)?))) {
        let g: Genome = serde_json::from_str(&g)?;
        if g == clamp_genome(g) { return Ok((g, Some(id))); }
        db.execute("UPDATE automl SET status = 'skipped', finished = ?1 WHERE id = ?2", params![crate::util::now_ts(), id])?;
    }
    let done: Vec<(i64, Genome, f64)> = db.prepare("SELECT id, genome, coalesce(d, -1) FROM automl WHERE status = 'done' AND objective = ?1 ORDER BY d DESC")?.query_map([objective], |x| Ok((x.get::<_, i64>(0)?, x.get::<_, String>(1)?, x.get::<_, f64>(2)?)))?
        .flatten().filter_map(|(i, g, d)| serde_json::from_str::<Genome>(&g).ok().map(|g| (i, g, d))).collect();
    let seen = |g: &Genome| db.query_row("SELECT count(*) FROM automl WHERE label = ?1", [g.label()], |x| x.get::<_, i64>(0)).unwrap_or(0) > 0;
    for _ in 0..50 {
        let (child, parent) = if done.len() < 8 || r.unit() < 0.1 { (random_genome(&mut r), None) } else {
            // tournament over the best five, then mutate (or cross with another good one)
            let top = &done[..done.len().min(5)];
            let a = &top[(r.bits() % top.len() as u64) as usize];
            if r.unit() < 0.25 { let b = &top[(r.bits() % top.len() as u64) as usize]; (crossover(&a.1, &b.1, &mut r), Some(a.0)) } else { (mutate(&a.1, &mut r), Some(a.0)) }
        };
        if !seen(&child) { return Ok((child, parent)); }
    }
    Ok((random_genome(&mut r), None))
}

// ------------------------------------------------------------------ data and evaluation
/// The rows of the bot's top-20 universe at every 15-minute decision moment, matching the live ridge's cadence.
pub fn load_rows(candle_db: &Path) -> Result<(Vec<Row>, i64)> {
    let db = Connection::open(candle_db)?;
    let series = candles::load_prepared(&db)?;
    let nb = FEATURES.len();
    let samples = mlbot::interactions(&mlbot::in_universe(&mlbot::extend(&series, &m::build(&series, 1)), nb));
    let newest = samples.iter().map(|s| s.ts).max().unwrap_or(0);
    let mut rows = deepnet::build_rows(&series, &samples, MAX_SEQ);
    rows.sort_by_key(|r| r.ts);
    Ok((rows, newest))
}

fn with_seq_len(r: &Row, l: usize) -> Row {
    let keep = l * deepnet::SEQ_F;
    Row { sym: r.sym, ts: r.ts, stat: r.stat.clone(), seq: r.seq[r.seq.len() - keep..].to_vec(), y: r.y }
}

#[derive(Debug, Clone)]
pub struct Eval { pub ic: f64, pub ic_lo: f64, pub ic_hi: f64, pub d: f64, pub d_lo: f64, pub d_hi: f64, pub secs: f64 }

/// Matches live scoring: standardise each model within each decision moment, then average them.
fn blend_scores(test: &[Sample], ridge: &[f64], deep: &[f64]) -> Vec<f64> {
    assert_eq!(test.len(), ridge.len());
    assert_eq!(test.len(), deep.len());
    let mut by = std::collections::BTreeMap::<i64, Vec<usize>>::new();
    for (i, s) in test.iter().enumerate() { by.entry(s.ts).or_default().push(i); }
    let mut out = vec![0.0; test.len()];
    for idx in by.values() {
        let r = crate::aimodel::zscore(&idx.iter().map(|&i| ridge[i]).collect::<Vec<_>>());
        let d = crate::aimodel::zscore(&idx.iter().map(|&i| deep[i]).collect::<Vec<_>>());
        for (k, &i) in idx.iter().enumerate() { out[i] = 0.5 * (r[k] + d[k]); }
    }
    out
}

/// Rolling out-of-sample test: the last `windows` blocks of 28 days, each predicted by a model trained only on data at least 3 days older. The deployed ridge/deep
/// blend is compared with the ridge on the same windows (paired, week-resampled, widened for `trials`).
pub fn evaluate(rows: &[Row], g: &Genome, windows: usize, trials: usize, threads: usize, seed: u64, scheme: Scheme) -> Option<Eval> {
    let started = std::time::Instant::now();
    let t1 = rows.last()?.ts;
    let (mut tests, mut deep_p, mut ridge_p): (Vec<Sample>, Vec<f64>, Vec<f64>) = (vec![], vec![], vec![]);
    for w in (0..windows).rev() {
        let start = t1 - (w as i64 + 1) * 28 * DAY_MS;
        let end = start + 28 * DAY_MS;
        let train: Vec<Row> = rows.iter().filter(|r| r.ts <= start - 3 * DAY_MS).map(|r| with_seq_len(r, g.seq_len)).collect();
        let test: Vec<Row> = rows.iter().filter(|r| r.ts >= start && r.ts < end).map(|r| with_seq_len(r, g.seq_len)).collect();
        if train.len() < 3000 || test.len() < 300 { continue; }
        let refs: Vec<&Row> = train.iter().collect();
        let model: DeepModel = deepnet::train(&refs, g, seed, threads);
        deep_p.extend(deepnet::predict(&model, &test));
        let to_sample = |r: &Row| Sample { ts: r.ts, week: r.ts.div_euclid(7 * DAY_MS), sym: 0, x: r.stat.clone(), excess: r.y, raw: r.y };
        let tr_s: Vec<Sample> = train.iter().map(to_sample).collect();
        let ridge = crate::mlscores::tuned_ridge(&tr_s, train.last()?.ts, scheme.window_days, scheme.half_life_days);
        let ts_s: Vec<Sample> = test.iter().map(to_sample).collect();
        ridge_p.extend(ts_s.iter().map(|s| ridge.predict(s)));
        tests.extend(ts_s);
    }
    if tests.len() < 500 { return None; }
    let blend = blend_scores(&tests, &ridge_p, &deep_p);
    let a = imp::adjusted(&tests, &blend, trials, 7);
    let (d, lo, hi) = imp::paired_ic(&tests, &ridge_p, &blend, trials, 9);
    Some(Eval { ic: a[0], ic_lo: a[1], ic_hi: a[2], d, d_lo: lo, d_hi: hi, secs: started.elapsed().as_secs_f64() })
}

// ------------------------------------------------------------------ the champion: refreshed as data grows, re-tested daily, demoted when it stops winning

fn bag_dir(root: &Path) -> std::path::PathBuf { root.join("model").join("deep").join("bag") }
fn staging_dir(root: &Path) -> std::path::PathBuf { root.join("model").join("deep").join("bag_next") }
fn meta_path(root: &Path) -> std::path::PathBuf { root.join("model").join("deep").join("champion.meta.json") }

/// The champion is a BAG of models of the same genome trained with different seeds (averaged within the moment). On the EXISTING data it grows by one seed per trainer turn up to
/// `BAG`. When new data arrives a NEW bag is built beside it (`bag_next`) and swapped in whole, so the live bag never shrinks while it is being replaced.
pub const BAG: usize = 7;

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(d) = path.parent() { std::fs::create_dir_all(d)?; }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
/// The numeric file indices present in a bag directory, ascending.
fn indices(dir: &Path) -> Vec<usize> {
    let mut v: Vec<usize> = std::fs::read_dir(dir).into_iter().flatten().flatten().filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).and_then(|n| n.parse().ok())).collect();
    v.sort();
    v
}
fn dir_models(dir: &Path) -> Vec<DeepModel> { indices(dir).into_iter().filter_map(|k| serde_json::from_slice(&std::fs::read(dir.join(format!("{k}.json"))).ok()?).ok()).collect() }
fn add_to_dir(dir: &Path, model: &DeepModel) -> Result<()> { let next = indices(dir).last().map_or(0, |k| k + 1); write_atomic(&dir.join(format!("{next}.json")), &serde_json::to_vec(model)?) }

pub fn bag_size(root: &Path) -> usize { indices(&bag_dir(root)).len() }
/// Adds a model to the LIVE bag (the first build of a champion, or growing it on the existing data).
pub fn add_to_bag(root: &Path, model: &DeepModel, trained_through_ms: i64) -> Result<()> {
    add_to_dir(&bag_dir(root), model)?;
    write_atomic(&meta_path(root), &serde_json::to_vec(&serde_json::json!({ "trained_through_ms": trained_through_ms, "genome": model.genome.label() }))?)
}
pub fn clear_champion(root: &Path) { let _ = std::fs::remove_dir_all(bag_dir(root)); let _ = std::fs::remove_dir_all(staging_dir(root)); let _ = std::fs::remove_file(meta_path(root)); }
/// All readable models of the live champion bag (empty when there is no champion); a truncated or foreign file is skipped.
pub fn load_champions(root: &Path) -> Vec<DeepModel> { dir_models(&bag_dir(root)) }
fn champion_trained_through(root: &Path) -> Option<i64> { serde_json::from_slice::<serde_json::Value>(&std::fs::read(meta_path(root)).ok()?).ok()?["trained_through_ms"].as_i64() }

/// Adds one seed to the staging bag; when it is complete it replaces the live bag in one step and the meta moves to the new data. Returns true when the swap happened.
fn grow_staging(root: &Path, model: &DeepModel, trained_through_ms: i64) -> Result<bool> {
    add_to_dir(&staging_dir(root), model)?;
    if indices(&staging_dir(root)).len() < BAG { return Ok(false); }
    let old = root.join("model").join("deep").join("bag_old");
    let _ = std::fs::remove_dir_all(&old);
    if bag_dir(root).exists() { std::fs::rename(bag_dir(root), &old)?; }
    std::fs::rename(staging_dir(root), bag_dir(root))?;
    let _ = std::fs::remove_dir_all(&old);
    write_atomic(&meta_path(root), &serde_json::to_vec(&serde_json::json!({ "trained_through_ms": trained_through_ms, "genome": model.genome.label() }))?)?;
    Ok(true)
}

/// Keeps the champion alive: retrains it on all data as soon as a new session of bars has arrived, and every hour re-tests its genome on fresh out-of-sample windows; if the paired gain over
/// the ridge is no longer above zero at the adjusted level it is DEMOTED (the file is removed, the live ranking goes back to the ridge alone).
fn tend_champion(db: &Connection, root: &Path, w: &Worker, threads: usize, scheme: Scheme, objective: &str) -> Result<Option<String>> {
    let Ok((id, genome)) = db.query_row("SELECT id, genome FROM automl WHERE champion = 1 AND objective = ?1", [objective], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) else { return Ok(None) };
    let g: Genome = serde_json::from_str(&genome)?;
    let (finished,): (i64,) = db.query_row("SELECT coalesce(finished, 0) FROM automl WHERE id = ?1", [id], |r| Ok((r.get(0)?,)))?;
    let mut out = vec![];
    if crate::util::now_ts() - finished > 3600 {
        let trials = imp::count(db) + 1;
        if let Some(e) = evaluate(&w.rows, &g, 3, trials, threads, id as u64 + 1000, scheme) {
            db.execute("UPDATE automl SET ic = ?1, ic_lo = ?2, ic_hi = ?3, d = ?4, d_lo = ?5, d_hi = ?6, finished = ?7 WHERE id = ?8", params![e.ic, e.ic_lo, e.ic_hi, e.d, e.d_lo, e.d_hi, crate::util::now_ts(), id])?;
            imp::record(db, objective, &g.label(), "ic", e.ic, e.ic_lo, e.ic_hi, e.d_lo > 0.0)?;
            if e.d_lo <= 0.0 {
                db.execute("UPDATE automl SET champion = 0 WHERE id = ?1", [id])?;
                clear_champion(root);
                crate::ops::discord("Deep champion DEMOTED", &format!("{}: re-tested on fresh data, paired gain over the ridge {:+.4} [{:+.4},{:+.4}] is no longer above zero; the live ranking uses the ridge alone again", g.label(), e.d, e.d_lo, e.d_hi));
                return Ok(Some(format!("champion {} DEMOTED after a fresh re-test (paired {:+.4} [{:+.4},{:+.4}])", g.label(), e.d, e.d_lo, e.d_hi)));
            }
            out.push(format!("champion re-tested on fresh data: paired {:+.4} [{:+.4},{:+.4}], still a champion", e.d, e.d_lo, e.d_hi));
        }
    }
    let live = bag_size(root);
    let stale = champion_trained_through(root).is_none_or(|t| w.newest - t >= DAY_MS);
    let staging = indices(&staging_dir(root)).len();
    if live > 0 && (stale || staging > 0) {
        // new data: build the next bag beside the live one and swap it in whole (the live ranking keeps its champion meanwhile)
        let rows: Vec<Row> = w.rows.iter().map(|r| with_seq_len(r, g.seq_len)).collect();
        let refs: Vec<&Row> = rows.iter().collect();
        let model = deepnet::train(&refs, &g, id as u64 * 1000 + staging as u64 + w.newest as u64, threads);
        let swapped = grow_staging(root, &model, w.newest)?;
        out.push(if swapped { format!("champion bag replaced by a new one trained through {}", crate::util::utc_minute(w.newest / 1000)) } else { format!("next champion bag: seed {} of {BAG} (the live bag stays in use)", staging + 1) });
    } else if live < BAG {
        let rows: Vec<Row> = w.rows.iter().map(|r| with_seq_len(r, g.seq_len)).collect();
        let refs: Vec<&Row> = rows.iter().collect();
        let model = deepnet::train(&refs, &g, id as u64 * 1000 + live as u64 + w.newest as u64, threads);
        add_to_bag(root, &model, w.newest)?;
        out.push(format!("champion bag: seed {} of {BAG} trained on {} rows through {}", live + 1, rows.len(), crate::util::utc_minute(w.newest / 1000)));
    }
    Ok(if out.is_empty() { None } else { Some(out.join("; ")) })
}

// ------------------------------------------------------------------ one turn of the worker
pub struct Worker { pub rows: Vec<Row>, pub newest: i64 }

/// The trainer stands down above 1.5 load per core (it is also niced to the lowest priority and uses fewer threads while the US market is open).
pub fn load_too_high(load: f64, cores: f64) -> bool { load > 1.5 * cores }

/// Threads for the next step: two while the US market is open (13:00-21:00 UTC) so the live bot keeps its headroom, the configured number otherwise.
pub fn threads_now(configured: usize) -> usize { let hour = (crate::util::now_ts() / 3600) % 24; if (13..21).contains(&hour) { configured.min(2) } else { configured } }

/// Why the trainer should not run right now (None = go). The trainer is for the quiet hours of the machine, never at the cost of the bot or the Mac.
pub fn blocked(root: &Path) -> Option<String> {
    for f in ["STOP", "MAINTENANCE", "trainer/OFF", "brain/OFF"] { if root.join(f).exists() { return Some(format!("switch file {f}")); } }
    let s = crate::ops::take_sample();
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get()) as f64;
    if load_too_high(s.load, cores) { return Some(format!("load {:.1} is high for {cores:.0} cores", s.load)); }
    if s.memfree.is_some_and(|m| m < 35) { return Some(format!("free memory {}% is low", s.memfree.unwrap_or(0))); }
    if s.swap_mb > 1500 { return Some(format!("swap {} MB is in use", s.swap_mb)); }
    None
}

/// One iteration: refresh the data if there are newer bars, pick the next genome, train and evaluate it, record it, and keep the champion. Returns a report line.
pub fn step(candle_db: &Path, root: &Path, w: &mut Option<Worker>, threads: usize) -> Result<String> {
    let db = Connection::open(candle_db)?;
    init(&db)?; imp::init(&db)?;
    let scheme = Scheme::from_ledger(&db);
    let objective = objective_for(scheme);
    let legacy_champions: i64 = db.query_row("SELECT count(*) FROM automl WHERE champion = 1 AND objective != ?1", [&objective], |r| r.get(0))?;
    if legacy_champions > 0 {
        db.execute("UPDATE automl SET champion = 0 WHERE objective != ?1", [&objective])?;
        clear_champion(root);
    }
    let newest: i64 = db.query_row("SELECT coalesce(max(ts_ms), 0) FROM candles15", [], |r| r.get(0))?;
    if w.as_ref().is_none_or(|x| newest > x.newest + 6 * 3_600_000) { let (rows, n) = load_rows(candle_db)?; *w = Some(Worker { rows, newest: n.max(newest) }); }
    let worker = w.as_ref().ok_or_else(|| anyhow::anyhow!("no data"))?;
    let tended = tend_champion(&db, root, worker, threads, scheme, &objective)?;
    let (g, parent) = next_genome(&db, crate::util::now_ts() as u64, &objective)?;
    let label = g.label();
    let now = crate::util::now_ts();
    db.execute("INSERT OR IGNORE INTO automl(label, genome, status, parent, created, objective) VALUES (?1, ?2, 'pending', ?3, ?4, ?5)", params![label, serde_json::to_string(&g)?, parent, now, objective])?;
    let id: i64 = db.query_row("SELECT id FROM automl WHERE label = ?1", [&label], |r| r.get(0))?;
    let trials = imp::count(&db) + 1;
    let Some(e) = evaluate(&worker.rows, &g, 3, trials, threads, id as u64, scheme) else { db.execute("UPDATE automl SET status = 'skipped', finished = ?1 WHERE id = ?2", params![now, id])?; return Ok(format!("{label}: not enough data to evaluate")) };
    db.execute("UPDATE automl SET status = 'done', ic = ?1, ic_lo = ?2, ic_hi = ?3, d = ?4, d_lo = ?5, d_hi = ?6, secs = ?7, finished = ?8 WHERE id = ?9", params![e.ic, e.ic_lo, e.ic_hi, e.d, e.d_lo, e.d_hi, e.secs, crate::util::now_ts(), id])?;
    let champion = e.d_lo > 0.0;
    imp::record(&db, &objective, &label, "ic", e.ic, e.ic_lo, e.ic_hi, champion)?;
    let mut line = format!("{label}: blend IC {:+.4} [{:+.4},{:+.4}], paired vs the ridge {:+.4} [{:+.4},{:+.4}] at {trials} trials, {:.0}s -> {}", e.ic, e.ic_lo, e.ic_hi, e.d, e.d_lo, e.d_hi, e.secs, if champion { "CHAMPION" } else { "no evidence" });
    if champion {
        db.execute("UPDATE automl SET champion = 0", [])?;
        db.execute("UPDATE automl SET champion = 1 WHERE id = ?1", [id])?;
        // retrain on everything and save it for live blending
        let rows: Vec<Row> = worker.rows.iter().map(|r| with_seq_len(r, g.seq_len)).collect();
        let refs: Vec<&Row> = rows.iter().collect();
        let model = deepnet::train(&refs, &g, id as u64, threads);
        clear_champion(root);
        add_to_bag(root, &model, worker.newest)?;
        line += &format!("; trained on all {} rows as the first model of the champion bag", rows.len());
    }
    Ok(match tended { Some(t) => format!("{t} | {line}"), None => line })
}

/// The worker: forever, one genome after another, whatever the hour; it only pauses for the switch files and an overloaded machine (re-checked every minute).
/// Everything it learns is on disk (the population table, the trial ledger, the champion file), so a restart loses nothing.
pub fn run_forever(candle_db: &Path, root: &Path, logs: &Path, threads: usize) -> ! {
    let log = |line: String| { let l = format!("{} {}\n", crate::util::utc_minute(crate::util::now_ts()), line); let _ = std::fs::OpenOptions::new().create(true).append(true).open(logs.join("trainer.log")).and_then(|mut f| std::io::Write::write_all(&mut f, l.as_bytes())); };
    let mut worker: Option<Worker> = None;
    let mut last_block = String::new();
    let mut last_pause_line = 0i64;
    log(format!("trainer started: {threads} threads, low priority"));
    loop {
        if let Some(why) = blocked(root) {
            if why != last_block || crate::util::now_ts() - last_pause_line > 1800 { log(format!("paused: {why}")); last_block = why; last_pause_line = crate::util::now_ts(); } // a heartbeat every half hour while paused, so the dashboard can tell paused from dead
            std::thread::sleep(std::time::Duration::from_secs(60));
            continue;
        }
        if !last_block.is_empty() { log("resumed".into()); last_block.clear(); }
        match step(candle_db, root, &mut worker, threads_now(threads)) {
            Ok(line) => { log(line.clone()); if line.contains("CHAMPION") { crate::ops::discord("Deep model CHAMPION (paired vs the ridge, multiple-testing adjusted)", &line); } }
            Err(e) => { log(format!("step failed: {e:#}")); std::thread::sleep(std::time::Duration::from_secs(120)); }
        }
        // and one cheap ridge experiment (training window / recency) between genomes, so the existing data is questioned at the trainer's pace, not once an hour
        match crate::mlround::explore_next(candle_db, &root.join("data").join("data.sqlite3")) {
            Ok(l) => { log(l.clone()); if l.contains("ADOPTED") { crate::ops::discord("ML experiment ADOPTED (paired, multiple-testing adjusted)", &l); } }
            Err(e) => log(format!("experiment failed: {e:#}")),
        }
        std::thread::sleep(std::time::Duration::from_secs(20));
    }
}

/// The population's state as text: how many genomes tried, the best by paired gain over the ridge, the champion.
pub fn status(db: &Connection) -> Result<String> {
    init(db)?;
    let objective = objective_for(Scheme::from_ledger(db));
    let (n, done): (i64, i64) = db.query_row("SELECT count(*), coalesce(sum(status = 'done'), 0) FROM automl WHERE objective = ?1", [&objective], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let best = db.query_row("SELECT label, ic, d, d_lo, d_hi FROM automl WHERE status = 'done' AND objective = ?1 ORDER BY d DESC LIMIT 1", [&objective], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?, r.get::<_, f64>(4)?))).ok();
    let champ: Option<String> = db.query_row("SELECT label FROM automl WHERE champion = 1 AND objective = ?1", [&objective], |r| r.get(0)).ok();
    Ok(format!("{n} {objective} genomes proposed, {done} evaluated; best by paired gain over the ridge: {}; champion: {}",
        best.map_or("none yet".into(), |b| format!("{} (blend IC {:+.4}, paired {:+.4} [{:+.4},{:+.4}])", b.0, b.1, b.2, b.3, b.4)), champ.unwrap_or_else(|| "none (nothing has beaten the ridge at the adjusted level)".into())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn the_search_scores_the_same_within_moment_blend_as_live_inference() {
        let at = |ts| Sample { ts, week: 0, sym: 0, x: vec![], excess: 0.0, raw: 0.0 };
        let test = vec![at(1), at(1), at(1), at(2), at(2), at(2)];
        let ridge = [1.0, 2.0, 3.0, 100.0, 200.0, 300.0];
        let deep = [3.0, 2.0, 1.0, 30.0, 20.0, 10.0];
        let blend = blend_scores(&test, &ridge, &deep);
        assert!(blend.iter().all(|x| x.abs() < 1e-12), "opposing rankings cancel within each moment: {blend:?}");
        let deep = [1.0, 2.0, 3.0, 10.0, 20.0, 30.0];
        let blend = blend_scores(&test, &ridge, &deep);
        assert!((blend[0] + (1.5f64).sqrt()).abs() < 1e-12);
        assert_eq!(&blend[..3], &blend[3..]);
    }

    #[test]
    fn old_standalone_trials_are_preserved_but_not_used_as_blend_parents() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE automl (id INTEGER PRIMARY KEY, label TEXT NOT NULL UNIQUE, genome TEXT NOT NULL, status TEXT NOT NULL, parent INTEGER, ic REAL, ic_lo REAL, ic_hi REAL, d REAL, d_lo REAL, d_hi REAL, secs REAL, created INTEGER NOT NULL, finished INTEGER, champion INTEGER NOT NULL DEFAULT 0);").unwrap();
        let g = random_genome(&mut Rng::new(7));
        db.execute("INSERT INTO automl(label, genome, status, d, created) VALUES (?1, ?2, 'done', 0.5, 0)", params![g.label(), serde_json::to_string(&g).unwrap()]).unwrap();
        init(&db).unwrap();
        let objective: String = db.query_row("SELECT objective FROM automl WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(objective, "standalone");
        assert!(next_genome(&db, 8, &objective_for(Scheme::default())).unwrap().1.is_none());
        assert!(status(&db).unwrap().contains("0 ridge-blend-15m-v2/all-history genomes"));
    }

    #[test]
    fn a_new_live_ridge_scheme_gets_its_own_trial_population() {
        let expanding = objective_for(Scheme::default());
        let rolling = objective_for(Scheme { window_days: Some(126), half_life_days: None });
        assert_ne!(expanding, rolling);
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let g = random_genome(&mut Rng::new(7));
        db.execute("INSERT INTO automl(label, genome, status, d, created, objective) VALUES (?1, ?2, 'done', 0.5, 0, ?3)", params![g.label(), serde_json::to_string(&g).unwrap(), expanding]).unwrap();
        assert!(next_genome(&db, 8, &rolling).unwrap().1.is_none());
    }

    #[test]
    fn genomes_stay_trainable_mutate_and_cross_without_leaving_the_allowed_range() {
        let mut r = Rng::new(5);
        for _ in 0..300 {
            let g = random_genome(&mut r);
            let m = mutate(&g, &mut r);
            let c = crossover(&g, &m, &mut r);
            for x in [g, m, c] {
                assert_eq!(x.width % x.heads, 0, "heads divide the width: {x:?}");
                assert!((0.0..=0.4).contains(&x.dropout) && (2e-4..=8e-3).contains(&x.lr) && (4..=24).contains(&x.moments_per_batch));
                match x.kind {
                    Kind::Res => assert!((16..=64).contains(&x.width) && (1..=3).contains(&x.blocks) && x.seq_len == 8 && (6..=24).contains(&x.epochs), "{x:?}"),
                    Kind::Transformer => assert!((8..=16).contains(&x.width) && x.blocks == 1 && (8..=16).contains(&x.seq_len) && (6..=10).contains(&x.epochs) && x.moments_per_batch >= 8, "{x:?}"),
                }
            }
        }
        let g = random_genome(&mut r);
        assert!((0..20).any(|_| mutate(&g, &mut r) != g), "mutation changes something");
    }

    #[test]
    fn oversized_pending_genomes_are_skipped_before_training() {
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let objective = objective_for(Scheme::default());
        let mut oversized = random_genome(&mut Rng::new(17));
        oversized.kind = Kind::Transformer;
        oversized.width = 32;
        oversized.blocks = 2;
        oversized.seq_len = 48;
        oversized.epochs = 24;
        let label = oversized.label();
        db.execute("INSERT INTO automl(label, genome, status, created, objective) VALUES (?1, ?2, 'pending', 0, ?3)", params![label, serde_json::to_string(&oversized).unwrap(), objective]).unwrap();
        let (next, parent) = next_genome(&db, 18, &objective).unwrap();
        assert_eq!(next, clamp_genome(next));
        assert!(parent.is_none());
        let status: String = db.query_row("SELECT status FROM automl WHERE label = ?1", [&label], |r| r.get(0)).unwrap();
        assert_eq!(status, "skipped");
    }

    #[test]
    fn the_population_proposes_random_genomes_first_then_children_of_the_best_never_repeating_one() {
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let objective = objective_for(Scheme::default());
        let mut seen = std::collections::BTreeSet::new();
        for i in 0..14u64 {
            let (g, parent) = next_genome(&db, 100 + i, &objective).unwrap();
            assert!(seen.insert(g.label()), "never proposes the same genome twice: {}", g.label());
            if i < 8 { assert!(parent.is_none(), "the first eight are random"); }
            db.execute("INSERT INTO automl(label, genome, status, parent, d, created, objective) VALUES (?1, ?2, 'done', ?3, ?4, 0, ?5)", params![g.label(), serde_json::to_string(&g).unwrap(), parent, i as f64 * 0.001, objective]).unwrap();
        }
        let kids = db.query_row("SELECT count(*) FROM automl WHERE parent IS NOT NULL", [], |r| r.get::<_, i64>(0)).unwrap();
        assert!(kids >= 3, "after the random start most proposals have a parent among the best: {kids}");
        // a pending genome is always run before a new one is made
        let (g, _) = next_genome(&db, 7, &objective).unwrap();
        db.execute("INSERT INTO automl(label, genome, status, created, objective) VALUES (?1, ?2, 'pending', 0, ?3)", params![g.label(), serde_json::to_string(&g).unwrap(), objective]).unwrap();
        assert_eq!(next_genome(&db, 8, &objective).unwrap().0, g);
        assert!(status(&db).unwrap().contains("genomes proposed"));
    }

    #[test]
    fn the_champion_bag_grows_one_seed_at_a_time_loads_back_and_can_be_cleared() {
        let d = tempfile::tempdir().unwrap();
        let g = clamp_genome(Genome { kind: Kind::Res, width: 16, blocks: 1, heads: 1, dropout: 0.0, lr: 0.01, weight_decay: 0.0, epochs: 2, seq_len: 8, moments_per_batch: 4, mse_weight: 0.1 });
        let mut s = 5u64; let mut rnd = move || { s ^= s << 13; s ^= s >> 7; s ^= s << 17; (s >> 11) as f64 / (1u64 << 53) as f64 - 0.5 };
        let rows: Vec<Row> = (0..40i64).flat_map(|day| (0..10).map(move |k| (day, k))).map(|(day, _)| Row { sym: 0, ts: day * DAY_MS, stat: (0..3).map(|_| rnd()).collect(), seq: vec![0.0; 8 * deepnet::SEQ_F], y: rnd() }).collect();
        let refs: Vec<&Row> = rows.iter().collect();
        assert_eq!((bag_size(d.path()), load_champions(d.path()).len()), (0, 0));
        for seed in 0..3 { add_to_bag(d.path(), &deepnet::train(&refs, &g, seed, 1), 123).unwrap(); assert_eq!(bag_size(d.path()), seed as usize + 1); }
        let bag = load_champions(d.path());
        assert_eq!(bag.len(), 3);
        assert_ne!(deepnet::predict(&bag[0], &rows[..3]), deepnet::predict(&bag[1], &rows[..3]), "different seeds give different members");
        assert_eq!(champion_trained_through(d.path()), Some(123));
        clear_champion(d.path());
        assert_eq!((bag_size(d.path()), champion_trained_through(d.path())), (0, None));
    }

    #[test]
    fn the_trainer_stops_for_the_switch_files() {
        let d = tempfile::tempdir().unwrap();
        for f in ["STOP", "MAINTENANCE", "brain/OFF", "trainer/OFF"] {
            std::fs::create_dir_all(d.path().join(f).parent().unwrap()).unwrap();
            std::fs::write(d.path().join(f), "").unwrap();
            assert!(blocked(d.path()).is_some_and(|w| w.contains(f)), "{f}");
            std::fs::remove_file(d.path().join(f)).unwrap();
        }
    }

    #[test]
    fn a_new_bag_is_built_beside_the_live_one_and_swapped_in_whole_and_damaged_files_are_skipped() {
        let d = tempfile::tempdir().unwrap();
        let g = clamp_genome(Genome { kind: Kind::Res, width: 16, blocks: 1, heads: 1, dropout: 0.0, lr: 0.01, weight_decay: 0.0, epochs: 1, seq_len: 8, moments_per_batch: 4, mse_weight: 0.1 });
        let mut s = 3u64; let mut rnd = move || { s ^= s << 13; s ^= s >> 7; s ^= s << 17; (s >> 11) as f64 / (1u64 << 53) as f64 - 0.5 };
        let rows: Vec<Row> = (0..30i64).flat_map(|day| (0..10).map(move |_| day)).map(|day| Row { sym: 0, ts: day * DAY_MS, stat: (0..3).map(|_| rnd()).collect(), seq: vec![0.0; 8 * deepnet::SEQ_F], y: rnd() }).collect();
        let refs: Vec<&Row> = rows.iter().collect();
        let m = |seed| deepnet::train(&refs, &g, seed, 1);
        for k in 0..3 { add_to_bag(d.path(), &m(k), 100).unwrap(); }
        assert_eq!((bag_size(d.path()), champion_trained_through(d.path())), (3, Some(100)));
        // new data: the next bag grows in staging while the live bag is untouched
        for k in 0..BAG - 1 { assert!(!grow_staging(d.path(), &m(10 + k as u64), 200).unwrap()); assert_eq!((load_champions(d.path()).len(), champion_trained_through(d.path())), (3, Some(100)), "the live bag never shrinks while it is being replaced"); }
        assert!(grow_staging(d.path(), &m(99), 200).unwrap(), "the last seed completes the bag and swaps it in");
        assert_eq!((load_champions(d.path()).len(), champion_trained_through(d.path()), indices(&staging_dir(d.path())).len()), (BAG, Some(200), 0));
        // a truncated file (a crash mid-write) is skipped, not fatal, and the next member never overwrites an existing index
        std::fs::write(bag_dir(d.path()).join("3.json"), b"{\"genome\":").unwrap();
        assert_eq!(load_champions(d.path()).len(), BAG - 1);
        add_to_bag(d.path(), &m(5), 300).unwrap();
        assert_eq!(indices(&bag_dir(d.path())).last(), Some(&BAG), "the new member took a fresh index");
    }

    #[test]
    fn the_load_guard_scales_with_the_cores_and_the_market_hours_use_fewer_threads() {
        assert!(!load_too_high(14.9, 10.0) && load_too_high(15.1, 10.0) && !load_too_high(7.0, 10.0));
        assert!(threads_now(3) <= 3 && threads_now(3) >= 2);
    }

    /// REAL data, read-only (the lab's own candle store): the evaluation protocol must work on it, and the leakage control must come out empty -- a model trained on real
    /// features but labels shuffled within each moment has nothing to find, so its out-of-sample IC must be near zero and it must not beat the ridge. Skipped when the
    /// store is absent (a fresh checkout).
    #[test]
    fn on_the_real_candles_the_protocol_runs_and_shuffled_labels_find_nothing() {
        let path = crate::platform::lab_root().join("data/candles.sqlite3");
        if !path.exists() { return; }
        let (rows, newest) = load_rows(&path).expect("real rows");
        assert!(rows.len() > 5000 && newest > 0 && rows.iter().all(|r| r.seq.len() == MAX_SEQ * deepnet::SEQ_F && r.stat.iter().all(|v| v.is_finite())), "{} real rows", rows.len());
        assert!(rows.windows(2).any(|v| v[1].ts - v[0].ts == 900_000), "the evaluation includes adjacent 15-minute decisions");
        let g = clamp_genome(Genome { kind: Kind::Res, width: 16, blocks: 1, heads: 1, dropout: 0.0, lr: 0.003, weight_decay: 0.01, epochs: 6, seq_len: 8, moments_per_batch: 8, mse_weight: 0.1 });
        let real = evaluate(&rows, &g, 2, 30, 2, 1, Scheme::default()).expect("an evaluation on real data");
        assert!(real.ic.is_finite() && real.secs > 0.0);
        // the control: shuffle the labels WITHIN each moment (real features, real returns, no link between them)
        let mut shuffled = rows.clone();
        let mut by: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
        for (i, r) in rows.iter().enumerate() { by.entry(r.ts).or_default().push(i); }
        let mut st = 7u64;
        for idx in by.values() { for k in (1..idx.len()).rev() { st ^= st << 13; st ^= st >> 7; st ^= st << 17; let j = (st % (k as u64 + 1)) as usize; shuffled[idx[k]].y = rows[idx[j]].y; shuffled[idx[j]].y = rows[idx[k]].y; } }
        let control = evaluate(&shuffled, &g, 2, 30, 2, 1, Scheme::default()).expect("a control evaluation");
        assert!(control.ic.abs() < 0.06, "shuffled labels must show no skill: IC {:+.4}", control.ic);
        assert!(control.d_lo <= 0.0, "and must not beat the ridge: {control:?}");
    }
}
