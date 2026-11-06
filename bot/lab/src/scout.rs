//! Research scout (user 2026-10-09: "research on internet and try things, legit sources"). Reads recent quantitative-finance
//! papers from arXiv's public API (export.arxiv.org) and, when arXiv is busy, from OpenAlex (api.openalex.org, an open index of
//! journals, working papers and preprints incl. SSRN); these are the only hosts this module contacts. It asks the local `scout` agent to turn
//! each relevant abstract into an entry idea: existing option tokens the strategy loop can test right away, or a new mechanism
//! that needs a code change in the bot repo (listed for review, never implemented automatically). Every idea keeps its paper
//! link; nothing a paper claims is treated as a result; ideas count only after the lab's own backtests pass them.

use anyhow::{bail, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

/// arXiv's public API (first choice).
const HOST: &str = "https://export.arxiv.org/api/query";
/// OpenAlex works API (fallback when arXiv is busy).
const OPENALEX: &str = "https://api.openalex.org/works";

/// One paper from the feed.
#[derive(Debug, Clone)]
pub struct Paper {
    pub url: String,
    pub title: String,
    pub summary: String,
    pub published: String,
}

/// Creates the research-ideas table if missing.
pub fn init(mem: &Connection) -> Result<()> {
    mem.execute_batch(
        "CREATE TABLE IF NOT EXISTS research_ideas (
            url TEXT PRIMARY KEY, ts INTEGER NOT NULL, title TEXT NOT NULL, published TEXT NOT NULL, relevant INTEGER NOT NULL,
            idea TEXT NOT NULL, tokens TEXT NOT NULL, new_mechanism TEXT NOT NULL, query TEXT NOT NULL);",
    )?;
    // ideas recorded by the brain were tagged "hermes:<author>" until 2026-10-13
    mem.execute("UPDATE research_ideas SET query = 'idea:' || substr(query, 8) WHERE query LIKE 'hermes:%'", [])?;
    Ok(())
}

/// Text between the first `<tag ...>` and `</tag>` in `s`, XML entities and whitespace tidied.
fn tag(s: &str, name: &str) -> Option<String> {
    let start = s.find(&format!("<{name}"))?;
    let open_end = s[start..].find('>')? + start + 1;
    let end = s[open_end..].find(&format!("</{name}>"))? + open_end;
    let t = s[open_end..end].replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&");
    Some(t.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Papers from an Atom feed (arXiv API response).
pub fn parse_feed(xml: &str) -> Vec<Paper> {
    xml.split("<entry>").skip(1).filter_map(|e| {
        let e = e.split("</entry>").next()?;
        Some(Paper { url: tag(e, "id")?, title: tag(e, "title")?, summary: tag(e, "summary")?, published: tag(e, "published").unwrap_or_default() })
    }).collect()
}

/// The newest papers for `query` from arXiv (most recently submitted first).
pub fn fetch(query: &str, max: usize) -> Result<Vec<Paper>> {
    // arXiv asks API clients to identify themselves and to wait between requests: one request, a polite pause and one retry on 429/503.
    let mut last = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(15 * attempt as u64));
        }
        let r = ureq::get(HOST)
            .set("User-Agent", "qwen-lab-research-scout/1.0 (private research tool; one request per pass)")
            .query("search_query", query)
            .query("sortBy", "submittedDate")
            .query("sortOrder", "descending")
            .query("max_results", &max.to_string())
            .timeout(std::time::Duration::from_secs(30))
            .call();
        match r {
            Ok(resp) => {
                let body = resp.into_string()?;
                if !body.contains("<feed") { bail!("arXiv did not return a feed"); }
                return Ok(parse_feed(&body));
            }
            Err(ureq::Error::Status(code, _)) if code == 429 || code == 503 => last = format!("arXiv answered {code} (busy)"),
            Err(e) => return Err(e.into()),
        }
    }
    bail!("{last}; trying again next pass")
}

/// The newest papers with abstracts for a plain-text search from OpenAlex (abstract rebuilt from its inverted index).
pub fn fetch_openalex(search: &str, max: usize) -> Result<Vec<Paper>> {
    let resp = ureq::get(OPENALEX)
        .set("User-Agent", "qwen-lab-research-scout/1.0 (private research tool; one request per pass)")
        .query("search", search)
        .query("filter", "has_abstract:true,from_publication_date:2018-01-01")
        .query("sort", "publication_date:desc")
        .query("per-page", &max.to_string())
        .query("select", "id,doi,title,publication_date,abstract_inverted_index")
        .timeout(std::time::Duration::from_secs(30))
        .call()?;
    let v: Value = resp.into_json()?;
    Ok(v["results"].as_array().cloned().unwrap_or_default().iter().filter_map(|w| {
        let idx = w["abstract_inverted_index"].as_object()?;
        let mut words: Vec<(u64, &str)> = idx.iter().flat_map(|(word, pos)| pos.as_array().into_iter().flatten().filter_map(move |p| p.as_u64().map(|p| (p, word.as_str())))).collect();
        words.sort_by_key(|x| x.0);
        let summary = words.iter().map(|x| x.1).collect::<Vec<_>>().join(" ");
        let url = w["doi"].as_str().or_else(|| w["id"].as_str())?.to_string();
        Some(Paper { url, title: w["title"].as_str()?.to_string(), summary, published: w["publication_date"].as_str().unwrap_or("").to_string() })
    }).filter(|p| p.summary.len() > 200).collect())
}

/// Relevant research ideas that map to options and have not been measured yet (interval, variant key), newest first: the strategy
/// loop tests these before its own picks.
pub fn untested(mem: &Connection, cfg: &crate::config::StrategyConfig) -> Result<Vec<(String, String)>> {
    init(mem)?;
    let iv = cfg.intervals.first().cloned().unwrap_or_else(|| "5h".into());
    let mut st = mem.prepare("SELECT tokens FROM research_ideas WHERE relevant = 1 AND tokens != '[]' ORDER BY ts DESC")?;
    let rows: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    let mut out: Vec<(String, String)> = Vec::new();
    for t in rows {
        let toks: Vec<String> = serde_json::from_str(&t).unwrap_or_default();
        if toks.is_empty() || !crate::strategy::valid(&toks) { continue; }
        let k = crate::strategy::key(&toks);
        let tested: bool = mem.query_row("SELECT EXISTS (SELECT 1 FROM strategy_runs WHERE interval = ?1 AND tokens = ?2 AND cut = 'A')", params![iv, k], |r| r.get(0))?;
        if !tested && !out.iter().any(|x| x.1 == k) { out.push((iv.clone(), k)); }
    }
    Ok(out)
}

/// Every stored research idea for the dashboard, newest first: paper, link, idea, options and the test status of those options.
pub fn ideas_with_results(mem: &Connection, leaderboard: &[Value], limit: usize) -> Result<Vec<Value>> {
    init(mem)?;
    let mut st = mem.prepare("SELECT url, ts, title, published, relevant, idea, tokens, new_mechanism, query FROM research_ideas ORDER BY relevant DESC, ts DESC LIMIT ?1")?;
    let rows = st.query_map([limit as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, bool>(4)?,
        r.get::<_, String>(5)?, r.get::<_, String>(6)?, r.get::<_, String>(7)?, r.get::<_, String>(8)?)))?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows.into_iter().map(|(url, ts, title, published, relevant, idea, toks, mech, query)| {
        let t: Vec<String> = serde_json::from_str(&toks).unwrap_or_default();
        let k = (!t.is_empty()).then(|| crate::strategy::key(&t));
        let result = k.as_ref().and_then(|k| leaderboard.iter().find(|l| l["tokens"].as_str() == Some(k.as_str())).cloned());
        let status = match (&k, &result) {
            (None, _) => if relevant { if mech.is_empty() { "no option fits" } else { "needs new code" } } else { "not relevant" },
            (Some(_), None) => "queued for testing",
            (Some(_), Some(r)) => r["status"].as_str().unwrap_or("tested"),
        };
        json!({"url": url, "ts": ts, "title": title, "published": published, "relevant": relevant, "idea": idea, "tokens": t, "variant": k,
               "new_mechanism": mech, "source": if let Some(a) = query.strip_prefix("idea:") { if a.starts_with("brain-") { a.to_string() } else { "earlier research (retired proposers)".to_string() } } else if query.starts_with("openalex") { "OpenAlex".to_string() } else { "arXiv".to_string() }, "status": status, "result": result})
    }).collect())
}

/// The newest relevant ideas as whiteboard text for the proposer: idea, tokens to test, source link.
pub fn board_text(mem: &Connection, limit: usize) -> Result<String> {
    init(mem)?;
    let mut st = mem.prepare("SELECT title, url, idea, tokens, new_mechanism FROM research_ideas WHERE relevant = 1 ORDER BY ts DESC LIMIT ?1")?;
    let rows: Vec<(String, String, String, String, String)> = st.query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<_, _>>()?;
    if rows.is_empty() { return Ok(String::new()); }
    let mut s = String::from("IDEAS FROM PUBLISHED RESEARCH (arXiv / OpenAlex; untested until the lab's own backtests pass them):\n");
    for (title, url, idea, toks, mech) in rows {
        let toks: Vec<String> = serde_json::from_str(&toks).unwrap_or_default();
        s.push_str(&format!("  - {} ({url}): {idea}{}{}\n", title.chars().take(90).collect::<String>(),
            if toks.is_empty() { String::new() } else { format!(" -> test with {}", toks.join("+")) },
            if mech.is_empty() { String::new() } else { format!(" (needs new code: {mech})") }));
    }
    Ok(s)
}

/// Words that show a paper is about markets or trading (any one is enough).
const MARKET_TERMS: &[&str] = &["stock", "equit", "market", "trading", "trader", "return", "portfolio", "price", "volatil", "momentum",
    "invest", "asset", "financ", "liquidity", "alpha", "factor", "hedge", "futures", "etf", "s&p", "index fund", "arbitrage",
    "crowding", "order book", "quant", "securit", "bubble", "earnings", "dividend", "short sell", "reversal", "breadth", "vix",
    "option", "kelly", "trend-follow", "trend follow", "monetary", "interest rate", "housing", "bank", "inflation", "economic"];

/// True when a paper is about markets: its title, or the search-result summary the agent was shown for its link (research
/// cache), contains a market or trading term. On 2026-10-11 papers about plant invasions, marine environments and energy
/// transitions were recorded as "relevant" ideas, with trading ideas attached that the papers do not contain.
pub fn is_market_paper(mem: &Connection, url: &str, title: &str) -> bool {
    let has = |t: &str| { let t = t.to_lowercase(); MARKET_TERMS.iter().any(|w| t.contains(w)) };
    if has(title) {
        return true;
    }
    let caches: Vec<String> = mem.prepare("SELECT text FROM research_cache WHERE text LIKE ?1")
        .and_then(|mut st| st.query_map([format!("%{url}%")], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()).unwrap_or_default();
    caches.iter().any(|c| {
        let lines: Vec<&str> = c.lines().collect();
        lines.iter().position(|l| l.contains(url)).is_some_and(|i| has(&lines[i..(i + 4).min(lines.len())].join(" ")))
    })
}

/// Marks recorded ideas whose paper is not about markets as not relevant (kept, never deleted); returns how many.
pub fn demote_off_topic(mem: &Connection) -> Result<usize> {
    init(mem)?;
    crate::facts::init(mem)?;
    let rows: Vec<(String, String)> = mem.prepare("SELECT url, title FROM research_ideas WHERE relevant = 1")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    let mut n = 0;
    for (url, title) in rows {
        if !is_market_paper(mem, &url, &title) {
            n += mem.execute("UPDATE research_ideas SET relevant = 0 WHERE url = ?1", [&url])?;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {

    /// Test: papers about markets pass, off-topic papers (recorded as "relevant" on 2026-10-11) do not.
    #[test]
    fn only_market_papers_count() {
        let m = Connection::open_in_memory().unwrap();
        crate::facts::init(&m).unwrap();
        for t in ["Sizing the Risk: Kelly, VIX, and Hybrid Approaches in Put-Writing on Index Options", "The Science and Practice of Trend-Following Systems", "Pockets of predictability in stock returns"] {
            assert!(is_market_paper(&m, "https://arxiv.org/x", t), "{t}");
        }
        for t in ["Feedbacks between natural climate solutions and plant invasions", "Understanding, measuring, and projecting technology growth in energy transitions"] {
            assert!(!is_market_paper(&m, "https://arxiv.org/y", t), "{t}");
        }
        // the search summary the agent saw counts too
        m.execute("INSERT INTO research_cache(ts, text) VALUES (1, '- A crowding model (2025) https://arxiv.org/z\n  We study stock returns under crowding')", []).unwrap();
        assert!(is_market_paper(&m, "https://arxiv.org/z", "A crowding model"));
    }

    use super::*;

    /// Test: an arXiv Atom feed parses into papers with link, title, abstract and date.
    #[test]
    fn atom_feed_parses() {
        let xml = r#"<feed><entry><id>http://arxiv.org/abs/2501.00001v1</id><published>2025-01-01T00:00:00Z</published>
<title>Short-term  reversal &amp; volume</title><summary> Stocks that fell
 revert. </summary></entry><entry><id>x</id></entry></feed>"#;
        let p = parse_feed(xml);
        assert_eq!(p.len(), 1, "an entry without title or summary is skipped");
        assert_eq!(p[0].url, "http://arxiv.org/abs/2501.00001v1");
        assert_eq!(p[0].title, "Short-term reversal & volume");
        assert_eq!(p[0].summary, "Stocks that fell revert.");
    }
}
