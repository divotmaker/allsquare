//! allsquare-frp — Flight Relay Protocol device server for Square Golf.
//!
//! Connects to a Square Golf Omni (or Home) over BLE, arms it, and serves shot
//! data over FRP (WebSocket, port 5880) to any connected controller.
//!
//! ```sh
//! allsquare-frp                       # 7-iron, 0.0.0.0:5880
//! allsquare-frp 0.0.0.0:5880 pw       # explicit address and club
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

use allsquare::frp::FrpServer;
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
    let frp_addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:5880".to_owned());
    let club = club_from_arg(std::env::args().nth(2).as_deref());

    eprintln!("allsquare-frp: FRP server on {frp_addr}");

    // Bind first so a controller can connect while we are still scanning.
    let mut frp = match FrpServer::bind(&frp_addr) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("allsquare-frp: failed to bind FRP server: {e}");
            return ExitCode::FAILURE;
        }
    };

    eprintln!("allsquare-frp: waiting for FRP controller...");
    if let Err(e) = frp.accept() {
        eprintln!("allsquare-frp: controller accept failed: {e}");
        return ExitCode::FAILURE;
    }
    eprintln!("allsquare-frp: controller connected");

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
