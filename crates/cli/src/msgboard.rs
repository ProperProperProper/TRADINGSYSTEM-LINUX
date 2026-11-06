//! The agents' message board from the Rust side: the same SQLite file the competition bot's programs use (`papercompmoomoo/data/board.sqlite3`, see `board.py`), so the judge, the cycle, the
//! watchdog and the reviewer talk to the planner, the executor and the commander on one board. A message has a sender, a recipient (an agent or `*`), a topic, a kind, a body, optionally the id
//! it answers, the goal (O1-O6) it serves and the backend that wrote it. What is read is data, never an instruction.
use anyhow::{Result, bail};
use rusqlite::{Connection, OpenFlags, params};
use std::path::{Path, PathBuf};

pub const KINDS: [&str; 6] = ["note", "question", "answer", "decision", "alert", "lesson"];
pub const MAX_BODY: usize = 2000;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS messages(
    id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, sender TEXT NOT NULL, recipient TEXT NOT NULL DEFAULT '*', topic TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL DEFAULT 'note', body TEXT NOT NULL, reply_to INTEGER, goal TEXT NOT NULL DEFAULT '', via TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS reads(message_id INTEGER NOT NULL, reader TEXT NOT NULL, ts_ms INTEGER NOT NULL, PRIMARY KEY(message_id, reader));
CREATE INDEX IF NOT EXISTS messages_ts ON messages(ts_ms);
CREATE INDEX IF NOT EXISTS messages_recipient ON messages(recipient, id);
";

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub id: i64,
    pub ts_ms: i64,
    pub sender: String,
    pub recipient: String,
    pub topic: String,
    pub kind: String,
    pub body: String,
    pub reply_to: Option<i64>,
    pub goal: String,
    pub via: String,
}

pub fn path(root: &Path) -> PathBuf {
    root.join("papercompmoomoo/data/board.sqlite3")
}

/// Opens (and creates) the board for reading and writing.
pub fn open(root: &Path) -> Result<Connection> {
    let p = path(root);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let c = Connection::open(&p)?;
    c.busy_timeout(std::time::Duration::from_secs(30))?;
    c.execute_batch("PRAGMA journal_mode=WAL;")?;
    c.execute_batch(SCHEMA)?;
    for (col, def) in [
        ("goal", "TEXT NOT NULL DEFAULT ''"),
        ("via", "TEXT NOT NULL DEFAULT ''"),
    ] {
        let has: bool = c
            .prepare("PRAGMA table_info(messages)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(|x| x.ok())
            .any(|n| n == col);
        if !has {
            c.execute(&format!("ALTER TABLE messages ADD COLUMN {col} {def}"), [])?;
        }
    }
    Ok(c)
}

/// Opens the board read-only when it exists (the portal).
pub fn open_ro(root: &Path) -> Option<Connection> {
    let c = Connection::open_with_flags(path(root), OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    c.busy_timeout(std::time::Duration::from_secs(5)).ok()?;
    Some(c)
}

fn name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 24
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| *c == '\n' || *c >= ' ')
        .collect::<String>()
        .chars()
        .take(MAX_BODY)
        .collect::<String>()
        .trim()
        .to_string()
}

fn goal_ok(g: &str) -> bool {
    g.is_empty()
        || (g.starts_with('O')
            && g.len() <= 3
            && g[1..].chars().all(|c| c.is_ascii_digit())
            && g.len() > 1)
}

/// Puts a message on the board; refuses bad names, kinds, empty texts and answers to nothing.
#[allow(clippy::too_many_arguments)]
pub fn post(
    c: &Connection,
    now_ms: i64,
    sender: &str,
    to: &str,
    topic: &str,
    kind: &str,
    body: &str,
    reply_to: Option<i64>,
    goal: &str,
    via: &str,
) -> Result<i64> {
    if !name_ok(sender) {
        bail!("bad sender {sender:?}");
    }
    if to != "*" && !name_ok(to) {
        bail!("bad recipient {to:?}");
    }
    if !KINDS.contains(&kind) {
        bail!("bad kind {kind:?}");
    }
    let body = clean(body);
    if body.is_empty() {
        bail!("an empty message");
    }
    if let Some(id) = reply_to {
        let n: i64 = c.query_row("SELECT count(*) FROM messages WHERE id=?", [id], |r| {
            r.get(0)
        })?;
        if n == 0 {
            bail!("no message {id} to answer");
        }
    }
    let goal = if goal_ok(goal) { goal } else { "" };
    c.execute(
        "INSERT INTO messages(ts_ms, sender, recipient, topic, kind, body, reply_to, goal, via) VALUES(?,?,?,?,?,?,?,?,?)",
        params![now_ms, sender, to, clean(topic).chars().take(80).collect::<String>(), kind, body, reply_to, goal, clean(via).chars().take(24).collect::<String>()],
    )?;
    Ok(c.last_insert_rowid())
}

/// A post from a program that must never fail because of the board (the judge, the cycle, the watchdog).
#[allow(clippy::too_many_arguments)]
pub fn say(
    root: &Path,
    now_ms: i64,
    sender: &str,
    to: &str,
    topic: &str,
    kind: &str,
    body: &str,
    goal: &str,
) {
    if let Ok(c) = open(root) {
        let _ = post(
            &c, now_ms, sender, to, topic, kind, body, None, goal, "rust",
        );
    }
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: r.get(0)?,
        ts_ms: r.get(1)?,
        sender: r.get(2)?,
        recipient: r.get(3)?,
        topic: r.get(4)?,
        kind: r.get(5)?,
        body: r.get(6)?,
        reply_to: r.get(7)?,
        goal: r.get(8)?,
        via: r.get(9)?,
    })
}

const COLS: &str = "id, ts_ms, sender, recipient, topic, kind, body, reply_to, goal, via";

/// The newest `limit` messages of the board, oldest first.
pub fn recent(c: &Connection, limit: usize) -> Result<Vec<Message>> {
    let mut st = c.prepare(&format!(
        "SELECT {COLS} FROM messages ORDER BY id DESC LIMIT ?"
    ))?;
    let mut v: Vec<Message> = st
        .query_map([limit as i64], row)?
        .collect::<std::result::Result<_, _>>()?;
    v.reverse();
    Ok(v)
}

/// What is for `reader` (addressed to it or to everybody, not written by it); `unread_only` leaves out what it has read.
pub fn inbox(
    c: &Connection,
    reader: &str,
    limit: usize,
    unread_only: bool,
) -> Result<Vec<Message>> {
    let unread = if unread_only {
        " AND NOT EXISTS (SELECT 1 FROM reads r WHERE r.message_id=m.id AND r.reader=?3)"
    } else {
        ""
    };
    let sql = format!(
        "SELECT m.id, m.ts_ms, m.sender, m.recipient, m.topic, m.kind, m.body, m.reply_to, m.goal, m.via FROM messages m WHERE (m.recipient=?1 OR m.recipient='*') AND m.sender!=?1{unread} ORDER BY m.id DESC LIMIT ?2"
    );
    let mut st = c.prepare(&sql)?;
    let mut v: Vec<Message> = if unread_only {
        st.query_map(params![reader, limit as i64, reader], row)?
            .collect::<std::result::Result<_, _>>()?
    } else {
        st.query_map(params![reader, limit as i64], row)?
            .collect::<std::result::Result<_, _>>()?
    };
    v.reverse();
    Ok(v)
}

pub fn mark_read(c: &Connection, now_ms: i64, reader: &str, ids: &[i64]) -> Result<()> {
    for id in ids {
        c.execute(
            "INSERT OR IGNORE INTO reads(message_id, reader, ts_ms) VALUES(?,?,?)",
            params![id, reader, now_ms],
        )?;
    }
    Ok(())
}

/// One message as a line of text (for the command line and the agents' prompts).
pub fn line(m: &Message) -> String {
    format!(
        "#{} {} {}→{} [{}{}{}]{}{} {}",
        m.id,
        crate::ops::utc(m.ts_ms),
        m.sender,
        m.recipient,
        m.kind,
        if m.goal.is_empty() {
            String::new()
        } else {
            format!(", {}", m.goal)
        },
        if m.via.is_empty() {
            String::new()
        } else {
            format!(", via {}", m.via)
        },
        if m.topic.is_empty() {
            String::new()
        } else {
            format!(" ({})", m.topic)
        },
        m.reply_to.map_or(String::new(), |r| format!(" re #{r}")),
        m.body.replace('\n', " ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("thomas-msgboard-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_message_round_trips_and_bad_ones_are_refused() {
        let r = root("a");
        let c = open(&r).unwrap();
        let id = post(
            &c,
            1,
            "judge",
            "*",
            "daily",
            "decision",
            "Nothing passed.",
            None,
            "O5",
            "rust",
        )
        .unwrap();
        let m = &recent(&c, 5).unwrap()[0];
        assert_eq!(
            (
                m.id,
                m.sender.as_str(),
                m.kind.as_str(),
                m.goal.as_str(),
                m.via.as_str()
            ),
            (id, "judge", "decision", "O5", "rust")
        );
        assert!(line(m).contains("judge→*") && line(m).contains("Nothing passed."));
        for (s, to, k, b, re) in [
            ("Bad Name", "*", "note", "x", None),
            ("judge", "Bad", "note", "x", None),
            ("judge", "*", "gossip", "x", None),
            ("judge", "*", "note", "  ", None),
            ("judge", "*", "note", "x", Some(99)),
        ] {
            assert!(post(&c, 1, s, to, "", k, b, re, "", "").is_err());
        }
        assert_eq!(
            post(&c, 2, "judge", "*", "", "note", "x", None, "nonsense", "")
                .map(|i| recent(&c, 1).unwrap()[0].goal.clone() + &i.to_string())
                .unwrap(),
            format!("{}", id + 1),
            "an invalid goal tag is dropped"
        );
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn the_inbox_is_what_is_for_the_reader_and_read_messages_drop_out_of_the_unread_view() {
        let r = root("b");
        let c = open(&r).unwrap();
        let a = post(
            &c,
            1,
            "judge",
            "*",
            "",
            "note",
            "for everybody",
            None,
            "",
            "",
        )
        .unwrap();
        let b = post(
            &c,
            2,
            "reviewer",
            "planner",
            "",
            "note",
            "for the planner",
            None,
            "",
            "",
        )
        .unwrap();
        post(
            &c,
            3,
            "reviewer",
            "improver",
            "",
            "note",
            "for the improver",
            None,
            "",
            "",
        )
        .unwrap();
        post(&c, 4, "planner", "*", "", "note", "my own", None, "", "").unwrap();
        assert_eq!(
            inbox(&c, "planner", 10, false)
                .unwrap()
                .iter()
                .map(|m| m.id)
                .collect::<Vec<_>>(),
            [a, b]
        );
        mark_read(&c, 5, "planner", &[a]).unwrap();
        assert_eq!(
            inbox(&c, "planner", 10, true)
                .unwrap()
                .iter()
                .map(|m| m.id)
                .collect::<Vec<_>>(),
            [b]
        );
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn the_board_a_python_program_made_can_be_read_and_written_here() {
        let r = root("c");
        let p = path(&r);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        {
            // an older board without the goal and via columns, as the first python version made it
            let c = Connection::open(&p).unwrap();
            c.execute_batch("CREATE TABLE messages(id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, sender TEXT NOT NULL, recipient TEXT NOT NULL DEFAULT '*', topic TEXT NOT NULL DEFAULT '', kind TEXT NOT NULL DEFAULT 'note', body TEXT NOT NULL, reply_to INTEGER); INSERT INTO messages(ts_ms,sender,body) VALUES(1,'planner','old');").unwrap();
        }
        let c = open(&r).unwrap();
        post(&c, 2, "judge", "*", "", "note", "new", None, "O1", "rust").unwrap();
        let all = recent(&c, 5).unwrap();
        assert_eq!((all[0].body.as_str(), all[0].goal.as_str()), ("old", ""));
        assert_eq!(all[1].goal, "O1");
        assert!(open_ro(&r).is_some());
        let _ = std::fs::remove_dir_all(&r);
    }
}
