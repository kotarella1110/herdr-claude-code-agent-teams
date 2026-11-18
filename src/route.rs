use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use crate::shell;
use crate::state::State;

const MAX_NAME_LENGTH: usize = 64;
const MAX_CANDIDATES: usize = 50;
const CGROUP_PREFIX: &str = r#"sh -c '{ echo $PPID > "$1"/cgroup.procs; } 2>/dev/null' sh "#;

pub(crate) type Registered = BTreeMap<String, PathBuf>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rule {
    Registered,
    Inside,
    Beside,
}

impl fmt::Display for Rule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Registered => "registered by the lead",
            Self::Inside => "repository inside the lead's directory",
            Self::Beside => "repository next to the lead's directory",
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Route {
    pub(crate) name: String,
    pub(crate) dir: PathBuf,
    pub(crate) rule: Rule,
}

fn is_teammate_name(name: &str) -> bool {
    name.len() <= MAX_NAME_LENGTH
        && name.chars().next().is_some_and(|first| first.is_ascii_alphanumeric())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn is_repository(dir: &Path) -> bool {
    dir.is_dir() && dir.join(".git").exists()
}

fn without_collision_suffix(name: &str) -> Option<&str> {
    let (base, suffix) = name.rsplit_once('-')?;
    (!base.is_empty() && !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()))
        .then_some(base)
}

fn find_exact(name: &str, lead_dir: &Path, registered: &Registered) -> Option<(PathBuf, Rule)> {
    if let Some(dir) = registered.get(name).filter(|dir| dir.is_dir()) {
        return Some((dir.clone(), Rule::Registered));
    }
    let inside = lead_dir.join(name);
    if is_repository(&inside) {
        return Some((inside, Rule::Inside));
    }
    let beside = lead_dir.parent()?.join(name);
    (beside != lead_dir && is_repository(&beside)).then_some((beside, Rule::Beside))
}

fn find(name: &str, lead_dir: &Path, registered: &Registered) -> Option<Route> {
    if !is_teammate_name(name) {
        return None;
    }
    let (dir, rule) = find_exact(name, lead_dir, registered)
        .or_else(|| find_exact(without_collision_suffix(name)?, lead_dir, registered))?;
    let dir = fs::canonicalize(dir).ok()?;
    Some(Route { name: name.to_owned(), dir, rule })
}

struct Launch {
    cd_at: usize,
    dir: String,
    rest_at: usize,
}

fn parse_launch(line: &str) -> Option<Launch> {
    let cd_at = match line.strip_prefix(CGROUP_PREFIX) {
        Some(after_prefix) => {
            let (_, length) = shell::read_word(after_prefix)?;
            let separator_at = CGROUP_PREFIX.len() + length;
            line[separator_at..].starts_with("; ").then_some(separator_at + "; ".len())?
        }
        None => 0,
    };
    if !line[cd_at..].starts_with("cd ") {
        return None;
    }
    let word_at = cd_at + "cd ".len();
    let (dir, length) = shell::read_word(&line[word_at..])?;
    let rest_at = word_at + length;
    line[rest_at..].starts_with(" && ").then_some(Launch { cd_at, dir, rest_at })
}

fn words(command: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut rest = command.trim_start();
    while !rest.is_empty() {
        let (word, length) = shell::read_word(rest)?;
        words.push(word);
        rest = rest[length..].trim_start();
    }
    Some(words)
}

fn flag_value(words: &[String], flag: &str) -> Option<String> {
    words.iter().enumerate().find_map(|(index, word)| match word.strip_prefix(flag)? {
        "" => words.get(index + 1).cloned(),
        attached => attached.strip_prefix('=').map(str::to_owned),
    })
}

fn agent_name(command: &str) -> Option<String> {
    flag_value(&words(command)?, "--agent-name")
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Teammate {
    pub(crate) name: String,
    pub(crate) team: Option<String>,
    pub(crate) model: Option<String>,
}

pub(crate) fn teammate(line: &str) -> Option<Teammate> {
    let launch = parse_launch(line)?;
    let words = words(&line[launch.rest_at + " && ".len()..])?;
    Some(Teammate {
        name: flag_value(&words, "--agent-name")?,
        team: flag_value(&words, "--team-name"),
        model: flag_value(&words, "--model"),
    })
}

pub(crate) fn reroute(line: &str, registered: &Registered) -> Option<(String, Route)> {
    let launch = parse_launch(line)?;
    let name = agent_name(&line[launch.rest_at + " && ".len()..])?;
    let route = find(&name, Path::new(&launch.dir), registered)?;
    if fs::canonicalize(&launch.dir).is_ok_and(|lead_dir| lead_dir == route.dir) {
        return None;
    }
    let rewritten = format!(
        "{}cd {}{}",
        &line[..launch.cd_at],
        shell::quote(route.dir.to_str()?),
        &line[launch.rest_at..]
    );
    Some((rewritten, route))
}

pub(crate) fn register(args: &[String]) -> ExitCode {
    match register_route(args) {
        Ok((name, dir)) => {
            println!("teammate {name} will start in {}", dir.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("herdr-claude-code-agent-teams: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn register_route(args: &[String]) -> Result<(String, PathBuf)> {
    let [name, dir] = args else {
        bail!("usage: herdr-claude-code-agent-teams route <teammate-name> <directory>");
    };
    if !is_teammate_name(name) {
        bail!(
            "{name:?} is not a teammate name; use up to {MAX_NAME_LENGTH} letters, digits, hyphens and underscores"
        );
    }
    let dir = fs::canonicalize(dir).with_context(|| format!("{dir} is not an existing directory"))?;
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    if dir.to_str().is_none() {
        bail!("{} is not valid UTF-8", dir.display());
    }
    let state =
        State::from_env().context("run this from the Bash tool of a team lead started by this plugin")?;
    state.with_idmap(|idmap| idmap.routes.insert(name.clone(), dir.clone()))?;
    Ok((name.clone(), dir))
}

fn repositories_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| is_repository(&entry.path()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| is_teammate_name(name))
        .collect();
    names.sort();
    names
}

fn candidates(lead_dir: &Path) -> Vec<String> {
    let mut names = repositories_in(lead_dir);
    if let Some(parent) = lead_dir.parent() {
        for name in repositories_in(parent) {
            if parent.join(&name) != lead_dir && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names.truncate(MAX_CANDIDATES);
    names
}

pub(crate) fn hint(lead_dir: &Path, launcher: &Path) -> String {
    let command = format!("{} route", shell::quote(&launcher.to_string_lossy()));
    let nearby = match candidates(lead_dir).as_slice() {
        [] => String::new(),
        names => format!(
            " A teammate named after one of these repositories starts there without that step: {}.",
            names.join(", ")
        ),
    };
    format!(
        "Teammates you spawn start in your working directory, so they read its CLAUDE.md and skills. \
         To have a teammate start in another directory, such as another repository, run \
         `{command} <teammate-name> <directory>` with the Bash tool before spawning it, \
         then spawn the teammate with exactly that name.{nearby} \
         When work spans repositories, give each repository its own teammate."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Tree(PathBuf);

    impl Tree {
        fn new(repositories: &[&str], folders: &[&str]) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let root = env::temp_dir().join(format!(
                "herdr-claude-code-agent-teams-route-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            for repository in repositories {
                fs::create_dir_all(root.join(repository).join(".git")).unwrap();
            }
            for folder in folders {
                fs::create_dir_all(root.join(folder)).unwrap();
            }
            Self(fs::canonicalize(root).unwrap())
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.0.join(relative)
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn launch(dir: &Path, name: &str) -> String {
        format!(
            "cd {} && env CLAUDECODE=1 /opt/claude --agent-id {name}@team --agent-name {} --team-name team",
            shell::quote(&dir.to_string_lossy()),
            shell::quote(name)
        )
    }

    fn nothing_registered() -> Registered {
        Registered::new()
    }

    #[test]
    fn find_uses_a_registered_directory_then_a_repository_inside_then_one_beside() {
        let tree = Tree::new(&["work/api", "api", "web"], &["elsewhere/api"]);
        let lead = tree.path("work");
        let registered = Registered::from([
            ("api".into(), tree.path("elsewhere/api")),
            ("web".into(), tree.path("missing")),
        ]);
        let found = |name: &str, registered: &Registered| {
            find(name, &lead, registered).map(|route| (route.dir, route.rule))
        };

        assert_eq!(found("api", &registered), Some((tree.path("elsewhere/api"), Rule::Registered)));
        assert_eq!(found("api", &nothing_registered()), Some((tree.path("work/api"), Rule::Inside)));
        assert_eq!(found("web", &registered), Some((tree.path("web"), Rule::Beside)));
        assert_eq!(found("researcher", &registered), None);
    }

    #[test]
    fn find_falls_back_to_the_name_without_claude_codes_collision_suffix() {
        let tree = Tree::new(&["work/api", "work/api-v2"], &[]);
        let lead = tree.path("work");
        let found = |name: &str| find(name, &lead, &nothing_registered()).map(|route| route.dir);

        assert_eq!(found("api-2"), Some(tree.path("work/api")));
        assert_eq!(found("api-v2"), Some(tree.path("work/api-v2")));
        assert_eq!(found("api-"), None);
    }

    #[test]
    fn find_resolves_symlinks_to_the_real_directory() {
        let tree = Tree::new(&["real/api"], &["work"]);
        std::os::unix::fs::symlink(tree.path("real/api"), tree.path("work/api")).unwrap();

        assert_eq!(
            find("api", &tree.path("work"), &nothing_registered()).map(|route| route.dir),
            Some(tree.path("real/api"))
        );
    }

    #[test]
    fn find_ignores_names_claude_code_would_never_give_a_teammate() {
        let tree = Tree::new(&["work/next.js", "work/a b", "x"], &[]);
        let lead = tree.path("work");

        for name in ["next.js", "a b", "..", ".", "", "../x", "-x"] {
            assert_eq!(find(name, &lead, &nothing_registered()), None, "{name}");
        }
    }

    #[test]
    fn is_teammate_name_accepts_what_claude_code_accepts() {
        for name in ["api", "web-2", "a_b", "A1"] {
            assert!(is_teammate_name(name), "{name}");
        }
        for name in ["", "-api", "a/b", "..", "a b", "日本", &"a".repeat(65)] {
            assert!(!is_teammate_name(name), "{name}");
        }
    }

    #[test]
    fn reroute_rewrites_only_the_cd_target() {
        let tree = Tree::new(&["work/api"], &[]);

        let (line, route) = reroute(&launch(&tree.path("work"), "api"), &nothing_registered()).unwrap();

        assert_eq!(line, launch(&tree.path("work/api"), "api"));
        assert_eq!(route.name, "api");
    }

    #[test]
    fn reroute_keeps_a_cgroup_prefix() {
        let tree = Tree::new(&["work/api"], &[]);
        let prefix = format!("{CGROUP_PREFIX}/sys/fs/cgroup/x; ");

        let (line, _) =
            reroute(&format!("{prefix}{}", launch(&tree.path("work"), "api")), &nothing_registered())
                .unwrap();

        assert_eq!(line, format!("{prefix}{}", launch(&tree.path("work/api"), "api")));
    }

    #[test]
    fn reroute_reads_the_agent_name_as_a_whole_word() {
        let tree = Tree::new(&["work/api", "work/web"], &[]);
        let line = format!(
            "cd {} && env NOTE='x --agent-name web' claude --agent-name api",
            tree.path("work").display()
        );

        let (_, route) = reroute(&line, &nothing_registered()).unwrap();

        assert_eq!(route.dir, tree.path("work/api"));
    }

    #[test]
    fn reroute_never_rewrites_a_cd_inside_a_later_argument() {
        let tree = Tree::new(&["x/api"], &[]);
        let x = tree.path("x");

        for line in [
            format!("cd /w/a*b && env X='q; cd {} && y' claude --agent-name api", x.display()),
            format!("true; cd {} && claude --agent-name api", x.display()),
        ] {
            assert_eq!(reroute(&line, &nothing_registered()), None, "{line}");
        }
    }

    #[test]
    fn reroute_leaves_unmatched_or_unfamiliar_commands_alone() {
        let tree = Tree::new(&["work/api"], &[]);

        assert_eq!(reroute(&launch(&tree.path("work"), "researcher"), &nothing_registered()), None);
        assert_eq!(reroute("claude --agent-name api", &nothing_registered()), None);
        assert_eq!(reroute("cd $HOME && claude --agent-name api", &nothing_registered()), None);
        assert_eq!(reroute(&launch(&tree.path("work/api"), "api"), &nothing_registered()), None);
    }

    #[test]
    fn teammate_reads_the_name_team_and_model_from_the_launch_line() {
        let line =
            format!("{CGROUP_PREFIX}/sys/fs/cgroup/x; {} --model haiku", launch(Path::new("/w"), "api"));

        assert_eq!(
            teammate(&line),
            Some(Teammate { name: "api".into(), team: Some("team".into()), model: Some("haiku".into()) })
        );
        assert_eq!(
            teammate("cd /w && claude --agent-name api"),
            Some(Teammate { name: "api".into(), team: None, model: None })
        );
        assert_eq!(
            teammate("cd /w && claude --agent-name=-x --team-name=-core --model 'opus[1m]'"),
            Some(Teammate { name: "-x".into(), team: Some("-core".into()), model: Some("opus[1m]".into()) })
        );
        assert_eq!(teammate("cd /w && claude --team-name team"), None);
        assert_eq!(teammate("claude --agent-name api"), None);
    }

    #[test]
    fn candidates_list_repositories_inside_before_those_beside_and_skip_impossible_names() {
        let tree = Tree::new(&["work/zeta", "work/next.js", "alpha", "work", "my repo"], &["work/docs"]);

        assert_eq!(candidates(&tree.path("work")), ["zeta", "alpha"]);
    }

    #[test]
    fn hint_names_the_route_command_and_nearby_repositories() {
        let tree = Tree::new(&["work/api", "web", "work"], &[]);

        let hint = hint(&tree.path("work"), Path::new("/opt/teams bin/teams"));

        assert!(hint.contains("`'/opt/teams bin/teams' route <teammate-name> <directory>`"), "{hint}");
        assert!(hint.contains("without that step: api, web."), "{hint}");
    }

    #[test]
    fn hint_without_nearby_repositories_still_names_the_route_command() {
        let tree = Tree::new(&[], &["lonely/inner"]);

        let hint = hint(&tree.path("lonely/inner"), Path::new("/opt/teams"));

        assert!(hint.contains("`/opt/teams route <teammate-name> <directory>`"), "{hint}");
        assert!(!hint.contains("without that step"), "{hint}");
    }
}
