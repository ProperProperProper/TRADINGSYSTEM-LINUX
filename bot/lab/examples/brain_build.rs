//! Replays the lab's real history in time order: would "build on what already passed" have picked holdout passers better than chance?
//! At each tested variant only EARLIER variants' outcomes are used. `cargo run --release --example brain_build -- <copy of memory.sqlite3>`.
use std::collections::BTreeMap;
fn main() -> anyhow::Result<()> {
    let mem = rusqlite::Connection::open_with_flags(std::env::args().nth(1).expect("memory db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let lb = qwen_lab::strategy::leaderboard(&mem)?;
    let regime = qwen_lab::strategy::current_regime(&mem);
    let mut ts: BTreeMap<String, i64> = BTreeMap::new();
    { let mut st = mem.prepare("SELECT tokens, min(ts) FROM strategy_runs WHERE interval = '5h' AND regime = ?1 GROUP BY tokens")?; for r in st.query_map([&regime], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.flatten() { ts.insert(r.0, r.1); } }
    let mut v: Vec<(i64, Vec<String>, bool)> = lb.iter().filter(|r| r["interval"] == "5h").filter_map(|r| {
        let t = r["tokens"].as_str()?; let t0 = *ts.get(t)?;
        Some((t0, t.split('+').map(String::from).collect(), r["status"].as_str()? != "failed-holdout"))
    }).collect();
    v.sort_by_key(|x| x.0);
    println!("{} variants in time order, {} passed the holdout ({:.1}%)", v.len(), v.iter().filter(|x| x.2).count(), 100.0 * v.iter().filter(|x| x.2).count() as f64 / v.len() as f64);
    // earlier outcome of each option: (appearances, passes)
    let mut seen: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    let (mut sel, mut rest) = ((0u32, 0u32), (0u32, 0u32));
    let (mut sel_late, mut rest_late) = ((0u32, 0u32), (0u32, 0u32));
    for (i, (_, opts, pass)) in v.iter().enumerate() {
        // rule: some option has passed in an earlier variant with a rate of at least 40% over at least 3 appearances
        let build = opts.iter().any(|o| seen.get(o).is_some_and(|&(n, p)| n >= 3 && p as f64 / n as f64 >= 0.4));
        let (a, b) = if build { (&mut sel, &mut sel_late) } else { (&mut rest, &mut rest_late) };
        a.0 += 1; a.1 += *pass as u32;
        if i >= v.len() / 2 { b.0 += 1; b.1 += *pass as u32; }
        for o in opts { let e = seen.entry(o.clone()).or_default(); e.0 += 1; e.1 += *pass as u32; }
    }
    let pc = |x: (u32, u32)| if x.0 == 0 { "-".into() } else { format!("{}/{} = {:.0}%", x.1, x.0, 100.0 * x.1 as f64 / x.0 as f64) };
    println!("rule 'builds on an option that passed >=40% of >=3 earlier appearances':\n  all history : picked {} | others {}\n  later half  : picked {} | others {}", pc(sel), pc(rest), pc(sel_late), pc(rest_late));
    // the legal signal: validated or candidate variants only (what an agent is told), in time order
    let vc: Vec<(i64, Vec<String>)> = lb.iter().filter(|r| r["interval"] == "5h" && matches!(r["status"].as_str(), Some("validated") | Some("candidate"))).filter_map(|r| { let t = r["tokens"].as_str()?; Some((*ts.get(t)?, t.split('+').map(String::from).collect())) }).collect();
    for (t0, o) in &vc { println!("validated/candidate at {t0}: {}", o.join("+")); }
    let (mut s2, mut r2) = ((0u32, 0u32), (0u32, 0u32));
    for (t0, opts, pass) in &v { let build = vc.iter().any(|(t1, vo)| t1 < t0 && opts.iter().any(|o| vo.contains(o))); let a = if build { &mut s2 } else { &mut r2 }; a.0 += 1; a.1 += *pass as u32; }
    println!("rule 'contains an option of an EARLIER validated/candidate variant': picked {} | others {}", pc(s2), pc(r2));
    println!("final option pass rates (appearances >= 5):");
    let mut f: Vec<_> = seen.iter().filter(|(_, v)| v.0 >= 5).collect(); f.sort_by(|a, b| (b.1 .1 as f64 / b.1 .0 as f64).total_cmp(&(a.1 .1 as f64 / a.1 .0 as f64)));
    for (o, v) in f.iter().take(10) { println!("  {o:<22} {}/{}", v.1, v.0); }
    Ok(())
}
