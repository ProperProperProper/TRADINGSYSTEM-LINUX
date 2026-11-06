//! `qwen-notify [--title T] [MESSAGE]` -- posts a change notification to the owner's Discord (message from stdin when omitted).
//! Exit 0 when sent, skipped as a duplicate, or not configured (so shell scripts can call it without `|| true`); 1 only on a send error.
use std::io::Read;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut title = String::new();
    if args.first().map(String::as_str) == Some("--title") && args.len() >= 2 { title = args[1].clone(); args.drain(..2); }
    let body = if args.is_empty() { let mut s = String::new(); let _ = std::io::stdin().read_to_string(&mut s); s } else { args.join(" ") };
    if body.trim().is_empty() && title.is_empty() { eprintln!("usage: qwen-notify [--title T] MESSAGE"); std::process::exit(2); }
    match qwen_lab::notify::send(&title, body.trim()) {
        Ok(qwen_lab::notify::Outcome::Sent) => {}
        Ok(qwen_lab::notify::Outcome::NotConfigured) => eprintln!("qwen-notify: Discord not configured (write the webhook URL to ~/.config/qwen-lab/discord_webhook)"),
        Ok(qwen_lab::notify::Outcome::Duplicate) => {}
        Err(e) => { eprintln!("qwen-notify: {e}"); std::process::exit(1); }
    }
}
