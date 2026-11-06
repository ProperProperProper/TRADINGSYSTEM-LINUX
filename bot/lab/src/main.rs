use anyhow::{bail, Context, Result};
use qwen_lab::config::Config;
use qwen_lab::context::{self, Ctx};
use qwen_lab::{btarchive, cull, dash, db, ingest, report, schedule, tick};
use std::path::PathBuf;

const HELP: &str = "qwen-lab: the SPY bot's research lab (the lab's own models propose, deterministic code judges)

usage: qwen-lab [--config lab.toml] <command> [flags]

  init                                create folders, lab.toml (if missing) and both databases
  status                              sizes, counts, hash-chain check
  ingest                              validate and load the exports folder
  import-paper                        read-only import of the bot's paper archive (forward record)
  paper                               import, then show the PAPER RECORD block and the search-decay check
  mcp                                 MCP server (stdio) for Claude Code: the lab's tools
  highlights                          find, explain and remember runs >= 15% over 14 sessions (HIGHLIGHTS.md)
  winrate                             why runs win (>=60% of trades profitable) or lose, learned early, judged on later blocks
  improve [--rounds N | --forever | install | uninstall]    the model learns from backtest results and proposes strategy variants; code scores them (holdout)
  import-bt                           read-only import of the bot's backtest archive (the main real input)
  tick [--full|--auto]      one scheduled learning pass (light or full)
  schedule [install|uninstall|status] hourly launchd job that runs `tick --auto`
  dash [serve|install|uninstall|status|url]  read-only monitoring page on 127.0.0.1 (separate from the bot)
  report                              write the report to reports/
  cull [--apply] [--force]            enforce the data size limit (dry run unless --apply)
  quant [--training]                  the quant review of the strategy tests (paired, multiple testing, risk, decay)
  rule-study [--run]                  test the block-loss rule (absolute -8% vs relative to the current strategy) on all 5 cuts
  backup                              copy memory.sqlite3 (keeps 14)
";

/// Value following the command-line flag `name`.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// Whether the command line contains `name`.
fn has(args: &[String], name: &str) -> bool { args.iter().any(|a| a == name) }

/// Numeric value of flag `name`, or `default` when absent.
fn num<T: std::str::FromStr>(args: &[String], name: &str, default: T) -> Result<T> {
    match flag(args, name) { Some(v) => v.parse().map_err(|_| anyhow::anyhow!("bad value for {name}")), None => Ok(default) }
}

/// Path of lab.toml: `--config <path>` or the default root's lab.toml.
fn config_path(args: &[String]) -> PathBuf {
    flag(args, "--config").map(PathBuf::from).unwrap_or_else(|| Config::default().root.join("lab.toml"))
}

/// Opens and migrates both databases; refuses to mix real and synthetic data in one lab.
fn open_all(cfg: &Config) -> Result<(rusqlite::Connection, rusqlite::Connection)> {
    let mem = db::open(&cfg.memory_db(), true)?;
    let data = db::open(&cfg.data_db(), false)?;
    db::migrate_memory(&mem)?;
    if db::migrate_data(&data)? {
        // the paper copy was rebuilt: forget its import watermarks so the next import repopulates it from the archive
        mem.execute("DELETE FROM counters WHERE name LIKE 'paper_%'", [])?;
    }
    let stored: Option<String> = mem.query_row("SELECT version FROM schema_versions WHERE component = 'mode'", [], |r| r.get(0)).ok();
    match stored {
        Some(m) if m != cfg.mode => bail!("this lab was created in {m} mode but lab.toml says {}; the two never mix", cfg.mode),
        Some(_) => {}
        None => { mem.execute("INSERT INTO schema_versions(component, version, applied) VALUES ('mode', ?1, strftime('%s','now'))", [&cfg.mode])?; }
    }
    Ok((mem, data))
}

/// Command-line entry point: dispatches init, import, explore, confirm, tick, improve, dashboard, MCP and the other commands.
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd_idx = args.iter().position(|a| !a.starts_with("--") && (args.iter().position(|b| b == "--config").is_none_or(|i| args.iter().position(|x| x == a) != Some(i + 1))));
    let Some(ci) = cmd_idx else { print!("{HELP}"); return Ok(()) };
    let cmd = args[ci].as_str();
    let rest = &args[ci + 1..];
    if cmd == "help" { print!("{HELP}"); return Ok(()); }
    let cfg_path = config_path(&args);

    if cmd == "init" {
        let mode = "real".to_string(); // real data only: there is no other mode
        let root = cfg_path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
        let cfg = if cfg_path.exists() { Config::load(&cfg_path)? } else { Config::for_root(&root, &mode) };
        if !cfg_path.exists() { std::fs::write(&cfg_path, toml::to_string_pretty(&cfg)?)?; }
        for d in [cfg.exports_dir(), cfg.quarantine_dir(), cfg.requests_dir(), cfg.reports_dir(), cfg.logs_dir(), cfg.backup_dir()] { std::fs::create_dir_all(d)?; }
        open_all(&cfg)?;
        println!("initialised {} in {} mode (config {})", cfg.root.display(), cfg.mode, cfg_path.display());
        return Ok(());
    }

    let cfg = Config::load(&cfg_path).with_context(|| "run `qwen-lab init` first")?;
    let (mem, mut data) = open_all(&cfg)?;
    match cmd {
        "status" => {
            println!("mode {} | data {:.1} MB | memory {:.2} MB", cfg.mode, db::size_bytes(&cfg.data_db()) as f64 / 1e6, db::size_bytes(&cfg.memory_db()) as f64 / 1e6);
            for (label, sql) in [("runs", "SELECT count(*) FROM runs"), ("trades", "SELECT count(*) FROM trades")] {
                println!("{label}: {}", data.query_row::<i64, _, _>(sql, [], |r| r.get(0))?);
            }
            println!("windows: {}", context::windows_info(&data)?.iter().map(|(w, c)| if *c { w.clone() } else { format!("{w} (provisional)") }).collect::<Vec<_>>().join(", "));
            let p = qwen_lab::paper::import_best_effort(&cfg, &mem, &data);
            let b = btarchive::import_best_effort(&cfg, &mem, &data);
            println!("backtest archive: source {} | {} runs ({} with trades, {} out-of-sample), {} trades{}", b.source, b.runs_total, b.runs_with_trades, b.oos_runs, b.trades_total, if b.note.is_empty() { String::new() } else { format!(" | {}", b.note) });
            println!("paper record: source {} | state {} | {} trades, {} days{}", p.source, p.state, p.trades_total, p.days, if p.stale { " | STALE" } else { "" });
            println!("kill switch: {}", if cfg.stop_file().exists() { "ENGAGED" } else { "off" });
        }
        "ingest" => println!("{:?}", ingest::ingest(&cfg, &mem, &mut data)?),
        "import-paper" | "paper" => {
            let rep = qwen_lab::paper::import(&cfg, &mem, &data, qwen_lab::util::now_ts() * 1000)?;
            println!("{rep:#?}");
            if cmd == "paper" {
                println!("\n{}", qwen_lab::paper::prompt_block(&cfg, &data, &rep)?);
                println!("{:?}", qwen_lab::paper::search_decay(&data, 8, 500)?);
            }
        }
        "study-import" => {
            // `study-import [--cut B] <variant_study output>...` (default cut A).
            let cut = rest.iter().position(|a| a == "--cut").and_then(|i| rest.get(i + 1)).map_or("A", |s| s.as_str());
            for f in rest.iter().filter(|a| !a.starts_with("--") && a.as_str() != cut) {
                println!("{f}: {} rows (cut {cut})", qwen_lab::strategy::import_output_cut(&mem, &std::fs::read_to_string(f)?, cut)?);
            }
        }
        "mcp" => {
            // MCP server (stdio): the lab's tools and its local agents for Claude Code.
            let ctx = Ctx { cfg: &cfg, mem: &mem, data: &data };
            return qwen_lab::agents::serve_mcp_view(&ctx, true);
        }
        "origins-backfill" => {
            // who proposed each tested variant, recovered from the strategy loop's logs (the loop also does this once by itself)
            let ctx = Ctx { cfg: &cfg, mem: &mem, data: &data };
            qwen_lab::strategy::init(&mem)?;
            println!("origins recovered: {}", qwen_lab::strategy::origins_from_logs(&ctx)?);
        }
        "quant" => {
            // the quant review (src/quant.rs): --training = the agents' view; default = the unseen half (human view), with decay
            let h = if rest.iter().any(|a| a == "--training") { qwen_lab::quant::Half::Training } else { qwen_lab::quant::Half::Unseen };
            println!("{}", qwen_lab::quant::review(&mem, h)?.0);
            if h == qwen_lab::quant::Half::Unseen {
                println!("  Overfitting decay per option (mean paired gain: training half -> unseen half):");
                for (o, n, a, b) in qwen_lab::quant::decay(&mem)? { println!("    {o} ({n} variants): {a:+.2} -> {b:+.2}"); }
            }
        }
        "rule-study" => {
            // test the block-loss rule before any change: both rules on every cut; --run backtests the missing cuts first
            let ctx = Ctx { cfg: &cfg, mem: &mem, data: &data };
            qwen_lab::strategy::init(&mem)?;
            println!("{}", qwen_lab::strategy::rule_study(&ctx, rest.iter().any(|a| a == "--run"))?);
        }
        "promote" => {
            // promotion requests for variants that pass the full validation (also hourly in the tick)
            for n in qwen_lab::promote::run(&mem, &cfg.root)? { println!("{n}"); }
        }
        "promotion-status" => {
            // `promotion-status <interval|options> <live|failed|reverted> <detail>`: reported by the bot's promotion job
            let v = rest.first().context("promotion-status <variant> <status> <detail>")?;
            let st = rest.get(1).context("status")?;
            qwen_lab::promote::set_status(&mem, &cfg.root, v, st, &rest.iter().skip(2).cloned().collect::<Vec<_>>().join(" "))?;
            println!("{v}: {st}");
        }
        "note" => {
            // `note --to <agent> <text>`: a note from Claude Code (acting for the owner) to an agent; shown to it as a trusted instruction
            let to = rest.iter().position(|a| a == "--to").and_then(|i| rest.get(i + 1)).context("--to <agent>")?.clone();
            let text: String = rest.iter().enumerate().filter(|(i, a)| a.as_str() != "--to" && rest.get(i.wrapping_sub(1)).is_none_or(|p| p != "--to")).map(|(_, a)| a.as_str()).collect::<Vec<_>>().join(" ");
            anyhow::ensure!(text.trim().len() >= 5, "note text missing");
            qwen_lab::agents::init_messages(&mem)?;
            mem.execute("INSERT INTO agent_messages(ts, sender, recipient, text, read) VALUES (?1, 'claude-code', ?2, ?3, 0)", rusqlite::params![qwen_lab::util::now_ts(), to, text.chars().take(1000).collect::<String>()])?;
            println!("note from claude-code to {to} stored");
        }
        "guard-scan" => {
            // `guard-scan <file or dir>...`: the security guard over files (Hermes skills after every run); prints FLAGGED lines
            let paths: Vec<std::path::PathBuf> = rest.iter().map(std::path::PathBuf::from).collect();
            let flagged = qwen_lab::guard::scan_paths(&mem, &paths);
            for (f, why) in &flagged { println!("FLAGGED {}\t{why}", f.display()); }
            println!("scanned {} path(s): {} flagged", paths.len(), flagged.len());
        }
        "winrate" => {
            // Why runs win (>=60% of trades profitable) or lose: learned on early blocks, judged on later ones.
            let cap = cfg.strategy.equities.first().copied().unwrap_or(5000.0);
            let (summary, found) = qwen_lab::winrate::analyse(&mem, &data, cap)?;
            println!("{summary}");
            for f in found.iter().take(30) {
                println!("[{}] {} ({})", if f.confirmed { "CONFIRMED" } else { "not confirmed" }, qwen_lab::winrate::sentence(f), f.kind);
            }
        }
        "highlights" => {
            // Find, explain and remember every run at or above 15% over 14 sessions; writes HIGHLIGHTS.md.
            let ctx = Ctx { cfg: &cfg, mem: &mem, data: &data };
            for l in qwen_lab::highlights::scan(&ctx)? { println!("{l}"); }
            println!("report: {}", cfg.root.join("HIGHLIGHTS.md").display());
        }
        "improve" => {
            // The model learns from the backtest results and proposes strategy variants; code scores and judges them.
            match rest.first().map(String::as_str) {
                Some("install") => { println!("{}", schedule::install_improve(&cfg, &cfg_path)?); return Ok(()); }
                Some("uninstall") => { println!("{}", qwen_lab::service::uninstall(qwen_lab::jobs::IMPROVE)?); return Ok(()); }
                _ => {}
            }
            let ctx = Ctx { cfg: &cfg, mem: &mem, data: &data };
            if has(rest, "--forever") {
                // 24/7 on the existing data: round after round, resumable, with periodic backups of the memory database.
                return qwen_lab::strategy::improve_forever(&ctx);
            }
            for r in 0..num(rest, "--rounds", 1usize)? {
                let attempt = qwen_lab::strategy::next_proposal_attempt(&mem)? as u64;
                match qwen_lab::strategy::round(&ctx, attempt) {
                    Ok(lines) => { println!("round {}:", r + 1); for l in lines { println!("  {l}"); } }
                    Err(e) => println!("round {} failed: {e:#}", r + 1),
                }
            }
            println!("winners so far:");
            for w in qwen_lab::strategy::winners(&mem)? { println!("  {w}"); }
        }
        "trainer" => {
            // the 24x7 deep-learning trainer: `trainer` (forever), `trainer once`, `trainer status`, `trainer install|uninstall`
            use qwen_lab::automl;
            match rest.first().map(String::as_str) {
                Some("install") => println!("{}", qwen_lab::schedule::install_trainer(&cfg, &cfg_path)?),
                Some("uninstall") => println!("{}", qwen_lab::schedule::uninstall_trainer()?),
                Some("status") => { let db = rusqlite::Connection::open(cfg.candles_db())?; println!("{}", automl::status(&db)?); }
                Some("once") => { let mut w = None; println!("{}", automl::step(&cfg.candles_db(), &cfg.root, &mut w, 3)?); }
                _ => automl::run_forever(&cfg.candles_db(), &cfg.root, &cfg.logs_dir(), 3),
            }
        }
        "model" => {
            // the packaged, versioned model: train / versions / predict / explain / card / maintain
            use qwen_lab::{aimodel, candles, mlscores::Scheme};
            let db = rusqlite::Connection::open(cfg.candles_db())?;
            let series = candles::load_prepared(&db)?;
            let root = cfg.root.clone();
            match rest.first().map(String::as_str) {
                Some("train") => {
                    let v = aimodel::versions(&root).last().map_or(1, |v| v + 1);
                    let a = aimodel::train(&series, Scheme::from_ledger(&db), v)?;
                    aimodel::save(&root, &a)?; aimodel::set_current(&root, v)?;
                    println!("trained version {v}: {} samples through {}, held-out IC {:+.4}, scheme {}", a.samples, qwen_lab::util::utc_minute(a.trained_through_ms / 1000), a.heldout_ic, a.scheme);
                }
                Some("maintain") => for l in aimodel::maintain(&root, &series, Scheme::from_ledger(&db))? { println!("{l}"); },
                Some("versions") => {
                    let cur = aimodel::current(&root);
                    let monitor = std::fs::read_to_string(aimodel::dir(&root).join("monitor.json")).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
                    println!("version  created           trained through    samples  held-out  live out-of-sample IC (mean, se, sessions)");
                    for v in aimodel::versions(&root) {
                        let a = aimodel::load(&root, v)?;
                        let live = monitor.as_ref().and_then(|m| m[format!("v{v}")]["summary"].as_array().cloned()).map(|s| format!("{:+.4} ({:.4}) over {}", s[0].as_f64().unwrap_or(0.0), s[1].as_f64().unwrap_or(0.0), monitor.as_ref().map_or(0, |m| m[format!("v{v}")]["sessions"].as_u64().unwrap_or(0)))).unwrap_or_else(|| "too few sessions yet".into());
                        println!("{:<8} {:<17} {:<17} {:>8} {:>+9.4}  {}", format!("v{v}{}", if cur == Some(v) { " *" } else { "" }), qwen_lab::util::utc_minute(a.created), qwen_lab::util::utc_minute(a.trained_through_ms / 1000), a.samples, a.heldout_ic, live);
                    }
                }
                Some("predict") | Some("explain") => {
                    let v = aimodel::current(&root).ok_or_else(|| anyhow::anyhow!("no model yet: qwen-lab model train"))?;
                    let a = aimodel::load(&root, v)?;
                    let Some((ts, picks)) = aimodel::predict_live(&series, &a, &qwen_lab::automl::load_champions(&root)) else { println!("not enough stocks with features at the newest bar"); return Ok(()) };
                    println!("version {v}, ranking of the bot's top-20 liquid stocks for the bar opening {} (expected excess return over the next session, calibrated out of sample)", qwen_lab::util::utc_minute(ts / 1000));
                    let want = rest.get(1).map(|s| s.to_uppercase());
                    for (i, p) in picks.iter().enumerate().filter(|(_, p)| want.as_ref().is_none_or(|w| &p.symbol == w || rest[0] == "predict")) {
                        println!("{:>2}. {:<6} score {:+.5} | percentile {:>3.0} | expected {:+5.1} bps, hit {:.0}% | because {}", i + 1, p.symbol, p.score, p.pct, p.expected_bps, p.hit * 100.0,
                            p.why.iter().map(|w| format!("{} {:+.5}", w.0, w.1)).collect::<Vec<_>>().join("; "));
                    }
                }
                Some("scoreboard") => { aimodel::resolve_predictions(&db, &series)?; println!("{}", aimodel::scoreboard(&db, None)?.line()); }
                Some("trades") => { let data = rusqlite::Connection::open_with_flags(cfg.data_db(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?; println!("{}", qwen_lab::intel::gate_on_trades(&db, &data)?); }
                Some("decide") | Some("risk") => println!("{}", qwen_lab::intel::briefing(&root, &db, &series)?),
                Some("card") => println!("{}", aimodel::card(&root)?),
                _ => println!("usage: model train | maintain | versions | predict | explain <SYMBOL> | decide | trades | scoreboard | card"),
            }
        }
        "candles" => {
            // the lab's own copy of the market candles and the machine-learning test on them
            match rest.first().map(String::as_str) {
                Some("import") => {
                    let hist = qwen_lab::platform::bot_history_json();
                    let bot = qwen_lab::platform::bot_store_path();
                    let out = rusqlite::Connection::open(cfg.candles_db())?;
                    let (read, written, dropped) = qwen_lab::candles::import(Some(&hist), Some(&bot), &out)?;
                    let n_univ = qwen_lab::candles::import_universe(&qwen_lab::platform::bot_holdings_csv(), &out).unwrap_or(0);
                    println!("universe: {n_univ} SPY holdings");
                    println!("read {read} bars, store now holds {written} (dropped {dropped} invalid)");
                }
                Some("improve") => {
                    let r = qwen_lab::mlround::run(&cfg.candles_db(), &cfg.data_db())?;
                    println!("{}", r.text);
                }
                Some("scores") => {
                    // the model's score for every bar, in the file the bot's own backtester reads (`variant_study 5h sig:Ml`)
                    let out = rest.get(1).cloned().unwrap_or_else(|| qwen_lab::platform::bot_scores_path().to_string_lossy().into_owned());
                    let db = rusqlite::Connection::open(cfg.candles_db())?;
                    let (syms, n) = qwen_lab::mlscores::write(&qwen_lab::candles::load_prepared(&db)?, std::path::Path::new(&out), qwen_lab::mlscores::Scheme::from_ledger(&db))?;
                    println!("wrote {n} scores for {syms} symbols to {out}");
                }
                Some("ml") => {
                    let db = rusqlite::Connection::open(cfg.candles_db())?;
                    println!("{}", { let s = qwen_lab::candles::load_prepared(&db)?; qwen_lab::mlcandle::report(&s, &qwen_lab::candles::load_costs(&db)).text });
                }
                _ => println!("usage: candles import | candles ml | candles improve | candles scores [out.json]"),
            }
        }
        "brain" => {
            // the lab's own small ML model: `brain eval` = walk-forward test on the real history; `brain propose [N]` = best untested combinations
            let obs = qwen_lab::brain::load(&mem)?;
            match rest.first().map(String::as_str) {
                Some("eval") => println!("{}", qwen_lab::brain::report_eval(&obs)),
                Some("propose") => {
                    let n: usize = rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(6);
                    let tested: std::collections::BTreeSet<String> = obs.iter().map(|o| qwen_lab::strategy::key(&o.options.iter().cloned().collect::<Vec<_>>())).collect();
                    for (c, mu, sd) in qwen_lab::brain::propose(&obs, &tested, n, 1.0) { println!("{:<40} predicted {mu:+.2} +/- {sd:.2}", qwen_lab::strategy::key(&c.iter().cloned().collect::<Vec<_>>())); }
                }
                Some("cycle") => {
                    // the strategist's job: queue untested variants through the validated queue (author brain-strategist)
                    let ctx = Ctx { cfg: &cfg, mem: &mem, data: &data };
                    qwen_lab::labtools::init(&mem)?;
                    for l in qwen_lab::brain::cycle(&ctx, rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(2))? { println!("{l}"); }
                }
                Some("run") | Some("all") | Some("serve") => {
                    // the team (src/brain_roles.rs): `brain run <role>` one role, `brain all` every role once, `brain serve [secs]` always on (a role per interval)
                    let ctx = Ctx { cfg: &cfg, mem: &mem, data: &data };
                    qwen_lab::labtools::init(&mem)?;
                    qwen_lab::brain_roles::init_store(&mem)?;
                    match rest[0].as_str() {
                        "run" => { let role = rest.get(1).map(String::as_str).unwrap_or(""); for l in qwen_lab::brain_roles::run_role(&ctx, role) { println!("{role}: {l}"); } }
                        "all" => for role in qwen_lab::brain_roles::ROLES { for l in qwen_lab::brain_roles::run_role(&ctx, role) { println!("{role}: {l}"); } },
                        _ => qwen_lab::brain_roles::serve(&ctx, rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(300)),
                    }
                }
                Some("install") => println!("{}", qwen_lab::schedule::install_brain(&cfg, &cfg_path)?),
                Some("uninstall") => println!("{}", qwen_lab::schedule::uninstall_brain()?),
                Some("lessons") => {
                    // search the stored lessons and skills: `brain lessons <word> [word ...]`
                    let words: Vec<String> = rest[1..].to_vec();
                    for l in qwen_lab::brain_roles::lessons(&mem, &words, 20)? { println!("{l}"); }
                }
                Some("ml") => {
                    // the pre-registered machine-learning test on the real trades (also run daily by the statistician)
                    let trades = qwen_lab::mltrade::load(&data)?;
                    match qwen_lab::mltrade::watch(&trades, 10) { Some(w) => println!("{}", w.summary()), None => println!("too few stored trades to judge") }
                }
                Some("report") => println!("{}", qwen_lab::brain::report(&mem)?),
                _ => println!("usage: brain eval | ml | lessons <words> | propose [N] | cycle [N] | run <role> | all | serve [secs] | install | uninstall | report"),
            }
        }
        "announce" => {
            // posts any new better-parameter / validated / forward-winner / training news to Discord (also runs inside `improve --forever`)
            for l in qwen_lab::announce::scan(&mem)? { println!("{l}"); }
        }
        "import-bt" => println!("{:#?}", btarchive::import(&cfg, &mem, &data)?),
        "tick" => {
            let ctx = Ctx { cfg: &cfg, mem: &mem, data: &data };
            let now = qwen_lab::util::now_ts();
            let full = has(rest, "--full") || (has(rest, "--auto") && tick::auto_full(&cfg, &mem, now, qwen_lab::sysinfo::local_hour()));
            let rep = tick::tick(&ctx, &tick::TickOptions { full, now }, &tick::Probes::live(&cfg))?;
            print!("{}", rep.text());
        }
        "dash" => match rest.first().map(|s| s.as_str()).unwrap_or("serve") {
            "install" => println!("{}", schedule::install_dash(&cfg, &cfg_path)?),
            "uninstall" => println!("{}", schedule::uninstall_dash()?),
            "status" => println!("{}", schedule::dash_status()),
            "url" => println!("http://127.0.0.1:{}/", cfg.dashboard.port),
            _ => dash::serve(cfg.clone())?,
        },
        "schedule" => match rest.first().map(|s| s.as_str()).unwrap_or("status") {
            "install" => println!("{}", schedule::install(&cfg, &cfg_path)?),
            "uninstall" => println!("{}", schedule::uninstall()?),
            _ => println!("{}", schedule::status()),
        },
        "report" => println!("{}", report::write(&cfg, &mem, &data)?.display()),
        "cull" => {
            let r = cull::cull(&cfg, &mem, &data, !has(rest, "--apply"), has(rest, "--force"))?;
            println!("{r:?}");
        }
        "backup" => {
            std::fs::create_dir_all(cfg.backup_dir())?;
            let dest = cfg.backup_dir().join(format!("memory-{}.sqlite3", qwen_lab::util::now_ts()));
            mem.execute("VACUUM INTO ?1", [dest.to_string_lossy().as_ref()])?;
            let mut files: Vec<_> = std::fs::read_dir(cfg.backup_dir())?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            files.sort();
            while files.len() > 14 { std::fs::remove_file(files.remove(0))?; }
            println!("backup written to {}", dest.display());
        }
        _ => print!("{HELP}"),
    }
    Ok(())
}
