//! Every terminal's program status (OSC 7501), and every chat's, shared with the
//! connections watching it.
//!
//! Terminals and chats publish their complete records whenever they change. Each watching
//! connection sends only the latest records of the terminals and chats that changed, so a
//! program that reports faster than clients read costs one message per terminal per
//! interval.

use compi_protocol::ServerMessage;
use compi_protocol::program_status::{
    ChatProgramStatus, ProgramStatusRecord, SurfaceProgramStatus,
};
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

/// Every chat's program status, which the daemon derives from what its harness does.
#[derive(Default)]
pub struct ChatStatusHub {
    chats: Mutex<HashMap<String, PublishedChat>>,
    /// Counts publications, so watchers skip the lock while nothing changed.
    change: AtomicU64,
}

struct PublishedChat {
    change: u64,
    records: Vec<ProgramStatusRecord>,
}

impl ChatStatusHub {
    /// Replaces the chat's records; no records removes it.
    pub fn publish(&self, chat_id: &str, records: Vec<ProgramStatusRecord>) {
        let Ok(mut chats) = self.chats.lock() else {
            return;
        };
        if records.is_empty() {
            if chats.remove(chat_id).is_some() {
                self.change.fetch_add(1, Ordering::AcqRel);
            }
            return;
        }
        let change = self.change.fetch_add(1, Ordering::AcqRel) + 1;
        chats.insert(chat_id.to_owned(), PublishedChat { change, records });
    }
}

/// What one connection has been sent.
pub struct ProgramStatusWatch {
    /// The hubs' change counts (surfaces, chats) when the connection was last brought up
    /// to date.
    seen: Option<(u64, u64)>,
    sent: HashMap<SurfaceId, (ProcessLifetimeId, u64)>,
    sent_chats: HashMap<String, u64>,
    next_round: Instant,
}

/// Records to send; `change` is `None` when the surface or chat no longer reports
/// anything.
pub enum PendingStatus {
    Surface {
        status: SurfaceProgramStatus,
        change: Option<u64>,
    },
    Chat {
        status: ChatProgramStatus,
        change: Option<u64>,
    },
}

impl PendingStatus {
    pub fn message(&self) -> ServerMessage {
        match self {
            Self::Surface { status, .. } => ServerMessage::ProgramStatusChanged {
                status: status.clone(),
            },
            Self::Chat { status, .. } => ServerMessage::ChatProgramStatusChanged {
                status: status.clone(),
            },
        }
    }
}

impl Default for ProgramStatusWatch {
    fn default() -> Self {
        Self {
            seen: None,
            sent: HashMap::new(),
            sent_chats: HashMap::new(),
            next_round: Instant::now(),
        }
    }
}

impl ProgramStatusWatch {
    /// Every surface and chat whose records differ from what this connection was sent:
    /// all of them the first time, then at most one round per [`CHANGE_INTERVAL`].
    pub fn pending(
        &mut self,
        hub: &ProgramStatusHub,
        chat_hub: &ChatStatusHub,
        now: Instant,
    ) -> Vec<PendingStatus> {
        let current = (
            hub.change.load(Ordering::Acquire),
            chat_hub.change.load(Ordering::Acquire),
        );
        if now < self.next_round || self.seen == Some(current) {
            return Vec::new();
        }
        let mut pending = Vec::new();
        // Each count is read under its lock, so it covers exactly what the lock showed.
        let surface_change = {
            let Ok(surfaces) = hub.surfaces.lock() else {
                return Vec::new();
            };
            pending.extend(
                surfaces
                    .iter()
                    .filter(|(surface_id, published)| {
                        self.sent.get(*surface_id).is_none_or(|(lifetime, change)| {
                            *change != published.change || *lifetime != published.lifetime
                        })
                    })
                    .map(|(surface_id, published)| PendingStatus::Surface {
                        status: SurfaceProgramStatus {
                            surface_id: surface_id.clone(),
                            process_lifetime_id: published.lifetime.clone(),
                            records: published.records.clone(),
                        },
                        change: Some(published.change),
                    }),
            );
            pending.extend(
                self.sent
                    .iter()
                    .filter(|(surface_id, _)| !surfaces.contains_key(*surface_id))
                    .map(|(surface_id, (lifetime, _))| PendingStatus::Surface {
                        status: SurfaceProgramStatus {
                            surface_id: surface_id.clone(),
                            process_lifetime_id: lifetime.clone(),
                            records: Vec::new(),
                        },
                        change: None,
                    }),
            );
            hub.change.load(Ordering::Acquire)
        };
        let chat_change = {
            let Ok(chats) = chat_hub.chats.lock() else {
                return Vec::new();
            };
            pending.extend(
                chats
                    .iter()
                    .filter(|(chat_id, published)| {
                        self.sent_chats.get(*chat_id) != Some(&published.change)
                    })
                    .map(|(chat_id, published)| PendingStatus::Chat {
                        status: ChatProgramStatus {
                            chat_id: chat_id.clone(),
                            records: published.records.clone(),
                        },
                        change: Some(published.change),
                    }),
            );
            pending.extend(
                self.sent_chats
                    .keys()
                    .filter(|chat_id| !chats.contains_key(*chat_id))
                    .map(|chat_id| PendingStatus::Chat {
                        status: ChatProgramStatus {
                            chat_id: chat_id.clone(),
                            records: Vec::new(),
                        },
                        change: None,
                    }),
            );
            chat_hub.change.load(Ordering::Acquire)
        };
        self.seen = Some((surface_change, chat_change));
        self.next_round = now + CHANGE_INTERVAL;
        pending
    }

    /// The client has been sent `pending`.
    pub fn delivered(&mut self, pending: &PendingStatus) {
        match pending {
            PendingStatus::Surface {
                status,
                change: Some(change),
            } => {
                self.sent.insert(
                    status.surface_id.clone(),
                    (status.process_lifetime_id.clone(), *change),
                );
            }
            PendingStatus::Surface {
                status,
                change: None,
            } => {
                self.sent.remove(&status.surface_id);
            }
            PendingStatus::Chat {
                status,
                change: Some(change),
            } => {
                self.sent_chats.insert(status.chat_id.clone(), *change);
            }
            PendingStatus::Chat {
                status,
                change: None,
            } => {
                self.sent_chats.remove(&status.chat_id);
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

    fn round(
        watch: &mut ProgramStatusWatch,
        hub: &ProgramStatusHub,
        chats: &ChatStatusHub,
        now: Instant,
    ) -> Vec<String> {
        let pending = watch.pending(hub, chats, now);
        let mut seen: Vec<_> = pending
            .iter()
            .map(|pending| {
                let (name, records) = match pending {
                    PendingStatus::Surface { status, .. } => {
                        (status.surface_id.to_string(), &status.records)
                    }
                    PendingStatus::Chat { status, .. } => {
                        (format!("chat {}", status.chat_id), &status.records)
                    }
                };
                let message = records
                    .first()
                    .and_then(|record| record.message.as_deref())
                    .unwrap_or("removed");
                format!("{name}:{message}")
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
        let chats = ChatStatusHub::default();
        let lifetime = ProcessLifetimeId::new("l1");
        let (a, b) = (SurfaceId::new("a"), SurfaceId::new("b"));
        hub.publish(&a, &lifetime, records("one"));
        let mut watch = ProgramStatusWatch::default();
        let start = Instant::now();
        assert_eq!(round(&mut watch, &hub, &chats, start), ["a:one"]);

        hub.publish(&a, &lifetime, records("two"));
        hub.publish(&a, &lifetime, records("three"));
        hub.publish(&b, &lifetime, records("four"));
        assert!(
            round(&mut watch, &hub, &chats, start).is_empty(),
            "within the interval"
        );
        let later = start + CHANGE_INTERVAL;
        assert_eq!(
            round(&mut watch, &hub, &chats, later),
            ["a:three", "b:four"]
        );
        assert!(round(&mut watch, &hub, &chats, later + CHANGE_INTERVAL).is_empty());

        hub.publish(&a, &lifetime, Vec::new());
        hub.remove(&b, &ProcessLifetimeId::new("other"));
        let next = later + CHANGE_INTERVAL * 2;
        assert_eq!(round(&mut watch, &hub, &chats, next), ["a:removed"]);
        hub.remove(&b, &lifetime);
        assert_eq!(
            round(&mut watch, &hub, &chats, next + CHANGE_INTERVAL),
            ["b:removed"]
        );

        // A new connection hears only what is current.
        assert!(round(&mut ProgramStatusWatch::default(), &hub, &chats, next).is_empty());
    }

    #[test]
    fn chats_share_the_watch_and_its_interval() {
        let hub = ProgramStatusHub::default();
        let chats = ChatStatusHub::default();
        let lifetime = ProcessLifetimeId::new("l1");
        chats.publish("c1", records("one"));
        let mut watch = ProgramStatusWatch::default();
        let start = Instant::now();
        assert_eq!(round(&mut watch, &hub, &chats, start), ["chat c1:one"]);

        chats.publish("c1", records("two"));
        chats.publish("c1", records("three"));
        chats.publish("c2", records("four"));
        hub.publish(&SurfaceId::new("a"), &lifetime, records("five"));
        assert!(
            round(&mut watch, &hub, &chats, start).is_empty(),
            "within the interval"
        );
        let later = start + CHANGE_INTERVAL;
        assert_eq!(
            round(&mut watch, &hub, &chats, later),
            ["a:five", "chat c1:three", "chat c2:four"]
        );

        // A chat that stops reporting is sent once, with no records.
        chats.publish("c1", Vec::new());
        chats.publish("gone", Vec::new());
        let next = later + CHANGE_INTERVAL;
        assert_eq!(round(&mut watch, &hub, &chats, next), ["chat c1:removed"]);
        assert!(round(&mut watch, &hub, &chats, next + CHANGE_INTERVAL).is_empty());

        // A new connection hears every chat that reports.
        assert_eq!(
            round(&mut ProgramStatusWatch::default(), &hub, &chats, next),
            ["a:five", "chat c2:four"]
        );
    }

    #[test]
    fn undelivered_status_is_offered_again() {
        let hub = ProgramStatusHub::default();
        let chats = ChatStatusHub::default();
        let lifetime = ProcessLifetimeId::new("l1");
        hub.publish(&SurfaceId::new("a"), &lifetime, records("one"));
        chats.publish("c", records("two"));
        let mut watch = ProgramStatusWatch::default();
        let start = Instant::now();
        assert_eq!(watch.pending(&hub, &chats, start).len(), 2);
        watch.retry();
        assert_eq!(
            round(&mut watch, &hub, &chats, start + CHANGE_INTERVAL),
            ["a:one", "chat c:two"]
        );
    }
}
