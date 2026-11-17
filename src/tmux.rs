use std::collections::HashMap;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};

use crate::herdr::{self, HerdrError};
use crate::route;
use crate::shell;
use crate::state::{self, State};

const VERSION: &str = "tmux 3.5a";
const GLOBAL_FLAGS: &str = "2c:CDdf:hlL:NqS:T:uUvV";
const READ_ONLY_VERBS: [&str; 5] =
    ["show-options", "show-environment", "display-message", "list-panes", "list-sessions"];
const NO_OP_VERBS: [&str; 4] = ["set-option", "select-layout", "resize-pane", "refresh-client"];

#[derive(Debug, Default, PartialEq, Eq)]
struct Flags(HashMap<char, Option<String>>);

impl Flags {
    fn value(&self, flag: char) -> Option<&str> {
        self.0.get(&flag)?.as_deref()
    }

    fn has(&self, flag: char) -> bool {
        self.0.contains_key(&flag)
    }
}

type Fields = HashMap<&'static str, String>;

#[derive(Debug, PartialEq, Eq)]
enum Target<'a> {
    Pane(&'a str),
    Window(&'a str),
}

pub(crate) fn run(argv: &[String]) -> ExitCode {
    let state = match State::from_env() {
        Ok(state) => state,
        Err(error) => {
            eprintln!("{error:#}");
            return ExitCode::FAILURE;
        }
    };
    let Ok((global, rest)) = parse(argv, GLOBAL_FLAGS) else {
        return delegate_to_real_tmux(argv, &state, "");
    };
    if global.has('V') {
        println!("{VERSION}");
        return ExitCode::SUCCESS;
    }
    let Some((verb, verb_args)) = rest.split_first() else {
        return delegate_to_real_tmux(argv, &state, "");
    };
    let verb = canonical_verb(verb);
    let from_claude_code = global.value('S').is_some_and(|socket| Path::new(socket) == state.socket());
    if !from_claude_code && !READ_ONLY_VERBS.contains(&verb) && !NO_OP_VERBS.contains(&verb) {
        return delegate_to_real_tmux(argv, &state, verb);
    }

    let outcome = match verb {
        "split-window" => split_window(&state, verb_args),
        "respawn-pane" => respawn_pane(&state, verb_args),
        "kill-pane" => kill_pane(&state, verb_args),
        "select-pane" => select_pane(&state, verb_args),
        "list-panes" => list_panes(&state, verb_args),
        "display-message" => display_message(&state, verb_args),
        "show-options" => show_options(verb_args),
        "capture-pane" => capture_pane(&state, verb_args),
        "send-keys" => send_keys(&state, verb_args),
        "list-sessions" => list_sessions(verb_args),
        "show-environment" | "has-session" => Ok(String::new()),
        verb if NO_OP_VERBS.contains(&verb) => Ok(String::new()),
        _ => {
            state.log(verb, None, 0, "unhandled");
            return ExitCode::SUCCESS;
        }
    };
    let target = target_of(verb_args);
    match outcome {
        Ok(output) => {
            if !output.is_empty() {
                println!("{output}");
            }
            state.log(verb, target, 0, "");
            ExitCode::SUCCESS
        }
        Err(error) => {
            state.log(verb, target, 1, &loggable(&error));
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

fn loggable(error: &anyhow::Error) -> String {
    match error.downcast_ref::<HerdrError>() {
        Some(herdr_error) => {
            let subcommand: Vec<&str> = herdr_error.command.split(' ').take(3).collect();
            format!("{} failed", subcommand.join(" "))
        }
        None => format!("{error:#}"),
    }
}

fn delegate_to_real_tmux(argv: &[String], state: &State, verb: &str) -> ExitCode {
    state.log(verb, None, 0, "not from Claude Code; delegated to tmux");
    let Some(tmux) = real_tmux() else {
        eprintln!(
            "tmux: only Claude Code agent teams can use tmux in this pane, and no real tmux is installed"
        );
        return ExitCode::FAILURE;
    };
    let error = Command::new(tmux)
        .args(argv)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove(state::ENV)
        .exec();
    eprintln!("tmux: {error}");
    ExitCode::FAILURE
}

fn real_tmux() -> Option<PathBuf> {
    let shim = env::current_exe().and_then(fs::canonicalize).ok()?;
    env::split_paths(&env::var_os("PATH")?).map(|dir| dir.join("tmux")).find(|candidate| {
        fs::metadata(candidate)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
            && fs::canonicalize(candidate).is_ok_and(|real| real != shim)
    })
}

fn target_of(args: &[String]) -> Option<&str> {
    args.iter().enumerate().find_map(|(index, arg)| match arg.as_str() {
        "-t" => args.get(index + 1).map(String::as_str),
        attached => attached.strip_prefix("-t").filter(|value| !value.is_empty()),
    })
}

fn canonical_verb(verb: &str) -> &str {
    match verb {
        "show" | "showw" | "show-window-options" => "show-options",
        "showenv" => "show-environment",
        "display" => "display-message",
        "set" | "setw" | "set-window-option" => "set-option",
        "splitw" => "split-window",
        "killp" => "kill-pane",
        "selectp" => "select-pane",
        "respawnp" => "respawn-pane",
        "resizep" => "resize-pane",
        "selectl" => "select-layout",
        "lsp" => "list-panes",
        "has" => "has-session",
        "ls" => "list-sessions",
        "send" => "send-keys",
        "capturep" => "capture-pane",
        "refresh" => "refresh-client",
        other => other,
    }
}

fn parse(args: &[String], spec: &str) -> Result<(Flags, Vec<String>)> {
    let mut flags = Flags::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        let Some(letters) = arg.strip_prefix('-').filter(|letters| !letters.is_empty()) else {
            let mut positional = vec![arg.clone()];
            positional.extend(args.cloned());
            return Ok((flags, positional));
        };
        for (index, letter) in letters.char_indices() {
            let Some(position) = spec.find(letter).filter(|_| letter != ':') else {
                bail!("unknown flag -{letter}");
            };
            if spec[position + letter.len_utf8()..].starts_with(':') {
                let attached = &letters[index + letter.len_utf8()..];
                let value = if attached.is_empty() {
                    args.next().cloned().ok_or_else(|| anyhow!("-{letter} expects an argument"))?
                } else {
                    attached.to_owned()
                };
                flags.0.insert(letter, Some(value));
                break;
            }
            flags.0.insert(letter, None);
        }
    }
    Ok((flags, args.cloned().collect()))
}

fn parse_target(raw: &str) -> Target<'_> {
    let local = raw.rsplit_once(':').map_or(raw, |(_, rest)| rest);
    match local.rsplit_once('.') {
        Some((_, pane)) if !pane.is_empty() => Target::Pane(pane),
        _ if local.starts_with('%') => Target::Pane(local),
        _ => Target::Window(local),
    }
}

fn expand(format: &str, fields: &Fields) -> String {
    let mut output = String::new();
    let mut rest = format;
    while let Some(start) = rest.find("#{") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) if end > 0 && after[..end].bytes().all(|b| b.is_ascii_lowercase() || b == b'_') => {
                output.push_str(fields.get(&after[..end]).map_or("", String::as_str));
                rest = &after[end + 1..];
            }
            _ => {
                output.push_str("#{");
                rest = after;
            }
        }
    }
    output.push_str(rest);
    output
}

fn split_ratio(flags: &Flags) -> Option<String> {
    let percent: f64 = match (flags.value('l'), flags.value('p')) {
        (Some(size), _) => size.strip_suffix('%')?.parse().ok()?,
        (None, Some(percent)) => percent.parse().ok()?,
        (None, None) => return None,
    };
    (0.0..=100.0).contains(&percent).then(|| format!("{:.2}", 1.0 - percent / 100.0))
}

fn option_value(name: &str) -> &'static str {
    match name {
        "mouse" | "focus-events" => "on",
        "base-index" | "pane-base-index" => "0",
        "prefix" => "C-b",
        _ => "",
    }
}

fn herdr_key(key: &str) -> Option<&'static str> {
    match key {
        "Enter" | "C-m" => Some("enter"),
        "C-c" => Some("ctrl+c"),
        "Escape" => Some("esc"),
        "Tab" => Some("tab"),
        _ => None,
    }
}

fn session_fields() -> Fields {
    HashMap::from([("session_name", "herdr".to_owned())])
}

fn resolve_pane(state: &State, target: Option<&str>, allow_window: bool) -> Result<herdr::Pane> {
    let Some(raw) = target else {
        return locate(state, "%0")?.ok_or_else(|| anyhow!("no team lead"));
    };
    let tmux_id = state.with_idmap(|idmap| match parse_target(raw) {
        Target::Pane(pane) => Some(pane.to_owned()),
        Target::Window(window) if allow_window => {
            let leader_tab = idmap.map.get("@0");
            let tab = if window.is_empty() { leader_tab } else { idmap.map.get(window) };
            (tab.is_some() && tab == leader_tab).then(|| "%0".to_owned())
        }
        Target::Window(_) => None,
    })?;
    match tmux_id {
        Some(tmux_id) => locate(state, &tmux_id)?,
        None => None,
    }
    .ok_or_else(|| anyhow!("can't find pane: {raw}"))
}

fn locate(state: &State, tmux_id: &str) -> Result<Option<herdr::Pane>> {
    let (pane_id, terminal_id) =
        state.with_idmap(|idmap| (idmap.map.get(tmux_id).cloned(), idmap.terminals.get(tmux_id).cloned()))?;
    let Some(pane_id) = pane_id else {
        return Ok(None);
    };
    let found = match (&terminal_id, herdr::pane(&pane_id)) {
        (None, fetched) => fetched?,
        (Some(terminal), Ok(pane)) if pane.terminal_id.as_ref() == Some(terminal) => pane,
        (Some(terminal), _) => {
            match herdr::all_panes()?.into_iter().find(|pane| pane.terminal_id.as_ref() == Some(terminal)) {
                Some(pane) => pane,
                None => return Ok(None),
            }
        }
    };
    if found.id != pane_id {
        state.with_idmap(|idmap| idmap.relocate(tmux_id, &found.id))?;
        state.log("relocate", Some(tmux_id), 0, &format!("{pane_id} -> {}", found.id));
    }
    Ok(Some(found))
}

fn resolve_team_pane(state: &State, target: Option<&str>) -> Result<herdr::Pane> {
    let pane = resolve_pane(state, target, false)?;
    if state.with_idmap(|idmap| idmap.leader() == Some(pane.id.as_str()))? {
        bail!("refusing to act on the team lead pane");
    }
    Ok(pane)
}

fn resolve_tab(state: &State, target: Option<&str>) -> Result<String> {
    let raw = target.unwrap_or("");
    match parse_target(raw) {
        Target::Pane(_) => Ok(resolve_pane(state, target, false)?.tab_id),
        Target::Window(window) => {
            let window = if window.is_empty() { "@0" } else { window };
            state
                .with_idmap(|idmap| idmap.map.get(window).cloned())?
                .ok_or_else(|| anyhow!("can't find window: {raw}"))
        }
    }
}

fn pane_fields(state: &State, pane: herdr::Pane) -> Result<Fields> {
    let (pane_id, window_id, title) = state.with_idmap(|idmap| {
        (
            idmap.pane_alias(&pane.id, pane.terminal_id.as_deref()),
            idmap.tmux_id_for(&pane.tab_id, '@'),
            idmap.titles.get(&pane.id).cloned().unwrap_or_default(),
        )
    })?;
    let mut fields = session_fields();
    fields.extend([
        ("pane_id", pane_id),
        ("window_id", window_id),
        ("pane_current_path", pane.cwd.unwrap_or_default()),
        ("pane_title", title),
        ("session_id", "$0".to_owned()),
        ("window_index", "0".to_owned()),
        ("pane_index", "0".to_owned()),
        ("client_termtype", env::var("TERM").unwrap_or_else(|_| "xterm-256color".into())),
        ("client_control_mode", "0".to_owned()),
        ("pane_active", if pane.focused { "1" } else { "0" }.to_owned()),
    ]);
    Ok(fields)
}

fn split_window(state: &State, args: &[String]) -> Result<String> {
    let (flags, positional) = parse(args, "bc:de:fF:hIl:p:Pt:vZ")?;
    let source = resolve_pane(state, flags.value('t'), true)?.id;
    let direction = if flags.has('h') { "right" } else { "down" };
    let mut command: Vec<String> =
        ["pane", "split", "--pane", &source, "--direction", direction, "--no-focus"]
            .map(str::to_owned)
            .into();
    if let Some(ratio) = split_ratio(&flags) {
        command.extend(["--ratio".to_owned(), ratio]);
    }
    if let Some(cwd) = flags.value('c') {
        command.extend(["--cwd".to_owned(), cwd.to_owned()]);
    }
    let new_pane = herdr::split(&command)?;
    state.with_idmap(|idmap| idmap.pane_alias(&new_pane.id, new_pane.terminal_id.as_deref()))?;
    let new_pane = new_pane.id;

    let finish = || -> Result<String> {
        if !positional.is_empty() && positional != ["cat"] {
            run_in_pane(state, &new_pane, &command_line(&positional))?;
        }
        if flags.has('P') {
            return Ok(expand(
                flags.value('F').unwrap_or("#{pane_id}"),
                &pane_fields(state, herdr::pane(&new_pane)?)?,
            ));
        }
        Ok(String::new())
    };
    finish().inspect_err(|_| {
        let _ = herdr::call(&["pane", "close", &new_pane]);
        let _ = state.with_idmap(|idmap| idmap.forget(&new_pane));
    })
}

fn command_line(command: &[String]) -> String {
    if let [single] = command { single.clone() } else { shell::join(command) }
}

fn run_in_pane(state: &State, herdr_pane: &str, line: &str) -> Result<()> {
    let millis = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis());
    let script = state.dir().join(format!("run-{}-{millis}.sh", herdr_pane.replace(':', "_")));
    let body = format!(
        "rm -f -- \"$0\"\nsh -c {}\nstatus=$?\nif [ \"$status\" -eq 0 ]; then {} pane close {}; fi\nexit \"$status\"\n",
        shell::quote(line),
        shell::quote(&herdr::bin()),
        shell::quote(herdr_pane),
    );
    state::write_private(&script, body.as_bytes())?;
    herdr::call(&["pane", "run", herdr_pane, &format!("sh {}", shell::quote(&script.to_string_lossy()))])
        .inspect_err(|_| {
            let _ = fs::remove_file(&script);
        })?;
    Ok(())
}

fn respawn_pane(state: &State, args: &[String]) -> Result<String> {
    let (flags, positional) = parse(args, "c:e:kt:")?;
    let herdr_pane = resolve_team_pane(state, flags.value('t'))?.id;
    let line = command_line(&positional);
    let registered = state.with_idmap(|idmap| idmap.routes.clone())?;
    let line = match route::reroute(&line, &registered) {
        Some((rerouted, route)) => {
            state.log(
                "route",
                flags.value('t'),
                0,
                &format!("{} -> {} ({})", route.name, route.dir.display(), route.rule),
            );
            rerouted
        }
        None => line,
    };
    run_in_pane(state, &herdr_pane, &line)?;
    Ok(String::new())
}

fn kill_pane(state: &State, args: &[String]) -> Result<String> {
    let (flags, _) = parse(args, "t:")?;
    let herdr_pane = match resolve_team_pane(state, flags.value('t')) {
        Ok(pane) => pane.id,
        Err(error) => {
            if let Some(Target::Pane(tmux_id)) = flags.value('t').map(parse_target)
                && tmux_id != "%0"
            {
                state.with_idmap(|idmap| idmap.forget_tmux(tmux_id))?;
            }
            return Err(error);
        }
    };
    let closed = herdr::call(&["pane", "close", &herdr_pane]);
    state.with_idmap(|idmap| idmap.forget(&herdr_pane))?;
    closed.map(|_| String::new())
}

fn select_pane(state: &State, args: &[String]) -> Result<String> {
    let (flags, _) = parse(args, "DdegLlMmP:RT:t:UZ")?;
    if let Some(title) = flags.value('T') {
        let herdr_pane = resolve_pane(state, flags.value('t'), true)?.id;
        herdr::call(&["pane", "rename", &herdr_pane, title])?;
        state.with_idmap(|idmap| idmap.titles.insert(herdr_pane, title.to_owned()))?;
    }
    Ok(String::new())
}

fn list_panes(state: &State, args: &[String]) -> Result<String> {
    let (flags, _) = parse(args, "aF:f:O:rst:")?;
    let tab = resolve_tab(state, flags.value('t'))?;
    let workspace = tab.split(':').next().unwrap_or(&tab);
    let panes = herdr::panes_in_workspace(workspace)?;
    let owned: Vec<bool> = state.with_idmap(|idmap| {
        let owned_panes = idmap.owned_panes();
        panes
            .iter()
            .map(|pane| {
                owned_panes.contains(pane.id.as_str())
                    || pane.terminal_id.as_deref().is_some_and(|terminal| idmap.owns_terminal(terminal))
            })
            .collect()
    })?;
    let format = flags.value('F').unwrap_or("#{pane_id}");
    panes
        .into_iter()
        .zip(owned)
        .filter(|(pane, owned)| *owned && pane.tab_id == tab)
        .map(|(pane, _)| Ok(expand(format, &pane_fields(state, herdr::pane(&pane.id)?)?)))
        .collect::<Result<Vec<_>>>()
        .map(|lines| lines.join("\n"))
}

fn display_message(state: &State, args: &[String]) -> Result<String> {
    let (flags, positional) = parse(args, "aCc:d:lINpt:F:v")?;
    if !flags.has('p') {
        return Ok(String::new());
    }
    let format = flags.value('F').or(positional.first().map(String::as_str)).unwrap_or("");
    let fields = match flags.value('t').map(parse_target) {
        Some(Target::Window(window)) => {
            resolve_tab(state, flags.value('t'))?;
            let mut fields = pane_fields(state, resolve_pane(state, None, false)?)?;
            if !window.is_empty() {
                fields.insert("window_id", window.to_owned());
            }
            fields
        }
        _ => pane_fields(state, resolve_pane(state, flags.value('t'), false)?)?,
    };
    Ok(expand(format, &fields))
}

fn show_options(args: &[String]) -> Result<String> {
    let (flags, positional) = parse(args, "AgHpqst:vw")?;
    let Some(name) = positional.first() else {
        return Ok(String::new());
    };
    let value = option_value(name);
    Ok(if flags.has('v') || value.is_empty() { value.to_owned() } else { format!("{name} {value}") })
}

fn capture_pane(state: &State, args: &[String]) -> Result<String> {
    let (flags, _) = parse(args, "ab:CeE:JMNpPqS:Tt:")?;
    let herdr_pane = resolve_pane(state, flags.value('t'), false)?.id;
    let response = herdr::call(&["pane", "read", &herdr_pane, "--source", "visible"])?;
    if !flags.has('p') {
        return Ok(String::new());
    }
    Ok(response.pointer("/result/text").and_then(|text| text.as_str()).unwrap_or_default().to_owned())
}

fn send_keys(state: &State, args: &[String]) -> Result<String> {
    let (flags, positional) = parse(args, "c:FHKlMN:Rt:X")?;
    let herdr_pane = resolve_team_pane(state, flags.value('t'))?.id;
    if flags.has('l') {
        herdr::call(&["pane", "send-text", &herdr_pane, &positional.join(" ")])?;
        return Ok(String::new());
    }
    for key in &positional {
        match herdr_key(key) {
            Some(herdr_key) => herdr::call(&["pane", "send-keys", &herdr_pane, herdr_key])?,
            None => herdr::call(&["pane", "send-text", &herdr_pane, key])?,
        };
    }
    Ok(String::new())
}

fn list_sessions(args: &[String]) -> Result<String> {
    let (flags, _) = parse(args, "F:f:O:r")?;
    Ok(expand(flags.value('F').unwrap_or("#{session_name}"), &session_fields()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|&arg| arg.to_owned()).collect()
    }

    #[test]
    fn parse_reads_flags_with_separate_and_attached_values() {
        let (flags, positional) =
            parse(&strings(&["-dh", "-l", "70%", "-t%0", "-PF", "#{pane_id}"]), "bc:de:fF:hIl:p:Pt:vZ")
                .unwrap();

        assert!(flags.has('d'));
        assert!(flags.has('h'));
        assert_eq!(flags.value('l'), Some("70%"));
        assert_eq!(flags.value('t'), Some("%0"));
        assert!(flags.has('P'));
        assert_eq!(flags.value('F'), Some("#{pane_id}"));
        assert_eq!(positional, Vec::<String>::new());
    }

    #[test]
    fn parse_uses_the_verb_specific_flags() {
        let (flags, positional) = parse(&strings(&["-l", "-t", "%1", "hello"]), "c:FHKlMN:Rt:X").unwrap();

        assert!(flags.has('l'));
        assert_eq!(flags.value('t'), Some("%1"));
        assert_eq!(positional, strings(&["hello"]));
    }

    #[test]
    fn parse_stops_at_the_first_positional_or_double_dash() {
        let (flags, positional) =
            parse(&strings(&["-S", "/tmp/sock", "split-window", "-h"]), GLOBAL_FLAGS).unwrap();
        assert_eq!(flags.value('S'), Some("/tmp/sock"));
        assert_eq!(positional, strings(&["split-window", "-h"]));

        let (_, positional) = parse(&strings(&["-t", "%1", "--", "-not-a-flag"]), "t:").unwrap();
        assert_eq!(positional, strings(&["-not-a-flag"]));
    }

    #[test]
    fn parse_rejects_unknown_flags_and_missing_values() {
        assert_eq!(parse(&strings(&["-z"]), "t:").unwrap_err().to_string(), "unknown flag -z");
        assert_eq!(parse(&strings(&["-:"]), "t:").unwrap_err().to_string(), "unknown flag -:");
        assert_eq!(parse(&strings(&["-t"]), "t:").unwrap_err().to_string(), "-t expects an argument");
    }

    #[test]
    fn parse_target_reads_session_window_and_pane_forms() {
        assert_eq!(parse_target("%1"), Target::Pane("%1"));
        assert_eq!(parse_target("@0"), Target::Window("@0"));
        assert_eq!(parse_target("herdr:@0"), Target::Window("@0"));
        assert_eq!(parse_target("herdr:@0.%2"), Target::Pane("%2"));
        assert_eq!(parse_target("herdr:"), Target::Window(""));
    }

    #[test]
    fn expand_fills_known_names_and_blanks_unknown_ones() {
        let fields: Fields = HashMap::from([
            ("session_name", "herdr".into()),
            ("window_id", "@0".into()),
            ("pane_id", "%0".into()),
        ]);

        assert_eq!(expand("#{session_name}:#{window_id}.#{pane_id}", &fields), "herdr:@0.%0");
        assert_eq!(expand("#{pane_tty}|", &fields), "|");
    }

    #[test]
    fn expand_keeps_text_that_is_not_a_format_name() {
        assert_eq!(expand("#{} #{Upper} #{open", &Fields::new()), "#{} #{Upper} #{open");
    }

    #[test]
    fn split_ratio_converts_the_new_pane_share_to_the_source_share() {
        let (flags, _) = parse(&strings(&["-l", "70%"]), "l:p:").unwrap();
        assert_eq!(split_ratio(&flags).as_deref(), Some("0.30"));

        let (flags, _) = parse(&strings(&["-p", "25"]), "l:p:").unwrap();
        assert_eq!(split_ratio(&flags).as_deref(), Some("0.75"));

        let (flags, _) = parse(&strings(&["-l", "40"]), "l:p:").unwrap();
        assert_eq!(split_ratio(&flags), None);

        let (flags, _) = parse(&strings(&["-l", "150%"]), "l:p:").unwrap();
        assert_eq!(split_ratio(&flags), None);
    }

    #[test]
    fn canonical_verb_expands_aliases() {
        assert_eq!(canonical_verb("splitw"), "split-window");
        assert_eq!(canonical_verb("display"), "display-message");
        assert_eq!(canonical_verb("new-window"), "new-window");
    }

    #[test]
    fn show_options_answers_values_like_tmux() {
        assert_eq!(show_options(&strings(&["-Av", "mouse"])).unwrap(), "on");
        assert_eq!(show_options(&strings(&["-g", "prefix"])).unwrap(), "prefix C-b");
        assert_eq!(show_options(&strings(&["-gv", "status"])).unwrap(), "");
    }

    #[test]
    fn herdr_key_maps_named_keys_and_leaves_text_alone() {
        assert_eq!(herdr_key("C-m"), Some("enter"));
        assert_eq!(herdr_key("Escape"), Some("esc"));
        assert_eq!(herdr_key("hello"), None);
    }
}
