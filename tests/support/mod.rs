#![allow(dead_code)]

use std::env;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

const FAKE_HERDR: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_HERDR_LOG"
case "$1 $2" in
  "pane get")
    printf '{"result":{"pane":{"pane_id":"%s","tab_id":"w1:t1","cwd":"/work","focused":false}}}\n' "$3" ;;
  "pane split")
    while ! mkdir "$FAKE_HERDR_LOG.lock" 2>/dev/null; do sleep 0.01; done
    count=$(($(cat "$FAKE_HERDR_LOG.splits" 2>/dev/null || echo 0) + 1))
    echo "$count" > "$FAKE_HERDR_LOG.splits"
    rmdir "$FAKE_HERDR_LOG.lock"
    printf '{"result":{"pane":{"pane_id":"w1:p%s","tab_id":"w1:t1"}}}\n' "$((count + 1))" ;;
  "pane list")
    printf '{"result":{"panes":[{"pane_id":"w1:p1","tab_id":"w1:t1"},{"pane_id":"w1:p2","tab_id":"w1:t1"},{"pane_id":"w1:p8","tab_id":"w1:t1"},{"pane_id":"w1:p7","tab_id":"w1:t2"}]}}\n' ;;
  "pane read")
    printf '{"result":{"text":"screen of %s"}}\n' "$3" ;;
  "pane run")
    if [ -n "$FAKE_HERDR_FAIL_RUN" ]; then echo "pane run failed" >&2; exit 1; fi
    printf '{"result":{}}\n' ;;
  *)
    printf '{"result":{}}\n' ;;
esac
"#;

const FAKE_REAL_TMUX: &str = r#"#!/bin/sh
printf '%s|TMUX=%s\n' "$*" "$TMUX" >> "$FAKE_REAL_TMUX_LOG"
"#;

const FAKE_CLAUDE: &str = r#"#!/bin/sh
printf 'args=%s\n' "$*"
printf 'TMUX=%s\n' "$TMUX"
printf 'TMUX_PANE=%s\n' "$TMUX_PANE"
printf 'TEAMS=%s\n' "$CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS"
printf 'TRUECOLOR=%s\n' "$CLAUDE_CODE_TMUX_TRUECOLOR"
printf 'STATE=%s\n' "$HERDR_CLAUDE_CODE_AGENT_TEAMS_STATE"
printf 'tmux=%s\n' "$(command -v tmux)"
"#;

pub(crate) const LEADER_ONLY: &str =
    r#"{"next_pane":1,"next_window":1,"map":{"%0":"w1:p1","@0":"w1:t1"},"titles":{}}"#;
pub(crate) const LEADER_AND_TEAMMATE: &str =
    r#"{"next_pane":2,"next_window":1,"map":{"%0":"w1:p1","%1":"w1:p2","@0":"w1:t1"},"titles":{}}"#;

#[derive(Debug)]
pub(crate) struct Sandbox {
    pub(crate) root: PathBuf,
}

impl Sandbox {
    pub(crate) fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let root = env::temp_dir().join(format!(
            "herdr-claude-code-agent-teams-it-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("state")).unwrap();
        write_executable(&root.join("bin/herdr"), FAKE_HERDR);
        write_executable(&root.join("bin/claude"), FAKE_CLAUDE);
        fs::create_dir_all(root.join("real")).unwrap();
        write_executable(&root.join("real/tmux"), FAKE_REAL_TMUX);
        symlink(binary(), root.join("bin/tmux")).unwrap();
        Self { root }
    }

    pub(crate) fn with_idmap(idmap: &str) -> Self {
        let sandbox = Self::new();
        fs::write(sandbox.state_dir().join("idmap.json"), idmap).unwrap();
        sandbox
    }

    pub(crate) fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    pub(crate) fn socket(&self) -> String {
        self.state_dir().join("tmux.sock").display().to_string()
    }

    pub(crate) fn idmap(&self) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(self.state_dir().join("idmap.json")).unwrap()).unwrap()
    }

    pub(crate) fn herdr_calls(&self) -> Vec<String> {
        fs::read_to_string(self.root.join("herdr.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    pub(crate) fn real_tmux_calls(&self) -> Vec<String> {
        fs::read_to_string(self.root.join("real-tmux.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    pub(crate) fn calls_log(&self) -> String {
        fs::read_to_string(self.state_dir().join("calls.log")).unwrap_or_default()
    }

    pub(crate) fn run_scripts(&self) -> Vec<PathBuf> {
        fs::read_dir(self.state_dir())
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.file_name().unwrap().to_string_lossy().starts_with("run-"))
            .collect()
    }

    fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}:/usr/bin:/bin",
                    self.root.join("bin").display(),
                    self.root.join("real").display()
                ),
            )
            .env("HOME", &self.root)
            .env("HERDR_BIN_PATH", self.root.join("bin/herdr"))
            .env("FAKE_HERDR_LOG", self.root.join("herdr.log"))
            .env("FAKE_REAL_TMUX_LOG", self.root.join("real-tmux.log"));
        command
    }

    fn shim(&self) -> Command {
        let mut command = self.command(&self.root.join("bin/tmux"));
        command.env("HERDR_CLAUDE_CODE_AGENT_TEAMS_STATE", self.state_dir()).env("TMUX_PANE", "%0");
        command
    }

    pub(crate) fn tmux(&self, args: &[&str]) -> Output {
        self.shim().arg("-S").arg(self.socket()).args(args).output().unwrap()
    }

    pub(crate) fn tmux_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        self.shim().arg("-S").arg(self.socket()).args(args).envs(envs.iter().copied()).output().unwrap()
    }

    pub(crate) fn tmux_without_socket(&self, args: &[&str]) -> Output {
        self.shim().args(args).output().unwrap()
    }

    pub(crate) fn launcher(&self, envs: &[(&str, &str)]) -> Output {
        self.command(&binary()).args(["--model", "sonnet"]).envs(envs.iter().copied()).output().unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_herdr-claude-code-agent-teams"))
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

pub(crate) fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim_end().to_owned()
}

pub(crate) fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim_end().to_owned()
}

pub(crate) fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}
