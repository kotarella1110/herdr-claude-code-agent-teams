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
    assert_eq!(value(&lines, "args"), r#"--settings {"teammateMode":"tmux"} --model sonnet"#);
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
