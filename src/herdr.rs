use std::env;
use std::fmt;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rustix::process::{Pid, Signal, kill_process_group};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Pane {
    #[serde(rename = "pane_id")]
    pub(crate) id: String,
    pub(crate) tab_id: String,
    #[serde(default)]
    pub(crate) cwd: Option<String>,
    #[serde(default)]
    pub(crate) foreground_cwd: Option<String>,
    #[serde(default)]
    pub(crate) focused: bool,
    #[serde(default)]
    pub(crate) agent: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ProcessInfo {
    pub(crate) shell_pid: Option<u32>,
    pub(crate) foreground_process_group_id: Option<u32>,
    #[serde(default)]
    pub(crate) foreground_processes: Vec<ForegroundProcess>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ForegroundProcess {
    pub(crate) name: String,
}

#[derive(Debug)]
pub(crate) struct CommandError {
    pub(crate) command: String,
    detail: String,
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.command, self.detail)
    }
}

impl std::error::Error for CommandError {}

pub(crate) fn bin() -> String {
    env::var("HERDR_BIN_PATH").ok().filter(|path| !path.is_empty()).unwrap_or_else(|| "herdr".into())
}

pub(crate) fn call<S: AsRef<str>>(args: &[S]) -> Result<Value> {
    let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    let stdout = output("herdr", &bin(), &args)?;
    if stdout.trim_start().starts_with('{') {
        serde_json::from_str(&stdout).with_context(|| format!("herdr {}", args.join(" ")))
    } else {
        Ok(Value::Null)
    }
}

pub(crate) fn output(name: &str, program: &str, args: &[&str]) -> Result<String> {
    let describe = || format!("{name} {}", args.join(" "));

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .with_context(describe)?;
    let stdout = read_in_background(child.stdout.take());
    let stderr = read_in_background(child.stderr.take());

    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().with_context(describe)? {
            break status;
        }
        if Instant::now() >= deadline {
            kill_group(&mut child);
            bail!("{}: timed out after {}s", describe(), TIMEOUT.as_secs());
        }
        thread::sleep(Duration::from_millis(10));
    };
    let (Some(stdout), Some(stderr)) = (receive_until(&stdout, deadline), receive_until(&stderr, deadline))
    else {
        kill_group(&mut child);
        bail!("{}: output did not close within {}s", describe(), TIMEOUT.as_secs());
    };

    if !status.success() {
        let detail = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };
        return Err(CommandError { command: describe(), detail: detail.to_owned() }.into());
    }
    Ok(stdout)
}

fn read_in_background(pipe: Option<impl Read + Send + 'static>) -> Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_string(&mut text);
        }
        let _ = sender.send(text);
    });
    receiver
}

fn receive_until(receiver: &Receiver<String>, deadline: Instant) -> Option<String> {
    receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok()
}

fn kill_group(child: &mut Child) {
    if let Some(group) = i32::try_from(child.id()).ok().and_then(Pid::from_raw) {
        let _ = kill_process_group(group, Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) fn result_field<T: DeserializeOwned>(mut response: Value, field: &str) -> Result<T> {
    let value = response
        .pointer_mut(&format!("/result/{field}"))
        .map(Value::take)
        .with_context(|| format!("herdr response has no result.{field}"))?;
    serde_json::from_value(value).with_context(|| format!("unexpected result.{field} from herdr"))
}

pub(crate) fn pane(pane_id: &str) -> Result<Pane> {
    result_field(call(&["pane", "get", pane_id])?, "pane")
}

pub(crate) fn current_pane() -> Result<Pane> {
    match env::var("HERDR_PANE_ID") {
        Ok(pane_id) if !pane_id.is_empty() => pane(&pane_id),
        _ => result_field(call(&["pane", "current"])?, "pane"),
    }
}

pub(crate) fn process_info(pane_id: &str) -> Result<ProcessInfo> {
    result_field(call(&["pane", "process-info", "--pane", pane_id])?, "process_info")
}

pub(crate) fn panes_in_workspace(workspace: &str) -> Result<Vec<Pane>> {
    result_field(call(&["pane", "list", "--workspace", workspace])?, "panes")
}

pub(crate) fn split<S: AsRef<str>>(args: &[S]) -> Result<Pane> {
    result_field(call(args)?, "pane")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn result_field_reads_a_pane_with_optional_fields_missing() {
        let response = json!({"result": {"pane": {"pane_id": "w1:p2", "tab_id": "w1:t1"}}});

        let pane: Pane = result_field(response, "pane").unwrap();

        assert_eq!(pane.id, "w1:p2");
        assert_eq!(pane.tab_id, "w1:t1");
        assert_eq!(pane.cwd, None);
        assert!(!pane.focused);
        assert_eq!(pane.agent, None);
    }

    #[test]
    fn result_field_fails_when_the_field_is_absent() {
        let response = json!({"result": {"panes": []}});

        let error = result_field::<Pane>(response, "pane").unwrap_err();

        assert_eq!(error.to_string(), "herdr response has no result.pane");
    }

    #[test]
    fn result_field_reads_process_info() {
        let response = json!({"result": {"process_info": {
            "shell_pid": 10,
            "foreground_process_group_id": 42,
            "foreground_processes": [{"name": "vim", "pid": 42}]
        }}});

        let info: ProcessInfo = result_field(response, "process_info").unwrap();

        assert_eq!(info.shell_pid, Some(10));
        assert_eq!(info.foreground_process_group_id, Some(42));
        assert_eq!(info.foreground_processes[0].name, "vim");
    }
}
