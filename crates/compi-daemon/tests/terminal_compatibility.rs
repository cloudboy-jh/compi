use compi_client::{MirrorApply, ScreenMirror};
use compi_daemon::screen;
use compi_daemon::terminal::{Delta, TerminalState};
use compi_protocol::{ScreenMessage, ScrollbackChange};

#[test]
fn sequence_gap_preserves_known_state_until_snapshot_recovery() {
    let mut terminal = TerminalState::new(10, 2);
    let initial = screen::snapshot(terminal.snapshot());
    let mut replica = ScreenMirror::default();
    replica.apply(ScreenMessage::Snapshot {
        snapshot: initial.clone(),
    });
    terminal.advance(b"first");
    let (second, _) = terminal.advance(b"second");
    assert_eq!(
        replica.apply(ScreenMessage::Delta {
            delta: screen::delta(second.unwrap()),
        }),
        MirrorApply::Gap {
            expected: 1,
            actual: 2,
        }
    );
    assert_eq!(replica.snapshot(), Some(&initial));
    let current = screen::snapshot(terminal.snapshot());
    assert_eq!(
        replica.apply(ScreenMessage::Snapshot {
            snapshot: current.clone(),
        }),
        MirrorApply::Applied
    );
    assert_eq!(replica.snapshot(), Some(&current));
    let resized = terminal.resize(5, 3).unwrap();
    assert_eq!(
        replica.apply(ScreenMessage::Delta {
            delta: screen::delta(resized),
        }),
        MirrorApply::Applied
    );
    assert_eq!(
        replica.snapshot(),
        Some(&screen::snapshot(terminal.snapshot()))
    );
}

#[test]
fn image_anchor_tracks_retained_history_without_retransmitting_pixels() {
    let mut terminal = TerminalState::new(10, 2);
    terminal.set_resource_limits(2, 8);
    terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=1,p=7;AQIDBA==\x1b\\");
    let initial = screen::snapshot(terminal.snapshot());
    let mut replica = ScreenMirror::default();
    replica.apply(ScreenMessage::Snapshot {
        snapshot: initial.clone(),
    });
    for (output, expected_row) in [
        (b"one\r\ntwo\r\nthree".as_slice(), Some(-1)),
        (b"\r\nfour".as_slice(), Some(-2)),
        (b"\r\nfive".as_slice(), None),
    ] {
        let delta = terminal.advance(output).0.unwrap();
        assert!(
            delta.images.is_none(),
            "moving an anchor must not resend its pixels"
        );
        assert_eq!(
            replica.apply(ScreenMessage::Delta {
                delta: screen::delta(delta)
            }),
            MirrorApply::Applied
        );
        let authoritative = screen::snapshot(terminal.snapshot());
        assert_eq!(
            authoritative
                .placements
                .first()
                .map(|placement| placement.row),
            expected_row
        );
        assert_eq!(authoritative.images, initial.images);
        assert_eq!(replica.snapshot(), Some(&authoritative));
    }
    // Once the anchor's history is gone, its unreferenced pixels may be reclaimed.
    let (delta, replies) = terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=2;BQYHCA==\x1b\\");
    assert!(replies.is_empty());
    assert_eq!(
        replica.apply(ScreenMessage::Delta {
            delta: screen::delta(delta.unwrap())
        }),
        MirrorApply::Applied
    );
    assert_eq!(replica.snapshot().unwrap().images[0].id, 2);
    assert_eq!(
        replica.snapshot(),
        Some(&screen::snapshot(terminal.snapshot()))
    );
}

#[test]
fn resize_reflows_text_and_logical_image_anchor_without_retransmitting_payload() {
    let mut terminal = TerminalState::new(10, 3);
    terminal.advance(b"abcdefghijK\x1b_Ga=T,f=32,s=1,v=1,i=1,p=7;AQIDBA==\x1b\\");
    let initial = screen::snapshot(terminal.snapshot());
    let mut replica = ScreenMirror::default();
    replica.apply(ScreenMessage::Snapshot {
        snapshot: initial.clone(),
    });
    let delta = terminal.resize(5, 3).unwrap();
    assert_eq!(
        replica.apply(ScreenMessage::Delta {
            delta: screen::delta(delta)
        }),
        MirrorApply::Applied
    );
    let resized = screen::snapshot(terminal.snapshot());
    assert_eq!(resized.images, initial.images);
    assert_eq!(resized.placements[0].row, 2);
    assert!(resized.scrollback.is_empty());
    assert_eq!(
        resized.cells[0]
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>(),
        "abcde"
    );
    assert_eq!(replica.snapshot(), Some(&resized));
    terminal.clear_scrollback();
    // Clearing history does not remove the placement while its reflowed anchor remains on-screen.
    assert_eq!(terminal.snapshot().placements, resized.placements);
}

#[test]
fn clearing_history_removes_only_history_image_anchors() {
    let mut terminal = TerminalState::new(10, 2);
    terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;AQIDBA==\x1b\\one\r\ntwo\r\nthree");
    terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=2;BQYHCA==\x1b\\");
    let before = terminal.snapshot();
    assert_eq!(
        before
            .placements
            .iter()
            .map(|placement| placement.row)
            .collect::<Vec<_>>(),
        [-1, 1]
    );
    terminal.clear_scrollback();
    let after = terminal.snapshot();
    assert_eq!(after.images, before.images);
    assert_eq!(after.placements, vec![before.placements[1]]);
}

/// A small deterministic generator, so a failing run replays exactly.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }
}

fn apply(replica: &mut ScreenMirror, delta: Option<Delta>) {
    if let Some(delta) = delta {
        assert_eq!(
            replica.apply(ScreenMessage::Delta {
                delta: screen::delta(delta)
            }),
            MirrorApply::Applied
        );
    }
}

fn lines(random: &mut Lcg) -> Vec<u8> {
    let mut output = Vec::new();
    for _ in 0..random.below(12) {
        match random.below(4) {
            0 => output.extend_from_slice(b"\x1b[1;31merror\x1b[0m: "),
            1 => output.extend_from_slice("漢字かな ".as_bytes()),
            2 => output.extend_from_slice(b"\x1b]8;;https://example.com\x07link\x1b]8;;\x07 "),
            _ => {}
        }
        let length = random.below(30) as usize;
        output.extend((0..length).map(|index| b'a' + (index % 26) as u8));
        output.extend_from_slice(b"\r\n");
    }
    output
}

#[test]
fn replica_history_follows_appends_evictions_held_frames_resizes_and_clears() {
    let mut terminal = TerminalState::new(20, 5);
    terminal.set_resource_limits(40, 1024);
    let mut replica = ScreenMirror::default();
    replica.apply(ScreenMessage::Snapshot {
        snapshot: screen::snapshot(terminal.snapshot()),
    });
    let mut random = Lcg(7);
    let (mut appends, mut replaces) = (0, 0);
    for step in 0..3_000 {
        match random.below(20) {
            0 => {
                let cols = 4 + random.below(30) as u16;
                let rows = 2 + random.below(8) as u16;
                let delta = terminal.resize(cols, rows);
                apply(&mut replica, delta);
            }
            1 => {
                let rows = terminal.snapshot().rows;
                let cols = terminal.snapshot().cols;
                let delta = terminal.resize(cols, rows % 9 + 2);
                apply(&mut replica, delta);
            }
            2 => apply(&mut replica, terminal.advance(b"\x1b[3J").0),
            3 if step % 7 == 0 => {
                terminal.clear_scrollback();
                replica.apply(ScreenMessage::Snapshot {
                    snapshot: screen::snapshot(terminal.snapshot()),
                });
            }
            4 if step % 5 == 0 => apply(&mut replica, terminal.advance(b"\x1bc").0),
            5..=7 => {
                // Synchronized output held over several reads, sometimes with a
                // snapshot (an attach or gap recovery) taken in the middle.
                let mut output = b"\x1b[?2026h".to_vec();
                output.extend(lines(&mut random));
                assert!(terminal.advance(&output).0.is_none());
                for _ in 0..random.below(4) {
                    if random.below(3) == 0 {
                        replica.apply(ScreenMessage::Snapshot {
                            snapshot: screen::snapshot(terminal.snapshot()),
                        });
                    }
                    assert!(terminal.advance(&lines(&mut random)).0.is_none());
                }
                let delta = if random.below(2) == 0 {
                    terminal.advance(b"\x1b[?2026l").0
                } else {
                    terminal.finish_synchronized_output()
                };
                apply(&mut replica, delta);
            }
            _ => {
                let (delta, _) = terminal.advance(&lines(&mut random));
                match delta.as_ref().and_then(|delta| delta.scrollback.as_ref()) {
                    Some(ScrollbackChange::Append { .. }) => appends += 1,
                    Some(ScrollbackChange::Replace { .. }) => replaces += 1,
                    None => {}
                }
                apply(&mut replica, delta);
            }
        }
        assert_eq!(
            replica.snapshot(),
            Some(&screen::snapshot(terminal.snapshot())),
            "step {step}"
        );
    }
    // Plain output appends; it replaces only when one read scrolls past the whole history.
    assert!(appends > 1_000, "{appends} appends");
    assert!(replaces < appends / 10, "{replaces} replaces");
}
