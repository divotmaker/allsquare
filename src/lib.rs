//! Client library for the **Square Golf Omni** launch monitor, by Invant.
//!
//! # Device support
//!
//! Verified against an **Omni** (hw 3.1, fw lm 1.5.8) on Linux and Windows.
//!
//! The original **Square / Square Home is not supported**. It shares the GATT
//! profile and most of the wire protocol, but its club-selection codes are a
//! different scheme entirely — the original encodes regular-vs-swing-stick in
//! the second byte where the Omni encodes a club category, so
//! [`Club::code`](club::Club::code) would select the wrong club on every club
//! but the putter. It also reports battery through the standard `0x2a19`
//! characteristic rather than the Omni's `0x91` notification, and has an
//! alignment command this crate does not implement. Supporting it means a
//! second club table and a device-family split, plus hardware to test on.
//!
//! Reverse engineered for interoperability under DMCA §1201(f). No licensing is
//! circumvented, no proprietary code is reproduced, and no features are unlocked.
//!
//! # Shape of the API
//!
//! Synchronous and caller-driven, like [`ironsight`] and [`10over`]: wrap a
//! [`Transport`] in a [`Client`] and call [`Client::poll`] in a loop.
//!
//! ```no_run
//! use allsquare::{Client, Event, Club, SpinMode};
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # fn connect() -> Result<impl allsquare::Transport, Box<dyn std::error::Error>> {
//! #     struct T; impl allsquare::Transport for T {
//! #         fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> { unimplemented!() }
//! #         fn write(&mut self, _: &[u8]) -> std::io::Result<()> { unimplemented!() }
//! #         fn read_characteristic(&mut self, _: u128) -> std::io::Result<Vec<u8>> { unimplemented!() }
//! #     }
//! #     Ok(T)
//! # }
//! let mut client = Client::new(connect()?);
//! client.arm(Club::Iron7, SpinMode::Advanced)?;
//!
//! loop {
//!     match client.poll()? {
//!         Some(Event::Shot { ball, club }) => {
//!             println!("{:.2} m/s, {:.1}°", ball.speed, ball.launch_angle);
//!             if let Some(c) = club {
//!                 println!("  club {:?} m/s", c.club_speed);
//!             }
//!         }
//!         Some(other) => println!("{other:?}"),
//!         None => std::thread::sleep(std::time::Duration::from_millis(20)),
//!     }
//! }
//! # }
//! ```
//!
//! # Things that will bite you
//!
//! These are device behaviours, not API quirks, and [`Client`] handles all of
//! them — but they matter if you use [`protocol`] directly:
//!
//! - **Never pair or bond.** The device does neither, and OS pairing dialogs
//!   fail. Connect as a plain GATT client.
//! - **The device sends every notification twice**, byte-identical. Deduplicate
//!   or every shot counts twice.
//! - **Club metrics are pull-based and can be stale.** The device retains the
//!   last shot across disconnects, so a `0x87` response must be correlated with
//!   a preceding ball packet.
//! - **The device is a GATT *client* too.** It sends ATT requests to the host
//!   after connecting; if nothing answers them it terminates the link at ~33s.
//!   Any normal OS GATT stack answers them for you — this only matters if you
//!   bypass one (e.g. a raw L2CAP socket on Linux).
//! - **`0x11` is not a universal prefix.** `0x91` (battery) and `0x71` (clock)
//!   arrive unprompted.
//!
//! # Units
//!
//! Speeds are m/s, angles degrees, spin RPM. The device's unit setting
//! ([`Client::set_units`]) affects only its own display, never the wire.
//!
//! [`ironsight`]: https://github.com/divotmaker/ironsight
//! [`10over`]: https://github.com/divotmaker/10over

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod client;
pub mod club;
pub mod error;
pub mod protocol;

#[cfg(any(feature = "bluez", feature = "btleplug"))]
pub mod ble;

#[cfg(feature = "bluez")]
pub mod bluez;

#[cfg(feature = "frp")]
pub mod frp;

#[cfg(feature = "btleplug")]
pub mod btleplug_transport;

pub use client::{Client, Event, Firmware, Transport};
pub use club::{Category, Club, Handed};
pub use error::{Error, Result};
pub use protocol::{
    BallMetrics, ChargingState, ClubMetrics, Command, DeviceState, DistanceUnit, Notification,
    Sensor, SpeedUnit, SpinMode,
};
