use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{self, Command, ExitCode};

use anyhow::{Context, Result, anyhow, bail};
use rustix::io::Errno;
use rustix::process::{Pid, test_kill_process};

use crate::herdr;
use crate::route;
use crate::state::{self, State};

const TEAMMATE_MODE: &str = r#"{"teammateMode":"tmux"}"#;
const APPEND_SYSTEM_PROMPT: &str = "--append-system-prompt";
const TEAM_PROMPT: &str = "You can lead an agent team whose teammates run in their own herdr panes. \
    When the user asks for an agent team or teammates, spawn each teammate by calling the Agent tool with a name. \
    An Agent call without a name starts an ordinary subagent that gets no pane and is not part of the team; \
    that is still fine for your own quick lookups, but not for a teammate. \
    A teammate stays idle in its pane after finishing a task, so once you have its result \
    and nothing left for it to do, stop it to close its pane.";

pub(crate) fn run(args: &[String]) -> ExitCode {
    let error = match herdr_pane() {
        Some(pane_id) => match prepare(&pane_id) {
            Ok(state) => exec_team_lead(&state, args),
            Err(error) => error,
        },
        None => anyhow!(Command::new("claude").args(args).exec()).context("cannot run claude"),
    };
    eprintln!("herdr-claude-code-agent-teams: {error:#}");
    ExitCode::FAILURE
}

fn herdr_pane() -> Option<String> {
    if env::var("HERDR_ENV").as_deref() != Ok("1") {
        return None;
    }
    env::var("HERDR_PANE_ID").ok().filter(|pane_id| !pane_id.is_empty())
}

fn absolute_env(name: &str) -> Option<PathBuf> {
    env::var_os(name).map(PathBuf::from).filter(|path| path.is_absolute())
}

pub(crate) fn state_root() -> Result<PathBuf> {
    let base = match absolute_env("XDG_STATE_HOME") {
        Some(dir) => dir,
        None => absolute_env("HOME").context("HOME is not an absolute path")?.join(".local/state"),
    };
    Ok(base.join("herdr-claude-code-agent-teams"))
}

pub(crate) fn session_key(socket: Option<&OsStr>) -> String {
    let hash =
        socket.map_or(&[][..], OsStr::as_bytes).iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, &byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    format!("{hash:016x}")
}

fn prepare(pane_id: &str) -> Result<State> {
    let root = state_root()?;
    if root.as_os_str().as_bytes().contains(&b',') {
        bail!("{} contains a comma, which Claude Code cannot pass back as a tmux socket", root.display());
    }
    state::create_private_dir(&root)?;
    let session_dir = root.join(session_key(env::var_os("HERDR_SOCKET_PATH").as_deref()));
    state::create_private_dir(&session_dir)?;
    remove_finished_leads(&session_dir);

    let state = State::new(session_dir.join(format!("{}-{}", pane_id.replace(':', "_"), process::id())));
    let tab_id = herdr::pane(pane_id)?.tab_id;
    state.reset(pane_id, &tab_id)?;

    let bin = state.dir().join("bin");
    state::create_private_dir(&bin)?;
    let shim = bin.join("tmux");
    match fs::remove_file(&shim) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            return Err(error).with_context(|| shim.display().to_string());
        }
        _ => {}
    }
    let exe = env::current_exe()
        .and_then(fs::canonicalize)
        .context("cannot locate herdr-claude-code-agent-teams")?;
    symlink(&exe, &shim).with_context(|| shim.display().to_string())?;
    Ok(state)
}

fn remove_finished_leads(session_dir: &Path) {
    let Ok(entries) = fs::read_dir(session_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|name| name.rsplit_once('-')).map(|(_, pid)| pid) else {
            continue;
        };
        if pid.parse::<i32>().is_ok_and(|pid| !is_running(pid)) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

pub(crate) fn is_running(pid: i32) -> bool {
    let Some(pid) = Pid::from_raw(pid) else {
        return false;
    };
    matches!(test_kill_process(pid), Ok(()) | Err(Errno::PERM))
}

fn search_path(state_dir: &Path) -> Result<OsString> {
    let inherited = env::var_os("PATH").unwrap_or_default();
    let paths = std::iter::once(state_dir.join("bin")).chain(env::split_paths(&inherited));
    env::join_paths(paths).context("cannot build PATH")
}

fn tmux_env(state: &State) -> OsString {
    let mut value = state.socket().into_os_string();
    value.push(format!(",{},0", process::id()));
    value
}

fn lead_prompt(args: &[String]) -> Vec<String> {
    if args.iter().any(|arg| arg.starts_with(APPEND_SYSTEM_PROMPT)) {
        return Vec::new();
    }
    let mut prompt = TEAM_PROMPT.to_owned();
    if let (Ok(lead_dir), Ok(launcher)) = (env::current_dir(), env::current_exe().and_then(fs::canonicalize))
    {
        prompt.push(' ');
        prompt.push_str(&route::hint(&lead_dir, &launcher));
    }
    vec![APPEND_SYSTEM_PROMPT.to_owned(), prompt]
}

fn exec_team_lead(state: &State, args: &[String]) -> anyhow::Error {
    let path = match search_path(state.dir()) {
        Ok(path) => path,
        Err(error) => return error,
    };
    let error = Command::new("claude")
        .arg("--settings")
        .arg(TEAMMATE_MODE)
        .args(lead_prompt(args))
        .args(args)
        .env("PATH", path)
        .env("TMUX", tmux_env(state))
        .env("TMUX_PANE", "%0")
        .env("CLAUDE_CODE_TMUX_TRUECOLOR", "1")
        .env(state::ENV, state.dir())
        .env("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1")
        .exec();
    anyhow!(error).context("cannot run claude")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_path_puts_the_shim_directory_first() {
        let path = search_path(Path::new("/state/w1_p1-1")).unwrap();

        let first = env::split_paths(&path).next().unwrap();
        assert_eq!(first, PathBuf::from("/state/w1_p1-1/bin"));
    }

    #[test]
    fn session_key_differs_between_herdr_servers() {
        let default = session_key(Some(OsStr::new("/home/me/.config/herdr/herdr.sock")));
        let named = session_key(Some(OsStr::new("/home/me/.config/herdr/sessions/work/herdr.sock")));

        assert_ne!(default, named);
        assert_eq!(default, session_key(Some(OsStr::new("/home/me/.config/herdr/herdr.sock"))));
        assert_eq!(session_key(None), "cbf29ce484222325");
    }

    #[test]
    fn tmux_env_points_at_the_state_socket() {
        let state = State::new(PathBuf::from("/state/w1_p1-7"));

        assert_eq!(tmux_env(&state), OsString::from(format!("/state/w1_p1-7/tmux.sock,{},0", process::id())));
    }

    #[test]
    fn is_running_tells_live_and_finished_processes_apart() {
        assert!(is_running(i32::try_from(process::id()).unwrap()));
        assert!(is_running(1));
        assert!(!is_running(999_999_999));
    }
}
