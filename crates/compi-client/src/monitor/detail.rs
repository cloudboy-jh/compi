//! The newest totals sample's breakdowns, kept pure: the volumes and interfaces tables
//! of the Disk and Network pages (their columns, which a narrow table gives up first,
//! the interfaces' order) and the shares their bars fill. The pages draw them.

use super::{compare_names, fit_columns, unavailable_last};
use compi_protocol::system_metrics::{NetworkInterface, Volume};

/// The narrowest a volume's or an interface's name gets before other columns make room.
pub const DETAIL_NAME_MIN_WIDTH: f32 = 120.0;

/// `part` of `whole`, from 0 to 1; `None` for an empty whole.
pub fn share(part: u64, whole: u64) -> Option<f64> {
    (whole > 0).then(|| (part as f64 / whole as f64).clamp(0.0, 1.0))
}

/// The space a volume has in use.
pub fn volume_used(volume: &Volume) -> u64 {
    volume.total_bytes.saturating_sub(volume.available_bytes)
}

/// What an interface moves now, received and sent together; `None` while it has no
/// rate (the daemon's first sample).
pub fn interface_traffic(interface: &NetworkInterface) -> Option<u64> {
    match (
        interface.received_bytes_per_second,
        interface.sent_bytes_per_second,
    ) {
        (None, None) => None,
        (received, sent) => Some(received.unwrap_or(0) + sent.unwrap_or(0)),
    }
}

/// The indices of `interfaces` in the table's order: the busiest now first, those
/// without a rate last, ties by name.
pub fn interface_order(interfaces: &[NetworkInterface]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..interfaces.len()).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&interfaces[a], &interfaces[b]);
        unavailable_last(interface_traffic(a), interface_traffic(b), |a, b| b.cmp(&a))
            .then_with(|| compare_names(&a.name, &b.name))
    });
    order
}

/// The volumes table's columns, in the order drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolumeColumn {
    Name,
    MountPoint,
    FileSystem,
    /// What is in use of the volume, as a bar under the figures.
    Used,
    Available,
}

impl VolumeColumn {
    pub const ALL: [Self; 5] = [
        Self::Name,
        Self::MountPoint,
        Self::FileSystem,
        Self::Used,
        Self::Available,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::MountPoint => "Mount point",
            Self::FileSystem => "File system",
            Self::Used => "Used",
            Self::Available => "Available",
        }
    }

    /// The column's width in pixels; Name takes what the others leave.
    pub const fn width(self) -> f32 {
        match self {
            Self::Name => DETAIL_NAME_MIN_WIDTH,
            Self::MountPoint => 160.0,
            Self::FileSystem => 88.0,
            Self::Used => 168.0,
            Self::Available => 88.0,
        }
    }

    /// Amounts read right-aligned.
    pub const fn numeric(self) -> bool {
        matches!(self, Self::Available)
    }
}

/// What a narrow volumes table gives up first, in order; Name and Used stay.
const VOLUME_DROP_ORDER: [VolumeColumn; 3] = [
    VolumeColumn::FileSystem,
    VolumeColumn::MountPoint,
    VolumeColumn::Available,
];

/// The volumes table's columns at `width` pixels: every column while Name keeps its
/// minimum, else without File system, then Mount point, then Available.
pub fn volume_columns(width: f32) -> Vec<VolumeColumn> {
    fit_columns(
        &VolumeColumn::ALL,
        &VOLUME_DROP_ORDER,
        width,
        VolumeColumn::width,
    )
}

/// The interfaces table's columns, in the order drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterfaceColumn {
    Name,
    Down,
    Up,
    Received,
    Sent,
}

impl InterfaceColumn {
    pub const ALL: [Self; 5] = [Self::Name, Self::Down, Self::Up, Self::Received, Self::Sent];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Down => "Down",
            Self::Up => "Up",
            Self::Received => "Received",
            Self::Sent => "Sent",
        }
    }

    /// The column's width in pixels; Name takes what the others leave.
    pub const fn width(self) -> f32 {
        match self {
            Self::Name => DETAIL_NAME_MIN_WIDTH,
            Self::Down | Self::Up => 88.0,
            Self::Received | Self::Sent => 96.0,
        }
    }

    /// Amounts read right-aligned; the name left.
    pub const fn numeric(self) -> bool {
        !matches!(self, Self::Name)
    }
}

/// What a narrow interfaces table gives up first, in order; the name and rates stay.
const INTERFACE_DROP_ORDER: [InterfaceColumn; 2] =
    [InterfaceColumn::Sent, InterfaceColumn::Received];

/// The interfaces table's columns at `width` pixels: every column while Name keeps
/// its minimum, else without Sent, then Received.
pub fn interface_columns(width: f32) -> Vec<InterfaceColumn> {
    fit_columns(
        &InterfaceColumn::ALL,
        &INTERFACE_DROP_ORDER,
        width,
        InterfaceColumn::width,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interface(name: &str, received: Option<u64>, sent: Option<u64>) -> NetworkInterface {
        NetworkInterface {
            name: name.into(),
            received_bytes_per_second: received,
            sent_bytes_per_second: sent,
            ..NetworkInterface::default()
        }
    }

    #[test]
    fn interfaces_sort_by_traffic_now_then_name_without_rates_last() {
        let interfaces = [
            interface("en1", Some(0), Some(0)),
            interface("utun0", None, None),
            interface("en0", Some(900), Some(100)),
            interface("bridge0", None, Some(2_000)),
            interface("En2", Some(0), None),
            interface("awdl0", None, None),
        ];
        let names: Vec<&str> = interface_order(&interfaces)
            .into_iter()
            .map(|index| interfaces[index].name.as_str())
            .collect();
        assert_eq!(names, ["bridge0", "en0", "en1", "En2", "awdl0", "utun0"]);
        assert_eq!(interface_traffic(&interfaces[3]), Some(2_000));
        assert_eq!(interface_traffic(&interfaces[1]), None);
        assert!(interface_order(&[]).is_empty());
    }

    #[test]
    fn volumes_fill_by_what_is_in_use() {
        let volume = Volume {
            total_bytes: 1_000,
            available_bytes: 250,
            ..Volume::default()
        };
        assert_eq!(volume_used(&volume), 750);
        assert_eq!(share(volume_used(&volume), volume.total_bytes), Some(0.75));
        // A host that reports more free than the whole: nothing in use, never negative.
        let odd = Volume {
            total_bytes: 100,
            available_bytes: 200,
            ..Volume::default()
        };
        assert_eq!(volume_used(&odd), 0);
        assert_eq!(share(5, 0), None);
        assert_eq!(share(300, 100), Some(1.0));
    }

    #[test]
    fn narrow_tables_give_up_their_extra_columns_in_order() {
        let all: f32 = VolumeColumn::ALL.iter().map(|column| column.width()).sum();
        assert_eq!(volume_columns(all), VolumeColumn::ALL);
        assert_eq!(
            volume_columns(all - 1.0),
            [
                VolumeColumn::Name,
                VolumeColumn::MountPoint,
                VolumeColumn::Used,
                VolumeColumn::Available
            ]
        );
        assert_eq!(
            volume_columns(all - VolumeColumn::FileSystem.width() - 1.0),
            [
                VolumeColumn::Name,
                VolumeColumn::Used,
                VolumeColumn::Available
            ]
        );
        assert_eq!(
            volume_columns(0.0),
            [VolumeColumn::Name, VolumeColumn::Used]
        );
        let all: f32 = InterfaceColumn::ALL
            .iter()
            .map(|column| column.width())
            .sum();
        assert_eq!(interface_columns(all), InterfaceColumn::ALL);
        assert_eq!(
            interface_columns(all - 1.0),
            [
                InterfaceColumn::Name,
                InterfaceColumn::Down,
                InterfaceColumn::Up,
                InterfaceColumn::Received
            ]
        );
        assert_eq!(
            interface_columns(0.0),
            [
                InterfaceColumn::Name,
                InterfaceColumn::Down,
                InterfaceColumn::Up
            ]
        );
    }
}
