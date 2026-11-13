//! The driver's settings (`config` feature).
//!
//! [`Config`] is a serde struct that implements flightrelay's
//! [`DriverConfig`]. It needs no BLE stack, so a settings form or a
//! WebAssembly UI can use it without one; the `driver` feature runs a device
//! with it.
//!
//! Every field has a default and is left out of the serialized form while it
//! holds it, so a default config writes nothing. Clubs are FRP club codes
//! (`"7I"`, `"PW"`, `"DR"`), read leniently: case and surrounding whitespace
//! are ignored, letter-first forms (`"I7"`) and wedge lofts (`"52"`) are
//! accepted, and a code that names no club leaves the default in place with a
//! warning when the device connects, instead of refusing the config.
//!
//! ```
//! use allsquare::config::Config;
//! use flightrelay::config::DriverConfig;
//!
//! let config = Config::from_map(
//!     serde_json::json!({ "club": "pw", "advanced_spin": false })
//!         .as_object()
//!         .cloned()
//!         .unwrap_or_default(),
//! )?;
//! assert!(!config.advanced_spin);
//! assert_eq!(config.to_map().len(), 2); // everything else is at its default
//! # Ok::<(), flightrelay::config::ConfigError>(())
//! ```

use std::collections::BTreeMap;

use flightrelay::config::{DriverConfig, is_default, is_default_value};
use serde::{Deserialize, Serialize};

use crate::club::Club;
use crate::impact::ImpactCalibration;
use crate::protocol::SpinMode;

/// The club armed when nothing else names one.
pub const DEFAULT_CLUB: &str = "7I";

/// Settings of the Square Golf driver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The device to connect to: its BLE address (on macOS, the peripheral
    /// UUID) or its advertised name, e.g. `SquareGolf(CD01)`, ignoring case.
    /// Unset or blank: the first Square Golf device found.
    #[serde(skip_serializing_if = "is_default")]
    pub address: Option<String>,
    /// The club armed on connect, as an FRP club code, when the host has no
    /// club in play yet. Once a host names a club, the device follows it.
    /// Default `7I`.
    #[serde(skip_serializing_if = "is_default_club")]
    pub club: String,
    /// Use the device's advanced spin measurement. Default true.
    #[serde(skip_serializing_if = "is_default_true")]
    pub advanced_spin: bool,
    /// Discard shots that read zero spin, unless the putter is in play.
    ///
    /// A struck ball always spins, so a zero-spin read is a failed read,
    /// typically a ball near the front of the detection zone; a spinless shot
    /// flies far too long in a simulator. Putts read zero spin every time and
    /// are never discarded. Default true.
    #[serde(skip_serializing_if = "is_default_true")]
    pub discard_non_putting_zero_spin: bool,
    /// Per-club face impact calibration: the distance from the bottom edge of
    /// the club sticker's dot down to face center, in millimeters, keyed by
    /// club code like `club`. Clubs not listed use the built-in values of
    /// [`ImpactCalibration::default`]; the putter has none.
    #[serde(
        skip_serializing_if = "is_default",
        alias = "dot_bottom_to_face_centre_mm"
    )]
    pub dot_bottom_to_face_center_mm: BTreeMap<String, f64>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            address: None,
            club: DEFAULT_CLUB.to_owned(),
            advanced_spin: true,
            discard_non_putting_zero_spin: true,
            dot_bottom_to_face_center_mm: BTreeMap::new(),
        }
    }
}

#[allow(clippy::ptr_arg)] // a `skip_serializing_if` function takes a reference to the field
fn is_default_club(v: &String) -> bool {
    is_default_value(&v.as_str(), &DEFAULT_CLUB)
}

#[allow(clippy::trivially_copy_pass_by_ref)] // a `skip_serializing_if` function takes a reference
fn is_default_true(v: &bool) -> bool {
    is_default_value(v, &true)
}

impl DriverConfig for Config {
    fn example() -> Self {
        Self {
            address: Some(String::new()),
            ..Self::default()
        }
    }
}

/// What the driver runs with, read from a [`Config`].
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "driver"), allow(dead_code))] // read by the driver
pub(crate) struct Settings {
    /// The pin, if one is set.
    #[cfg_attr(not(any(feature = "bluez", feature = "btleplug")), allow(dead_code))]
    // read by the BLE connect
    pub address: Option<String>,
    /// The club armed when the turn's hints name none.
    pub club: flightrelay::Club,
    pub spin: SpinMode,
    pub discard_zero_spin: bool,
    pub calibration: ImpactCalibration,
    /// Values that were ignored, for the host's log.
    pub warnings: Vec<String>,
}

impl Config {
    /// Interpret the lenient fields. Each value that is ignored adds a
    /// warning and leaves its default in place.
    #[cfg_attr(not(feature = "driver"), allow(dead_code))] // read by the driver
    pub(crate) fn settings(&self) -> Settings {
        let mut warnings = Vec::new();

        let address = self.address.as_deref().map(str::trim).and_then(|a| {
            if a.is_empty() {
                None
            } else if is_zero_address(a) {
                warnings.push(format!(
                    "ignoring configured address {a}: it is the placeholder macOS reports \
                     for a hidden BLE address, not a device address; auto-discovering instead"
                ));
                None
            } else {
                Some(a.to_owned())
            }
        });

        let club = parse_club(&self.club).unwrap_or_else(|| {
            warnings.push(format!(
                "unknown club '{}' in club, arming {DEFAULT_CLUB}",
                self.club
            ));
            flightrelay::Club::Iron7
        });

        let mut calibration = ImpactCalibration::default();
        for (key, &mm) in &self.dot_bottom_to_face_center_mm {
            let Some(frp) = parse_club(key) else {
                warnings.push(format!(
                    "unknown club '{key}' in dot_bottom_to_face_center_mm, ignoring"
                ));
                continue;
            };
            let Some(square) = exact_club(frp) else {
                let armed = nearest_club(frp).map_or_else(
                    || "another club".to_owned(),
                    |c| c.to_string().to_uppercase(),
                );
                warnings.push(format!(
                    "Square Golf has no code for '{key}'; it is armed as {armed}, whose \
                     calibration applies; ignoring its dot_bottom_to_face_center_mm"
                ));
                continue;
            };
            if !mm.is_finite() || mm < 0.0 {
                warnings.push(format!(
                    "invalid dot_bottom_to_face_center_mm for '{key}': {mm}, ignoring"
                ));
                continue;
            }
            calibration.set_dot_bottom_to_face_center_mm(square, Some(mm));
        }

        Settings {
            address,
            club,
            spin: if self.advanced_spin {
                SpinMode::Advanced
            } else {
                SpinMode::Standard
            },
            discard_zero_spin: self.discard_non_putting_zero_spin,
            calibration,
            warnings,
        }
    }
}

/// Whether `address` is the all-zero placeholder macOS reports in place of a
/// hidden BLE address. It identifies no device.
#[cfg_attr(not(feature = "driver"), allow(dead_code))] // read by the driver
fn is_zero_address(address: &str) -> bool {
    address.trim().eq_ignore_ascii_case("00:00:00:00:00:00")
}

/// Read a club code leniently: case and surrounding whitespace are ignored.
///
/// Numbered clubs take the digit-first (`"7I"`) or letter-first (`"I7"`)
/// form; hybrids also `"3HY"`. The driver also reads as `"D"`, `"DRIVER"`,
/// `"1W"` or `"W1"`, the mini driver as `"MINI"`, `"MINIDRIVER"` or
/// `"MINI DRIVER"`, and the putter as `"P"` or `"PUTTER"`. Wedges also read
/// from their loft (46/48 PW, 50/52 GW, 54/56 SW, 58/60/62/64 LW, with or
/// without a trailing `W`), and as `"A"`, `"AW"` or `"UW"` (gap), `"S"`
/// (sand), `"L"` or `"ULW"` (lob). `None` for anything else.
#[must_use]
pub fn parse_club(code: &str) -> Option<flightrelay::Club> {
    use flightrelay::Club as F;
    let upper = code.trim().to_uppercase();
    let bytes = upper.as_bytes();
    // A loft with a trailing `W` (e.g. "52W") reads as the bare loft.
    let code = if bytes.len() == 3
        && bytes[2] == b'W'
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
    {
        &upper[..2]
    } else {
        upper.as_str()
    };
    Some(match code {
        "DR" | "D" | "DRIVER" | "1W" | "W1" => F::Driver,
        "MD" | "MINI" | "MINIDRIVER" | "MINI DRIVER" => F::MiniDriver,
        "2W" | "W2" => F::Wood2,
        "3W" | "W3" => F::Wood3,
        "4W" | "W4" => F::Wood4,
        "5W" | "W5" => F::Wood5,
        "6W" | "W6" => F::Wood6,
        "7W" | "W7" => F::Wood7,
        "8W" | "W8" => F::Wood8,
        "9W" | "W9" => F::Wood9,
        "1H" | "H1" | "1HY" => F::Hybrid1,
        "2H" | "H2" | "2HY" => F::Hybrid2,
        "3H" | "H3" | "3HY" => F::Hybrid3,
        "4H" | "H4" | "4HY" => F::Hybrid4,
        "5H" | "H5" | "5HY" => F::Hybrid5,
        "6H" | "H6" | "6HY" => F::Hybrid6,
        "7H" | "H7" | "7HY" => F::Hybrid7,
        "8H" | "H8" | "8HY" => F::Hybrid8,
        "9H" | "H9" | "9HY" => F::Hybrid9,
        "1I" | "I1" => F::Iron1,
        "2I" | "I2" => F::Iron2,
        "3I" | "I3" => F::Iron3,
        "4I" | "I4" => F::Iron4,
        "5I" | "I5" => F::Iron5,
        "6I" | "I6" => F::Iron6,
        "7I" | "I7" => F::Iron7,
        "8I" | "I8" => F::Iron8,
        "9I" | "I9" => F::Iron9,
        "PW" | "46" | "48" => F::PitchingWedge,
        "GW" | "AW" | "UW" | "A" | "50" | "52" => F::GapWedge,
        "SW" | "S" | "54" | "56" => F::SandWedge,
        "LW" | "L" | "ULW" | "58" | "60" | "62" | "64" => F::LobWedge,
        "PT" | "P" | "PUTTER" => F::Putter,
        _ => return None,
    })
}

/// The Square Golf club the device is armed with for an FRP club.
///
/// A club the device has a code for maps to itself. One it has none for maps
/// to the nearest club of the same kind it does have, taking the stronger
/// loft on a tie: MD → DR; 2W, 4W → 3W; 6W → 5W; 8W, 9W → 7W; 1H, 2H → 3H;
/// 6H–9H → 5H; 1I, 2I → 3I. `None` for a club code this crate does not know.
#[must_use]
pub fn nearest_club(club: flightrelay::Club) -> Option<Club> {
    use flightrelay::Club as F;
    Some(match club {
        F::Driver | F::MiniDriver => Club::Driver,
        F::Wood2 | F::Wood3 | F::Wood4 => Club::Wood3,
        F::Wood5 | F::Wood6 => Club::Wood5,
        F::Wood7 | F::Wood8 | F::Wood9 => Club::Wood7,
        F::Hybrid1 | F::Hybrid2 | F::Hybrid3 => Club::Hybrid3,
        F::Hybrid4 => Club::Hybrid4,
        F::Hybrid5 | F::Hybrid6 | F::Hybrid7 | F::Hybrid8 | F::Hybrid9 => Club::Hybrid5,
        F::Iron1 | F::Iron2 | F::Iron3 => Club::Iron3,
        F::Iron4 => Club::Iron4,
        F::Iron5 => Club::Iron5,
        F::Iron6 => Club::Iron6,
        F::Iron7 => Club::Iron7,
        F::Iron8 => Club::Iron8,
        F::Iron9 => Club::Iron9,
        F::PitchingWedge => Club::PitchingWedge,
        F::GapWedge => Club::GapWedge,
        F::SandWedge => Club::SandWedge,
        F::LobWedge => Club::LobWedge,
        F::Putter => Club::Putter,
        _ => return None,
    })
}

/// The Square Golf club with the same code, or `None` for a club the device
/// has no code for (MD, 2W, 4W, 6W, 8W, 9W, 1H, 2H, 6H–9H, 1I, 2I).
#[cfg_attr(not(feature = "driver"), allow(dead_code))] // read by the driver
fn exact_club(club: flightrelay::Club) -> Option<Club> {
    use flightrelay::Club as F;
    match club {
        F::MiniDriver
        | F::Wood2
        | F::Wood4
        | F::Wood6
        | F::Wood8
        | F::Wood9
        | F::Hybrid1
        | F::Hybrid2
        | F::Hybrid6
        | F::Hybrid7
        | F::Hybrid8
        | F::Hybrid9
        | F::Iron1
        | F::Iron2 => None,
        _ => nearest_club(club),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flightrelay::Club as F;
    use serde_json::{Map, Value, json};

    fn map(value: &Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn the_default_config_writes_nothing_and_reads_back() {
        let config = Config::default();
        assert!(config.to_map().is_empty(), "{:?}", config.to_map());
        assert_eq!(Config::from_map(Map::new()), Ok(Config::default()));
        assert_eq!(Config::from_map(config.to_map()), Ok(config));
    }

    #[test]
    fn defaults_match_the_documented_values() {
        let s = Config::default().settings();
        assert_eq!(s.address, None);
        assert_eq!(s.club, F::Iron7);
        assert_eq!(s.spin, SpinMode::Advanced);
        assert!(s.discard_zero_spin);
        assert_eq!(s.calibration, ImpactCalibration::default());
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    }

    #[test]
    fn explicit_falses_are_written_and_read_back() {
        let config = Config {
            advanced_spin: false,
            discard_non_putting_zero_spin: false,
            ..Config::default()
        };
        let written = config.to_map();
        assert_eq!(written.get("advanced_spin"), Some(&json!(false)));
        assert_eq!(
            written.get("discard_non_putting_zero_spin"),
            Some(&json!(false))
        );
        assert_eq!(Config::from_map(written), Ok(config));
    }

    #[test]
    fn a_section_as_flighthook_writes_it_reads_the_same() {
        let config = Config::from_map(map(&json!({
            "address": "SquareGolf(CD01)",
            "club": "pw",
            "advanced_spin": true,
            "dot_bottom_to_face_centre_mm": { "7i": 21.5, "PT": 8 },
            "name": "a host key, ignored",
        })))
        .expect("reads");
        assert_eq!(config.address.as_deref(), Some("SquareGolf(CD01)"));
        assert_eq!(config.club, "pw");
        let s = config.settings();
        assert_eq!(s.club, F::PitchingWedge);
        assert_eq!(
            s.calibration.dot_bottom_to_face_center_mm(Club::Iron7),
            Some(21.5)
        );
        assert_eq!(
            s.calibration.dot_bottom_to_face_center_mm(Club::Putter),
            Some(8.0)
        );
        // Written with the American key.
        assert!(config.to_map().contains_key("dot_bottom_to_face_center_mm"));
    }

    #[test]
    fn a_wrong_type_names_the_field() {
        let e = Config::from_map(map(&json!({ "advanced_spin": "maybe" }))).unwrap_err();
        assert_eq!(e.field.as_deref(), Some("advanced_spin"), "{e}");
        let e = Config::from_map(map(
            &json!({ "dot_bottom_to_face_center_mm": { "7I": "x" } }),
        ))
        .unwrap_err();
        assert_eq!(
            e.field.as_deref(),
            Some("dot_bottom_to_face_center_mm.7I"),
            "{e}"
        );
        let e = Config::from_map(map(&json!({ "club": 7 }))).unwrap_err();
        assert_eq!(e.field.as_deref(), Some("club"), "{e}");
    }

    #[test]
    fn unknown_clubs_and_bad_calibration_are_warned_and_skipped() {
        let config = Config {
            club: "ZZ".into(),
            dot_bottom_to_face_center_mm: BTreeMap::from([
                ("ZZ".to_owned(), 10.0),
                ("2I".to_owned(), 10.0),
                ("8I".to_owned(), -1.0),
                ("9I".to_owned(), f64::NAN),
                ("GW".to_owned(), 30.0),
            ]),
            ..Config::default()
        };
        assert_eq!(config.validate(), Ok(()));
        let s = config.settings();
        assert_eq!(s.club, F::Iron7);
        assert_eq!(s.warnings.len(), 5, "{:#?}", s.warnings);
        assert!(s.warnings[0].contains("unknown club 'ZZ' in club"));
        assert!(
            s.warnings
                .iter()
                .any(|w| w.contains("'2I'") && w.contains("3I"))
        );
        let defaults = ImpactCalibration::default();
        for club in [Club::Iron3, Club::Iron8, Club::Iron9] {
            assert_eq!(
                s.calibration.dot_bottom_to_face_center_mm(club),
                defaults.dot_bottom_to_face_center_mm(club)
            );
        }
        assert_eq!(
            s.calibration.dot_bottom_to_face_center_mm(Club::GapWedge),
            Some(30.0)
        );
    }

    #[test]
    fn blank_and_placeholder_addresses_discover() {
        let blank = Config {
            address: Some("  ".into()),
            ..Config::default()
        };
        assert_eq!(blank.settings().address, None);
        assert!(blank.settings().warnings.is_empty());
        let zero = Config {
            address: Some("00:00:00:00:00:00".into()),
            ..Config::default()
        };
        let s = zero.settings();
        assert_eq!(s.address, None);
        assert_eq!(s.warnings.len(), 1);
        let pinned = Config {
            address: Some(" SquareGolf(CD01) ".into()),
            ..Config::default()
        };
        assert_eq!(
            pinned.settings().address.as_deref(),
            Some("SquareGolf(CD01)")
        );
    }

    #[test]
    fn club_codes_read_leniently() {
        assert_eq!(parse_club("7i"), Some(F::Iron7));
        assert_eq!(parse_club(" I7 "), Some(F::Iron7));
        assert_eq!(parse_club("dr"), Some(F::Driver));
        assert_eq!(parse_club("52W"), Some(F::GapWedge));
        assert_eq!(parse_club("putter"), Some(F::Putter));
        assert_eq!(parse_club("3hy"), Some(F::Hybrid3));
        assert_eq!(parse_club("mini driver"), Some(F::MiniDriver));
        assert_eq!(parse_club("7"), None);
        assert_eq!(parse_club(""), None);
        // Every FRP code reads as itself.
        for club in F::ALL {
            assert_eq!(parse_club(&club.to_string()), Some(*club), "{club}");
        }
    }

    #[test]
    fn clubs_without_a_code_map_to_the_nearest_of_their_kind() {
        assert_eq!(nearest_club(F::MiniDriver), Some(Club::Driver));
        assert_eq!(nearest_club(F::Wood2), Some(Club::Wood3));
        assert_eq!(nearest_club(F::Wood4), Some(Club::Wood3));
        assert_eq!(nearest_club(F::Wood6), Some(Club::Wood5));
        assert_eq!(nearest_club(F::Wood9), Some(Club::Wood7));
        assert_eq!(nearest_club(F::Hybrid1), Some(Club::Hybrid3));
        assert_eq!(nearest_club(F::Hybrid2), Some(Club::Hybrid3));
        assert_eq!(nearest_club(F::Hybrid9), Some(Club::Hybrid5));
        assert_eq!(nearest_club(F::Iron1), Some(Club::Iron3));
        assert_eq!(nearest_club(F::Iron2), Some(Club::Iron3));
        // Clubs with a code map to themselves.
        assert_eq!(nearest_club(F::Iron7), Some(Club::Iron7));
        assert_eq!(nearest_club(F::Putter), Some(Club::Putter));
        assert_eq!(exact_club(F::Iron7), Some(Club::Iron7));
        assert_eq!(exact_club(F::Iron2), None);
        // Every FRP club is either exact or nearest.
        for club in F::ALL {
            assert!(nearest_club(*club).is_some(), "{club}");
        }
    }
}
