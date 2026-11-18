mod support;

use std::fs;

use serde_json::json;
use support::{LEADER_AND_TEAMMATE, LEADER_AND_TRACKED_TEAMMATE, LEADER_ONLY, Sandbox, mode, stderr, stdout};

#[test]
fn version_answers_like_tmux() {
    let sandbox = Sandbox::new();

    let output = sandbox.tmux_without_socket(&["-V"]);

    assert!(output.status.success());
    assert_eq!(stdout(&output), "tmux 3.5a");
}

#[test]
fn display_message_expands_the_leader_location() {
    let sandbox = Sandbox::with_idmap(LEADER_ONLY);

    let output = sandbox.tmux_without_socket(&[
        "display-message",
        "-p",
        "-t",
        "%0",
        "#{session_name}:#{window_id}.#{pane_id}",
    ]);

    assert_eq!(stdout(&output), "herdr:@0.%0");
}

#[test]
fn display_message_rejects_unknown_windows() {
    let sandbox = Sandbox::with_idmap(LEADER_ONLY);

    let output = sandbox.tmux(&["display-message", "-p", "-t", "@7", "#{pane_id}"]);

    assert!(!output.status.success());
    assert_eq!(stderr(&output), "can't find window: @7");
}

#[test]
fn split_window_in_claude_codes_form_registers_each_new_pane() {
    let sandbox = Sandbox::with_idmap(LEADER_ONLY);
    let split = ["split-window", "-d", "-t", "%0", "-h", "-l", "70%", "-P", "-F", "#{pane_id}", "--", "cat"];

    let first = sandbox.tmux(&split);
    let second =
        sandbox.tmux(&["split-window", "-d", "-t", "%1", "-v", "-P", "-F", "#{pane_id}", "--", "cat"]);

    assert_eq!(stdout(&first), "%1");
    assert_eq!(stdout(&second), "%2");
    let calls = sandbox.herdr_calls();
    assert!(calls.contains(&"pane split --pane w1:p1 --direction right --no-focus --ratio 0.30".to_owned()));
    assert!(calls.contains(&"pane split --pane w1:p2 --direction down --no-focus".to_owned()));
    assert!(!calls.iter().any(|call| call.starts_with("pane run")));
    assert_eq!(sandbox.idmap()["map"]["%2"], json!("w1:p3"));
}

#[test]
fn respawn_pane_runs_a_private_self_deleting_script_that_closes_the_pane_on_success() {
    let sandbox = Sandbox::with_idmap(LEADER_AND_TEAMMATE);

    let output = sandbox.tmux(&["respawn-pane", "-k", "-t", "%1", "--", "cd /work && env SECRET=1 claude"]);

    assert!(output.status.success(), "{output:?}");
    let run = sandbox.herdr_calls().into_iter().find(|call| call.starts_with("pane run w1:p2 ")).unwrap();
    let script = run.trim_start_matches("pane run w1:p2 sh ");
    assert_eq!(mode(script.as_ref()), 0o600);
    let body = fs::read_to_string(script).unwrap();
    assert!(body.starts_with("rm -f -- \"$0\"\nsh -c 'cd /work && env SECRET=1 claude'\n"), "{body}");
    assert!(body.contains("pane close w1:p2"), "{body}");
    assert!(!sandbox.calls_log().contains("SECRET"));
}

#[test]
fn respawn_pane_starts_a_teammate_named_after_a_nearby_repository_in_that_repository() {
    let sandbox = Sandbox::with_idmap(LEADER_AND_TEAMMATE);
    let lead = sandbox.root.join("work/lead");
    fs::create_dir_all(&lead).unwrap();
    fs::create_dir_all(sandbox.root.join("work/api/.git")).unwrap();
    let launch = |dir: &std::path::Path| {
        format!("cd {} && env A=1 claude --agent-name api --team-name t", dir.display())
    };

    let output = sandbox.tmux(&["respawn-pane", "-k", "-t", "%1", "--", &launch(&lead)]);

    assert!(output.status.success(), "{output:?}");
    let run = sandbox.herdr_calls().into_iter().find(|call| call.starts_with("pane run w1:p2 ")).unwrap();
    let body = fs::read_to_string(run.trim_start_matches("pane run w1:p2 sh ")).unwrap();
    let api = fs::canonicalize(sandbox.root.join("work/api")).unwrap();
    assert!(body.contains(&launch(&api)), "{body}");
    assert!(sandbox.calls_log().contains("api -> "), "{}", sandbox.calls_log());
}

#[test]
fn respawn_pane_starts_a_teammate_where_the_lead_registered_it() {
    let sandbox = Sandbox::with_idmap(LEADER_AND_TEAMMATE);
    let target = sandbox.root.join("anywhere/api");
    fs::create_dir_all(&target).unwrap();
    let target = fs::canonicalize(&target).unwrap();

    let registered = sandbox.route(&["api", target.to_str().unwrap()]);
    let output =
        sandbox.tmux(&["respawn-pane", "-k", "-t", "%1", "--", "cd /nowhere && env claude --agent-name api"]);

    assert!(registered.status.success(), "{registered:?}");
    assert_eq!(stdout(&registered), format!("teammate api will start in {}", target.display()));
    assert!(output.status.success(), "{output:?}");
    let run = sandbox.herdr_calls().into_iter().find(|call| call.starts_with("pane run w1:p2 ")).unwrap();
    let body = fs::read_to_string(run.trim_start_matches("pane run w1:p2 sh ")).unwrap();
    assert!(body.contains(&format!("cd {} && env claude --agent-name api", target.display())), "{body}");
    assert!(sandbox.calls_log().contains("(registered by the lead)"), "{}", sandbox.calls_log());
}

#[test]
fn route_resolves_a_relative_directory_and_wins_over_a_nearby_repository() {
    let sandbox = Sandbox::with_idmap(LEADER_AND_TEAMMATE);
    let lead = sandbox.root.join("work/lead");
    fs::create_dir_all(lead.join("api/.git")).unwrap();
    fs::create_dir_all(sandbox.root.join("other/api")).unwrap();
    let other = fs::canonicalize(sandbox.root.join("other/api")).unwrap();

    let registered = sandbox.route_in(&sandbox.root, &["api", "other/api"]);
    let line = format!("cd {} && env claude --agent-name api", lead.display());
    let output = sandbox.tmux(&["respawn-pane", "-k", "-t", "%1", "--", &line]);

    assert!(registered.status.success(), "{registered:?}");
    assert!(output.status.success(), "{output:?}");
    let run = sandbox.herdr_calls().into_iter().find(|call| call.starts_with("pane run w1:p2 ")).unwrap();
    let body = fs::read_to_string(run.trim_start_matches("pane run w1:p2 sh ")).unwrap();
    assert!(body.contains(&format!("cd {} && env claude --agent-name api", other.display())), "{body}");
}

#[test]
fn route_refuses_bad_names_missing_directories_and_callers_outside_a_lead() {
    let sandbox = Sandbox::with_idmap(LEADER_ONLY);
    let dir = sandbox.root.to_str().unwrap();

    let bad_name = sandbox.route(&["../api", dir]);
    let missing = sandbox.route(&["api", "/no/such/dir"]);
    let outside = sandbox.launcher_with_args(&["route", "api", dir], &[]);

    assert!(stderr(&bad_name).contains("is not a teammate name"), "{bad_name:?}");
    assert!(stderr(&missing).contains("is not an existing directory"), "{missing:?}");
    assert!(stderr(&outside).contains("run this from the Bash tool of a team lead"), "{outside:?}");
    assert_eq!(sandbox.idmap()["routes"], serde_json::Value::Null);
}

#[test]
fn split_window_closes_the_new_pane_when_the_command_cannot_start() {
    let sandbox = Sandbox::with_idmap(LEADER_ONLY);

    let output =
        sandbox.tmux_with_env(&["split-window", "-t", "%0", "claude"], &[("FAKE_HERDR_FAIL_RUN", "1")]);

    assert!(!output.status.success());
    assert!(sandbox.herdr_calls().contains(&"pane close w1:p2".to_owned()));
    assert_eq!(sandbox.idmap()["map"].get("%1"), None);
    assert_eq!(sandbox.run_scripts(), Vec::<std::path::PathBuf>::new());
}

#[test]
fn list_panes_shows_only_the_team_panes_of_the_tab() {
    let sandbox = Sandbox::with_idmap(LEADER_AND_TEAMMATE);

    let by_pane = sandbox.tmux(&["list-panes", "-t", "%0", "-F", "#{pane_id}"]);
    let by_window = sandbox.tmux_without_socket(&["list-panes", "-t", "herdr:@0", "-F", "#{pane_id}"]);

    assert_eq!(stdout(&by_pane), "%0\n%1");
    assert_eq!(stdout(&by_window), "%0\n%1");
}

#[test]
fn select_pane_title_renames_the_herdr_pane() {
    let sandbox = Sandbox::with_idmap(LEADER_AND_TEAMMATE);

    let output = sandbox.tmux(&["select-pane", "-t", "%1", "-T", "alpha"]);

    assert!(output.status.success());
    assert!(sandbox.herdr_calls().contains(&"pane rename w1:p2 alpha".to_owned()));
    assert_eq!(sandbox.idmap()["titles"]["w1:p2"], json!("alpha"));
}

#[test]
fn kill_pane_closes_the_pane_and_forgets_it() {
    let sandbox = Sandbox::with_idmap(LEADER_AND_TEAMMATE);

    let output = sandbox.tmux(&["kill-pane", "-t", "%1"]);

    assert!(output.status.success());
    assert!(sandbox.herdr_calls().contains(&"pane close w1:p2".to_owned()));
    assert_eq!(sandbox.idmap()["map"].get("%1"), None);
}

#[test]
fn kill_pane_closes_a_teammate_pane_the_user_moved_elsewhere_and_forgets_it() {
    for forgets_old_ids in ["", "1"] {
        let sandbox = Sandbox::with_idmap(LEADER_AND_TRACKED_TEAMMATE);

        let output = sandbox.tmux_with_env(
            &["kill-pane", "-t", "%1"],
            &[("FAKE_HERDR_MOVED", "w1:p2 w2:p1"), ("FAKE_HERDR_FORGETS_OLD_IDS", forgets_old_ids)],
        );

        assert!(output.status.success(), "{output:?}");
        let calls = sandbox.herdr_calls();
        assert!(calls.contains(&"pane close w2:p1".to_owned()), "{calls:?}");
        assert_eq!(sandbox.idmap()["map"].get("%1"), None, "{forgets_old_ids:?}");
        assert_eq!(sandbox.idmap()["terminals"].get("%1"), None, "{forgets_old_ids:?}");
    }
}

#[test]
fn kill_pane_forgets_a_teammate_pane_that_already_closed() {
    for idmap in [LEADER_AND_TEAMMATE, LEADER_AND_TRACKED_TEAMMATE] {
        let sandbox = Sandbox::with_idmap(idmap);

        let output = sandbox.tmux_with_env(&["kill-pane", "-t", "%1"], &[("FAKE_HERDR_CLOSED", "w1:p2")]);

        assert!(!output.status.success());
        assert_eq!(sandbox.idmap()["map"].get("%1"), None, "{idmap}");
        assert!(sandbox.idmap()["map"].get("%0").is_some());
    }
}

#[test]
fn a_moved_teammate_pane_keeps_its_tmux_id_and_title() {
    let sandbox = Sandbox::with_idmap(LEADER_AND_TRACKED_TEAMMATE);

    let output = sandbox.tmux_with_env(
        &["display-message", "-p", "-t", "%1", "#{pane_id} #{pane_title}"],
        &[("FAKE_HERDR_MOVED", "w1:p2 w2:p1")],
    );

    assert_eq!(stdout(&output), "%1 alpha");
    assert_eq!(sandbox.idmap()["map"]["%1"], json!("w2:p1"));
    assert_eq!(sandbox.idmap()["titles"]["w2:p1"], json!("alpha"));
    assert!(sandbox.calls_log().contains("w1:p2 -> w2:p1"));
}

#[test]
fn split_window_remembers_the_terminal_of_the_new_pane() {
    let sandbox = Sandbox::with_idmap(LEADER_ONLY);

    sandbox.tmux(&["split-window", "-d", "-t", "%0", "-h", "-P", "-F", "#{pane_id}", "--", "cat"]);

    assert_eq!(sandbox.idmap()["terminals"]["%1"], json!("term_w1_p2"));
}

#[test]
fn destructive_verbs_never_touch_the_team_lead() {
    let sandbox = Sandbox::with_idmap(LEADER_AND_TEAMMATE);

    for args in
        [&["kill-pane"][..], &["kill-pane", "-t", "%0"], &["kill-pane", "-t", "@0"], &["send-keys", "x"]]
    {
        let output = sandbox.tmux(args);
        assert!(!output.status.success(), "{args:?}");
    }
    assert!(
        !sandbox.herdr_calls().iter().any(|call| call.contains("w1:p1") && !call.starts_with("pane get"))
    );
}

#[test]
fn send_keys_literal_types_the_text_into_the_target_pane() {
    let sandbox = Sandbox::with_idmap(LEADER_AND_TEAMMATE);

    let output = sandbox.tmux(&["send-keys", "-l", "-t", "%1", "hello world"]);

    assert!(output.status.success(), "{output:?}");
    assert!(sandbox.herdr_calls().contains(&"pane send-text w1:p2 hello world".to_owned()));
}

#[test]
fn capture_pane_reads_the_target_pane_with_boolean_flags_before_the_target() {
    let sandbox = Sandbox::with_idmap(LEADER_AND_TEAMMATE);

    let output = sandbox.tmux(&["capture-pane", "-p", "-e", "-t", "%1"]);

    assert_eq!(stdout(&output), "screen of w1:p2");
    assert!(!sandbox.calls_log().contains("screen of"));
}

#[test]
fn unknown_flags_fail_instead_of_guessing() {
    let sandbox = Sandbox::with_idmap(LEADER_AND_TEAMMATE);

    let output = sandbox.tmux(&["kill-pane", "-z", "-t", "%1"]);

    assert!(!output.status.success());
    assert_eq!(stderr(&output), "unknown flag -z");
    assert_eq!(sandbox.herdr_calls(), Vec::<String>::new());
}

#[test]
fn unknown_target_fails_like_tmux() {
    let sandbox = Sandbox::with_idmap(LEADER_ONLY);

    let output = sandbox.tmux(&["kill-pane", "-t", "%5"]);

    assert!(!output.status.success());
    assert_eq!(stderr(&output), "can't find pane: %5");
}

#[test]
fn commands_not_from_claude_code_go_to_the_real_tmux_without_the_fake_session() {
    let sandbox = Sandbox::with_idmap(LEADER_AND_TEAMMATE);

    for args in [&["kill-pane", "-t", "%1"][..], &["-q", "new-session", "-d"], &["has-session", "-t", "work"]]
    {
        sandbox.tmux_without_socket(args);
    }

    assert_eq!(sandbox.herdr_calls(), Vec::<String>::new());
    assert_eq!(
        sandbox.real_tmux_calls(),
        ["kill-pane -t %1|TMUX=", "-q new-session -d|TMUX=", "has-session -t work|TMUX="]
    );
}

#[test]
fn no_op_verbs_never_reach_the_real_tmux() {
    let sandbox = Sandbox::with_idmap(LEADER_ONLY);

    let output = sandbox.tmux_without_socket(&["set", "-as", "terminal-features", ",*:RGB"]);

    assert!(output.status.success());
    assert_eq!(sandbox.real_tmux_calls(), Vec::<String>::new());
}

#[test]
fn failures_are_logged_without_the_command_or_typed_text() {
    let sandbox = Sandbox::with_idmap(LEADER_AND_TEAMMATE);

    let output = sandbox.tmux_with_env(
        &["respawn-pane", "-t", "%1", "--", "export AWS_SECRET_ACCESS_KEY=hunter2"],
        &[("FAKE_HERDR_FAIL_RUN", "1")],
    );

    assert!(!output.status.success());
    let log = sandbox.calls_log();
    assert!(log.contains(r#""message":"herdr pane run failed""#), "{log}");
    assert!(!log.contains("hunter2"), "{log}");
}

#[test]
fn kill_pane_rejects_flags_it_does_not_implement() {
    let sandbox = Sandbox::with_idmap(LEADER_AND_TEAMMATE);

    let output = sandbox.tmux(&["kill-pane", "-a", "-t", "%1"]);

    assert_eq!(stderr(&output), "unknown flag -a");
    assert_eq!(sandbox.herdr_calls(), Vec::<String>::new());
}

#[test]
fn unhandled_verbs_from_claude_code_succeed_and_are_logged() {
    let sandbox = Sandbox::with_idmap(LEADER_ONLY);

    let output = sandbox.tmux(&["new-window", "-d"]);

    assert!(output.status.success());
    let last: serde_json::Value = serde_json::from_str(sandbox.calls_log().lines().last().unwrap()).unwrap();
    assert_eq!(last["verb"], json!("new-window"));
    assert_eq!(last["message"], json!("unhandled"));
}

#[test]
fn show_options_reports_tmux_features_as_enabled() {
    let sandbox = Sandbox::with_idmap(LEADER_ONLY);

    let output = sandbox.tmux_without_socket(&["show", "-Av", "mouse"]);

    assert_eq!(stdout(&output), "on");
}

#[test]
fn concurrent_calls_keep_the_log_parseable_and_ids_unique() {
    let sandbox = Sandbox::with_idmap(LEADER_ONLY);

    std::thread::scope(|scope| {
        for _ in 0..12 {
            scope
                .spawn(|| sandbox.tmux(&["split-window", "-t", "%0", "-P", "-F", "#{pane_id}", "--", "cat"]));
        }
    });

    let log = sandbox.calls_log();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 12);
    assert!(lines.iter().all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok()));
    let map = sandbox.idmap()["map"].as_object().unwrap().clone();
    assert_eq!(map.keys().filter(|id| id.starts_with('%')).count(), 13);
}
