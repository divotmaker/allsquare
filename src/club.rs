//! Club identification.
//!
//! `ClubSelect` (`0x82`) carries `[club_number] [category] [handedness]`. The
//! category exists to disambiguate numbers that would otherwise collide — a
//! 5-wood and a 5-iron are both number 5.
//!
//! Recovered by cycling every club in the Omni app's UI while returning to a
//! known club between each selection. Entries marked *inferred* follow the same
//! numbering but were not themselves observed on the wire, because the test
//! unit's bag did not offer them.
//!
//! Note these are **not** the original Square's codes, which used a
//! regular/swing-stick distinction in the second byte.

use core::fmt;

/// Club category — the second byte of a `ClubSelect` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Category {
    /// Driver. Only member is the driver itself.
    Driver = 0x00,
    /// Fairway woods and hybrids.
    WoodHybrid = 0x01,
    /// Irons and wedges.
    IronWedge = 0x02,
    /// Putter.
    Putter = 0x03,
}

/// A selectable club.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum Club {
    /// Driver.
    Driver,
    /// 3-wood.
    Wood3,
    /// 5-wood (inferred code).
    Wood5,
    /// 7-wood (inferred code).
    Wood7,
    /// 3-hybrid.
    Hybrid3,
    /// 4-hybrid (inferred code).
    Hybrid4,
    /// 5-hybrid.
    Hybrid5,
    /// 3-iron (inferred code).
    Iron3,
    /// 4-iron.
    Iron4,
    /// 5-iron.
    Iron5,
    /// 6-iron.
    Iron6,
    /// 7-iron.
    Iron7,
    /// 8-iron.
    Iron8,
    /// 9-iron.
    Iron9,
    /// Pitching wedge.
    PitchingWedge,
    /// Gap wedge.
    GapWedge,
    /// Sand wedge.
    SandWedge,
    /// Lob wedge (inferred code).
    LobWedge,
    /// Putter.
    Putter,
}

/// Player handedness, the third byte of a `ClubSelect` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Handed {
    /// Right-handed.
    #[default]
    Right,
    /// Left-handed.
    Left,
}

impl Handed {
    #[must_use]
    pub(crate) const fn code(self) -> u8 {
        match self {
            Handed::Right => 0x00,
            Handed::Left => 0x01,
        }
    }
}

impl Club {
    /// The `(club_number, category)` pair sent in `ClubSelect`.
    #[must_use]
    pub const fn code(self) -> (u8, Category) {
        use Category::{Driver as D, IronWedge as I, Putter as P, WoodHybrid as W};
        match self {
            Club::Driver => (0x01, D),
            // Woods take their own number; 3w confirmed, 5w/7w inferred.
            Club::Wood3 => (0x03, W),
            Club::Wood5 => (0x05, W),
            Club::Wood7 => (0x07, W),
            // Hybrids continue at 13; 3h and 5h confirmed, 4h inferred.
            Club::Hybrid3 => (0x0D, W),
            Club::Hybrid4 => (0x0E, W),
            Club::Hybrid5 => (0x0F, W),
            // Irons take their own number; 4i-9i confirmed, 3i inferred.
            Club::Iron3 => (0x03, I),
            Club::Iron4 => (0x04, I),
            Club::Iron5 => (0x05, I),
            Club::Iron6 => (0x06, I),
            Club::Iron7 => (0x07, I),
            Club::Iron8 => (0x08, I),
            Club::Iron9 => (0x09, I),
            // Wedges continue the iron run; LW inferred.
            Club::PitchingWedge => (0x0A, I),
            Club::GapWedge => (0x0B, I),
            Club::SandWedge => (0x0C, I),
            Club::LobWedge => (0x0D, I),
            Club::Putter => (0x01, P),
        }
    }

    /// Whether this club's code has been observed on the wire, as opposed to
    /// inferred from the numbering scheme.
    #[must_use]
    pub const fn is_confirmed(self) -> bool {
        !matches!(
            self,
            Club::Wood5 | Club::Wood7 | Club::Hybrid4 | Club::Iron3 | Club::LobWedge
        )
    }

    /// Recover a club from a wire code, if it maps to a known one.
    #[must_use]
    pub fn from_code(number: u8, category: u8) -> Option<Self> {
        const ALL: &[Club] = &[
            Club::Driver,
            Club::Wood3,
            Club::Wood5,
            Club::Wood7,
            Club::Hybrid3,
            Club::Hybrid4,
            Club::Hybrid5,
            Club::Iron3,
            Club::Iron4,
            Club::Iron5,
            Club::Iron6,
            Club::Iron7,
            Club::Iron8,
            Club::Iron9,
            Club::PitchingWedge,
            Club::GapWedge,
            Club::SandWedge,
            Club::LobWedge,
            Club::Putter,
        ];
        ALL.iter().copied().find(|c| {
            let (n, cat) = c.code();
            n == number && cat as u8 == category
        })
    }

    /// Short display name, e.g. `7i`.
    #[must_use]
    pub const fn short_name(self) -> &'static str {
        match self {
            Club::Driver => "Dr",
            Club::Wood3 => "3w",
            Club::Wood5 => "5w",
            Club::Wood7 => "7w",
            Club::Hybrid3 => "3h",
            Club::Hybrid4 => "4h",
            Club::Hybrid5 => "5h",
            Club::Iron3 => "3i",
            Club::Iron4 => "4i",
            Club::Iron5 => "5i",
            Club::Iron6 => "6i",
            Club::Iron7 => "7i",
            Club::Iron8 => "8i",
            Club::Iron9 => "9i",
            Club::PitchingWedge => "PW",
            Club::GapWedge => "GW",
            Club::SandWedge => "SW",
            Club::LobWedge => "LW",
            Club::Putter => "Pt",
        }
    }
}

impl fmt::Display for Club {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.short_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmed_codes_match_the_capture() {
        // Every one of these was observed in the app's club sweep.
        assert_eq!(Club::Driver.code(), (0x01, Category::Driver));
        assert_eq!(Club::Wood3.code(), (0x03, Category::WoodHybrid));
        assert_eq!(Club::Hybrid3.code(), (0x0D, Category::WoodHybrid));
        assert_eq!(Club::Hybrid5.code(), (0x0F, Category::WoodHybrid));
        assert_eq!(Club::Iron7.code(), (0x07, Category::IronWedge));
        assert_eq!(Club::PitchingWedge.code(), (0x0A, Category::IronWedge));
        assert_eq!(Club::GapWedge.code(), (0x0B, Category::IronWedge));
        assert_eq!(Club::SandWedge.code(), (0x0C, Category::IronWedge));
        assert_eq!(Club::Putter.code(), (0x01, Category::Putter));
    }

    #[test]
    fn category_disambiguates_colliding_numbers() {
        // The whole reason the category byte exists.
        let (wood_num, wood_cat) = Club::Wood5.code();
        let (iron_num, iron_cat) = Club::Iron5.code();
        assert_eq!(wood_num, iron_num);
        assert_ne!(wood_cat, iron_cat);

        // Driver and putter are both club number 1.
        assert_eq!(Club::Driver.code().0, Club::Putter.code().0);
    }

    #[test]
    fn round_trips_through_wire_codes() {
        for club in [
            Club::Driver,
            Club::Wood3,
            Club::Hybrid5,
            Club::Iron7,
            Club::SandWedge,
            Club::Putter,
        ] {
            let (n, cat) = club.code();
            assert_eq!(Club::from_code(n, cat as u8), Some(club));
        }
        assert_eq!(Club::from_code(0xFF, 0xFF), None);
    }

    #[test]
    fn inferred_entries_are_flagged() {
        assert!(Club::Iron7.is_confirmed());
        assert!(!Club::LobWedge.is_confirmed());
        assert!(!Club::Wood5.is_confirmed());
    }
}
