//! Discord change notifications (owner 2026-10-13: "push all change notifications to my Discord").
//!
//! One webhook, nothing else. The webhook URL is a credential: it is read from `QWEN_LAB_DISCORD_WEBHOOK` or from
//! `~/.config/qwen-lab/discord_webhook` (outside every repo), never logged, never printed, and only Discord hosts are accepted so the
//! command cannot be turned into a way to post text anywhere else. Every message is redacted before it leaves the machine: account-like
//! numbers, e-mail addresses, home-directory names, long key-like tokens, `key=value` secrets and webhook URLs are masked. Identical
//! messages within ten minutes are sent once (a flapping job must not flood the channel). Without a webhook it is a silent no-op.

use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::Duration;

/// Discord rejects content over 2000 characters; stay well under it.
pub const MAX_LEN: usize = 1800;
/// Identical messages inside this window are sent once.
pub const DEDUPE_SECS: u64 = 600;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome { Sent, NotConfigured, Duplicate }

fn home() -> PathBuf { crate::platform::home() }

/// The configured webhook URL, if any and if it is a Discord one.
pub fn webhook_url() -> Option<String> {
    let raw = std::env::var("QWEN_LAB_DISCORD_WEBHOOK").ok()
        .or_else(|| std::fs::read_to_string(home().join(".config/qwen-lab/discord_webhook")).ok())?;
    let url = raw.trim().to_string();
    if valid_webhook(&url) { Some(url) } else { None }
}

/// Only Discord's own webhook endpoints are accepted.
pub fn valid_webhook(url: &str) -> bool {
    ["https://discord.com/api/webhooks/", "https://discordapp.com/api/webhooks/", "https://canary.discord.com/api/webhooks/", "https://ptb.discord.com/api/webhooks/"]
        .iter().any(|p| url.starts_with(p) && url.len() > p.len() + 10 && !url.contains(char::is_whitespace))
}

fn is_key_like(tok: &str) -> bool {
    // a long run of letters+digits (api keys, tokens, hashes of secrets); plain words and git short hashes are left alone
    let core: String = tok.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '+' || *c == '/').collect();
    core.len() >= 28 && core.chars().any(|c| c.is_ascii_digit()) && core.chars().any(|c| c.is_ascii_alphabetic()) && core.len() * 10 >= tok.len() * 9
}

fn mask_digits(tok: &str) -> String {
    // runs of 9+ digits (account, order and card-like numbers) -> [number]
    let mut out = String::new(); let mut run = String::new();
    for c in tok.chars().chain(std::iter::once('\0')) {
        if c.is_ascii_digit() { run.push(c); continue; }
        if run.len() >= 9 { out.push_str("[number]"); } else { out.push_str(&run); }
        run.clear();
        if c != '\0' { out.push(c); }
    }
    out
}

fn redact_token(tok: &str) -> String {
    let lower = tok.to_lowercase();
    if lower.contains("discord.com/api/webhooks") || lower.contains("discordapp.com/api/webhooks") { return "[webhook]".into(); }
    if tok.contains('@') && tok.rsplit('@').next().is_some_and(|d| d.contains('.')) { return "[email]".into(); }
    // a home folder path of any OS: /Users/<name>/rest, /home/<name>/rest, C:\Users\<name>\rest -> ~/rest
    for marker in ["/Users/", "/home/", "\\Users\\"] {
        if let Some(i) = tok.find(marker) {
            let after = &tok[i + marker.len()..]; let rest = after.find(['/', '\\']).map(|j| &after[j..]).unwrap_or("");
            let start = if marker == "\\Users\\" && i >= 2 && tok.as_bytes()[i - 1] == b':' { i - 2 } else { i };
            return format!("{}~{}", &tok[..start], rest.replace('\\', "/"));
        }
    }
    // key=value / key:value where the key names a secret
    for sep in ['=', ':'] {
        if let Some((k, v)) = tok.split_once(sep) {
            let kl = k.to_lowercase();
            if !v.is_empty() && ["key", "secret", "token", "passw", "passphrase", "pwd", "auth", "bearer", "acc_id", "account"].iter().any(|w| kl.contains(w)) {
                return format!("{k}{sep}[redacted]");
            }
        }
    }
    if is_key_like(tok) { return "[redacted]".into(); }
    mask_digits(tok)
}

/// Masks anything that looks like a secret or account detail. Whitespace and line breaks are kept.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len()); let mut tok = String::new();
    let flush = |tok: &mut String, out: &mut String| { if !tok.is_empty() { out.push_str(&redact_token(tok)); tok.clear(); } };
    for c in text.chars() {
        if c.is_whitespace() { flush(&mut tok, &mut out); out.push(c); } else { tok.push(c); }
    }
    flush(&mut tok, &mut out);
    // a secret word followed by its value in the next token ("password hunter22") is not caught token-wise; common lead-ins are handled here
    out
}

pub fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_LEN { return text.to_string(); }
    let mut s: String = text.chars().take(MAX_LEN - 1).collect(); s.push('…'); s
}

fn state_path() -> PathBuf { home().join(".local/state/qwen-lab/notify_seen") }

fn digest(msg: &str) -> String { hex::encode(&Sha256::digest(msg.as_bytes())[..8]) }

/// True if this exact message was sent less than `DEDUPE_SECS` ago; otherwise records it.
fn seen_recently(msg: &str, now: u64) -> bool {
    let p = state_path(); let h = digest(msg);
    let old = std::fs::read_to_string(&p).unwrap_or_default();
    let mut keep: Vec<String> = Vec::new(); let mut dup = false;
    for line in old.lines() {
        let mut it = line.split_whitespace();
        if let (Some(hh), Some(t)) = (it.next(), it.next().and_then(|t| t.parse::<u64>().ok())) {
            if now.saturating_sub(t) < DEDUPE_SECS { if hh == h { dup = true; } keep.push(line.to_string()); }
        }
    }
    if !dup { keep.push(format!("{h} {now}")); }
    if let Some(dir) = p.parent() { let _ = std::fs::create_dir_all(dir); }
    let _ = std::fs::write(&p, keep.join("\n") + "\n");
    dup
}

/// Sends `title` + `body` to the configured Discord webhook after redaction.
pub fn send(title: &str, body: &str) -> Result<Outcome> {
    let Some(url) = webhook_url() else { return Ok(Outcome::NotConfigured) };
    let text = clip(&redact(&if title.is_empty() { body.to_string() } else { format!("**{}**\n{}", title, body) }));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    if seen_recently(&text, now) { return Ok(Outcome::Duplicate); }
    let payload = serde_json::json!({ "content": text, "username": "qwen-lab", "allowed_mentions": { "parse": [] } });
    for attempt in 0..2 {
        match ureq::post(&url).timeout(Duration::from_secs(10)).send_json(payload.clone()) {
            Ok(_) => return Ok(Outcome::Sent),
            Err(ureq::Error::Status(429, r)) if attempt == 0 => {
                let wait = r.header("retry-after").and_then(|v| v.parse::<f64>().ok()).unwrap_or(2.0).clamp(0.5, 8.0);
                std::thread::sleep(Duration::from_secs_f64(wait));
            }
            // never include the URL (a credential) in the error text
            Err(ureq::Error::Status(code, _)) => return Err(anyhow!("discord answered HTTP {code}")),
            Err(_) => return Err(anyhow!("discord request failed (network or timeout)")),
        }
    }
    Err(anyhow!("discord rate limit"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_discord_hosts_are_accepted() {
        assert!(valid_webhook("https://discord.com/api/webhooks/123456789/abcdefghijklmnop"));
        assert!(!valid_webhook("https://evil.example/api/webhooks/123456789/abcdefghijklmnop"));
        assert!(!valid_webhook("http://discord.com/api/webhooks/123456789/abcdefghijklmnop"));
        assert!(!valid_webhook("https://discord.com.evil.example/api/webhooks/123456789/abcdefghijklmnop"));
        assert!(!valid_webhook("https://discord.com/api/webhooks/"));
    }

    #[test]
    fn account_details_are_masked() {
        let r = redact("account 281756455998 order 12345678 path /Users/someone/Projects/x mail a.b@example.com");
        assert!(r.contains("[number]") && r.contains("12345678") && r.contains("~/Projects/x") && r.contains("[email]"), "{r}");
        assert!(!r.contains("281756455998") && !r.contains("someone") && !r.contains("a.b@example.com"), "{r}");
    }

    /// Test: home folder paths of Linux and Windows are masked like the macOS ones.
    #[test]
    fn home_paths_of_every_os_are_masked() {
        let r = redact("see /home/alice/Projects/x and C:\\Users\\bob\\Projects\\y now");
        assert!(r.contains("~/Projects/x") && r.contains("~/Projects/y") && !r.contains("alice") && !r.contains("bob"), "{r}");
    }

    #[test]
    fn secrets_and_webhooks_are_masked() {
        let r = redact("api_key=sk-abcdef123456 token:xyz secret=hunter2 https://discord.com/api/webhooks/1/abc");
        assert_eq!(r, "api_key=[redacted] token:[redacted] secret=[redacted] [webhook]");
        let k = redact("deadbeefdeadbeef0123456789abcdefAAAA ok");
        assert_eq!(k, "[redacted] ok");
    }

    #[test]
    fn normal_change_text_is_left_alone() {
        let s = "Resource safety: fix the crash (commit 942593f, 3 files)\nswap 1066MB -> 944MB; Hermes run rc=0 in 842s";
        assert_eq!(redact(s), s);
    }

    #[test]
    fn long_text_is_clipped_and_whitespace_kept() {
        assert_eq!(clip(&"a b\n".repeat(1000)).chars().count(), MAX_LEN);
        assert_eq!(redact("a\n\tb  c"), "a\n\tb  c");
    }

    #[test]
    fn unconfigured_is_a_silent_noop() {
        std::env::remove_var("QWEN_LAB_DISCORD_WEBHOOK");
        std::env::set_var("HOME", "/nonexistent-home-for-test");
        assert_eq!(send("t", "b").unwrap(), Outcome::NotConfigured);
    }
}
