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
//! allsquare-frp                              # serve on 0.0.0.0:5880, driver
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

use allsquare::frp::{ControllerCommand, FrpDevice};
use allsquare::{Client, Club, ClubMetrics, Event, Handed, SpinMode, Transport, ble};

fn club_from_arg(arg: Option<&str>) -> Club {
    match arg.unwrap_or("driver") {
        "7i" => Club::Iron7,
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
        _ => Club::Driver,
    }
}

/// The Square Golf club for an FRP club, `None` for clubs the device has no code for.
fn club_from_frp(club: flightrelay::Club) -> Option<Club> {
    use flightrelay::Club as F;
    Some(match club {
        F::Driver => Club::Driver,
        F::Wood3 => Club::Wood3,
        F::Wood5 => Club::Wood5,
        F::Wood7 => Club::Wood7,
        F::Hybrid3 => Club::Hybrid3,
        F::Hybrid4 => Club::Hybrid4,
        F::Hybrid5 => Club::Hybrid5,
        F::Iron3 => Club::Iron3,
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

fn handed_from_frp(handed: flightrelay::Handedness) -> Handed {
    match handed {
        flightrelay::Handedness::Right => Handed::Right,
        flightrelay::Handedness::Left => Handed::Left,
    }
}

/// Apply a controller's club and hand to the device; the detection mode has no
/// Square Golf counterpart and is only logged.
fn apply_controller_command<T: Transport>(
    client: &mut Client<T>,
    cmd: ControllerCommand,
    club: &mut Club,
    armed: bool,
) {
    eprintln!(
        "allsquare-frp: controller command: mode {:?}, hand {:?}, club {:?}",
        cmd.mode, cmd.handed, cmd.club
    );
    if let Some(handed) = cmd.handed {
        client.set_handed(handed_from_frp(handed));
    }
    if let Some(requested) = cmd.club {
        match club_from_frp(requested) {
            Some(c) => *club = c,
            None => {
                eprintln!(
                    "allsquare-frp: no Square Golf code for club {requested}, keeping {club}"
                );
            }
        }
    }
    // Before the device is armed, the club and the hand are applied by the
    // arm that follows the handshake.
    if armed && (cmd.club.is_some() || cmd.handed.is_some()) {
        match client.select_club(*club) {
            Ok(()) => eprintln!("allsquare-frp: selected {club}"),
            Err(e) => eprintln!("allsquare-frp: club select failed: {e}"),
        }
    }
}

/// Format an optional club metric, `-` when the device did not track it.
fn fmt(v: Option<f64>) -> String {
    v.map_or_else(|| "-".to_string(), |v| format!("{v:.2}"))
}

/// Format the impact location: unscaled wire values with `raw-face-impact`.
fn fmt_impact(c: &ClubMetrics) -> String {
    if cfg!(feature = "raw-face-impact") {
        let raw = |v: Option<f64>| v.map_or_else(|| "-".to_string(), |v| format!("{v:.0}"));
        format!(
            "impact raw H {} / V {}",
            raw(c.impact_horizontal),
            raw(c.impact_vertical)
        )
    } else {
        format!(
            "impact H {} mm / V {} mm (beta)",
            fmt(c.impact_horizontal),
            fmt(c.impact_vertical)
        )
    }
}

fn main() -> ExitCode {
    let frp_target = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:5880".to_owned());
    let mut club = club_from_arg(std::env::args().nth(2).as_deref());
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
    let mut armed = false;

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
                        armed = true;
                    }
                    Event::Shot { ball, club: c } => {
                        eprintln!(
                            "allsquare-frp: shot — {:.2} m/s, launch {:.1}°, spin {} rpm{}",
                            ball.speed,
                            ball.launch_angle,
                            ball.total_spin,
                            if c.is_some() { "" } else { " (no club data)" }
                        );
                        if let Some(c) = c {
                            eprintln!(
                                "allsquare-frp:   club {} m/s, path {}°, face {}°, attack {}°, \
                                 loft {}°, {}",
                                fmt(c.club_speed),
                                fmt(c.path),
                                fmt(c.face_angle),
                                fmt(c.attack_angle),
                                fmt(c.dynamic_loft),
                                fmt_impact(c),
                            );
                        }
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
                // chipping differ only by club — so a detection mode is logged
                // and ignored; the club and the player's hand are applied.
                if let Some(cmd) = frp.check_controller() {
                    apply_controller_command(&mut client, cmd, &mut club, armed);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frp_clubs_map_to_square_clubs() {
        assert_eq!(club_from_frp(flightrelay::Club::Driver), Some(Club::Driver));
        assert_eq!(club_from_frp(flightrelay::Club::Iron7), Some(Club::Iron7));
        assert_eq!(
            club_from_frp(flightrelay::Club::SandWedge),
            Some(Club::SandWedge)
        );
        assert_eq!(club_from_frp(flightrelay::Club::Putter), Some(Club::Putter));
    }

    #[test]
    fn frp_clubs_without_a_square_code_map_to_none() {
        assert_eq!(club_from_frp(flightrelay::Club::MiniDriver), None);
        assert_eq!(club_from_frp(flightrelay::Club::Iron1), None);
        assert_eq!(club_from_frp(flightrelay::Club::Hybrid9), None);
    }

    #[test]
    fn frp_handedness_maps_to_square_handedness() {
        assert_eq!(
            handed_from_frp(flightrelay::Handedness::Right),
            Handed::Right
        );
        assert_eq!(handed_from_frp(flightrelay::Handedness::Left), Handed::Left);
    }
}
