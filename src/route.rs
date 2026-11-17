use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::shell;

const MAX_NAME_LENGTH: usize = 64;
const CGROUP_PREFIX: &str = r#"sh -c '{ echo $PPID > "$1"/cgroup.procs; } 2>/dev/null' sh "#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rule {
    Inside,
    Beside,
}

impl fmt::Display for Rule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
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

fn find_exact(name: &str, lead_dir: &Path) -> Option<(PathBuf, Rule)> {
    let inside = lead_dir.join(name);
    if is_repository(&inside) {
        return Some((inside, Rule::Inside));
    }
    let beside = lead_dir.parent()?.join(name);
    (beside != lead_dir && is_repository(&beside)).then_some((beside, Rule::Beside))
}

fn find(name: &str, lead_dir: &Path) -> Option<Route> {
    if !is_teammate_name(name) {
        return None;
    }
    let (dir, rule) =
        find_exact(name, lead_dir).or_else(|| find_exact(without_collision_suffix(name)?, lead_dir))?;
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

fn agent_name(command: &str) -> Option<String> {
    let mut words = Vec::new();
    let mut rest = command.trim_start();
    while !rest.is_empty() {
        let (word, length) = shell::read_word(rest)?;
        words.push(word);
        rest = rest[length..].trim_start();
    }
    words.windows(2).find(|pair| pair[0] == "--agent-name").map(|pair| pair[1].clone())
}

pub(crate) fn reroute(line: &str) -> Option<(String, Route)> {
    let launch = parse_launch(line)?;
    let name = agent_name(&line[launch.rest_at + " && ".len()..])?;
    let route = find(&name, Path::new(&launch.dir))?;
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

    #[test]
    fn find_prefers_a_repository_inside_to_one_beside() {
        let tree = Tree::new(&["work/api", "api", "web"], &[]);
        let lead = tree.path("work");
        let found = |name: &str| find(name, &lead).map(|route| (route.dir, route.rule));

        assert_eq!(found("api"), Some((tree.path("work/api"), Rule::Inside)));
        assert_eq!(found("web"), Some((tree.path("web"), Rule::Beside)));
        assert_eq!(found("researcher"), None);
    }

    #[test]
    fn find_falls_back_to_the_name_without_claude_codes_collision_suffix() {
        let tree = Tree::new(&["work/api", "work/api-v2"], &[]);
        let lead = tree.path("work");
        let found = |name: &str| find(name, &lead).map(|route| route.dir);

        assert_eq!(found("api-2"), Some(tree.path("work/api")));
        assert_eq!(found("api-v2"), Some(tree.path("work/api-v2")));
        assert_eq!(found("api-"), None);
    }

    #[test]
    fn find_resolves_symlinks_to_the_real_directory() {
        let tree = Tree::new(&["real/api"], &["work"]);
        std::os::unix::fs::symlink(tree.path("real/api"), tree.path("work/api")).unwrap();

        assert_eq!(find("api", &tree.path("work")).map(|route| route.dir), Some(tree.path("real/api")));
    }

    #[test]
    fn find_ignores_names_claude_code_would_never_give_a_teammate() {
        let tree = Tree::new(&["work/next.js", "work/a b", "x"], &[]);
        let lead = tree.path("work");

        for name in ["next.js", "a b", "..", ".", "", "../x", "-x"] {
            assert_eq!(find(name, &lead), None, "{name}");
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

        let (line, route) = reroute(&launch(&tree.path("work"), "api")).unwrap();

        assert_eq!(line, launch(&tree.path("work/api"), "api"));
        assert_eq!(route.name, "api");
    }

    #[test]
    fn reroute_keeps_a_cgroup_prefix() {
        let tree = Tree::new(&["work/api"], &[]);
        let prefix = format!("{CGROUP_PREFIX}/sys/fs/cgroup/x; ");

        let (line, _) = reroute(&format!("{prefix}{}", launch(&tree.path("work"), "api"))).unwrap();

        assert_eq!(line, format!("{prefix}{}", launch(&tree.path("work/api"), "api")));
    }

    #[test]
    fn reroute_reads_the_agent_name_as_a_whole_word() {
        let tree = Tree::new(&["work/api", "work/web"], &[]);
        let line = format!(
            "cd {} && env NOTE='x --agent-name web' claude --agent-name api",
            tree.path("work").display()
        );

        let (_, route) = reroute(&line).unwrap();

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
            assert_eq!(reroute(&line), None, "{line}");
        }
    }

    #[test]
    fn reroute_leaves_unmatched_or_unfamiliar_commands_alone() {
        let tree = Tree::new(&["work/api"], &[]);

        assert_eq!(reroute(&launch(&tree.path("work"), "researcher")), None);
        assert_eq!(reroute("claude --agent-name api"), None);
        assert_eq!(reroute("cd $HOME && claude --agent-name api"), None);
        assert_eq!(reroute(&launch(&tree.path("work/api"), "api")), None);
    }
}
