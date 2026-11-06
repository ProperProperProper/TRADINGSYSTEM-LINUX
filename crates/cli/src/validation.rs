//! Rust-owned updates to ThomasAi validation records.
use anyhow::{Result, bail};
use rusqlite::{Connection, params};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS validations(
 id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL,
 subject TEXT NOT NULL, primary_backend TEXT NOT NULL, validator TEXT NOT NULL,
 verdict TEXT NOT NULL, finding TEXT NOT NULL, evidence TEXT NOT NULL DEFAULT '',
 brain_ids TEXT NOT NULL DEFAULT '[]', board_ids TEXT NOT NULL DEFAULT '[]',
 rust_status TEXT NOT NULL DEFAULT '', model_version TEXT NOT NULL DEFAULT '',
 strategy_version TEXT NOT NULL DEFAULT '');";

const STATUSES: [&str; 4] = ["pending", "pass", "fail", "inconclusive"];

pub fn set_rust_status(c: &Connection, id: i64, status: &str) -> Result<()> {
    c.execute_batch(SCHEMA)?;
    if !STATUSES.contains(&status) {
        bail!("unknown Rust status {status:?}");
    }
    let changed = c.execute(
        "UPDATE validations SET rust_status=? WHERE id=?",
        params![status, id],
    )?;
    if changed != 1 {
        bail!("unknown validation id {id}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_existing_records_get_valid_rust_statuses() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute("INSERT INTO validations(ts_ms,subject,primary_backend,validator,verdict,finding) VALUES(1,'x','omlx','claude','accept','y')", []).unwrap();
        set_rust_status(&c, 1, "pass").unwrap();
        assert_eq!(
            c.query_row("SELECT rust_status FROM validations WHERE id=1", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "pass"
        );
        assert!(set_rust_status(&c, 1, "accept").is_err());
        assert!(set_rust_status(&c, 99, "fail").is_err());
    }
}
