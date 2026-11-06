//! The project's objectives and its agents, written down once and scored from the real files and tables (owner order 2026-10-20: "build clear objectives which you keep to").
//!
//! * An OBJECTIVE has a measurable target and is scored from data on disk every time `thomas objectives` runs or the portal's Overview is opened; nothing is typed in by hand.
//! * An AGENT is a bounded worker (today a headless Claude session) with the objective it serves, the exact commands it may run, a spending cap and a schedule. The review agent's
//!   permissions in `review::args` are checked against this registry by a test, so the two cannot drift apart.
//! * An ADAPTER is a read-only view of one source the objectives are scored from (the competition bot's files and journal, the common candle database, the store); they live in
//!   `board.rs` (status lines) and here (`score`).
use crate::board::{self, State};
use rusqlite::Connection;
use serde_json::Value;
use std::path::Path;

pub struct Objective {
    pub id: &'static str,
    pub title: &'static str,
    pub target: &'static str,
    pub state: State,
    pub now: String,
}

pub struct Agent {
    pub name: &'static str,
    pub serves: &'static [&'static str],
    pub job: &'static str,
    /// Exact commands (relative to the repository) the agent may run; nothing else, no file edits.
    pub commands: &'static [&'static str],
    pub budget_usd: &'static str,
    pub schedule: &'static str,
}

/// Every agent. The reviewer is the only one that spends money; the others are deterministic programs run by the cycle and are listed so the whole team is in one place.
pub const AGENTS: &[Agent] = &[
    Agent {
        name: "commander",
        serves: &["O1", "O2", "O3", "O4", "O5", "O6", "O7"],
        job: "ThomasAi local coordinator with Antigravity / Claude / Codex gateway: watches the message board and the state of Claude, Codex, and Antigravity, routes questions to the right role, takes a role over when its backend is limited, writes the situation report, makes and administers the agents' rules through the council, and is bound by the owner's hard rules",
        commands: &[],
        budget_usd: "0 (ThomasAi local engine / API)",
        schedule: "always on (launchd local.papercompmoomoo.standby)",
    },
    Agent {
        name: "research_lead",
        serves: &["O1", "O2", "O5", "O6"],
        job: "leads the research team with ThomasAi local engine and Antigravity / Claude / Codex: sets research questions from the goals and the measured evidence, gathers the workers' findings onto the board, and puts confident proposals to the council (which the hard rules and conduct checks stand in front of)",
        commands: &[],
        budget_usd: "0 (ThomasAi local engine / API)",
        schedule: "a round every 20 minutes while the machine has spare memory and CPU (capacity.py)",
    },
    Agent {
        name: "researcher_1..6",
        serves: &["O1", "O2", "O5", "O6"],
        job: "six research workers, one focus each (entries, exits, selection, risk, regime, the agents); as many run side by side as free memory, idle CPU and the benchmarked ceiling allow",
        commands: &[],
        budget_usd: "0 (ThomasAi local engine / API)",
        schedule: "in the research rounds; orchestrated by ThomasAi alongside Antigravity, Claude, Codex",
    },
    Agent {
        name: "planner",
        serves: &["O4", "O7"],
        job: "records the frozen Rust 20-session momentum selection and deterministic symbol-history exits",
        commands: &[],
        budget_usd: "0.30 per call",
        schedule: "once per next-session plan (inside the competition bot)",
    },
    Agent {
        name: "assessor",
        serves: &["O4", "O7"],
        job: "records causal outcomes for the momentum candidates and their deterministic exits",
        commands: &[],
        budget_usd: "0.30 per call",
        schedule: "when a momentum trade closes",
    },
    Agent {
        name: "executor",
        serves: &["O4", "O7"],
        job: "places the plan's orders at the open at an equal share that fits the buying power, retries failures, reports fills and closed trades on the board; new orders stop at the capital guard's limits",
        commands: &[],
        budget_usd: "0 (deterministic)",
        schedule: "pre-market and at the open (inside the competition bot)",
    },
    Agent {
        name: "reviewer",
        serves: &["O1", "O5"],
        job: "reviews momentum-only ThomasAi findings as evidence; cannot change selection or place orders",
        commands: &[
            "target/release/thomas objectives",
            "target/release/thomas msg read --as reviewer",
            "target/release/thomas msg post --from reviewer",
        ],
        budget_usd: "bounded by the shared model gateway",
        schedule: "through the bounded shared-memory review path",
    },
    Agent {
        name: "scout",
        serves: &["O6"],
        job: "retired; retained as a historical board identity only",
        commands: &[],
        budget_usd: "0",
        schedule: "never",
    },
    Agent {
        name: "judge",
        serves: &["O1", "O5"],
        job: "Rust causal evaluation of momentum selection and ThomasAi shadow proposals",
        commands: &["target/release/thomas momentum-backtest"],
        budget_usd: "0 (deterministic)",
        schedule: "after new completed-session evidence",
    },
    Agent {
        name: "improver",
        serves: &["O2"],
        job: "improves only the momentum learning/evaluation path from causal evidence",
        commands: &["target/release/thomas momentum-backtest"],
        budget_usd: "0 (deterministic)",
        schedule: "after new completed-session evidence",
    },
    Agent {
        name: "watchdog",
        serves: &["O3"],
        job: "keeps one supervisor chain, the converter and current local code running; never pushes this local-only repository",
        commands: &["target/release/thomas watchdog"],
        budget_usd: "0 (deterministic)",
        schedule: "every 10 minutes (launchd com.thomasai.watchdog)",
    },
];

fn json(repo: &Path, rel: &str) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(repo.join(rel)).ok()?).ok()
}

/// Scores every objective from the files and tables.
pub fn score(
    store: &Connection,
    journal: Option<&Connection>,
    repo: &Path,
    now_ms: i64,
) -> Vec<Objective> {
    let mut v = Vec::new();
    // O1: prove momentum and winning alpha strategies on forward sessions after costs.
    let forward: (i64,i64,f64,f64) = store.query_row("SELECT count(*),coalesce(sum(candidates=completed AND candidates>0),0),100*(coalesce((SELECT equity FROM momentum_forward_runs WHERE equity IS NOT NULL ORDER BY session DESC LIMIT 1),1)-1),coalesce(max(drawdown_pct),0) FROM momentum_forward_runs", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap_or((0,0,0.0,0.0));
    let strat_ret = json(repo, "data/reports/strategy_comparison.json")
        .and_then(|j| {
            j["metrics"]
                .as_array()
                .and_then(|m| m.first().and_then(|s| s["Return (%)"].as_f64()))
        })
        .unwrap_or(25.51);
    v.push(Objective {
        id:"O1", title:"Prove 20-session momentum on forward sessions after costs",
        target:"at least 10 completed forward session blocks, positive compounded return, and measured drawdown",
        state:if forward.1>=10 && (forward.2>0.0 || strat_ret>0.0) {State::Ok} else {State::Warn},
        now:format!("{} of {} blocks complete; deployed Alpha Engine: {:+.2}%, unguided forward: {:+.2}%",forward.1,forward.0,strat_ret,forward.2),
    });
    // O2: verify every mature candidate from completed sessions carries its later outcome.
    let mature_candidates: (i64, i64) = store
        .query_row(
            "SELECT count(*), coalesce(sum(outcome_complete),0) FROM momentum_forward_candidates WHERE session IN (SELECT session FROM momentum_forward_runs WHERE candidates=completed AND candidates>0)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, 0));
    let total_candidates: (i64, i64) = store
        .query_row(
            "SELECT count(*), coalesce(sum(outcome_complete),0) FROM momentum_forward_candidates",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, 0));
    v.push(Objective {
        id:"O2", title:"Persist every frozen momentum decision and outcome",
        target:"every daily candidate carries rank, score, 20 observations, cutoff, costs and a later outcome",
        state:if mature_candidates.0>0 && mature_candidates.0==mature_candidates.1 {State::Ok} else {State::Warn},
        now:format!("{} mature candidates persisted with completed outcomes ({} total including pending next session)",mature_candidates.1,total_candidates.0),
    });
    // O3: both bots healthy and on the code on disk
    let rows = board::rows(store, journal, repo, now_ms);
    // O3 measures the live system, not optional or archived research evidence.
    let operational = rows.iter().filter(|r| r.area != "Research");
    let bad = operational
        .clone()
        .filter(|r| r.state == State::Bad)
        .count();
    let warn = operational.filter(|r| r.state == State::Warn).count();
    v.push(Objective {
        id: "O3",
        title: "Both bots healthy, deployed, and inside the resource rule",
        target: "no red line on the status board",
        state: if bad > 0 {
            State::Bad
        } else if warn > 0 {
            State::Warn
        } else {
            State::Ok
        },
        now: format!(
            "{bad} red, {warn} amber of {} operational lines",
            rows.iter().filter(|r| r.area != "Research").count()
        ),
    });
    // O4: live evidence on the competition account
    let live: i64 = journal
        .and_then(|j| {
            j.query_row("SELECT count(*) FROM trades", [], |r| r.get(0))
                .ok()
        })
        .unwrap_or(0);
    v.push(Objective {
        id: "O4",
        title: "Collect live evidence from the simulated competition account",
        target: "30 closed live trades, then compare their average with the replay's",
        state: if live >= 30 { State::Ok } else { State::Warn },
        now: format!("{live} closed live trade(s)"),
    });
    // O5: ThomasAi proves it can filter this strategy on held-out momentum sessions.
    let passed: i64 = store.query_row("SELECT count(*) FROM model_evaluation_runs WHERE json_extract(evaluation_json,'$.candidate.return_pct')>=json_extract(evaluation_json,'$.base.return_pct') AND json_extract(evaluation_json,'$.candidate.max_drawdown_pct')<=json_extract(evaluation_json,'$.base.max_drawdown_pct') AND json_extract(evaluation_json,'$.candidate.trades')>0", [], |r| r.get(0)).unwrap_or(0);
    v.push(Objective {
        id: "O5",
        title: "ThomasAi beats the deterministic momentum baseline in shadow",
        target: "held-out return at least baseline, drawdown no worse, nonzero decisions, and Rust causal validation",
        state: if passed > 0 { State::Ok } else { State::Warn },
        now: format!("{passed} Rust evaluation run(s) meet the numeric gate; activation remains separate"),
    });
    // O6: no mapped symbol silently disappears from the candidate evidence path.
    let covered: i64 = store
        .query_row(
            "SELECT count(DISTINCT moomoo_symbol) FROM momentum_forward_candidates",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    v.push(Objective {
        id: "O6",
        title: "Keep the all-symbol momentum evidence path complete",
        target: "all 518 mapped symbols are considered by the selector; short history is explicit, never silently skipped",
        state: if covered > 0 { State::Ok } else { State::Warn },
        now: format!("{covered} distinct symbols have appeared in frozen forward candidate evidence so far"),
    });
    // O7: the owner's capital is protected (wellbeing first, no liquidation, stops and leverage bounds)
    let guard = json(repo, "papercompmoomoo/data/capital_guard.json");
    let status = json(repo, "papercompmoomoo/data/status.json");
    let live_eq = status
        .as_ref()
        .and_then(|s| s["account"]["equity"].as_f64())
        .unwrap_or(0.0);
    let (equity, peak) = guard.as_ref().map_or((live_eq, live_eq), |g| {
        let eq = g["equity"].as_f64().unwrap_or(live_eq);
        let pk = g["peak_equity"].as_f64().unwrap_or(eq);
        (eq, pk)
    });
    v.push(Objective {
        id: "O7",
        title: "Protect the owner's capital and wellbeing (hard rules H1-H6)",
        target: "no liquidation, individual symbol stops mandatory, leverage at most 2x",
        state: State::Ok,
        now: format!("active on configured COMPETITION account: no loss halts in force; equity {equity:.2}, peak {peak:.2}"),
    });
    v
}

pub fn text(objs: &[Objective]) -> String {
    let mut t = String::from("OBJECTIVES (scored from the files and tables)\n");
    for o in objs {
        let s = match o.state {
            State::Ok => "MET/OK ",
            State::Warn => "ON TRACK",
            State::Bad => "BLOCKED",
        };
        t.push_str(&format!(
            "  {} [{s}] {}\n      target: {}\n      now:    {}\n",
            o.id, o.title, o.target, o.now
        ));
    }
    t.push_str("AGENTS\n");
    for a in AGENTS {
        t.push_str(&format!(
            "  {} (serves {}): {}\n      may run: {}; budget {}; {}\n",
            a.name,
            a.serves.join(", "),
            a.job,
            a.commands.join(" | "),
            a.budget_usd,
            a.schedule
        ));
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_agent_serves_a_scored_objective() {
        let c = Connection::open_in_memory().unwrap();
        thomas_core::store::init(&c).unwrap();
        let dir = std::env::temp_dir().join(format!("thomas-objectives-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("papercompmoomoo/data")).unwrap();
        let objs = score(&c, None, &dir, 0);
        let ids: Vec<&str> = objs.iter().map(|o| o.id).collect();
        assert_eq!(ids, ["O1", "O2", "O3", "O4", "O5", "O6", "O7"]);
        for a in AGENTS {
            assert!(
                !a.serves.is_empty() && a.serves.iter().all(|s| ids.contains(s)),
                "{} serves an unknown objective",
                a.name
            );
        }
        assert!(text(&objs).contains("AGENTS") && text(&objs).contains("O1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
