use std::env;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, anyhow, bail};

use crate::herdr::{self, ProcessInfo};
use crate::shell;
use crate::state;

const CLAUDE_ARGS_TEMPLATE: &str = "\
# Arguments passed to claude when the open and here actions start a team lead.
# Write them on one line below, for example:
# --model sonnet --advisor opus
";

pub(crate) fn run() -> ExitCode {
    let outcome = match env::var("HERDR_PLUGIN_ACTION_ID").as_deref() {
        Ok("open") => open_split(),
        Ok("here") => run_here(),
        Ok("configure") => open_config_popup(),
        Ok(other) => Err(anyhow!("unknown action: {other}")),
        Err(_) => Err(anyhow!("HERDR_PLUGIN_ACTION_ID is not set")),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

pub(crate) fn edit_config() -> ExitCode {
    let error = match prepare_claude_args_file() {
        Ok(path) => {
            let editor = env::var("VISUAL")
                .or_else(|_| env::var("EDITOR"))
                .ok()
                .filter(|editor| !editor.trim().is_empty())
                .unwrap_or_else(|| "vi".into());
            anyhow!(Command::new("sh").arg("-c").arg(format!("{editor} \"$1\"")).arg("sh").arg(path).exec())
                .context("cannot start the editor")
        }
        Err(error) => error,
    };
    eprintln!("{error:#}");
    ExitCode::FAILURE
}

fn claude_args_path() -> Result<PathBuf> {
    env::var_os("HERDR_PLUGIN_CONFIG_DIR")
        .map(|dir| PathBuf::from(dir).join("claude-args"))
        .context("HERDR_PLUGIN_CONFIG_DIR is not set")
}

fn prepare_claude_args_file() -> Result<PathBuf> {
    let path = claude_args_path()?;
    if let Some(dir) = path.parent() {
        state::create_private_dir(dir)?;
    }
    match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
        Ok(mut file) => {
            file.write_all(CLAUDE_ARGS_TEMPLATE.as_bytes()).with_context(|| path.display().to_string())?;
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).with_context(|| path.display().to_string()),
    }
    Ok(path)
}

fn open_config_popup() -> Result<()> {
    let plugin = env::var("HERDR_PLUGIN_ID").context("HERDR_PLUGIN_ID is not set")?;
    herdr::call(&["plugin", "pane", "open", "--plugin", &plugin, "--entrypoint", "configure"])?;
    Ok(())
}

fn claude_args() -> Result<String> {
    if env::var_os("HERDR_PLUGIN_CONFIG_DIR").is_none() {
        return Ok(String::new());
    }
    let path = claude_args_path()?;
    match fs::read_to_string(&path) {
        Ok(text) => parse_claude_args(&text).with_context(|| path.display().to_string()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| path.display().to_string()),
    }
}

fn parse_claude_args(text: &str) -> Result<String> {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#'));
    let args = lines.next().unwrap_or_default();
    if lines.next().is_some() || args.chars().any(char::is_control) {
        bail!("write the claude arguments on a single line");
    }
    Ok(args.to_owned())
}

fn launch_command() -> Result<String> {
    let exe = env::current_exe().context("cannot locate herdr-claude-code-agent-teams")?;
    Ok(command_line(&exe.to_string_lossy(), &claude_args()?))
}

fn command_line(launcher: &str, claude_args: &str) -> String {
    let launcher = shell::quote(launcher);
    if claude_args.is_empty() { launcher } else { format!("{launcher} {claude_args}") }
}

fn ensure_shell_is_idle(pane_id: &str, info: &ProcessInfo) -> Result<()> {
    if info.shell_pid.is_some() && info.foreground_process_group_id == info.shell_pid {
        return Ok(());
    }
    let running =
        info.foreground_processes.first().map_or("another program", |process| process.name.as_str());
    bail!("{running} is running in {pane_id}; run this action from a pane that is at a shell prompt")
}

fn open_split() -> Result<()> {
    let source = herdr::current_pane()?;
    let mut args: Vec<String> =
        ["pane", "split", "--pane", &source.id, "--direction", "right", "--focus"].map(str::to_owned).into();
    if let Some(cwd) = source.foreground_cwd.or(source.cwd) {
        args.extend(["--cwd".to_owned(), cwd]);
    }
    let new_pane = herdr::split(&args)?.id;
    herdr::call(&["pane", "run", &new_pane, &launch_command()?])?;
    Ok(())
}

fn run_here() -> Result<()> {
    let pane = herdr::current_pane()?;
    if let Some(agent) = pane.agent {
        bail!("{agent} is already running in {}", pane.id);
    }
    ensure_shell_is_idle(&pane.id, &herdr::process_info(&pane.id)?)?;
    let command = launch_command()?;
    herdr::call(&["pane", "send-keys", &pane.id, "ctrl+u"])?;
    herdr::call(&["pane", "run", &pane.id, &command])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::ForegroundProcess;

    #[test]
    fn command_line_quotes_the_launcher_and_appends_claude_args_verbatim() {
        assert_eq!(
            command_line(
                "/Users/me/Application Support/bin/herdr-claude-code-agent-teams",
                "--model sonnet --advisor opus"
            ),
            "'/Users/me/Application Support/bin/herdr-claude-code-agent-teams' --model sonnet --advisor opus"
        );
    }

    #[test]
    fn command_line_without_claude_args_is_just_the_launcher() {
        assert_eq!(
            command_line("/opt/bin/herdr-claude-code-agent-teams", ""),
            "/opt/bin/herdr-claude-code-agent-teams"
        );
    }

    #[test]
    fn parse_claude_args_skips_comments_and_blank_lines() {
        assert_eq!(parse_claude_args(CLAUDE_ARGS_TEMPLATE).unwrap(), "");
        assert_eq!(
            parse_claude_args(&format!("{CLAUDE_ARGS_TEMPLATE}\n  --model sonnet --advisor opus\n")).unwrap(),
            "--model sonnet --advisor opus"
        );
    }

    #[test]
    fn parse_claude_args_rejects_more_than_one_line() {
        for text in ["--model sonnet\nrm -rf ~", "--model sonnet\rrm -rf ~", "--model\tsonnet\u{1b}[A"] {
            assert_eq!(
                parse_claude_args(text).unwrap_err().to_string(),
                "write the claude arguments on a single line",
                "{text:?}"
            );
        }
    }

    #[test]
    fn ensure_shell_is_idle_accepts_a_shell_at_its_prompt() {
        let info = ProcessInfo {
            shell_pid: Some(10),
            foreground_process_group_id: Some(10),
            foreground_processes: vec![ForegroundProcess { name: "zsh".into() }],
        };

        assert!(ensure_shell_is_idle("w1:p1", &info).is_ok());
    }

    #[test]
    fn ensure_shell_is_idle_rejects_a_running_program() {
        let info = ProcessInfo {
            shell_pid: Some(10),
            foreground_process_group_id: Some(42),
            foreground_processes: vec![ForegroundProcess { name: "vim".into() }],
        };

        assert_eq!(
            ensure_shell_is_idle("w1:p1", &info).unwrap_err().to_string(),
            "vim is running in w1:p1; run this action from a pane that is at a shell prompt"
        );
    }
}
