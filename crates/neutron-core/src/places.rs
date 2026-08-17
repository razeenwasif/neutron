//! Sidebar destinations: known folders, volumes, and cloud roots.

use crate::namespace::NodeId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceKind {
    /// Desktop, Downloads, Documents, …
    KnownFolder,
    /// A local volume.
    Drive,
    /// A cloud provider root.
    Cloud,
    /// A WSL distribution, reached over `\\wsl.localhost\<Distro>`.
    Wsl,
    /// A shell namespace root — This PC, Network, Control Panel, Recycle Bin.
    Shell,
}

/// Capacity of a volume, when it could be determined.
///
/// Absent for removable drives with no media inserted and for network paths
/// that did not respond — querying those can block, so a missing figure is
/// normal and must render as "no bar" rather than as zero bytes free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    pub total: u64,
    pub free: u64,
}

impl Capacity {
    /// Fraction used, in `0.0..=1.0`.
    pub fn used_fraction(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        let used = self.total.saturating_sub(self.free);
        (used as f64 / self.total as f64) as f32
    }

    /// Whether to warn about low space. Matches Explorer's threshold of 10%.
    pub fn is_low(&self) -> bool {
        self.used_fraction() >= 0.9
    }
}

#[derive(Debug, Clone)]
pub struct Place {
    pub name: String,
    pub id: NodeId,
    pub kind: PlaceKind,
    pub capacity: Option<Capacity>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn used_fraction_is_the_complement_of_free() {
        let c = Capacity {
            total: 1000,
            free: 250,
        };
        assert!((c.used_fraction() - 0.75).abs() < 1e-6);
    }

    #[test]
    fn low_space_triggers_at_ninety_percent() {
        assert!(
            Capacity {
                total: 100,
                free: 10
            }
            .is_low()
        );
        assert!(
            !Capacity {
                total: 100,
                free: 11
            }
            .is_low()
        );
    }

    #[test]
    fn zero_capacity_does_not_divide_by_zero() {
        // An empty card reader reports a total of zero.
        let c = Capacity { total: 0, free: 0 };
        assert_eq!(c.used_fraction(), 0.0);
        assert!(!c.is_low());
    }

    #[test]
    fn free_exceeding_total_saturates_rather_than_underflowing() {
        // Quota-backed and network volumes can report inconsistent pairs.
        let c = Capacity {
            total: 100,
            free: 200,
        };
        assert_eq!(c.used_fraction(), 0.0);
    }
}
