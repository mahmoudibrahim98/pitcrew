//! Site recipes: what one cluster needs for the helper's job. [`generic`] is built in; users
//! add their own as TOML files in `~/.pitcrew/sites/` on the laptop ([`sites_dir`],
//! [`load_sites`]).
//!
//! A recipe file is read strictly: an unknown key, a table, or a value of the wrong type is an
//! error, and every value is checked as [`super::JobSpec::new`] checks it. See
//! `example-site.toml` beside this file, and the crate's README.
//!
//! **A recipe is trusted like a shell script the user runs.** Its `modules_init` script is
//! sourced in the job and its modules are loaded there, so a recipe can run code as the user
//! on the cluster. The checks keep its values from breaking the job script or changing which job
//! PitCrew acts on; they do not make a recipe from someone else safe to use unread.

use super::spec::{self, JobOptions, WallTime};
use crate::helper::HelperError;
use std::fmt;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// The largest recipe file read.
pub const MAX_SITE_FILE: u64 = 64 * 1024;

/// The keys a recipe file may have.
pub const SITE_KEYS: &[&str] = &[
    "description",
    "partition",
    "account",
    "qos",
    "time",
    "cpus",
    "memory",
    "gres",
    "sbatch",
    "modules_init",
    "modules",
    "last_hop",
    "socket",
];

/// How a connection from the laptop gets from the login node to the compute node the job runs
/// on. The tunnel (a later piece of work) follows it; the launcher records it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LastHop {
    /// `ssh <node>` from the login node, on sites that let users log in to nodes where they
    /// have a job. In a recipe: `"ssh"`.
    #[default]
    Ssh,
    /// `srun --jobid <job> --overlap …` from the login node, inside the job's own allocation,
    /// on sites that forbid ssh to compute nodes. Needs SLURM 20.11 or newer
    /// ([`crate::probe::SlurmTools::srun_overlap`]). In a recipe: `"srun"`.
    SrunOverlap,
}

impl LastHop {
    /// Its name in a recipe file.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ssh => "ssh",
            Self::SrunOverlap => "srun",
        }
    }
}

/// Where the helper's socket goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SocketPlace {
    /// `<root>/run/pitcrewd.sock`, in the home directory, which may be on a network
    /// filesystem. In a recipe: `"root"`.
    #[default]
    Root,
    /// `$TMPDIR/pitcrew-<job>.<pid>/pitcrewd.sock` on the compute node's own disk (`/tmp` when
    /// `$TMPDIR` is unset or not a plain path), for sites where sockets on the shared home are
    /// a problem. The tunnel then reaches it through a relay on the node. In a recipe:
    /// `"node-local"`.
    NodeLocal,
}

impl SocketPlace {
    /// Its name in a recipe file, and in the job script.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::NodeLocal => "node-local",
        }
    }
}

/// What a cluster needs for the helper's job. Values are checked when a [`super::JobSpec`] is
/// made from it, whoever wrote the recipe, but the recipe is trusted like a shell script the
/// user runs: its modules and their set-up script run as the user (see the module docs).
pub trait SiteRecipe: fmt::Debug + Send + Sync {
    /// Its name: 1 to 64 characters of `a-z 0-9 _ -`.
    fn name(&self) -> &str;

    /// One line for the user.
    fn description(&self) -> &str {
        ""
    }

    /// Job options the user's choices override, and extra `#SBATCH` options.
    fn defaults(&self) -> JobOptions {
        JobOptions::default()
    }

    /// Environment modules the job loads before it starts the helper.
    fn modules(&self) -> &[String] {
        &[]
    }

    /// The script sourced first to get the `module` command, where `/bin/sh` does not have it.
    fn modules_init(&self) -> Option<&str> {
        None
    }

    /// How the compute node is reached from the login node.
    fn last_hop(&self) -> LastHop {
        LastHop::Ssh
    }

    /// Where the helper's socket goes.
    fn socket(&self) -> SocketPlace {
        SocketPlace::Root
    }
}

/// A recipe as data: the built-in [`generic`] one, or one read from a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Site {
    /// Its name; for a file, the file's name without `.toml`.
    pub name: String,
    /// One line for the user.
    pub description: String,
    /// Job defaults and extra `#SBATCH` options.
    pub defaults: JobOptions,
    /// Modules to load.
    pub modules: Vec<String>,
    /// The script that sets up `module`.
    pub modules_init: Option<String>,
    /// How the compute node is reached.
    pub last_hop: LastHop,
    /// Where the socket goes.
    pub socket: SocketPlace,
}

impl SiteRecipe for Site {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn defaults(&self) -> JobOptions {
        self.defaults.clone()
    }

    fn modules(&self) -> &[String] {
        &self.modules
    }

    fn modules_init(&self) -> Option<&str> {
        self.modules_init.as_deref()
    }

    fn last_hop(&self) -> LastHop {
        self.last_hop
    }

    fn socket(&self) -> SocketPlace {
        self.socket
    }
}

/// The built-in recipe for any SLURM cluster: SLURM's own defaults (the user picks partition,
/// account and time), no modules, the socket under the root, and `ssh` to the node.
#[must_use]
pub fn generic() -> Site {
    Site {
        name: "generic".to_owned(),
        description: "Any SLURM cluster: the scheduler's defaults, the socket in ~/.pitcrew/run, \
                      ssh from the login node to the compute node"
            .to_owned(),
        ..Site::default()
    }
}

/// The recipes built in: [`generic`].
#[must_use]
pub fn builtin_sites() -> Vec<Site> {
    vec![generic()]
}

/// A recipe that could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("site recipe {file}: {why}")]
pub struct SiteError {
    /// The file, or the recipe's name.
    pub file: String,
    /// What is wrong, naming the key.
    pub why: String,
}

impl Site {
    /// Reads a recipe named `name` from TOML `text`. Strict: an unknown key, a table, a value
    /// of the wrong type, or a value that could not go into the job script is an error.
    ///
    /// # Errors
    /// [`SiteError`] naming the key and the problem.
    pub fn from_toml(name: &str, text: &str) -> Result<Self, SiteError> {
        let fail = |why: String| SiteError {
            file: name.to_owned(),
            why,
        };
        spec::check_site_name(name).map_err(|e| fail(e.to_string()))?;
        if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_SITE_FILE {
            return Err(fail(format!("is larger than {MAX_SITE_FILE} bytes")));
        }
        let doc = toml_edit::Document::parse(text).map_err(|e| {
            let line = e
                .span()
                .and_then(|span| text.get(..span.start))
                .map(|before| before.matches('\n').count() + 1);
            let message = e.message().replace(['\n', '\r'], " ");
            fail(match line {
                Some(line) => format!("is not valid TOML: line {line}: {message}"),
                None => format!("is not valid TOML: {message}"),
            })
        })?;
        let mut site = Site {
            name: name.to_owned(),
            ..Site::default()
        };
        for (key, item) in doc.iter() {
            if !SITE_KEYS.contains(&key) {
                return Err(fail(format!(
                    "has an unknown key {:?}; a recipe may have {}",
                    crate::helper::script::clean(key),
                    SITE_KEYS.join(", ")
                )));
            }
            let wrong = |kind: &str| fail(format!("{key} must be {kind}"));
            let string = || {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| wrong("a string"))
            };
            let strings = || -> Result<Vec<String>, SiteError> {
                let array = item.as_array().ok_or_else(|| wrong("a list of strings"))?;
                array
                    .iter()
                    .map(|v| v.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| wrong("a list of strings"))
            };
            match key {
                "description" => site.description = string()?,
                "partition" => site.defaults.partition = Some(string()?),
                "account" => site.defaults.account = Some(string()?),
                "qos" => site.defaults.qos = Some(string()?),
                "memory" => site.defaults.memory = Some(string()?),
                "gres" => site.defaults.gres = Some(string()?),
                "time" => {
                    let text = string()?;
                    match spec::parse_wall_time(&text) {
                        Some(WallTime::Limited(time)) => site.defaults.time = Some(time),
                        _ => {
                            return Err(wrong(
                                "a SLURM time such as \"08:00:00\" or \"2-00:00:00\"",
                            ));
                        }
                    }
                }
                "cpus" => {
                    let cpus = item
                        .as_integer()
                        .and_then(|n| u32::try_from(n).ok())
                        .ok_or_else(|| wrong("a whole number"))?;
                    site.defaults.cpus = Some(cpus);
                }
                "sbatch" => site.defaults.sbatch = strings()?,
                "modules" => site.modules = strings()?,
                "modules_init" => site.modules_init = Some(string()?),
                "last_hop" => {
                    site.last_hop = match string()?.as_str() {
                        "ssh" => LastHop::Ssh,
                        "srun" => LastHop::SrunOverlap,
                        _ => return Err(wrong("\"ssh\" or \"srun\"")),
                    }
                }
                "socket" => {
                    site.socket = match string()?.as_str() {
                        "root" => SocketPlace::Root,
                        "node-local" => SocketPlace::NodeLocal,
                        _ => return Err(wrong("\"root\" or \"node-local\"")),
                    }
                }
                _ => return Err(fail(format!("has an unknown key {key:?}"))),
            }
        }
        site.check().map_err(|e| fail(e.to_string()))?;
        Ok(site)
    }

    /// Checks every value, as [`super::JobSpec::new`] would.
    ///
    /// # Errors
    /// [`HelperError::InvalidArgument`] naming the first bad one.
    pub fn check(&self) -> Result<(), HelperError> {
        spec::check_site_name(&self.name)?;
        if self.description.len() > 200 || self.description.chars().any(char::is_control) {
            return Err(HelperError::InvalidArgument(
                "the description must be one line of at most 200 bytes".to_owned(),
            ));
        }
        self.defaults.check()?;
        for module in &self.modules {
            spec::check_module(module)?;
        }
        if let Some(init) = &self.modules_init {
            spec::check_modules_init(init)?;
        }
        Ok(())
    }
}

/// Where the user's own recipes live: `~/.pitcrew/sites` on this machine (the laptop).
#[must_use]
pub fn sites_dir() -> Option<PathBuf> {
    crate::config::home_dir().map(|home| home.join(".pitcrew").join("sites"))
}

/// Reads the recipe file `path` (`<name>.toml`, at most [`MAX_SITE_FILE`] bytes).
///
/// # Errors
/// [`SiteError`] when it cannot be read or is not a valid recipe.
pub fn load_site(path: &Path) -> Result<Site, SiteError> {
    let fail = |why: String| SiteError {
        file: path.display().to_string(),
        why,
    };
    let name = match (path.file_stem(), path.extension()) {
        (Some(stem), Some(ext)) if ext == "toml" => stem.to_str().unwrap_or(""),
        _ => return Err(fail("is not a .toml file".to_owned())),
    };
    let file = std::fs::File::open(path).map_err(|e| fail(e.to_string()))?;
    let mut text = String::new();
    file.take(MAX_SITE_FILE + 1)
        .read_to_string(&mut text)
        .map_err(|e| fail(e.to_string()))?;
    Site::from_toml(name, &text).map_err(|e| fail(e.why))
}

/// Reads every `*.toml` in `dir` (normally [`sites_dir`]), in name order. A missing directory
/// has none; each file that fails is reported on its own.
#[must_use]
pub fn load_sites(dir: &Path) -> Vec<Result<Site, SiteError>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            return vec![Err(SiteError {
                file: dir.display().to_string(),
                why: e.to_string(),
            })];
        }
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml") && path.is_file())
        .collect();
    paths.sort();
    paths.iter().map(|path| load_site(path)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const EXAMPLE: &str = include_str!("example-site.toml");

    #[test]
    fn the_example_recipe_reads() {
        let site = Site::from_toml("example-cluster", EXAMPLE).unwrap();
        assert_eq!(site.name, "example-cluster");
        assert_eq!(site.defaults.partition.as_deref(), Some("gpu"));
        assert_eq!(site.defaults.account.as_deref(), Some("proj0001"));
        assert_eq!(site.defaults.time, Some(Duration::from_secs(8 * 3600)));
        assert_eq!(site.defaults.cpus, Some(2));
        assert_eq!(site.defaults.memory.as_deref(), Some("8G"));
        assert_eq!(site.defaults.gres.as_deref(), Some("gpu:1"));
        assert_eq!(site.defaults.sbatch, ["--constraint=a100", "--nodes=1"]);
        assert_eq!(
            site.modules_init.as_deref(),
            Some("/etc/profile.d/modules.sh")
        );
        assert_eq!(site.modules, ["example-toolchain/1.0", "nodejs/22"]);
        assert_eq!(site.last_hop, LastHop::SrunOverlap);
        assert_eq!(site.socket, SocketPlace::NodeLocal);
        assert!(!site.description.is_empty());
        site.check().unwrap();
        generic().check().unwrap();
        assert_eq!(builtin_sites(), [generic()]);
    }

    #[test]
    fn recipes_are_read_strictly() {
        let refused = |text: &str, why: &str| {
            let err = Site::from_toml("x", text).unwrap_err();
            assert!(err.why.contains(why), "{text:?}: {err}");
            assert!(!err.to_string().contains('\n'), "{err}");
        };
        refused("partitoin = \"gpu\"\n", "unknown key \"partitoin\"");
        refused("[slurm]\npartition = \"gpu\"\n", "unknown key \"slurm\"");
        refused("partition.name = \"gpu\"\n", "partition must be a string");
        refused("partition = 1\n", "partition must be a string");
        refused("partition = [\"gpu\"]\n", "partition must be a string");
        refused("cpus = \"2\"\n", "cpus must be a whole number");
        refused("cpus = -1\n", "cpus must be a whole number");
        refused("cpus = 0\n", "CPUs");
        refused("modules = \"a\"\n", "modules must be a list of strings");
        refused(
            "modules = [\"a\", 1]\n",
            "modules must be a list of strings",
        );
        refused("modules = [[\"a\"]]\n", "modules must be a list of strings");
        refused("modules = [\"a; rm -rf ~\"]\n", "module");
        refused("sbatch = [\"--wrap=id\"]\n", "#SBATCH option");
        refused("sbatch = [\"--partition=gpu\"]\n", "#SBATCH option");
        refused("time = \"soon\"\n", "time must be a SLURM time");
        refused("time = \"UNLIMITED\"\n", "time must be a SLURM time");
        refused("last_hop = \"telnet\"\n", "last_hop must be");
        refused("socket = \"nfs\"\n", "socket must be");
        refused("modules_init = \"profile.sh\"\n", "modules_init");
        refused(
            "partition = \"gpu\"\npartition = \"cpu\"\n",
            "not valid TOML: line 2",
        );
        refused("partition = \n", "not valid TOML");
        refused("[[sites]]\nname = \"a\"\n", "unknown key \"sites\"");
        refused("description = \"a\\nb\"\n", "description");
        refused(&format!("# {}\n", "x".repeat(70 * 1024)), "larger than");
        assert!(Site::from_toml("Bad Name", "").is_err());
        // Nothing at all is the generic recipe under another name.
        let empty = Site::from_toml("empty", "# nothing\n").unwrap();
        assert_eq!(
            empty,
            Site {
                name: "empty".into(),
                ..Site::default()
            }
        );
    }

    /// Fuzz finding R26 (`fuzz/regressions/remote_site`: the name its flags byte picks, and its
    /// text): a recipe loaded that no job script could be made from.
    #[test]
    fn recipes_with_hetjob_words_do_not_load() {
        let err = Site::from_toml("x", "partition = \"hetjobs\"\n").unwrap_err();
        assert!(err.why.contains("\"hetjob\""), "{err}");
        assert!(!err.to_string().contains('\n'), "{err}");
        for text in [
            "account = \"PackJob\"\n",
            "qos = \"a-hetjob\"\n",
            "gres = \"packjob:1\"\n",
            "sbatch = [\"--comment=HETJOB\"]\n",
        ] {
            assert!(Site::from_toml("x", text).is_err(), "{text:?}");
        }
        let site = Site {
            name: "x".into(),
            defaults: JobOptions {
                partition: Some("hetjobs".into()),
                ..JobOptions::default()
            },
            ..Site::default()
        };
        assert!(site.check().is_err());
    }

    #[test]
    fn recipe_files_are_found_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_sites(&dir.path().join("missing")).is_empty());
        std::fs::write(dir.path().join("b-site.toml"), "partition = \"gpu\"\n").unwrap();
        std::fs::write(dir.path().join("a-site.toml"), "partitoin = \"gpu\"\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a recipe").unwrap();
        std::fs::write(dir.path().join("Bad Name.toml"), "").unwrap();
        let sites = load_sites(dir.path());
        assert_eq!(sites.len(), 3, "{sites:?}");
        let err = sites[0].as_ref().unwrap_err();
        assert!(err.file.ends_with("Bad Name.toml"), "{err}");
        let err = sites[1].as_ref().unwrap_err();
        assert!(err.file.ends_with("a-site.toml"), "{err}");
        assert!(err.why.contains("unknown key \"partitoin\""), "{err}");
        let site = sites[2].as_ref().unwrap();
        assert_eq!(site.name, "b-site");
        assert_eq!(site.defaults.partition.as_deref(), Some("gpu"));

        let big = dir.path().join("big.toml");
        std::fs::write(&big, format!("# {}\n", "x".repeat(70 * 1024))).unwrap();
        assert!(load_site(&big).unwrap_err().why.contains("larger than"));
        assert!(load_site(&dir.path().join("notes.txt")).is_err());
    }
}
