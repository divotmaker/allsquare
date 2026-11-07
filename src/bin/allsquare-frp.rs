//! allsquare-frp — Flight Relay Protocol device for Square Golf.
//!
//! Connects to a Square Golf Omni over BLE, arms it, and serves its shots to
//! FRP controllers.
//!
//! ```sh
//! allsquare-frp                                      # listen on 0.0.0.0:5880
//! allsquare-frp --set club=PW                        # arm a pitching wedge
//! allsquare-frp --target ws://flighthook:5880/frp    # dial a controller
//! allsquare-frp --help                               # every flag and setting
//! ```

#[cfg(not(any(feature = "bluez", feature = "btleplug")))]
compile_error!(
    "allsquare-frp also needs a transport: enable `bluez` (Linux) or \
     `btleplug` (Windows/macOS), e.g. --features frp,bluez"
);

use std::process::ExitCode;

fn main() -> ExitCode {
    flightrelay::run::<allsquare::driver::Omni>()
}
