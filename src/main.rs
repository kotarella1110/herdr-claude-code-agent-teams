mod action;
mod herdr;
mod launcher;
mod route;
mod shell;
mod state;
mod tmux;

use std::env;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = env::args();
    let argv0 = args.next().unwrap_or_default();
    let rest: Vec<String> = args.collect();

    if Path::new(&argv0).file_name().is_some_and(|name| name == "tmux") {
        return tmux::run(&rest);
    }
    match rest.first().map(String::as_str) {
        Some("action") => action::run(),
        Some("edit-config") => action::edit_config(),
        Some("route") => route::register(&rest[1..]),
        _ => launcher::run(&rest),
    }
}
