use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rustix::process::geteuid;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub(crate) const ENV: &str = "HERDR_CLAUDE_CODE_AGENT_TEAMS_STATE";

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IdMap {
    next_pane: u32,
    next_window: u32,
    pub(crate) map: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) titles: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) routes: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub(crate) terminals: BTreeMap<String, String>,
}

impl IdMap {
    pub(crate) fn new(pane_id: &str, tab_id: &str, terminal_id: Option<&str>) -> Self {
        Self {
            next_pane: 1,
            next_window: 1,
            map: BTreeMap::from([("%0".into(), pane_id.into()), ("@0".into(), tab_id.into())]),
            titles: BTreeMap::new(),
            routes: BTreeMap::new(),
            terminals: terminal_id.map(|terminal| ("%0".into(), terminal.into())).into_iter().collect(),
        }
    }

    pub(crate) fn tmux_id_for(&mut self, herdr_id: &str, prefix: char) -> String {
        if let Some(tmux_id) = self
            .map
            .iter()
            .find(|(tmux_id, mapped)| *mapped == herdr_id && tmux_id.starts_with(prefix))
            .map(|(tmux_id, _)| tmux_id.clone())
        {
            return tmux_id;
        }
        let next = if prefix == '%' { &mut self.next_pane } else { &mut self.next_window };
        let tmux_id = format!("{prefix}{next}");
        *next += 1;
        self.map.insert(tmux_id.clone(), herdr_id.into());
        tmux_id
    }

    pub(crate) fn pane_alias(&mut self, pane_id: &str, terminal_id: Option<&str>) -> String {
        let known = terminal_id.and_then(|terminal| {
            self.terminals.iter().find(|(_, known)| *known == terminal).map(|(tmux_id, _)| tmux_id.clone())
        });
        if let Some(tmux_id) = known {
            self.relocate(&tmux_id, pane_id);
            return tmux_id;
        }
        let tmux_id = self.tmux_id_for(pane_id, '%');
        if let Some(terminal) = terminal_id {
            self.terminals.insert(tmux_id.clone(), terminal.into());
        }
        tmux_id
    }

    pub(crate) fn relocate(&mut self, tmux_id: &str, pane_id: &str) {
        let Some(mapped) = self.map.get_mut(tmux_id) else {
            return;
        };
        let previous = std::mem::replace(mapped, pane_id.into());
        if let Some(title) = self.titles.remove(&previous) {
            self.titles.insert(pane_id.into(), title);
        }
    }

    pub(crate) fn owns_terminal(&self, terminal_id: &str) -> bool {
        self.terminals.values().any(|known| known == terminal_id)
    }

    pub(crate) fn leader(&self) -> Option<&str> {
        self.map.get("%0").map(String::as_str)
    }

    pub(crate) fn owned_panes(&self) -> HashSet<&str> {
        self.map.iter().filter(|(tmux_id, _)| tmux_id.starts_with('%')).map(|(_, id)| id.as_str()).collect()
    }

    pub(crate) fn forget_tmux(&mut self, tmux_id: &str) {
        if let Some(herdr_id) = self.map.get(tmux_id).cloned() {
            self.forget(&herdr_id);
        }
    }

    pub(crate) fn forget(&mut self, herdr_id: &str) {
        let map = &self.map;
        self.terminals.retain(|tmux_id, _| map.get(tmux_id).is_none_or(|mapped| mapped != herdr_id));
        self.map.retain(|_, mapped| mapped != herdr_id);
        self.titles.remove(herdr_id);
    }
}

#[derive(Debug)]
pub(crate) struct State {
    dir: PathBuf,
}

impl State {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub(crate) fn from_env() -> Result<Self> {
        env::var_os(ENV).map(|dir| Self::new(dir.into())).with_context(|| format!("{ENV} is not set"))
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn socket(&self) -> PathBuf {
        self.dir.join("tmux.sock")
    }

    fn idmap_path(&self) -> PathBuf {
        self.dir.join("idmap.json")
    }

    fn lock_path(&self) -> PathBuf {
        self.dir.join("idmap.lock")
    }

    fn log_path(&self) -> PathBuf {
        self.dir.join("calls.log")
    }

    pub(crate) fn reset(&self, pane_id: &str, tab_id: &str, terminal_id: Option<&str>) -> Result<()> {
        create_private_dir(&self.dir)?;
        let idmap = IdMap::new(pane_id, tab_id, terminal_id);
        write_private(&self.idmap_path(), serde_json::to_string(&idmap)?.as_bytes())?;
        write_private(&self.log_path(), b"")
    }

    pub(crate) fn with_idmap<T>(&self, update: impl FnOnce(&mut IdMap) -> T) -> Result<T> {
        let lock = open_private(&self.lock_path())?;
        lock.lock().with_context(|| self.lock_path().display().to_string())?;

        let path = self.idmap_path();
        let mut text = String::new();
        File::open(&path)
            .and_then(|mut file| file.read_to_string(&mut text))
            .with_context(|| path.display().to_string())?;
        let original: IdMap = serde_json::from_str(&text).with_context(|| path.display().to_string())?;
        let mut idmap = original.clone();
        let output = update(&mut idmap);

        if idmap != original {
            let staging = self.dir.join("idmap.json.tmp");
            write_private(&staging, serde_json::to_string(&idmap)?.as_bytes())?;
            fs::rename(&staging, &path).with_context(|| path.display().to_string())?;
        }
        Ok(output)
    }

    pub(crate) fn log(&self, verb: &str, target: Option<&str>, exit: u8, message: &str) {
        let millis = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis());
        let line = json!({"t": millis, "verb": verb, "target": target, "exit": exit, "message": message});
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).mode(0o600).open(self.log_path()) {
            let _ = file.write_all(format!("{line}\n").as_bytes());
        }
    }
}

pub(crate) fn create_private_dir(dir: &Path) -> Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| dir.display().to_string())?;
    let metadata = fs::symlink_metadata(dir).with_context(|| dir.display().to_string())?;
    if !metadata.is_dir() || metadata.uid() != geteuid().as_raw() {
        bail!("{} must be a directory owned by the current user", dir.display());
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).with_context(|| dir.display().to_string())
}

fn open_private(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .with_context(|| path.display().to_string())
}

pub(crate) fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| path.display().to_string())?;
    file.write_all(contents).with_context(|| path.display().to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    pub(crate) fn temp_state() -> State {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = env::temp_dir().join(format!(
            "herdr-claude-code-agent-teams-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        State::new(dir)
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn new_idmap_maps_the_leader_to_pane_zero_and_window_zero() {
        let idmap = IdMap::new("w1:p1", "w1:t1", None);

        assert_eq!(idmap.leader(), Some("w1:p1"));
        assert_eq!(idmap.map.get("@0").map(String::as_str), Some("w1:t1"));
    }

    #[test]
    fn tmux_id_for_numbers_new_panes_and_reuses_known_ones() {
        let mut idmap = IdMap::new("w1:p1", "w1:t1", None);

        assert_eq!(idmap.tmux_id_for("w1:p2", '%'), "%1");
        assert_eq!(idmap.tmux_id_for("w1:p3", '%'), "%2");
        assert_eq!(idmap.tmux_id_for("w1:p2", '%'), "%1");
        assert_eq!(idmap.tmux_id_for("w1:p1", '%'), "%0");
        assert_eq!(idmap.tmux_id_for("w1:t2", '@'), "@1");
    }

    #[test]
    fn owned_panes_excludes_windows() {
        let idmap = IdMap::new("w1:p1", "w1:t1", None);

        assert_eq!(idmap.owned_panes(), HashSet::from(["w1:p1"]));
    }

    #[test]
    fn forget_removes_every_alias_and_the_title_of_the_pane() {
        let mut idmap = IdMap::new("w1:p1", "w1:t1", None);
        idmap.tmux_id_for("w1:p2", '%');
        idmap.titles.insert("w1:p2".into(), "alpha".into());

        idmap.forget("w1:p2");

        assert!(!idmap.owned_panes().contains("w1:p2"));
        assert!(!idmap.titles.contains_key("w1:p2"));
        assert!(idmap.owned_panes().contains("w1:p1"));
    }

    #[test]
    fn pane_alias_follows_a_terminal_that_moved_to_another_pane() {
        let mut idmap = IdMap::new("w1:p1", "w1:t1", Some("term_lead"));
        assert_eq!(idmap.pane_alias("w1:p2", Some("term_mate")), "%1");
        idmap.titles.insert("w1:p2".into(), "alpha".into());

        assert_eq!(idmap.pane_alias("w2:p1", Some("term_mate")), "%1");

        assert_eq!(idmap.map.get("%1").map(String::as_str), Some("w2:p1"));
        assert_eq!(idmap.titles.get("w2:p1").map(String::as_str), Some("alpha"));
        assert!(!idmap.titles.contains_key("w1:p2"));
        assert_eq!(idmap.pane_alias("w1:p5", Some("term_lead")), "%0");
        assert_eq!(idmap.leader(), Some("w1:p5"));
    }

    #[test]
    fn relocate_never_brings_back_a_forgotten_pane() {
        let mut idmap = IdMap::new("w1:p1", "w1:t1", None);
        idmap.pane_alias("w1:p2", Some("term_mate"));
        idmap.forget("w1:p2");

        idmap.relocate("%1", "w2:p1");

        assert_eq!(idmap.map.get("%1"), None);
    }

    #[test]
    fn pane_alias_without_a_terminal_id_falls_back_to_the_pane_id() {
        let mut idmap = IdMap::new("w1:p1", "w1:t1", None);

        assert_eq!(idmap.pane_alias("w1:p2", None), "%1");
        assert_eq!(idmap.pane_alias("w1:p2", None), "%1");
        assert!(idmap.terminals.is_empty());
    }

    #[test]
    fn forget_drops_the_terminal_of_the_pane() {
        let mut idmap = IdMap::new("w1:p1", "w1:t1", Some("term_lead"));
        idmap.pane_alias("w1:p2", Some("term_mate"));

        idmap.forget("w1:p2");

        assert!(!idmap.owns_terminal("term_mate"));
        assert!(idmap.owns_terminal("term_lead"));
    }

    #[test]
    fn an_idmap_written_before_terminal_ids_were_tracked_still_loads() {
        let idmap: IdMap =
            serde_json::from_str(r#"{"next_pane":1,"next_window":1,"map":{"%0":"w1:p1","@0":"w1:t1"}}"#)
                .unwrap();

        assert_eq!(idmap.leader(), Some("w1:p1"));
        assert!(idmap.terminals.is_empty());
    }

    #[test]
    fn create_private_dir_refuses_a_symlink() {
        let state = temp_state();
        create_private_dir(state.dir()).unwrap();
        let link = state.dir().join("link");
        std::os::unix::fs::symlink(env::temp_dir(), &link).unwrap();

        assert!(create_private_dir(&link).is_err());
        fs::remove_dir_all(state.dir()).unwrap();
    }

    #[test]
    fn reset_creates_files_only_the_owner_can_read() {
        let state = temp_state();

        state.reset("w1:p1", "w1:t1", None).unwrap();

        assert_eq!(mode(state.dir()), 0o700);
        assert_eq!(mode(&state.idmap_path()), 0o600);
        assert_eq!(mode(&state.log_path()), 0o600);
        fs::remove_dir_all(state.dir()).unwrap();
    }

    #[test]
    fn with_idmap_persists_changes_between_calls() {
        let state = temp_state();
        state.reset("w1:p1", "w1:t1", None).unwrap();

        let assigned = state.with_idmap(|idmap| idmap.tmux_id_for("w1:p2", '%')).unwrap();
        let reloaded = state.with_idmap(|idmap| idmap.map.get("%1").cloned()).unwrap();

        assert_eq!(assigned, "%1");
        assert_eq!(reloaded.as_deref(), Some("w1:p2"));
        assert_eq!(mode(&state.idmap_path()), 0o600);
        fs::remove_dir_all(state.dir()).unwrap();
    }

    #[test]
    fn with_idmap_does_not_rewrite_the_file_for_lookups() {
        let state = temp_state();
        state.reset("w1:p1", "w1:t1", None).unwrap();
        let before = fs::metadata(state.idmap_path()).unwrap().ino();

        state.with_idmap(|idmap| idmap.leader().map(str::to_owned)).unwrap();

        assert_eq!(fs::metadata(state.idmap_path()).unwrap().ino(), before);
        fs::remove_dir_all(state.dir()).unwrap();
    }

    #[test]
    fn log_records_the_verb_and_target_as_one_json_line() {
        let state = temp_state();
        state.reset("w1:p1", "w1:t1", None).unwrap();

        state.log("respawn-pane", Some("%1"), 0, "");
        state.log("kill-pane", Some("%9"), 1, "can't find pane: %9");

        let lines: Vec<serde_json::Value> = fs::read_to_string(state.log_path())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1]["verb"], json!("kill-pane"));
        assert_eq!(lines[1]["target"], json!("%9"));
        assert_eq!(lines[1]["exit"], json!(1));
        assert_eq!(lines[1]["message"], json!("can't find pane: %9"));
        fs::remove_dir_all(state.dir()).unwrap();
    }
}
