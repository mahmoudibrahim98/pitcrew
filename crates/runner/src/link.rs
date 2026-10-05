//! Linking sessions to workstreams by folder or branch (work package 4).
//!
//! A session is linked to the workstream whose location holds its working folder:
//! - the **deepest** location folder that contains the session's `cwd` wins;
//! - at equal depth, a location that also names the session's git **branch** wins over one
//!   without a branch (basis `branch`; otherwise `folder`). A location with another branch never
//!   matches;
//! - two workstreams tied for the best match link neither: the runner does not guess.
//!
//! Links made by a dispatch, by the agent claiming its task, by a person, or at import
//! (`dispatch`, `claimed`, `manual` and `imported`) are never overridden; the runner only
//! replaces its own `folder` and `branch` links.

use crate::derive::Linked;
use pitcrew_protocol::ids::{MachineId, SessionId, WorkstreamId};
use pitcrew_protocol::model::{LinkBasis, Location};
use std::collections::HashMap;
use std::fmt;
use std::sync::{PoisonError, RwLock};

/// A folder (and maybe a branch) that belongs to a workstream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkstreamLocation {
    /// The workstream.
    pub workstream: WorkstreamId,
    /// Its location.
    pub location: Location,
}

/// Where workstreams live, and how sessions are linked now. The hub provides it (stream E's
/// tables); [`MemoryLocations`] holds it in memory.
pub trait Locations: Send + Sync + fmt::Debug {
    /// Every workstream location. The runner uses those on its own machine.
    fn locations(&self) -> Vec<WorkstreamLocation>;

    /// Why the session is linked now, if it is. The runner never overrides a `dispatch`,
    /// `claimed`, `manual` or `imported` link.
    fn link_of(&self, session: SessionId) -> Option<LinkBasis>;
}

/// [`Locations`] in memory. After changing it, call `RunnerHandle::locations_changed` so the
/// runner links its sessions again.
#[derive(Debug, Default)]
pub struct MemoryLocations {
    state: RwLock<MemoryState>,
}

#[derive(Debug, Default)]
struct MemoryState {
    locations: Vec<WorkstreamLocation>,
    links: HashMap<SessionId, LinkBasis>,
}

impl MemoryLocations {
    /// With these locations and no links.
    #[must_use]
    pub fn new(locations: Vec<WorkstreamLocation>) -> Self {
        Self {
            state: RwLock::new(MemoryState {
                locations,
                links: HashMap::new(),
            }),
        }
    }

    /// Replaces the locations.
    pub fn set_locations(&self, locations: Vec<WorkstreamLocation>) {
        self.write(|s| s.locations = locations);
    }

    /// Records how a session is linked (e.g. a person linked it: `manual`), or that it is not.
    pub fn set_link(&self, session: SessionId, basis: Option<LinkBasis>) {
        self.write(|s| match basis {
            Some(b) => {
                s.links.insert(session, b);
            }
            None => {
                s.links.remove(&session);
            }
        });
    }

    fn write(&self, f: impl FnOnce(&mut MemoryState)) {
        f(&mut self.state.write().unwrap_or_else(PoisonError::into_inner));
    }
}

impl Locations for MemoryLocations {
    fn locations(&self) -> Vec<WorkstreamLocation> {
        self.state
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .locations
            .clone()
    }

    fn link_of(&self, session: SessionId) -> Option<LinkBasis> {
        self.state
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .links
            .get(&session)
            .copied()
    }
}

/// The link a session should have now, if it differs from the one the runner made (`current`).
/// `None` when a stronger link stands, nothing matches, or it is already linked so.
pub(crate) fn relink(
    machine: MachineId,
    locations: &[WorkstreamLocation],
    stands: Option<LinkBasis>,
    cwd: &str,
    branch: Option<&str>,
    current: Option<Linked>,
) -> Option<Linked> {
    if stands.is_some_and(|b| !matches!(b, LinkBasis::Folder | LinkBasis::Branch)) {
        return None;
    }
    let best = choose(machine, locations, cwd, branch)?;
    (current != Some(best)).then_some(best)
}

/// The unambiguous workstream containing a new session's folder, before its branch is known.
/// Uses the same path normalization, depth and tie rules as transcript linking.
#[must_use]
pub fn workstream_at(
    machine: MachineId,
    locations: &[WorkstreamLocation],
    cwd: &str,
) -> Option<WorkstreamId> {
    choose(machine, locations, cwd, None).map(|linked| linked.workstream)
}

/// The best location for a session in `cwd` on `branch`, by the rules in the module docs.
pub(crate) fn choose(
    machine: MachineId,
    locations: &[WorkstreamLocation],
    cwd: &str,
    branch: Option<&str>,
) -> Option<Linked> {
    let cwd = Folder::new(cwd)?;
    let mut best: Option<((usize, bool), WorkstreamId)> = None;
    let mut tied = false;
    for wl in locations.iter().filter(|l| l.location.machine == machine) {
        let Some(folder) = Folder::new(&wl.location.path) else {
            continue;
        };
        if !cwd.is_within(&folder) {
            continue;
        }
        let by_branch = match wl.location.branch.as_deref() {
            None => false,
            Some(b) if Some(b) == branch => true,
            Some(_) => continue,
        };
        let rank = (folder.parts.len(), by_branch);
        match best {
            Some((r, w)) if rank == r => tied |= w != wl.workstream,
            Some((r, _)) if rank < r => {}
            _ => {
                best = Some((rank, wl.workstream));
                tied = false;
            }
        }
    }
    if tied {
        return None;
    }
    best.map(|((_, by_branch), workstream)| Linked {
        workstream,
        basis: if by_branch {
            LinkBasis::Branch
        } else {
            LinkBasis::Folder
        },
    })
}

/// A folder path split into its parts. Windows paths (a drive letter or backslashes) compare
/// without case, with either separator.
struct Folder {
    parts: Vec<String>,
    windows: bool,
}

impl Folder {
    fn new(path: &str) -> Option<Self> {
        if path.is_empty() {
            return None;
        }
        let b = path.as_bytes();
        let windows =
            path.contains('\\') || (b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic());
        let parts = path
            .split(|c| c == '/' || (windows && c == '\\'))
            .filter(|p| !p.is_empty() && *p != ".")
            .map(|p| {
                if windows {
                    p.to_lowercase()
                } else {
                    p.to_owned()
                }
            })
            .collect();
        Some(Self { parts, windows })
    }

    fn is_within(&self, outer: &Self) -> bool {
        self.windows == outer.windows && self.parts.starts_with(&outer.parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(
        machine: MachineId,
        w: WorkstreamId,
        path: &str,
        branch: Option<&str>,
    ) -> WorkstreamLocation {
        WorkstreamLocation {
            workstream: w,
            location: Location {
                machine,
                path: path.into(),
                branch: branch.map(Into::into),
            },
        }
    }

    #[test]
    fn linking_rules() {
        let m = MachineId::new();
        let (repo, sub, feat, other_machine, twin) = (
            WorkstreamId::new(),
            WorkstreamId::new(),
            WorkstreamId::new(),
            WorkstreamId::new(),
            WorkstreamId::new(),
        );
        let locations = vec![
            at(m, repo, "/w/repo", None),
            at(m, sub, "/w/repo/paper/", None),
            at(m, feat, "/w/repo", Some("feat-x")),
            at(MachineId::new(), other_machine, "/w", None),
            at(m, twin, "/w/twins", None),
            at(m, WorkstreamId::new(), "/w/twins", None),
            at(m, WorkstreamId::new(), "", None),
        ];
        let folder = |w| {
            Some(Linked {
                workstream: w,
                basis: LinkBasis::Folder,
            })
        };
        let branch = |w| {
            Some(Linked {
                workstream: w,
                basis: LinkBasis::Branch,
            })
        };
        let cases: &[(&str, Option<&str>, Option<Linked>)] = &[
            // Longest prefix, by whole parts.
            ("/w/repo", None, folder(repo)),
            ("/w/repo/src", Some("main"), folder(repo)),
            ("/w/repo/paper", None, folder(sub)),
            ("/w/repo/paper/figs", Some("feat-x"), folder(sub)),
            ("/w/repository", None, None),
            // At equal depth a matching branch wins; another branch never matches.
            ("/w/repo/src", Some("feat-x"), branch(feat)),
            ("/w/repo", Some("feat-x"), branch(feat)),
            // Other machines' locations and ties are ignored.
            ("/w/elsewhere", None, None),
            ("/w/twins/a", None, None),
        ];
        for (cwd, br, want) in cases {
            assert_eq!(choose(m, &locations, cwd, *br), *want, "{cwd} {br:?}");
        }
    }

    #[test]
    fn windows_paths_ignore_case_and_separators() {
        let m = MachineId::new();
        let w = WorkstreamId::new();
        let locations = vec![at(m, w, r"C:\Work\Repo", None)];
        let got = choose(m, &locations, "c:/work/repo/src", None).map(|l| l.workstream);
        assert_eq!(got, Some(w));
        assert_eq!(choose(m, &locations, r"C:\Work\Repository", None), None);
    }

    #[test]
    fn stronger_links_win_and_repeats_are_quiet() {
        let m = MachineId::new();
        let (a, b) = (WorkstreamId::new(), WorkstreamId::new());
        let locations = vec![at(m, a, "/w/a", None), at(m, b, "/w/b", None)];
        let linked_a = Linked {
            workstream: a,
            basis: LinkBasis::Folder,
        };
        for stands in [
            LinkBasis::Manual,
            LinkBasis::Dispatch,
            LinkBasis::Claimed,
            LinkBasis::Imported,
        ] {
            assert_eq!(
                relink(m, &locations, Some(stands), "/w/a", None, None),
                None,
                "{stands:?} wins"
            );
        }
        assert_eq!(
            relink(m, &locations, None, "/w/a", None, None),
            Some(linked_a)
        );
        // Already linked so: nothing to say.
        assert_eq!(
            relink(
                m,
                &locations,
                Some(LinkBasis::Folder),
                "/w/a",
                None,
                Some(linked_a)
            ),
            None
        );
        // Its own inferred link moves when the session does.
        assert_eq!(
            relink(
                m,
                &locations,
                Some(LinkBasis::Folder),
                "/w/b",
                None,
                Some(linked_a)
            )
            .map(|l| l.workstream),
            Some(b)
        );
        // Nothing matches: the old link stays (there is no unlink event).
        assert_eq!(
            relink(m, &locations, None, "/elsewhere", None, Some(linked_a)),
            None
        );
    }

    #[test]
    fn memory_locations_hold_links() {
        let mem = MemoryLocations::default();
        let s = SessionId::new();
        assert_eq!(mem.link_of(s), None);
        mem.set_link(s, Some(LinkBasis::Manual));
        assert_eq!(mem.link_of(s), Some(LinkBasis::Manual));
        mem.set_link(s, None);
        assert_eq!(mem.link_of(s), None);
        let w = WorkstreamLocation {
            workstream: WorkstreamId::new(),
            location: Location {
                machine: MachineId::new(),
                path: "/w".into(),
                branch: None,
            },
        };
        mem.set_locations(vec![w.clone()]);
        assert_eq!(mem.locations(), vec![w]);
    }
}
