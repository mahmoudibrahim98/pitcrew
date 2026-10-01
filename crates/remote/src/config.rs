//! The user's hosts, from `~/.ssh/config`.
//!
//! This only *lists* names. What a host resolves to (host name, user, port, jump hosts) is ssh's
//! business: ask it with [`crate::Ssh::resolve`], which runs `ssh -G`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// ssh's own limit on nested `Include`s.
const MAX_INCLUDE_DEPTH: usize = 16;
/// The most files read for one listing. Without cycles, includes can still fan out: 16 levels
/// of two includes each would be 2^16 reads.
const MAX_FILES: usize = 256;

/// Hosts found in an ssh config, and anything the reader skipped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostList {
    /// Concrete `Host` names, in file order, without duplicates.
    pub hosts: Vec<String>,
    /// Things worth telling the user: ignored `Match` blocks, unreadable includes.
    pub notes: Vec<String>,
}

/// The user's home directory: `HOME`, or `USERPROFILE` on Windows.
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    let var = |name| std::env::var_os(name).filter(|v| !v.is_empty());
    var("HOME")
        .or_else(|| {
            if cfg!(windows) {
                var("USERPROFILE")
            } else {
                None
            }
        })
        .map(PathBuf::from)
}

/// Lists hosts from `~/.ssh/config`. A missing file is an empty list.
#[must_use]
pub fn list_hosts() -> HostList {
    match home_dir() {
        Some(home) => list_hosts_in(&home.join(".ssh").join("config"), &home),
        None => HostList {
            hosts: Vec::new(),
            notes: vec!["no home directory is set".to_owned()],
        },
    }
}

/// Lists hosts from the config file at `path`. Relative `Include` paths resolve against
/// `home/.ssh`, and `~` expands to `home`, as they do in a user config.
#[must_use]
pub fn list_hosts_in(path: &Path, home: &Path) -> HostList {
    let mut reader = Reader {
        home,
        list: HostList::default(),
        seen: HashSet::new(),
        stack: vec![identity(path)],
        files: 1,
    };
    match fs::read_to_string(path) {
        Ok(text) => reader.read(&text, path, 0),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => reader
            .list
            .notes
            .push(format!("could not read {}: {e}", path.display())),
    }
    reader.list
}

/// A file's canonical path, so two spellings of one file compare equal.
fn identity(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

struct Reader<'a> {
    home: &'a Path,
    list: HostList,
    seen: HashSet<String>,
    /// The files being read, outermost first: including one of them again is a cycle.
    stack: Vec<PathBuf>,
    /// Files read so far.
    files: usize,
}

impl Reader<'_> {
    fn read(&mut self, text: &str, file: &Path, depth: usize) {
        for (index, line) in text.lines().enumerate() {
            let Some((keyword, args)) = split_line(line) else {
                continue;
            };
            match keyword.to_ascii_lowercase().as_str() {
                "host" => {
                    for pattern in args {
                        if is_concrete(&pattern) && self.seen.insert(pattern.clone()) {
                            self.list.hosts.push(pattern);
                        }
                    }
                }
                "match" => self.list.notes.push(format!(
                    "{}:{}: Match blocks are not listed",
                    file.display(),
                    index + 1
                )),
                "include" => {
                    if depth >= MAX_INCLUDE_DEPTH {
                        self.list.notes.push(format!(
                            "{}:{}: Include nested too deeply; skipped",
                            file.display(),
                            index + 1
                        ));
                        continue;
                    }
                    for pattern in args {
                        self.include(&pattern, depth);
                    }
                }
                _ => {}
            }
        }
    }

    fn include(&mut self, pattern: &str, depth: usize) {
        let path = if let Some(rest) = pattern.strip_prefix("~/") {
            self.home.join(rest)
        } else if Path::new(pattern).is_absolute() {
            PathBuf::from(pattern)
        } else {
            self.home.join(".ssh").join(pattern)
        };
        for file in expand_glob(&path) {
            let id = identity(&file);
            if self.stack.contains(&id) {
                self.list.notes.push(format!(
                    "{} includes itself; skipped the cycle",
                    file.display()
                ));
                continue;
            }
            if self.files >= MAX_FILES {
                self.list.notes.push(format!(
                    "more than {MAX_FILES} config files; skipped {}",
                    file.display()
                ));
                continue;
            }
            self.files += 1;
            match fs::read_to_string(&file) {
                Ok(text) => {
                    self.stack.push(id);
                    self.read(&text, &file, depth + 1);
                    self.stack.pop();
                }
                Err(e) => self
                    .list
                    .notes
                    .push(format!("could not read {}: {e}", file.display())),
            }
        }
    }
}

/// A name that stands for one host: no wildcards, not negated, usable on a command line.
fn is_concrete(pattern: &str) -> bool {
    !pattern.is_empty()
        && !pattern.starts_with('!')
        && !pattern.contains(['*', '?'])
        && crate::quote::validate_host(pattern).is_ok()
}

/// Splits a config line into its keyword and arguments, as ssh does: the keyword ends at
/// whitespace or `=`, and arguments are separated by whitespace, with `"…"` or `'…'` quoting.
fn split_line(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let keyword = line[..end].to_owned();
    let mut rest = line[end..].trim_start();
    if let Some(after) = rest.strip_prefix('=') {
        rest = after.trim_start();
    }
    Some((keyword, split_args(rest)))
}

fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(&first) = chars.peek() else {
            break;
        };
        if first == '#' {
            break;
        }
        let mut arg = String::new();
        let mut quote = None;
        while let Some(c) = chars.next() {
            match (quote, c) {
                (None, c) if c.is_whitespace() => break,
                (None, '"' | '\'') => quote = Some(c),
                (Some(q), c) if c == q => quote = None,
                (_, '\\') if matches!(chars.peek(), Some('"' | '\'' | '\\')) => {
                    if let Some(next) = chars.next() {
                        arg.push(next);
                    }
                }
                (_, c) => arg.push(c),
            }
        }
        args.push(arg);
    }
    args
}

/// Expands `*` and `?` in each path component, in sorted order, like `glob(3)`. Hidden entries
/// only match a component that starts with `.`.
fn expand_glob(path: &Path) -> Vec<PathBuf> {
    let mut found = vec![PathBuf::new()];
    for component in path.components() {
        let part = component.as_os_str();
        let Some(text) = part.to_str().filter(|t| t.contains(['*', '?'])) else {
            for base in &mut found {
                base.push(part);
            }
            continue;
        };
        let mut next = Vec::new();
        for base in &found {
            let Ok(entries) = fs::read_dir(base) else {
                continue;
            };
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .filter(|name| {
                    (!name.starts_with('.') || text.starts_with('.')) && wild(text, name)
                })
                .collect();
            names.sort();
            next.extend(names.into_iter().map(|name| base.join(name)));
        }
        found = next;
    }
    found.into_iter().filter(|p| p.is_file()).collect()
}

/// Matches `*` (any run) and `?` (one character).
fn wild(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn lists_concrete_hosts_through_includes() {
        let home = tempfile::tempdir().unwrap();
        let ssh = home.path().join(".ssh");
        write(
            &ssh.join("config"),
            "# comment\n\
             Include conf.d/*.conf\n\
             Host cluster cluster-login *.example.org !bastion\n  User someone\n\
             Host=\"quoted one\" plain\n\
             Match host foo exec \"true\"\n  User x\n\
             host box1 # trailing comment\n\
             Include ~/.ssh/extra\n\
             Include missing/*.conf\n",
        );
        write(&ssh.join("conf.d/b.conf"), "Host bravo\n");
        write(&ssh.join("conf.d/a.conf"), "Host alpha cluster\n");
        write(&ssh.join("conf.d/.hidden.conf"), "Host hidden\n");
        write(&ssh.join("conf.d/c.txt"), "Host not-included\n");
        write(&ssh.join("extra"), "HOST gpu?? gpu01\n");

        let list = list_hosts_in(&ssh.join("config"), home.path());
        assert_eq!(
            list.hosts,
            [
                "alpha",
                "cluster",
                "bravo",
                "cluster-login",
                "plain",
                "box1",
                "gpu01"
            ]
        );
        assert_eq!(list.notes.len(), 1, "{:?}", list.notes);
        assert!(list.notes[0].contains("Match"));
    }

    #[test]
    fn a_missing_config_is_empty() {
        let home = tempfile::tempdir().unwrap();
        let list = list_hosts_in(&home.path().join(".ssh/config"), home.path());
        assert_eq!(list, HostList::default());
    }

    #[test]
    fn include_cycles_are_skipped() {
        let home = tempfile::tempdir().unwrap();
        let ssh = home.path().join(".ssh");
        // Used to read 2^16 files.
        write(&ssh.join("config"), "Host a\nInclude config config\n");
        let list = list_hosts_in(&ssh.join("config"), home.path());
        assert_eq!(list.hosts, ["a"]);
        assert_eq!(list.notes.len(), 2, "{:?}", list.notes);
        assert!(list.notes.iter().all(|n| n.contains("includes itself")));

        // A longer cycle, through another spelling of the same file.
        write(&ssh.join("config"), "Host a\nInclude b\n");
        write(&ssh.join("b"), "Host b\nInclude ~/.ssh/./config\n");
        let list = list_hosts_in(&ssh.join("config"), home.path());
        assert_eq!(list.hosts, ["a", "b"]);
        assert_eq!(list.notes.len(), 1, "{:?}", list.notes);
    }

    #[test]
    fn fan_out_is_bounded() {
        let home = tempfile::tempdir().unwrap();
        let ssh = home.path().join(".ssh");
        write(&ssh.join("config"), "Include l1\n");
        for level in 1..16 {
            write(
                &ssh.join(format!("l{level}")),
                &format!("Host h{level}\nInclude l{0} l{0}\n", level + 1),
            );
        }
        write(&ssh.join("l16"), "Host h16\n");
        let list = list_hosts_in(&ssh.join("config"), home.path());
        assert_eq!(list.hosts.len(), 16);
        assert!(list.notes.iter().any(|n| n.contains("more than")));
    }

    #[test]
    fn wildcards_match() {
        assert!(wild("*.conf", "a.conf"));
        assert!(wild("a?c", "abc"));
        assert!(wild("*", ""));
        assert!(!wild("*.conf", "a.txt"));
        assert!(wild("a*b*c", "axxbyyc"));
    }
}
