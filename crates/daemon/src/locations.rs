//! Workstream locations and firm session links read from the hub, without a cache.

use crate::setup::Workers;
use pitcrew_hub_work::{WorkService, query};
use pitcrew_protocol::ids::SessionId;
use pitcrew_protocol::model::LinkBasis;
use pitcrew_runner::{Locations, WorkstreamLocation};
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

/// The runner's locations source over the hub's current projections.
#[derive(Debug)]
pub struct HubLocations {
    work: Arc<WorkService>,
}

impl HubLocations {
    pub fn new(work: Arc<WorkService>) -> Self {
        Self { work }
    }
}

fn locations(work: &WorkService) -> pitcrew_hub_work::Result<Vec<WorkstreamLocation>> {
    work.read(|conn| {
        Ok(query::workstreams(conn, None)?
            .into_iter()
            .flat_map(|w| {
                w.locations
                    .into_iter()
                    .map(move |location| WorkstreamLocation {
                        workstream: w.id,
                        location,
                    })
            })
            .collect())
    })
}

impl Locations for HubLocations {
    fn locations(&self) -> Vec<WorkstreamLocation> {
        locations(&self.work).unwrap_or_else(|_| {
            tracing::warn!("cannot read workstream locations; no inferred links made");
            Vec::new()
        })
    }

    fn link_of(&self, session: SessionId) -> Option<LinkBasis> {
        self.work
            .read(|conn| Ok(query::session(conn, &session)?.and_then(|s| s.link_basis)))
            .unwrap_or_else(|_| {
                tracing::warn!(%session, "cannot read a session link; keeping its existing link");
                // Fail closed: never replace a link whose basis could not be read.
                Some(LinkBasis::Manual)
            })
    }
}

/// Stops the subscription on shutdown, including a failed server start.
#[derive(Debug)]
pub struct Watching(JoinHandle<()>);

impl Drop for Watching {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Subscribe before the initial read so a concurrent workstream creation cannot be missed.
/// Comparing snapshots also handles lagged notifications and avoids relinking after every turn.
pub fn watch(work: &Arc<WorkService>, workers: &Arc<Workers>) -> Watching {
    let mut appended = work.store().subscribe();
    let work = Arc::downgrade(work);
    let workers = Arc::downgrade(workers);
    Watching(tokio::spawn(async move {
        let mut previous = None;
        loop {
            let Some(work) = work.upgrade() else {
                break;
            };
            let current = tokio::task::spawn_blocking(move || locations(&work)).await;
            if let Ok(Ok(current)) = current {
                if previous.as_ref() != Some(&current) {
                    let Some(workers) = workers.upgrade() else {
                        break;
                    };
                    workers.locations_changed();
                    previous = Some(current);
                }
            } else {
                tracing::warn!("cannot check workstream locations after an append");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            match appended.recv().await {
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            }
        }
    }))
}
