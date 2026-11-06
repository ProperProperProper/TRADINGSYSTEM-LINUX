//! `qwen-ops install | uninstall | status | restart <label> | redeploy | monitor | supervisor | watch | watchdog | health` -- setting up and keeping the lab running on macOS,
//! Linux and Windows (src/opsctl.rs) and the resource-safety jobs (src/ops.rs, docs/RESOURCE_SAFETY.md). `--config <lab.toml>` is accepted by the first four.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("watch") => { qwen_lab::ops::watch(); 0 }
        Some("watchdog") => { qwen_lab::ops::watchdog(); 0 }
        Some("health") => { qwen_lab::ops::health(); 0 }
        Some("install") => match qwen_lab::opsctl::install(&args) { Ok(()) => 0, Err(e) => { eprintln!("install failed: {e:#}"); 1 } },
        Some("uninstall") => match qwen_lab::opsctl::uninstall() { Ok(()) => 0, Err(e) => { eprintln!("uninstall failed: {e:#}"); 1 } },
        Some("install-tools") => match qwen_lab::opsctl::install_tools_only() { Ok(()) => 0, Err(e) => { eprintln!("install-tools failed: {e:#}"); 1 } },
        Some("status") => { for l in qwen_lab::opsctl::status() { println!("{l}"); } 0 }
        Some("restart") => match args.get(1) { Some(l) => { qwen_lab::opsctl::restart(l); 0 } None => { eprintln!("usage: qwen-ops restart <label>"); 2 } },
        Some("monitor") => { qwen_lab::monitor::run(&args); 0 }
        Some("supervisor") => qwen_lab::supervisor::run(&args),
        Some("redeploy") => if qwen_lab::opsctl::redeploy(&args) { 0 } else { 1 },
        _ => { eprintln!("usage: qwen-ops install | install-tools | uninstall | status | restart <label> | redeploy | monitor | supervisor | watch | watchdog | health"); 2 }
    };
    std::process::exit(code);
}
