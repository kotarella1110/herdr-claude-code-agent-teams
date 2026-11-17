mod support;

use std::path::PathBuf;

use support::{Sandbox, mode, stdout};

fn value<'a>(lines: &'a str, key: &str) -> &'a str {
    lines.lines().find_map(|line| line.strip_prefix(&format!("{key}="))).unwrap()
}

#[test]
fn outside_herdr_it_runs_plain_claude() {
    let sandbox = Sandbox::new();

    let output = sandbox.launcher(&[]);

    let lines = stdout(&output);
    assert_eq!(value(&lines, "args"), "--model sonnet");
    assert_eq!(value(&lines, "TMUX"), "");
    assert_eq!(value(&lines, "TEAMS"), "");
}

#[test]
fn inside_herdr_it_starts_claude_as_a_team_lead_behind_the_tmux_shim() {
    let sandbox = Sandbox::new();
    let state_home = sandbox.root.join("xdg-state");

    let output = sandbox.launcher(&[
        ("HERDR_ENV", "1"),
        ("HERDR_PANE_ID", "w1:p1"),
        ("HERDR_SOCKET_PATH", "/tmp/herdr.sock"),
        ("XDG_STATE_HOME", state_home.to_str().unwrap()),
    ]);

    let lines = stdout(&output);
    let state_dir = PathBuf::from(value(&lines, "STATE"));
    assert!(state_dir.starts_with(state_home.join("herdr-claude-code-agent-teams")), "{lines}");
    assert!(state_dir.file_name().unwrap().to_str().unwrap().starts_with("w1_p1-"), "{lines}");
    assert_eq!(mode(&state_dir), 0o700);
    let args = value(&lines, "args");
    assert!(args.starts_with(r#"--settings {"teammateMode":"tmux"} --append-system-prompt "#), "{args}");
    assert!(args.ends_with(" --model sonnet"), "{args}");
    assert!(value(&lines, "TMUX").starts_with(&format!("{}/tmux.sock,", state_dir.display())), "{lines}");
    assert_eq!(value(&lines, "TMUX_PANE"), "%0");
    assert_eq!(value(&lines, "TEAMS"), "1");
    assert_eq!(value(&lines, "TRUECOLOR"), "1");
    assert_eq!(PathBuf::from(value(&lines, "tmux")), state_dir.join("bin/tmux"));

    let idmap: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(state_dir.join("idmap.json")).unwrap()).unwrap();
    assert_eq!(idmap["map"]["%0"], "w1:p1");
    assert_eq!(idmap["map"]["@0"], "w1:t1");
}

#[test]
fn a_relative_state_home_is_ignored() {
    let sandbox = Sandbox::new();

    let output =
        sandbox.launcher(&[("HERDR_ENV", "1"), ("HERDR_PANE_ID", "w1:p1"), ("XDG_STATE_HOME", "relative")]);

    let lines = stdout(&output);
    assert!(PathBuf::from(value(&lines, "STATE")).starts_with(sandbox.root.join(".local/state")), "{lines}");
}

#[test]
fn a_state_path_with_a_comma_is_refused() {
    let sandbox = Sandbox::new();
    let state_home = sandbox.root.join("a,b");

    let output = sandbox.launcher(&[
        ("HERDR_ENV", "1"),
        ("HERDR_PANE_ID", "w1:p1"),
        ("XDG_STATE_HOME", state_home.to_str().unwrap()),
    ]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("contains a comma"));
}

#[test]
fn the_lead_learns_to_spawn_named_teammates_and_where_they_start() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(sandbox.root.join("work/api/.git")).unwrap();
    let envs = [("HERDR_ENV", "1"), ("HERDR_PANE_ID", "w1:p1")];

    let hinted = stdout(&sandbox.launcher(&envs));
    let own = stdout(&sandbox.launcher_with_args(&["--append-system-prompt", "mine"], &envs));
    let own_file = stdout(&sandbox.launcher_with_args(&["--append-system-prompt-file", "/tmp/mine"], &envs));

    let args = value(&hinted, "args");
    assert!(args.contains("--append-system-prompt You can lead an agent team"), "{args}");
    assert!(args.contains("spawn each teammate by calling the Agent tool with a name."), "{args}");
    assert!(args.contains("stop it to close its pane."), "{args}");
    assert!(args.contains(" route <teammate-name> <directory>`"), "{args}");
    assert!(args.contains("without that step: api."), "{args}");
    assert_eq!(value(&own, "args"), r#"--settings {"teammateMode":"tmux"} --append-system-prompt mine"#);
    assert_eq!(
        value(&own_file, "args"),
        r#"--settings {"teammateMode":"tmux"} --append-system-prompt-file /tmp/mine"#
    );
}

#[test]
fn the_lead_still_learns_to_name_teammates_when_its_directory_is_gone() {
    let sandbox = Sandbox::new();

    let lines =
        stdout(&sandbox.launcher_in_a_deleted_directory(&[("HERDR_ENV", "1"), ("HERDR_PANE_ID", "w1:p1")]));

    let args = value(&lines, "args");
    assert!(args.contains("--append-system-prompt You can lead an agent team"), "{lines}");
    assert!(!args.contains(" route <teammate-name>"), "{args}");
}
