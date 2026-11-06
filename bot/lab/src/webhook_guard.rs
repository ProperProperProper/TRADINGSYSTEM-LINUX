//! Keeps Discord webhook URLs (and the file that holds one) out of git. Owner rule 2026-10-13: "do not push webhook to repo ever!".
//! Used by the `qwen-guard` binary, which the pre-commit and pre-push hooks of the lab repo and the bot repo call.
//! A real webhook is `discord.com/api/webhooks/<15+ digit id>/<30+ char token>`; the short fake URLs in unit tests do not match.

/// True if `text` contains something shaped like a real Discord webhook URL.
pub fn has_webhook(text: &str) -> bool {
    for host in ["discord.com/api/webhooks/", "discordapp.com/api/webhooks/"] {
        let mut from = 0;
        while let Some(i) = text[from..].find(host) {
            let rest = &text[from + i + host.len()..];
            let id = rest.chars().take_while(|c| c.is_ascii_digit()).count();
            if id >= 15 {
                let after = &rest[id..];
                if let Some(tok) = after.strip_prefix('/') {
                    let n = tok.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').count();
                    if n >= 30 { return true; }
                }
            }
            from += i + host.len();
        }
    }
    false
}

/// True if `path` is the webhook file by name.
pub fn is_webhook_file(path: &str) -> bool { path == "discord_webhook" || path.ends_with("/discord_webhook") }

/// Scans a unified diff: only added lines (`+...`, not the `+++` header) count.
pub fn diff_leaks(diff: &str) -> bool {
    diff.lines().any(|l| l.starts_with('+') && !l.starts_with("+++") && has_webhook(l))
}

#[cfg(test)]
mod tests {
    use super::*;
    // assembled at run time so this source file never contains a complete webhook-shaped string (the guard would, rightly, block the commit)
    fn real() -> String { format!("https://discord.com/api/{}/{}/{}", "webhooks", "1234567890123456789", "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789_-AbCd") }

    #[test]
    fn real_shaped_webhooks_are_found() {
        let r = real(); assert!(has_webhook(&r));
        assert!(has_webhook(&format!("url = \"{r}\"\nmore")));
        assert!(has_webhook(&r.replace("discord.com", "discordapp.com")));
        assert!(has_webhook(&r.replace("https://", "https://canary.")));
    }

    #[test]
    fn test_fixtures_and_prose_are_not() {
        assert!(!has_webhook("https://discord.com/api/webhooks/123456789/abcdefghijklmnop"));
        assert!(!has_webhook("https://discord.com/api/webhooks/"));
        assert!(!has_webhook("https://discord.com/api/webhooks/<numbers>/<long token>"));
        assert!(!has_webhook("only discord.com/api/webhooks/ appears in prose"));
    }

    #[test]
    fn a_later_real_one_after_a_fake_one_is_found() {
        assert!(has_webhook(&format!("fake https://discord.com/api/webhooks/1/x then {}", real())));
    }

    #[test]
    fn only_added_diff_lines_count() {
        assert!(diff_leaks(&format!("+++ b/x\n+key {}\n", real())));
        assert!(!diff_leaks(&format!("--- a/x\n-removed {}\n context\n", real())));
    }

    #[test]
    fn webhook_file_name() {
        assert!(is_webhook_file("discord_webhook") && is_webhook_file(".config/qwen-lab/discord_webhook"));
        assert!(!is_webhook_file("docs/discord_webhook_notes.md"));
    }
}
