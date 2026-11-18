mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use support::{LEADER_AND_TEAMMATE, Sandbox, stdout};

fn lead_dir(sandbox: &Sandbox, pid: u32) -> std::path::PathBuf {
    let root = sandbox.root.join(".local/state/herdr-claude-code-agent-teams");
    let dir = root.join("0123456789abcdef").join(format!("w1_p1-{pid}"));
    fs::create_dir_all(&dir).unwrap();
    for private in [root.as_path(), dir.parent().unwrap(), dir.as_path()] {
        fs::set_permissions(private, fs::Permissions::from_mode(0o700)).unwrap();
    }
    dir
}

#[test]
fn doctor_reports_a_healthy_setup() {
    let sandbox = Sandbox::new();

    let output = sandbox.doctor(&[]);

    assert!(output.status.success(), "{output:?}");
    let report = stdout(&output);
    for line in [
        "ok    herdr               0.9.5",
        "ok    claude              2.1.311 (Claude Code)",
        "ok    claude integration  current (v11)",
        "ok    locale              LANG=en_US.UTF-8",
        "(created when a lead starts)",
    ] {
        assert!(report.contains(line), "missing {line:?} in\n{report}");
    }
}

#[test]
fn doctor_fails_when_herdr_is_too_old() {
    let sandbox = Sandbox::new();

    let output = sandbox.doctor(&[("FAKE_HERDR_VERSION", "0.9.4")]);

    assert!(!output.status.success());
    assert!(
        stdout(&output).contains("FAIL  herdr               0.9.4; this plugin needs"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn doctor_lists_running_leads_and_tmux_commands_they_could_not_handle() {
    let sandbox = Sandbox::new();
    let dir = lead_dir(&sandbox, std::process::id());
    fs::write(dir.join("idmap.json"), LEADER_AND_TEAMMATE).unwrap();
    fs::write(
        dir.join("calls.log"),
        concat!(
            r#"{"t":1,"verb":"kill-pane","target":"%1","exit":1,"message":"herdr pane close failed"}"#,
            "\n",
            r#"{"t":2,"verb":"new-window","target":null,"exit":0,"message":"unhandled"}"#,
            "\n"
        ),
    )
    .unwrap();
    lead_dir(&sandbox, 999_999_999);

    let report = stdout(&sandbox.doctor(&[]));

    let leads: Vec<&str> = report.lines().filter(|line| line.contains(" lead ")).collect();
    assert_eq!(leads.len(), 1, "{report}");
    assert!(
        leads[0].starts_with(&format!(
            "warn  lead                w1:p1 (pid {}): 1 teammate pane(s), 1 failed tmux call(s); \
             Claude Code used tmux commands this plugin does not handle (new-window)",
            std::process::id()
        )),
        "{report}"
    );
}
