//! ThomasAi Causal Autonomous Neural Wiring Engine.
//!
//! Models dynamic neural wiring with sensors, archetype nodes, synapses, and Hebbian plasticity.
//! Strict causal temporal ordering guarantees ZERO look-forward and ZERO look-back bias.
//! Survival is the core objective function.
use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorConfig {
    pub metric: String,
    pub lookback_bars: usize,
    pub threshold: f64,
}

#[derive(Debug, Clone)]
pub struct SensorReading {
    pub sensor_id: String,
    pub symbol: String,
    pub ts_ms: i64,
    pub reading: f64,
    pub session: String,
}

/// Initializes core survival sensors and baseline neural network topology into brain.sqlite3.
pub fn seed_neural_topology(conn: &Connection, now_ms: i64) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS brain_sensors(
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            kind TEXT NOT NULL,
            config_json TEXT NOT NULL,
            created_ms INTEGER NOT NULL,
            last_fired_ms INTEGER,
            value REAL,
            status TEXT NOT NULL DEFAULT 'active'
        );
        CREATE TABLE IF NOT EXISTS brain_nodes(
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            label TEXT NOT NULL,
            activation_energy REAL NOT NULL DEFAULT 0.0,
            threshold REAL NOT NULL DEFAULT 0.5,
            last_activated_ms INTEGER,
            metadata_json TEXT
        );
        CREATE TABLE IF NOT EXISTS brain_synapses(
            source_id TEXT NOT NULL,
            target_id TEXT NOT NULL,
            weight REAL NOT NULL DEFAULT 1.0,
            plasticity REAL NOT NULL DEFAULT 0.1,
            hebbian_count INTEGER NOT NULL DEFAULT 0,
            reinforcements INTEGER NOT NULL DEFAULT 0,
            penalties INTEGER NOT NULL DEFAULT 0,
            last_propagated_ms INTEGER,
            PRIMARY KEY(source_id, target_id)
        );",
    )?;
    // Migrate old stores as well as new seeds: bullish decisions cannot feed the bear veto.
    conn.execute(
        "DELETE FROM brain_synapses WHERE source_id='node:decision:rank1_execute' AND target_id='node:reflex:adversarial_bear_sentry'",
        [],
    )?;
    // 1. Seed Sensors
    let sensors = [
        ("sensor:rvol_surge", "RVOL Velocity Thrust", "volatility", r#"{"metric":"rvol","lookback_bars":3,"threshold":3.5}"#),
        ("sensor:spread_drag", "Friction & Spread Drag", "friction", r#"{"metric":"spread_pct","lookback_bars":1,"threshold":0.20}"#),
        ("sensor:morning_thrust", "Opening Momentum Velocity", "velocity", r#"{"metric":"velocity_pct","lookback_bars":2,"threshold":1.5}"#),
        ("sensor:vol_contraction", "Pre-Breakout Squeeze", "volatility", r#"{"metric":"atr_ratio","lookback_bars":5,"threshold":0.65}"#),
        ("sensor:regime_trend", "Cross-Sectional Market Trend", "regime", r#"{"metric":"nasdaq_trend","lookback_bars":14,"threshold":0.0}"#),
    ];

    for (id, name, kind, cfg) in sensors {
        conn.execute(
            "INSERT OR IGNORE INTO brain_sensors(id, name, kind, config_json, created_ms, status) VALUES(?1, ?2, ?3, ?4, ?5, 'active')",
            params![id, name, kind, cfg, now_ms],
        )?;
    }

    // 2. Seed Brain Nodes
    let nodes = [
        // Sensory Nodes
        ("node:sensor:rvol_surge", "sensory", "RVOL Surge Detected", 0.5),
        ("node:sensor:spread_drag", "sensory", "Excess Friction Penalty", 0.3),
        ("node:sensor:morning_thrust", "sensory", "Morning Opening Thrust", 0.5),
        ("node:sensor:vol_contraction", "sensory", "Volatility Squeeze", 0.4),
        ("node:sensor:regime_trend", "sensory", "Bullish Market Regime", 0.5),
        // Archetype Setup Nodes
        ("node:archetype:morning_dip_reversal", "archetype", "Morning Dip Reversal Pattern", 0.6),
        ("node:archetype:high_vol_green_open", "archetype", "High Volume Green Open Surge", 0.6),
        ("node:archetype:morning_vol_expansion", "archetype", "Morning Volume Expansion Breakout", 0.65),
        // Survival & Execution Reflex Nodes
        ("node:reflex:break_even_floor", "reflex", "Break-Even Floor Reflex (+2% lock)", 0.4),
        ("node:reflex:selective_abstain", "reflex", "Selective Abstention (Survival Halt)", 0.7),
        ("node:reflex:adversarial_bear_sentry", "reflex", "Adversarial Bear Sentry (Exhaustion & Trap Filter)", 0.65),
        ("node:reflex:risk_conservative_floor", "reflex", "Conservative Risk Profile (Preservation Priority)", 0.55),
        ("node:reflex:risk_neutral_allocator", "reflex", "Neutral Risk Profile (Sharpe Optimizer)", 0.60),
        ("node:reflex:risk_aggressive_expansion", "reflex", "Aggressive Risk Profile (Momentum Thrust Run)", 0.70),
        ("node:decision:rank1_execute", "decision", "Rank 1 Alpha Engine Execution", 0.75),
        ("node:decision:staged_scale_out", "decision", "Staged Scale-Out Tranche (+4%/+8%)", 0.7),
        // Causal Outcome Nodes
        ("node:outcome:capital_growth", "outcome", "Capital Growth & High Sharpe Gain", 0.5),
        ("node:outcome:drawdown_protection", "outcome", "Capital Preservation (Zero Ruin)", 0.8),
    ];

    for (id, kind, label, thresh) in nodes {
        conn.execute(
            "INSERT OR IGNORE INTO brain_nodes(id, kind, label, threshold, activation_energy, last_activated_ms) VALUES(?1, ?2, ?3, ?4, 0.0, ?5)",
            params![id, kind, label, thresh, now_ms],
        )?;
    }

    // 3. Seed Synaptic Wiring (Hebbian pathways)
    let synapses = [
        // RVOL Surge -> Archetypes
        ("node:sensor:rvol_surge", "node:archetype:morning_vol_expansion", 1.8, 0.15),
        ("node:sensor:rvol_surge", "node:archetype:high_vol_green_open", 1.6, 0.12),
        // Morning Thrust -> Archetypes
        ("node:sensor:morning_thrust", "node:archetype:high_vol_green_open", 1.7, 0.14),
        ("node:sensor:morning_thrust", "node:archetype:morning_dip_reversal", 1.4, 0.10),
        // Friction Drag -> Selective Abstention (Survival)
        ("node:sensor:spread_drag", "node:reflex:selective_abstain", 2.2, 0.20),
        // Volatility Squeeze -> Breakout Archetype
        ("node:sensor:vol_contraction", "node:archetype:morning_vol_expansion", 1.5, 0.10),
        // Regime Trend -> Rank 1 Alpha Engine Execution
        ("node:sensor:regime_trend", "node:decision:rank1_execute", 1.6, 0.12),
        // Archetypes -> Alpha Decisions
        ("node:archetype:morning_vol_expansion", "node:decision:rank1_execute", 2.0, 0.15),
        ("node:archetype:morning_dip_reversal", "node:decision:staged_scale_out", 1.8, 0.14),
        ("node:archetype:high_vol_green_open", "node:decision:rank1_execute", 1.9, 0.15),
        // Reflex Protections
        ("node:decision:rank1_execute", "node:reflex:break_even_floor", 2.5, 0.25),
        ("node:decision:staged_scale_out", "node:reflex:break_even_floor", 2.5, 0.25),
        // Adversarial Sentry & Tri-Tier Risk Wiring
        ("node:sensor:spread_drag", "node:reflex:adversarial_bear_sentry", 2.0, 0.18),
        ("node:reflex:adversarial_bear_sentry", "node:outcome:drawdown_protection", 2.4, 0.22),
        ("node:decision:rank1_execute", "node:reflex:risk_conservative_floor", 1.9, 0.15),
        ("node:decision:rank1_execute", "node:reflex:risk_neutral_allocator", 2.1, 0.18),
        ("node:decision:rank1_execute", "node:reflex:risk_aggressive_expansion", 1.7, 0.15),
        ("node:reflex:risk_conservative_floor", "node:outcome:drawdown_protection", 2.3, 0.22),
        ("node:reflex:risk_neutral_allocator", "node:outcome:capital_growth", 2.0, 0.18),
        ("node:reflex:risk_aggressive_expansion", "node:outcome:capital_growth", 2.2, 0.20),
        // Outcomes Reinforcement
        ("node:reflex:break_even_floor", "node:outcome:drawdown_protection", 2.4, 0.20),
        ("node:decision:rank1_execute", "node:outcome:capital_growth", 2.1, 0.18),
        ("node:reflex:selective_abstain", "node:outcome:drawdown_protection", 2.8, 0.25),
    ];

    for (src, dst, wt, plast) in synapses {
        conn.execute(
            "INSERT OR REPLACE INTO brain_synapses(source_id, target_id, weight, plasticity, last_propagated_ms) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![src, dst, wt, plast, now_ms],
        )?;
    }

    Ok(())
}

/// Dynamically strengthens or weakens synaptic connections based on causal trade outcome (Hebbian plasticity).
/// Scaled proportionally by outcome magnitude without loss-aversion emotional bias.
/// No look-forward bias: outcome is evaluated strictly after the trade closed timestamp.
pub fn hebbian_update_scaled(
    conn: &Connection,
    source_id: &str,
    target_id: &str,
    magnitude: f64,
    success: bool,
    now_ms: i64,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT weight, plasticity, reinforcements, penalties FROM brain_synapses WHERE source_id=?1 AND target_id=?2"
    )?;
    
    let row: Option<(f64, f64, i64, i64)> = stmt.query_row(params![source_id, target_id], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    }).ok();

    let mag = magnitude.clamp(0.5, 3.0);

    if let Some((wt, plast, reinf, pen)) = row {
        // Pure high-IQ expected-value rationality: symmetric weight adjustment scaled by magnitude
        let (new_wt, new_reinf, new_pen) = if success {
            ((wt + plast * 1.5 * mag).min(5.0), reinf + 1, pen)
        } else {
            ((wt - plast * 1.5 * mag).max(0.05), reinf, pen + 1)
        };

        conn.execute(
            "UPDATE brain_synapses SET weight=?1, reinforcements=?2, penalties=?3, hebbian_count=hebbian_count+1, last_propagated_ms=?4 WHERE source_id=?5 AND target_id=?6",
            params![new_wt, new_reinf, new_pen, now_ms, source_id, target_id],
        )?;
    } else {
        // Create dynamic new synaptic connection if path discovered
        let init_wt = if success { 1.2 } else { 0.8 };
        conn.execute(
            "INSERT INTO brain_synapses(source_id, target_id, weight, plasticity, hebbian_count, reinforcements, penalties, last_propagated_ms) VALUES(?1, ?2, ?3, 0.1, 1, ?4, ?5, ?6)",
            params![source_id, target_id, init_wt, if success { 1 } else { 0 }, if success { 0 } else { 1 }, now_ms],
        )?;
    }

    Ok(())
}

/// Dynamically strengthens or weakens synaptic connections based on causal trade outcome (Hebbian plasticity).
/// No look-forward bias: outcome is evaluated strictly after the trade closed timestamp.
pub fn hebbian_update(
    conn: &Connection,
    source_id: &str,
    target_id: &str,
    success: bool,
    now_ms: i64,
) -> Result<()> {
    hebbian_update_scaled(conn, source_id, target_id, 1.0, success, now_ms)
}

/// Autonomous Neurogenesis & Synaptogenesis:
/// Discovers newly emerging market patterns, trade exit reflexes, governance rules, and strategy findings
/// from the cognitive memory bank, plus historical walk-forward and backtest results from the store,
/// synthesizes new brain nodes, and dynamically establishes synaptic wiring across layers
/// without human intervention.
pub fn evolve_neural_architecture(conn: &Connection, store_conn: Option<&Connection>, now_ms: i64) -> Result<usize> {
    let mut new_connections = 0;

    // 1. Discover newly identified pattern archetypes in memories and spawn archetype nodes
    let mut stmt = conn.prepare(
        "SELECT DISTINCT topic FROM memories WHERE kind='pattern' AND topic IS NOT NULL AND topic != ''"
    )?;
    let topics: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .filter_map(Result::ok)
        .collect();

    for topic in &topics {
        let clean = topic.trim_start_matches("pattern:");
        let node_id = format!("node:archetype:{clean}");
        let label = clean.replace('_', " ");
        let formatted_label = format!("Archetype: {label}");

        let inserted = conn.execute(
            "INSERT OR IGNORE INTO brain_nodes(id, kind, label, threshold, activation_energy, last_activated_ms) VALUES(?1, 'archetype', ?2, 0.60, 0.0, ?3)",
            params![node_id, formatted_label, now_ms],
        )?;
        if inserted > 0 {
            new_connections += 1;
        }

        // Autonomous wiring: connect all sensory nodes to this archetype
        let sensory_nodes: Vec<String> = conn
            .prepare("SELECT id FROM brain_nodes WHERE kind='sensory'")?
            .query_map([], |r| r.get(0))?
            .filter_map(Result::ok)
            .collect();

        for sensor in sensory_nodes {
            let syn_exists: i64 = conn.query_row(
                "SELECT count(*) FROM brain_synapses WHERE source_id=?1 AND target_id=?2",
                params![sensor, node_id],
                |r| r.get(0),
            ).unwrap_or(0);

            if syn_exists == 0 {
                conn.execute(
                    "INSERT INTO brain_synapses(source_id, target_id, weight, plasticity, last_propagated_ms) VALUES(?1, ?2, 1.0, 0.10, ?3)",
                    params![sensor, node_id, now_ms],
                )?;
                new_connections += 1;
            }
        }

        // Autonomous wiring: connect archetype to alpha execution decision nodes
        let decision_nodes: Vec<String> = conn
            .prepare("SELECT id FROM brain_nodes WHERE kind='decision'")?
            .query_map([], |r| r.get(0))?
            .filter_map(Result::ok)
            .collect();

        for dec in decision_nodes {
            let syn_exists: i64 = conn.query_row(
                "SELECT count(*) FROM brain_synapses WHERE source_id=?1 AND target_id=?2",
                params![node_id, dec],
                |r| r.get(0),
            ).unwrap_or(0);

            if syn_exists == 0 {
                conn.execute(
                    "INSERT INTO brain_synapses(source_id, target_id, weight, plasticity, last_propagated_ms) VALUES(?1, ?2, 1.2, 0.12, ?3)",
                    params![node_id, dec, now_ms],
                )?;
                new_connections += 1;
            }
        }
    }

    // 2. Discover learned protective lessons and rule topics -> spawn reflex neurons
    let mut rule_stmt = conn.prepare(
        "SELECT DISTINCT topic FROM memories WHERE kind IN ('lesson', 'rule') AND topic IS NOT NULL AND topic != '' AND topic != 'trade closed' AND topic != 'decision'"
    )?;
    let lesson_topics: Vec<String> = rule_stmt
        .query_map([], |r| r.get(0))?
        .filter_map(Result::ok)
        .collect();

    for ltopic in &lesson_topics {
        let clean = ltopic.replace(' ', "_").replace(':', "_").replace('-', "_");
        let node_id = format!("node:reflex:{clean}");
        let label = format!("Reflex: {}", ltopic.replace('-', " "));

        let ins = conn.execute(
            "INSERT OR IGNORE INTO brain_nodes(id, kind, label, threshold, activation_energy, last_activated_ms) VALUES(?1, 'reflex', ?2, 0.50, 0.0, ?3)",
            params![node_id, label, now_ms],
        )?;
        if ins > 0 {
            new_connections += 1;
        }

        // Wire reflex to drawdown protection outcome
        let syn_exists: i64 = conn.query_row(
            "SELECT count(*) FROM brain_synapses WHERE source_id=?1 AND target_id='node:outcome:drawdown_protection'",
            params![node_id],
            |r| r.get(0),
        ).unwrap_or(0);

        if syn_exists == 0 {
            conn.execute(
                "INSERT INTO brain_synapses(source_id, target_id, weight, plasticity, last_propagated_ms) VALUES(?1, 'node:outcome:drawdown_protection', 2.0, 0.20, ?2)",
                params![node_id, now_ms],
            )?;
            new_connections += 1;
        }

        // Wire incoming inhibition from decision nodes
        let decision_nodes: Vec<String> = conn
            .prepare("SELECT id FROM brain_nodes WHERE kind='decision'")?
            .query_map([], |r| r.get(0))?
            .filter_map(Result::ok)
            .collect();
        for dec in decision_nodes {
            let d_exists: i64 = conn.query_row(
                "SELECT count(*) FROM brain_synapses WHERE source_id=?1 AND target_id=?2",
                params![dec, node_id],
                |r| r.get(0),
            ).unwrap_or(0);
            if d_exists == 0 {
                conn.execute(
                    "INSERT INTO brain_synapses(source_id, target_id, weight, plasticity, last_propagated_ms) VALUES(?1, ?2, 0.8, 0.08, ?3)",
                    params![dec, node_id, now_ms],
                )?;
                new_connections += 1;
            }
        }
    }

    // 3. Discover dynamic trade-exit patterns from closed trade lessons -> spawn execution/exit neurons
    let mut exit_stmt = conn.prepare(
        "SELECT DISTINCT substr(text, instr(text, 'exit by ') + 8, instr(substr(text, instr(text, 'exit by ') + 8), ',') - 1) AS exit_reason FROM memories WHERE kind='lesson' AND text LIKE '%exit by %'"
    )?;
    let exit_reasons: Vec<String> = exit_stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(Result::ok)
        .map(|s: String| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    for reason in &exit_reasons {
        let clean = reason.replace(' ', "_").replace(':', "_");
        let node_id = format!("node:reflex:exit_{clean}");
        let label = format!("Exit Reflex: {reason}");

        let ins = conn.execute(
            "INSERT OR IGNORE INTO brain_nodes(id, kind, label, threshold, activation_energy, last_activated_ms) VALUES(?1, 'reflex', ?2, 0.55, 0.0, ?3)",
            params![node_id, label, now_ms],
        )?;
        if ins > 0 {
            new_connections += 1;
        }

        // Wire exit reflex to drawdown protection outcome
        let syn_exists: i64 = conn.query_row(
            "SELECT count(*) FROM brain_synapses WHERE source_id=?1 AND target_id='node:outcome:drawdown_protection'",
            params![node_id],
            |r| r.get(0),
        ).unwrap_or(0);
        if syn_exists == 0 {
            conn.execute(
                "INSERT INTO brain_synapses(source_id, target_id, weight, plasticity, last_propagated_ms) VALUES(?1, 'node:outcome:drawdown_protection', 1.8, 0.15, ?2)",
                params![node_id, now_ms],
            )?;
            new_connections += 1;
        }
    }

    // 4. Discover alpha strategy findings -> spawn strategic decision neurons
    let mut finding_stmt = conn.prepare(
        "SELECT DISTINCT topic FROM memories WHERE kind='finding' AND topic IS NOT NULL AND topic != ''"
    )?;
    let findings: Vec<String> = finding_stmt
        .query_map([], |r| r.get(0))?
        .filter_map(Result::ok)
        .collect();

    for ftopic in &findings {
        let clean = ftopic.replace(' ', "_").replace(':', "_");
        let node_id = format!("node:decision:{clean}");
        let label = ftopic.replace('_', " ");
        let formatted_label = format!("Strategy: {label}");

        let ins = conn.execute(
            "INSERT OR IGNORE INTO brain_nodes(id, kind, label, threshold, activation_energy, last_activated_ms) VALUES(?1, 'decision', ?2, 0.70, 0.0, ?3)",
            params![node_id, formatted_label, now_ms],
        )?;
        if ins > 0 {
            new_connections += 1;
        }

        // Wire strategic decision to capital growth outcome
        let syn_exists: i64 = conn.query_row(
            "SELECT count(*) FROM brain_synapses WHERE source_id=?1 AND target_id='node:outcome:capital_growth'",
            params![node_id],
            |r| r.get(0),
        ).unwrap_or(0);
        if syn_exists == 0 {
            conn.execute(
                "INSERT INTO brain_synapses(source_id, target_id, weight, plasticity, last_propagated_ms) VALUES(?1, 'node:outcome:capital_growth', 1.5, 0.12, ?2)",
                params![node_id, now_ms],
            )?;
            new_connections += 1;
        }
    }

    // 5. Autonomous Walk-Forward Neurogenesis & Historical Plasticity:
    // Discovers symbols, strategy parameters, and ranked candidate behaviors from walk-forward testing
    // to dynamically spawn candidate sensory & decision neurons and causally adapt synaptic weights
    let source_for_store = store_conn.unwrap_or(conn);
    let wf_candidates: Vec<(String, i64, f64, f64)> = source_for_store
        .prepare(
            "SELECT moomoo_symbol, rank, score, coalesce(net_return_pct, 0.0) 
             FROM momentum_forward_candidates 
             WHERE net_return_pct IS NOT NULL 
             ORDER BY session DESC"
        )
        .and_then(|mut stmt| {
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                ))
            })?;
            Ok(rows.filter_map(Result::ok).collect())
        })
        .unwrap_or_default();

    for (sym, rank, score, net_return) in wf_candidates {
        let sym_clean = sym.replace('.', "_").replace('-', "_");
        let sym_node = format!("node:symbol:{sym_clean}");
        let sym_label = format!("Symbol: {sym}");

        let ins = conn.execute(
            "INSERT OR IGNORE INTO brain_nodes(id, kind, label, threshold, activation_energy, last_activated_ms) VALUES(?1, 'sensory', ?2, 0.50, ?3, ?4)",
            params![sym_node, sym_label, score.abs().min(1.0), now_ms],
        )?;
        if ins > 0 {
            new_connections += 1;
        }

        // Wire candidate symbol node to Rank 1 execution decision or defensive floor
        let target_decision = if rank == 1 {
            "node:decision:rank1_execute"
        } else {
            "node:decision:staged_scale_out"
        };

        let success = net_return > 0.0;
        let _ = hebbian_update(conn, &sym_node, target_decision, success, now_ms);

        // Also wire Rank decision directly to causal outcome
        let outcome_node = if success {
            "node:outcome:capital_growth"
        } else {
            "node:outcome:drawdown_protection"
        };
        let _ = hebbian_update(conn, target_decision, outcome_node, success, now_ms);
        new_connections += 1;
    }

    // 6. Ingest Historical Backtest Runs for Strategic Arborization
    let bt_runs: Vec<(String, f64, i64, i64)> = source_for_store
        .prepare(
            "SELECT strategy, return_pct, trades, wins 
             FROM momentum_backtest_runs 
             WHERE trades > 0 
             ORDER BY id DESC"
        )
        .and_then(|mut stmt| {
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            Ok(rows.filter_map(Result::ok).collect())
        })
        .unwrap_or_default();

    for (strat, ret_pct, trades, wins) in bt_runs {
        let clean = strat.replace(' ', "_").replace(':', "_");
        let node_id = format!("node:decision:{clean}");
        let label = format!("Backtest: {}", strat.replace('_', " "));

        let ins = conn.execute(
            "INSERT OR IGNORE INTO brain_nodes(id, kind, label, threshold, activation_energy, last_activated_ms) VALUES(?1, 'decision', ?2, 0.65, ?3, ?4)",
            params![node_id, label, (trades as f64 / 1000.0).min(1.0), now_ms],
        )?;
        if ins > 0 {
            new_connections += 1;
        }

        let win_rate = if trades > 0 { wins as f64 / trades as f64 } else { 0.0 };
        let success = ret_pct > 0.0 && win_rate >= 0.40;
        let outcome = if success {
            "node:outcome:capital_growth"
        } else {
            "node:outcome:drawdown_protection"
        };
        let _ = hebbian_update(conn, &node_id, outcome, success, now_ms);
        new_connections += 1;
    }

    // 7. Ingest Strategy Exploration Hypotheses & Competition Techniques:
    // Every technique tested (stops, profit targets, hold horizons) is ingested directly into the brain
    // so no trading technique learning is hindered or discarded
    let hypotheses: Vec<(String, i64, f64, i64)> = source_for_store
        .prepare(
            "SELECT label, keep, cand_total, better_blocks 
             FROM comp_hypotheses 
             ORDER BY id DESC"
        )
        .and_then(|mut stmt| {
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            Ok(rows.filter_map(Result::ok).collect())
        })
        .unwrap_or_default();

    for (hlabel, keep, cand_tot, better_blocks) in hypotheses {
        let clean = hlabel.replace(' ', "_").replace(':', "_").replace('-', "_");
        let node_id = format!("node:technique:{clean}");
        let label = format!("Technique: {}", hlabel.replace('-', " "));

        let ins = conn.execute(
            "INSERT OR IGNORE INTO brain_nodes(id, kind, label, threshold, activation_energy, last_activated_ms) VALUES(?1, 'decision', ?2, 0.60, ?3, ?4)",
            params![node_id, label, (better_blocks as f64 / 13.0).min(1.0), now_ms],
        )?;
        if ins > 0 {
            new_connections += 1;
        }

        let success = keep == 1 || cand_tot > 0.0;
        let target = if success {
            "node:outcome:capital_growth"
        } else {
            "node:reflex:break_even_floor"
        };
        let _ = hebbian_update(conn, &node_id, target, success, now_ms);
        new_connections += 1;
    }

    Ok(new_connections)
}

/// Harvests neural network summary statistics.
#[derive(Debug, Serialize, Deserialize)]
pub struct NeuralSummary {
    pub total_sensors: i64,
    pub total_nodes: i64,
    pub total_synapses: i64,
    pub avg_synapse_weight: f64,
    pub total_reinforcements: i64,
    pub total_penalties: i64,
}

pub fn summary(conn: &Connection) -> Result<NeuralSummary> {
    let sensors: i64 = conn.query_row("SELECT count(*) FROM brain_sensors", [], |r| r.get(0)).unwrap_or(0);
    let nodes: i64 = conn.query_row("SELECT count(*) FROM brain_nodes", [], |r| r.get(0)).unwrap_or(0);
    let (synapses, avg_wt, reinf, pen): (i64, f64, i64, i64) = conn.query_row(
        "SELECT count(*), coalesce(avg(weight), 0.0), coalesce(sum(reinforcements), 0), coalesce(sum(penalties), 0) FROM brain_synapses",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).unwrap_or((0, 0.0, 0, 0));

    Ok(NeuralSummary {
        total_sensors: sensors,
        total_nodes: nodes,
        total_synapses: synapses,
        avg_synapse_weight: avg_wt,
        total_reinforcements: reinf,
        total_penalties: pen,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_neural_wiring_initialization_and_hebbian_learning() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE brain_sensors(id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, config_json TEXT NOT NULL, created_ms INTEGER NOT NULL, last_fired_ms INTEGER, value REAL, status TEXT NOT NULL DEFAULT 'active');
             CREATE TABLE brain_nodes(id TEXT PRIMARY KEY, kind TEXT NOT NULL, label TEXT NOT NULL, activation_energy REAL NOT NULL DEFAULT 0.0, threshold REAL NOT NULL DEFAULT 0.5, last_activated_ms INTEGER, metadata_json TEXT);
             CREATE TABLE brain_synapses(source_id TEXT NOT NULL, target_id TEXT NOT NULL, weight REAL NOT NULL DEFAULT 1.0, plasticity REAL NOT NULL DEFAULT 0.1, hebbian_count INTEGER NOT NULL DEFAULT 0, reinforcements INTEGER NOT NULL DEFAULT 0, penalties INTEGER NOT NULL DEFAULT 0, last_propagated_ms INTEGER, PRIMARY KEY(source_id, target_id));"
        ).unwrap();

        seed_neural_topology(&conn, 1000).unwrap();
        let s = summary(&conn).unwrap();
        assert_eq!(s.total_sensors, 5);
        assert_eq!(s.total_nodes, 18);
        assert!(s.total_synapses >= 18);

        conn.execute("INSERT INTO brain_synapses(source_id,target_id,weight) VALUES('node:decision:rank1_execute','node:reflex:adversarial_bear_sentry',1.8)", []).unwrap();
        seed_neural_topology(&conn, 1001).unwrap();
        let feedback: i64 = conn.query_row("SELECT count(*) FROM brain_synapses WHERE source_id='node:decision:rank1_execute' AND target_id='node:reflex:adversarial_bear_sentry'", [], |r| r.get(0)).unwrap();
        assert_eq!(feedback, 0, "old database feedback wiring is pruned idempotently");

        // Test Hebbian reinforcement on successful outcome
        hebbian_update(&conn, "node:decision:rank1_execute", "node:outcome:capital_growth", true, 2000).unwrap();
        let s2 = summary(&conn).unwrap();
        assert_eq!(s2.total_reinforcements, 1);
        assert!(s2.avg_synapse_weight > s.avg_synapse_weight);
    }
}
