use std::env;
use std::fmt;
use std::fs;
use std::io::{self, BufRead, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rustix::process::geteuid;
use serde_json::Value;

use crate::herdr;
use crate::launcher;
use crate::state::State;
use crate::tmux;

const MANIFEST: &str = include_str!("../herdr-plugin.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Ok,
    Warn,
    Fail,
}

impl fmt::Display for Level {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.pad(match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "FAIL",
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Check {
    level: Level,
    subject: String,
    detail: String,
}

impl Check {
    fn new(level: Level, subject: &str, detail: impl Into<String>) -> Self {
        Self { level, subject: subject.to_owned(), detail: detail.into() }
    }
}

pub(crate) fn run(args: &[String]) -> ExitCode {
    let checks = checks();
    println!("herdr-claude-code-agent-teams doctor\n");
    for check in &checks {
        println!("{:<5} {:<19} {}", check.level, check.subject, check.detail);
    }
    if args.iter().any(|arg| arg == "--wait") {
        println!(
            "\nThis popup runs with the herdr server's environment. Leads run in your shell, so if claude, \
             the locale or XDG_STATE_HOME differ there, run `{} doctor` in a pane instead.",
            env::current_exe()
                .map_or_else(|_| "herdr-claude-code-agent-teams".into(), |exe| exe.display().to_string())
        );
        print!("\nPress Enter to close.");
        let _ = io::stdout().flush();
        let _ = io::stdin().lock().read_line(&mut String::new());
    }
    if checks.iter().any(|check| check.level == Level::Fail) { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

fn checks() -> Vec<Check> {
    let mut checks =
        vec![plugin(), herdr_version(), claude_version(), claude_integration(), real_tmux(), locale()];
    checks.extend(state_and_leads());
    checks
}

fn plugin() -> Check {
    let location = env::current_exe()
        .and_then(fs::canonicalize)
        .map_or_else(|_| "location unknown".to_owned(), |exe| exe.display().to_string());
    Check::new(Level::Ok, "plugin", format!("{} ({location})", env!("CARGO_PKG_VERSION")))
}

fn manifest_value(key: &str) -> Option<&'static str> {
    MANIFEST.lines().find_map(|line| {
        line.strip_prefix(key)?.trim_start().strip_prefix('=')?.trim().strip_prefix('"')?.strip_suffix('"')
    })
}

fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split('.').map(|part| part.parse::<u32>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

fn herdr_version() -> Check {
    let minimum = manifest_value("min_herdr_version").unwrap_or("0.0.0");
    let installed = match herdr::output("herdr", &herdr::bin(), &["--version"]) {
        Ok(text) => text.trim().trim_start_matches("herdr").trim().to_owned(),
        Err(error) => return Check::new(Level::Fail, "herdr", format!("cannot run herdr: {error:#}")),
    };
    match (parse_version(&installed), parse_version(minimum)) {
        (Some(found), Some(needed)) if found < needed => {
            Check::new(Level::Fail, "herdr", format!("{installed}; this plugin needs {minimum} or later"))
        }
        (Some(_), _) => Check::new(Level::Ok, "herdr", installed),
        (None, _) => Check::new(Level::Warn, "herdr", format!("cannot read the version from {installed:?}")),
    }
}

fn claude_version() -> Check {
    match herdr::output("claude", "claude", &["--version"]) {
        Ok(text) => Check::new(Level::Ok, "claude", text.lines().next().unwrap_or_default().trim()),
        Err(error) => Check::new(Level::Fail, "claude", format!("cannot run claude: {error:#}")),
    }
}

fn claude_integration() -> Check {
    match herdr::output("herdr", &herdr::bin(), &["integration", "status"]) {
        Ok(text) => integration_check(&text),
        Err(error) => Check::new(Level::Warn, "claude integration", format!("cannot ask herdr: {error:#}")),
    }
}

fn integration_check(status: &str) -> Check {
    let Some(line) = status.lines().find_map(|line| line.strip_prefix("claude:")) else {
        return Check::new(Level::Warn, "claude integration", "herdr did not report it");
    };
    let state = line.split(" (/").next().unwrap_or(line).trim();
    if state.starts_with("current") {
        return Check::new(Level::Ok, "claude integration", state);
    }
    let fix = if state.starts_with("needs repair") { "repair it with" } else { "run" };
    Check::new(
        Level::Warn,
        "claude integration",
        format!(
            "{state}; {fix} `herdr integration install claude` so herdr shows whether each teammate is working"
        ),
    )
}

fn real_tmux() -> Check {
    match tmux::real_tmux() {
        Some(path) => Check::new(
            Level::Ok,
            "tmux",
            format!("{} (used for tmux calls that are not from Claude Code)", path.display()),
        ),
        None => Check::new(
            Level::Ok,
            "tmux",
            "not installed (only needed when other programs in a lead's pane run tmux)",
        ),
    }
}

fn locale() -> Check {
    locale_check(|name| env::var(name).ok())
}

fn locale_check(lookup: impl Fn(&str) -> Option<String>) -> Check {
    let Some((name, value)) = ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .find_map(|name| lookup(name).filter(|value| !value.is_empty()).map(|value| (name, value)))
    else {
        return Check::new(
            Level::Warn,
            "locale",
            "LANG is not set; Claude Code may draw non-ASCII text incorrectly",
        );
    };
    let lower = value.to_ascii_lowercase();
    if lower.contains("utf-8") || lower.contains("utf8") {
        Check::new(Level::Ok, "locale", format!("{name}={value}"))
    } else {
        Check::new(
            Level::Warn,
            "locale",
            format!("{name}={value} is not UTF-8; Claude Code may draw non-ASCII text incorrectly"),
        )
    }
}

fn state_and_leads() -> Vec<Check> {
    let root = match launcher::state_root() {
        Ok(root) => root,
        Err(error) => return vec![Check::new(Level::Fail, "state", format!("{error:#}"))],
    };
    let Ok(metadata) = fs::symlink_metadata(&root) else {
        return vec![Check::new(
            Level::Ok,
            "state",
            format!("{} (created when a lead starts)", root.display()),
        )];
    };
    let path = root.display();
    let mut checks = vec![if !metadata.is_dir() || metadata.uid() != geteuid().as_raw() {
        Check::new(Level::Fail, "state", format!("{path} must be a directory owned by you"))
    } else if root.as_os_str().as_bytes().contains(&b',') {
        Check::new(
            Level::Fail,
            "state",
            format!("{path} contains a comma, which Claude Code cannot pass back as a tmux socket"),
        )
    } else if metadata.permissions().mode() & 0o777 != 0o700 {
        Check::new(
            Level::Warn,
            "state",
            format!("{path} is readable by others until the next lead starts and sets mode 700"),
        )
    } else {
        Check::new(Level::Ok, "state", path.to_string())
    }];

    let servers: Vec<PathBuf> = match env::var_os("HERDR_SOCKET_PATH") {
        Some(socket) => vec![root.join(launcher::session_key(Some(&socket)))],
        None => fs::read_dir(&root).into_iter().flatten().flatten().map(|server| server.path()).collect(),
    };
    let mut leads: Vec<_> = servers
        .iter()
        .flat_map(|server| fs::read_dir(server).into_iter().flatten().flatten())
        .filter_map(|lead| {
            let name = lead.file_name().into_string().ok()?;
            let pid: i32 = name.rsplit_once('-')?.1.parse().ok()?;
            launcher::is_running(pid).then(|| (pid, lead.path()))
        })
        .collect();
    leads.sort();
    if leads.is_empty() {
        checks.push(Check::new(Level::Ok, "leads", "none running"));
    }
    checks.extend(leads.iter().map(|(pid, dir)| lead_check(*pid, dir)));
    checks
}

fn lead_check(pid: i32, dir: &Path) -> Check {
    let state = State::new(dir.to_owned());
    let Ok(idmap) = state.with_idmap(|idmap| idmap.clone()) else {
        return Check::new(
            Level::Warn,
            "lead",
            format!("pid {pid}: cannot read {}", dir.join("idmap.json").display()),
        );
    };
    let calls: Vec<Value> = fs::read_to_string(dir.join("calls.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let summary = LeadSummary {
        pid,
        pane: idmap.leader().unwrap_or("?").to_owned(),
        teammates: idmap.owned_panes().len().saturating_sub(1),
        routes: idmap.routes.iter().map(|(name, dir)| format!("{name} -> {}", dir.display())).collect(),
        failed: calls.iter().filter(|call| call["exit"].as_u64().is_some_and(|exit| exit != 0)).count(),
        unhandled: calls
            .iter()
            .filter(|call| call["message"] == "unhandled")
            .filter_map(|call| call["verb"].as_str().map(str::to_owned))
            .collect(),
        log: dir.join("calls.log").display().to_string(),
    };
    summary.check()
}

struct LeadSummary {
    pid: i32,
    pane: String,
    teammates: usize,
    routes: Vec<String>,
    failed: usize,
    unhandled: Vec<String>,
    log: String,
}

impl LeadSummary {
    fn check(&self) -> Check {
        let mut parts =
            vec![format!("{} (pid {}): {} teammate pane(s)", self.pane, self.pid, self.teammates)];
        if !self.routes.is_empty() {
            parts.push(format!("routes {}", self.routes.join(", ")));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed tmux call(s)", self.failed));
        }
        let detail = parts.join(", ");
        if self.unhandled.is_empty() {
            return Check::new(Level::Ok, "lead", detail);
        }
        let mut verbs = self.unhandled.clone();
        verbs.sort();
        verbs.dedup();
        Check::new(
            Level::Warn,
            "lead",
            format!(
                "{detail}; Claude Code used tmux commands this plugin does not handle ({}), see {}",
                verbs.join(", "),
                self.log
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn manifest_value_reads_the_minimum_herdr_version() {
        assert!(manifest_value("min_herdr_version").and_then(parse_version).is_some());
    }

    #[test]
    fn parse_version_compares_numerically() {
        assert!(parse_version("0.10.0") > parse_version("0.9.5"));
        assert_eq!(parse_version("0.9"), None);
        assert_eq!(parse_version("0.9.5-beta"), None);
    }

    #[test]
    fn integration_check_passes_only_when_the_claude_hook_is_installed() {
        let status = "pi: not installed (/h/.pi/x.ts)\nclaude: outdated (v7 < v11) (/h/.claude/hooks/x.sh)\n";
        let check = integration_check(status);
        assert_eq!(check.level, Level::Warn);
        assert!(
            check.detail.starts_with("outdated (v7 < v11); run `herdr integration install claude`"),
            "{}",
            check.detail
        );

        let check = integration_check("claude: current (v11) (/h/.claude/hooks/x.sh)\n");
        assert_eq!(check, Check::new(Level::Ok, "claude integration", "current (v11)"));

        let check = integration_check("claude: needs repair (v11) (/h/.claude/hooks/x.sh)\n");
        assert_eq!(check.level, Level::Warn);
        assert!(check.detail.starts_with("needs repair (v11); repair it with"), "{}", check.detail);

        assert_eq!(integration_check("claude: not installed (/h/.claude/hooks/x.sh)\n").level, Level::Warn);
        assert_eq!(integration_check("codex: current (v3)\n").level, Level::Warn);
    }

    #[test]
    fn locale_check_follows_the_posix_precedence() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            let map: HashMap<&str, &str> = pairs.iter().copied().collect();
            move |name: &str| map.get(name).map(|value| (*value).to_owned())
        };

        assert_eq!(locale_check(env(&[("LANG", "ja_JP.UTF-8")])).level, Level::Ok);
        assert_eq!(locale_check(env(&[("LC_ALL", "C"), ("LANG", "en_US.UTF-8")])).level, Level::Warn);
        assert_eq!(locale_check(env(&[("LC_ALL", ""), ("LC_CTYPE", "en_US.utf8")])).level, Level::Ok);
        assert_eq!(locale_check(env(&[])).level, Level::Warn);
    }

    #[test]
    fn a_lead_that_hit_an_unhandled_tmux_command_is_a_warning() {
        let mut summary = LeadSummary {
            pid: 42,
            pane: "w1:p1".into(),
            teammates: 2,
            routes: vec!["api -> /src/api".into()],
            failed: 1,
            unhandled: Vec::new(),
            log: "/state/calls.log".into(),
        };
        assert_eq!(
            summary.check(),
            Check::new(
                Level::Ok,
                "lead",
                "w1:p1 (pid 42): 2 teammate pane(s), routes api -> /src/api, 1 failed tmux call(s)"
            )
        );

        summary.unhandled = vec!["new-window".into(), "break-pane".into(), "new-window".into()];
        let check = summary.check();
        assert_eq!(check.level, Level::Warn);
        assert!(check.detail.ends_with("(break-pane, new-window), see /state/calls.log"), "{}", check.detail);
    }
}
