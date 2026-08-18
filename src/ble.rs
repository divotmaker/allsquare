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
/// Pass `address` to select a specific device, or `None` to take the first one
/// advertising the `SquareGolf` name prefix. No pairing is performed.
///
/// # Errors
/// [`crate::Error::NotFound`] if no device appears, or a backend error.
#[cfg(feature = "bluez")]
pub fn connect(address: Option<&str>) -> Result<BleTransport> {
    BluezTransport::connect(address, "hci0")
}

/// Scan for a Square Golf device and connect to it.
///
/// # Errors
/// [`crate::Error::NotFound`] if no device appears, or a backend error.
#[cfg(all(feature = "btleplug", not(feature = "bluez")))]
pub fn connect(address: Option<&str>) -> Result<BleTransport> {
    BtleplugTransport::connect(address)
}
