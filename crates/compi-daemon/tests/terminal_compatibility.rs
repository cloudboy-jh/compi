use compi_client::{MirrorApply, ScreenMirror};
use compi_daemon::screen;
use compi_daemon::terminal::TerminalState;
use compi_protocol::ScreenMessage;

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
