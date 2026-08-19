//! allsquare-frp — Flight Relay Protocol device for Square Golf.
//!
//! Connects to a Square Golf Omni (or Home) over BLE, arms it, and streams shot
//! data over FRP to a controller.
//!
//! The first argument selects the transport direction. A `ws://` or `wss://`
//! URL bridges this device to a central controller such as flighthook; anything
//! else is a bind address that controllers connect to.
//!
//! ```sh
//! allsquare-frp                              # serve on 0.0.0.0:5880, 7-iron
//! allsquare-frp 0.0.0.0:5880 pw              # serve, explicit address and club
//! allsquare-frp ws://flighthook:5880/frp pw  # bridge to flighthook
//! ```

#[cfg(not(any(feature = "bluez", feature = "btleplug")))]
compile_error!(
    "allsquare-frp also needs a transport: enable `bluez` (Linux) or \
     `btleplug` (Windows/macOS), e.g. --features frp,bluez"
);

use std::io::Write;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use allsquare::frp::FrpDevice;
use allsquare::{Client, Club, Event, SpinMode, ble};

fn club_from_arg(arg: Option<&str>) -> Club {
    match arg.unwrap_or("7i") {
        "driver" | "dr" => Club::Driver,
        "3w" => Club::Wood3,
        "3h" => Club::Hybrid3,
        "5h" => Club::Hybrid5,
        "4i" => Club::Iron4,
        "5i" => Club::Iron5,
        "6i" => Club::Iron6,
        "8i" => Club::Iron8,
        "9i" => Club::Iron9,
        "pw" => Club::PitchingWedge,
        "gw" => Club::GapWedge,
        "sw" => Club::SandWedge,
        "lw" => Club::LobWedge,
        "putter" | "pt" => Club::Putter,
        _ => Club::Iron7,
    }
}

fn main() -> ExitCode {
    let frp_target = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:5880".to_owned());
    let club = club_from_arg(std::env::args().nth(2).as_deref());
    let bridging = frp_target.starts_with("ws://") || frp_target.starts_with("wss://");

    // Open the endpoint first so it connects while we are still scanning.
    let frp = if bridging {
        eprintln!("allsquare-frp: bridging to controller at {frp_target}");
        FrpDevice::bridge(&frp_target, "allsquare")
    } else {
        eprintln!("allsquare-frp: serving controllers on {frp_target}");
        FrpDevice::serve(&frp_target)
    };
    let mut frp = match frp {
        Ok(s) => s,
        Err(e) => {
            eprintln!("allsquare-frp: failed to open FRP endpoint: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Scan and connect. The device is never paired or bonded.
    eprintln!("allsquare-frp: scanning for device...");
    let transport = match ble::connect(None) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("allsquare-frp: failed to connect: {e}");
            return ExitCode::FAILURE;
        }
    };
    let device_name = transport.name().to_owned();
    eprintln!("allsquare-frp: connected to {device_name}");
    frp.set_device_name(&device_name);

    let mut client = Client::new(transport);

    loop {
        match client.poll() {
            Ok(Some(event)) => {
                match &event {
                    Event::Connected {
                        firmware, model, ..
                    } => {
                        eprintln!("allsquare-frp: model {model}, firmware lm {}", firmware.lm);
                        frp.set_firmware(&firmware.lm);
                        frp.set_model(model);
                        if let Err(e) = frp.send_device_info() {
                            eprintln!("allsquare-frp: device_info failed: {e}");
                        }
                        if let Err(e) = client.arm(club, SpinMode::Advanced) {
                            eprintln!("allsquare-frp: arm failed: {e}");
                            return ExitCode::FAILURE;
                        }
                        eprintln!("allsquare-frp: armed with {club}");
                    }
                    Event::Shot { ball, club: c } => {
                        eprintln!(
                            "allsquare-frp: shot — {:.2} m/s, launch {:.1}°, spin {} rpm{}",
                            ball.speed,
                            ball.launch_angle,
                            ball.total_spin,
                            if c.is_some() { "" } else { " (no club data)" }
                        );
                    }
                    Event::StateChanged(s) => eprintln!("allsquare-frp: state {s:?}"),
                    _ => {}
                }

                if let Err(e) = frp.handle_event(&event) {
                    eprintln!("allsquare-frp: FRP send error: {e}");
                }
            }
            Ok(None) => {
                // Adopt a newly established controller connection
                match frp.poll_connection() {
                    Ok(true) => eprintln!("allsquare-frp: controller connected"),
                    Ok(false) => {}
                    Err(e) => eprintln!("allsquare-frp: telemetry resend failed: {e}"),
                }

                // Square Golf has no device-side shot mode — putting and
                // chipping differ only by club — so a detection-mode request is
                // logged and ignored rather than silently dropped.
                if let Some(mode) = frp.check_controller() {
                    eprintln!(
                        "allsquare-frp: controller asked for detection mode {mode} \
                         — not applicable, select a club instead"
                    );
                }
                thread::sleep(Duration::from_millis(2));
            }
            Err(e) => {
                eprintln!("allsquare-frp: poll error: {e}");
                return ExitCode::FAILURE;
            }
        }
        let _ = std::io::stderr().flush();
    }
}
