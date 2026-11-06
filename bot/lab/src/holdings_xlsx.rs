//! SPY's official daily holdings spreadsheet (State Street) -> holdings CSV (rank,ticker,name,weight_pct). Rust port of the bot's
//! ops/spy_holdings_xlsx.py. `qwen-tools spy-holdings-xlsx <holdings-daily-us-en-spy.xlsx> <out.csv>`.
//! Rows kept: USD equity lines with a ticker; the cash line (ticker "-") is dropped. Nothing is invented: names and weights are the file's.

use anyhow::{anyhow, bail, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::BTreeMap;
use std::io::Read;

fn local(name: &[u8]) -> &[u8] { name.rsplit(|b| *b == b':').next().unwrap_or(name) }

/// Text of every `<t>` inside each top-level `<si>` of sharedStrings.xml.
fn shared_strings(xml: &[u8]) -> Result<Vec<String>> {
    let mut rd = Reader::from_reader(xml);
    let (mut out, mut buf) = (Vec::new(), Vec::new());
    let (mut in_si, mut in_t, mut cur) = (false, false, String::new());
    loop {
        match rd.read_event_into(&mut buf)? {
            Event::Start(e) if local(e.name().as_ref()) == b"si" => { in_si = true; cur.clear(); }
            Event::Start(e) if in_si && local(e.name().as_ref()) == b"t" => in_t = true,
            Event::Text(t) if in_si && in_t => cur.push_str(&t.unescape()?),
            Event::End(e) if local(e.name().as_ref()) == b"t" => in_t = false,
            Event::End(e) if local(e.name().as_ref()) == b"si" => { in_si = false; out.push(std::mem::take(&mut cur)); }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// Each row of a worksheet as {column letters: cell text}; `strings` resolves shared-string cells.
pub fn sheet_rows(xml: &[u8], strings: &[String]) -> Result<Vec<BTreeMap<String, String>>> {
    let mut rd = Reader::from_reader(xml);
    let (mut rows, mut buf) = (Vec::new(), Vec::new());
    let mut row: Option<BTreeMap<String, String>> = None;
    let (mut col, mut ty, mut v, mut inline, mut in_v, mut in_t, mut in_c) = (String::new(), String::new(), None::<String>, String::new(), false, false, false);
    loop {
        match rd.read_event_into(&mut buf)? {
            Event::Start(e) if local(e.name().as_ref()) == b"row" => row = Some(BTreeMap::new()),
            Event::Start(e) if local(e.name().as_ref()) == b"c" && row.is_some() => {
                in_c = true; col.clear(); ty.clear(); v = None; inline.clear();
                for a in e.attributes().flatten() {
                    match local(a.key.as_ref()) { b"r" => col = String::from_utf8_lossy(&a.value).chars().take_while(|c| c.is_ascii_uppercase()).collect(), b"t" => ty = String::from_utf8_lossy(&a.value).into_owned(), _ => {} }
                }
            }
            Event::Start(e) if in_c && local(e.name().as_ref()) == b"v" => in_v = true,
            Event::Start(e) if in_c && local(e.name().as_ref()) == b"t" => in_t = true,
            Event::Text(t) if in_v => { let s = t.unescape()?; v = Some(v.unwrap_or_default() + &s); }
            Event::Text(t) if in_t => inline.push_str(&t.unescape()?),
            Event::End(e) if local(e.name().as_ref()) == b"v" => in_v = false,
            Event::End(e) if local(e.name().as_ref()) == b"t" => in_t = false,
            Event::End(e) if local(e.name().as_ref()) == b"c" && in_c => {
                in_c = false;
                if let Some(r) = row.as_mut() {
                    if ty == "inlineStr" { r.insert(col.clone(), inline.clone()); }
                    else if let Some(val) = &v { r.insert(col.clone(), if ty == "s" { val.trim().parse::<usize>().ok().and_then(|i| strings.get(i)).cloned().ok_or_else(|| anyhow!("bad shared string index {val}"))? } else { val.clone() }); }
                }
            }
            Event::End(e) if local(e.name().as_ref()) == b"row" => { if let Some(r) = row.take() { rows.push(r); } }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(rows)
}

fn read_rows(path: &str) -> Result<Vec<BTreeMap<String, String>>> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    let strings = if z.file_names().filter_map(Result::ok).any(|n| n == "xl/sharedStrings.xml") { let mut b = Vec::new(); z.by_name("xl/sharedStrings.xml")?.read_to_end(&mut b)?; shared_strings(&b)? } else { vec![] };
    let mut sheets: Vec<String> = z.file_names().filter_map(Result::ok).filter(|n| n.starts_with("xl/worksheets/sheet")).map(|n| n.into_owned()).collect();
    sheets.sort();
    let first = sheets.first().ok_or_else(|| anyhow!("no worksheet in {path}"))?.clone();
    let mut b = Vec::new(); z.by_name(&first)?.read_to_end(&mut b)?;
    sheet_rows(&b, &strings)
}

/// (as-of text, holdings sorted by weight descending) from the spreadsheet rows.
pub fn holdings(rows: &[BTreeMap<String, String>]) -> Result<(Option<String>, Vec<(f64, String, String)>)> {
    let (mut as_of, mut header, mut out): (Option<String>, Option<BTreeMap<String, String>>, Vec<(f64, String, String)>) = (None, None, vec![]);
    for r in rows {
        let first = r.get("A").map(String::as_str).unwrap_or("");
        if first.starts_with("Holdings:") { as_of = Some(r.get("B").map(String::as_str).unwrap_or("").replace("As of", "").trim().to_string()); }
        else if first == "Name" && r.get("B").map(String::as_str) == Some("Ticker") { header = Some(r.iter().map(|(k, v)| (v.clone(), k.clone())).collect()); }
        else if let Some(h) = &header {
            let col = |name: &str| h.get(name).ok_or_else(|| anyhow!("header has no {name} column"));
            if r.get(col("Local Currency")?).map(String::as_str) == Some("USD") {
                let ticker = r.get(col("Ticker")?).map(|t| t.trim().to_string()).unwrap_or_default();
                if !ticker.is_empty() && ticker != "-" {
                    let w: f64 = r.get(col("Weight")?).ok_or_else(|| anyhow!("row without a weight"))?.trim().parse()?;
                    out.push((w, ticker, r.get(col("Name")?).map(|n| n.replace(',', " ").trim().to_string()).unwrap_or_default()));
                }
            }
        }
    }
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok((as_of, out))
}

pub fn convert(src: &str, dst: &str) -> Result<String> {
    let (as_of, h) = holdings(&read_rows(src)?)?;
    let as_of = match as_of { Some(a) if !a.is_empty() && h.len() >= 400 => a, a => bail!("not a usable SPY holdings file (as of {:?}, {} rows)", a.unwrap_or_default(), h.len()) };
    let mut csv = format!("# SPY (State Street SPDR S&P 500 ETF Trust) holdings, official daily file, as of {as_of}. Public data; weights in percent.\nrank,ticker,name,weight_pct\n");
    for (i, (w, t, n)) in h.iter().enumerate() { csv.push_str(&format!("{},{t},{n},{}\n", i + 1, crate::misc_tools::py_float(*w))); }
    std::fs::write(dst, csv)?;
    Ok(format!("{} holdings as of {as_of}", h.len()))
}

pub fn main(args: &[String]) -> i32 {
    if args.len() != 2 { eprintln!("usage: spy-holdings-xlsx <holdings.xlsx> <out.csv>"); return 2; }
    match convert(&args[0], &args[1]) { Ok(s) => { println!("{s}"); 0 } Err(e) => { eprintln!("{e:#}"); 1 } }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cells: &[(&str, &str)]) -> BTreeMap<String, String> { cells.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }

    #[test]
    fn rows_become_sorted_holdings_and_drop_cash_and_non_usd() {
        let rows = vec![row(&[("A", "Holdings: SPY"), ("B", "As of 09-Oct-2026")]),
            row(&[("A", "Name"), ("B", "Ticker"), ("C", "Weight"), ("D", "Local Currency")]),
            row(&[("A", "Small, Co"), ("B", "SML"), ("C", "0.5"), ("D", "USD")]), row(&[("A", "Big Inc"), ("B", "BIG"), ("C", "7.25"), ("D", "USD")]),
            row(&[("A", "US Dollar"), ("B", "-"), ("C", "0.1"), ("D", "USD")]), row(&[("A", "Foreign"), ("B", "FRN"), ("C", "3"), ("D", "EUR")])];
        let (as_of, h) = holdings(&rows).unwrap();
        assert_eq!(as_of.as_deref(), Some("09-Oct-2026"));
        assert_eq!(h, vec![(7.25, "BIG".into(), "Big Inc".into()), (0.5, "SML".into(), "Small  Co".into())]);
    }

    #[test]
    fn shared_strings_and_inline_strings_resolve() {
        let strings = shared_strings(br#"<sst xmlns="x"><si><t>Name</t></si><si><r><t>Ti</t></r><r><t>cker</t></r></si></sst>"#).unwrap();
        assert_eq!(strings, vec!["Name", "Ticker"]);
        let sheet = br#"<worksheet xmlns="x"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C12"><v>4.5</v></c><c r="D1" t="inlineStr"><is><t>USD</t></is></c></row></sheetData></worksheet>"#;
        assert_eq!(sheet_rows(sheet, &strings).unwrap(), vec![row(&[("A", "Name"), ("B", "Ticker"), ("C", "4.5"), ("D", "USD")])]);
    }

    #[test]
    fn a_file_with_too_few_holdings_is_refused() {
        let d = tempfile::tempdir().unwrap(); let p = d.path().join("x.xlsx");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
        z.start_file("xl/worksheets/sheet1.xml", zip::write::FileOptions::default()).unwrap();
        std::io::Write::write_all(&mut z, br#"<worksheet><sheetData><row><c r="A1" t="inlineStr"><is><t>Holdings: SPY</t></is></c></row></sheetData></worksheet>"#).unwrap();
        z.finish().unwrap();
        assert!(convert(p.to_str().unwrap(), d.path().join("o.csv").to_str().unwrap()).unwrap_err().to_string().contains("not a usable SPY holdings file"));
    }
}
