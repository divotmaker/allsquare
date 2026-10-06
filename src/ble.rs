//! Platform-agnostic transport selection.
//!
//! Picks the backend appropriate to the target and enabled features, so callers
//! do not need `cfg` blocks. If both are enabled on Linux, `BlueZ` wins — it is
//! the verified path there.

use crate::error::Result;

#[cfg(feature = "bluez")]
pub use crate::bluez::BluezTransport;
#[cfg(feature = "btleplug")]
pub use crate::btleplug_transport::BtleplugTransport;

/// The transport this build will use.
#[cfg(feature = "bluez")]
pub type BleTransport = BluezTransport;

/// The transport this build will use.
#[cfg(all(feature = "btleplug", not(feature = "bluez")))]
pub type BleTransport = BtleplugTransport;

/// Scan for a Square Golf device and connect to it.
///
/// Pass `address` to select a specific device, or `None` to take the first
/// Square Golf device found. No pairing is performed.
///
/// A pinned value matches, case-insensitively, either the device's identifier
/// (its BLE address, or on macOS the peripheral UUID) or its advertised name,
/// e.g. `SquareGolf(54E4)`. The advertised name is the same on every OS, so it
/// is the portable way to pin a device.
///
/// # Errors
/// [`crate::Error::NotFound`] if no device appears, or a backend error.
#[cfg(feature = "bluez")]
pub fn connect(address: Option<&str>) -> Result<BleTransport> {
    BluezTransport::connect(address, "hci0")
}

/// Scan for a Square Golf device and connect to it.
///
/// Pass `address` to select a specific device, or `None` to take the first
/// Square Golf device found. No pairing is performed.
///
/// A pinned value matches, case-insensitively, either the device's identifier
/// (its BLE address, or on macOS the peripheral UUID) or its advertised name,
/// e.g. `SquareGolf(54E4)`. The advertised name is the same on every OS, so it
/// is the portable way to pin a device.
///
/// # Errors
/// [`crate::Error::NotFound`] if no device appears, or a backend error.
#[cfg(all(feature = "btleplug", not(feature = "bluez")))]
pub fn connect(address: Option<&str>) -> Result<BleTransport> {
    BtleplugTransport::connect(address)
}

/// Whether a device matches a pinned value.
///
/// `address` is the device's identifier and `reported` the name the BLE stack
/// reports for it. Matches the identifier or the advertised `SquareGolf…`
/// name, ignoring case and surrounding whitespace in the pin. A blank pin
/// matches nothing.
pub(crate) fn matches_pin(want: &str, address: &str, reported: &str) -> bool {
    let want = want.trim();
    !want.is_empty()
        && (address.eq_ignore_ascii_case(want)
            || crate::protocol::advertised_name(reported)
                .is_some_and(|n| n.eq_ignore_ascii_case(want)))
}

#[cfg(test)]
mod tests {
    use super::matches_pin;

    #[test]
    fn pins_by_address_ignoring_case() {
        assert!(matches_pin("dc:0d:30:62:54:e4", "DC:0D:30:62:54:E4", ""));
        assert!(matches_pin(
            "5F1A3C9E-0B6D-4E52-9A41-7C3D2B1E8F60",
            "5f1a3c9e-0b6d-4e52-9a41-7c3d2b1e8f60",
            "SGO300A"
        ));
    }

    #[test]
    fn pins_by_advertised_name() {
        assert!(matches_pin("SquareGolf(54E4)", "", "SquareGolf(54E4)"));
        assert!(matches_pin(
            "squaregolf(54e4)",
            "",
            "SGO300A [SquareGolf(54E4)]"
        ));
    }

    #[test]
    fn rejects_other_devices() {
        assert!(!matches_pin(
            "SquareGolf(54E4)",
            "DC:0D:30:62:11:22",
            "SquareGolf(1122)"
        ));
        // The GAP name is shared by every Omni, so it cannot pin one.
        assert!(!matches_pin("SGO300A", "", "SGO300A"));
        assert!(!matches_pin("", "", ""));
    }

    #[test]
    fn trims_the_pin() {
        assert!(matches_pin(" SquareGolf(54E4)\n", "", "SquareGolf(54E4)"));
        assert!(matches_pin("\tdc:0d:30:62:54:e4 ", "DC:0D:30:62:54:E4", ""));
    }

    #[test]
    fn blank_pin_matches_nothing() {
        assert!(!matches_pin("   ", "", ""));
        assert!(!matches_pin(" \t", "DC:0D:30:62:54:E4", "SquareGolf(54E4)"));
    }
}
