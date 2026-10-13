//! Every terminal's program status (OSC 7501), shared with the connections watching it.
//!
//! Terminals publish their complete records whenever they change. Each watching
//! connection sends only the latest records of the terminals that changed, so a program
//! that reports faster than clients read costs one message per terminal per interval.

use compi_protocol::program_status::{ProgramStatusRecord, SurfaceProgramStatus};
use compi_protocol::{ProcessLifetimeId, SurfaceId};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Shortest time between two rounds of changes sent to one connection.
const CHANGE_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Default)]
pub struct ProgramStatusHub {
    surfaces: Mutex<HashMap<SurfaceId, Published>>,
    /// Counts publications, so watchers skip the lock while nothing changed.
    change: AtomicU64,
}

struct Published {
    lifetime: ProcessLifetimeId,
    change: u64,
    records: Vec<ProgramStatusRecord>,
}

impl ProgramStatusHub {
    /// Replaces the surface's records; no records removes it.
    pub fn publish(
        &self,
        surface_id: &SurfaceId,
        lifetime: &ProcessLifetimeId,
        records: Vec<ProgramStatusRecord>,
    ) {
        let Ok(mut surfaces) = self.surfaces.lock() else {
            return;
        };
        if records.is_empty() {
            if surfaces.remove(surface_id).is_some() {
                self.change.fetch_add(1, Ordering::AcqRel);
            }
            return;
        }
        let change = self.change.fetch_add(1, Ordering::AcqRel) + 1;
        surfaces.insert(
            surface_id.clone(),
            Published {
                lifetime: lifetime.clone(),
                change,
                records,
            },
        );
    }

    /// Removes the records of a process lifetime that has ended or been replaced.
    pub fn remove(&self, surface_id: &SurfaceId, lifetime: &ProcessLifetimeId) {
        let Ok(mut surfaces) = self.surfaces.lock() else {
            return;
        };
        if surfaces
            .get(surface_id)
            .is_some_and(|published| published.lifetime == *lifetime)
        {
            surfaces.remove(surface_id);
            self.change.fetch_add(1, Ordering::AcqRel);
        }
    }
}

/// What one connection has been sent.
pub struct ProgramStatusWatch {
    /// The hub's change count when the connection was last brought up to date.
    seen: Option<u64>,
    sent: HashMap<SurfaceId, (ProcessLifetimeId, u64)>,
    next_round: Instant,
}

pub struct PendingStatus {
    pub status: SurfaceProgramStatus,
    /// `None` when the surface no longer reports anything.
    change: Option<u64>,
}

impl Default for ProgramStatusWatch {
    fn default() -> Self {
        Self {
            seen: None,
            sent: HashMap::new(),
            next_round: Instant::now(),
        }
    }
}

impl ProgramStatusWatch {
    /// Every surface whose records differ from what this connection was sent: all of
    /// them the first time, then at most one round per [`CHANGE_INTERVAL`].
    pub fn pending(&mut self, hub: &ProgramStatusHub, now: Instant) -> Vec<PendingStatus> {
        if now < self.next_round || self.seen == Some(hub.change.load(Ordering::Acquire)) {
            return Vec::new();
        }
        let Ok(surfaces) = hub.surfaces.lock() else {
            return Vec::new();
        };
        self.seen = Some(hub.change.load(Ordering::Acquire));
        self.next_round = now + CHANGE_INTERVAL;
        let mut pending: Vec<_> = surfaces
            .iter()
            .filter(|(surface_id, published)| {
                self.sent.get(*surface_id).is_none_or(|(lifetime, change)| {
                    *change != published.change || *lifetime != published.lifetime
                })
            })
            .map(|(surface_id, published)| PendingStatus {
                status: SurfaceProgramStatus {
                    surface_id: surface_id.clone(),
                    process_lifetime_id: published.lifetime.clone(),
                    records: published.records.clone(),
                },
                change: Some(published.change),
            })
            .collect();
        pending.extend(
            self.sent
                .iter()
                .filter(|(surface_id, _)| !surfaces.contains_key(*surface_id))
                .map(|(surface_id, (lifetime, _))| PendingStatus {
                    status: SurfaceProgramStatus {
                        surface_id: surface_id.clone(),
                        process_lifetime_id: lifetime.clone(),
                        records: Vec::new(),
                    },
                    change: None,
                }),
        );
        pending
    }

    /// The client has been sent `pending`.
    pub fn delivered(&mut self, pending: &PendingStatus) {
        let status = &pending.status;
        match pending.change {
            Some(change) => {
                self.sent.insert(
                    status.surface_id.clone(),
                    (status.process_lifetime_id.clone(), change),
                );
            }
            None => {
                self.sent.remove(&status.surface_id);
            }
        }
    }

    /// Some pending status could not be sent; look again next round.
    pub fn retry(&mut self) {
        self.seen = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::program_status::ProgramState;

    fn records(message: &str) -> Vec<ProgramStatusRecord> {
        vec![ProgramStatusRecord {
            id: None,
            state: ProgramState::Working,
            kind: None,
            progress: None,
            app: None,
            title: None,
            message: Some(message.into()),
        }]
    }

    fn round(watch: &mut ProgramStatusWatch, hub: &ProgramStatusHub, now: Instant) -> Vec<String> {
        let pending = watch.pending(hub, now);
        let mut seen: Vec<_> = pending
            .iter()
            .map(|pending| {
                let status = &pending.status;
                let message = status
                    .records
                    .first()
                    .and_then(|record| record.message.as_deref())
                    .unwrap_or("removed");
                format!("{}:{message}", status.surface_id)
            })
            .collect();
        for pending in &pending {
            watch.delivered(pending);
        }
        seen.sort();
        seen
    }

    #[test]
    fn watch_sends_latest_changes_once_per_round() {
        let hub = ProgramStatusHub::default();
        let lifetime = ProcessLifetimeId::new("l1");
        let (a, b) = (SurfaceId::new("a"), SurfaceId::new("b"));
        hub.publish(&a, &lifetime, records("one"));
        let mut watch = ProgramStatusWatch::default();
        let start = Instant::now();
        assert_eq!(round(&mut watch, &hub, start), ["a:one"]);

        hub.publish(&a, &lifetime, records("two"));
        hub.publish(&a, &lifetime, records("three"));
        hub.publish(&b, &lifetime, records("four"));
        assert!(
            round(&mut watch, &hub, start).is_empty(),
            "within the interval"
        );
        let later = start + CHANGE_INTERVAL;
        assert_eq!(round(&mut watch, &hub, later), ["a:three", "b:four"]);
        assert!(round(&mut watch, &hub, later + CHANGE_INTERVAL).is_empty());

        hub.publish(&a, &lifetime, Vec::new());
        hub.remove(&b, &ProcessLifetimeId::new("other"));
        let next = later + CHANGE_INTERVAL * 2;
        assert_eq!(round(&mut watch, &hub, next), ["a:removed"]);
        hub.remove(&b, &lifetime);
        assert_eq!(
            round(&mut watch, &hub, next + CHANGE_INTERVAL),
            ["b:removed"]
        );

        // A new connection hears only what is current.
        assert!(round(&mut ProgramStatusWatch::default(), &hub, next).is_empty());
    }

    #[test]
    fn undelivered_status_is_offered_again() {
        let hub = ProgramStatusHub::default();
        let lifetime = ProcessLifetimeId::new("l1");
        hub.publish(&SurfaceId::new("a"), &lifetime, records("one"));
        let mut watch = ProgramStatusWatch::default();
        let start = Instant::now();
        assert_eq!(watch.pending(&hub, start).len(), 1);
        watch.retry();
        assert_eq!(round(&mut watch, &hub, start + CHANGE_INTERVAL), ["a:one"]);
    }
}
