//! Connect to a Square Golf device, arm it, and print shots.
//!
//! ```sh
//! cargo run --example monitor --features bluez             # Linux
//! cargo run --example monitor --features btleplug          # Windows/macOS
//! cargo run --example monitor --features bluez -- 7i
//! ```

use std::time::Duration;

use allsquare::{Client, Club, Event, SpinMode, ble};

fn club_from_arg(arg: Option<&str>) -> Club {
    match arg.unwrap_or("7i") {
        "driver" | "dr" => Club::Driver,
        "3w" => Club::Wood3,
        "5h" => Club::Hybrid5,
        "4i" => Club::Iron4,
        "5i" => Club::Iron5,
        "6i" => Club::Iron6,
        "8i" => Club::Iron8,
        "9i" => Club::Iron9,
        "pw" => Club::PitchingWedge,
        "gw" => Club::GapWedge,
        "sw" => Club::SandWedge,
        "putter" | "pt" => Club::Putter,
        _ => Club::Iron7,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let club = club_from_arg(args.first().map(String::as_str));

    println!("scanning...");
    let transport = ble::connect(None)?;
    println!(
        "connected to {} [{}]",
        transport.name(),
        transport.address()
    );

    let mut client = Client::new(transport);

    loop {
        match client.poll()? {
            Some(Event::Connected {
                firmware,
                hardware,
                device_id,
                model,
            }) => {
                println!("model {model}");
                println!("firmware lm {} (mmi {})", firmware.lm, firmware.mmi);
                println!("hardware {hardware}, id {device_id}");
                client.arm(club, SpinMode::Advanced)?;
                println!("armed with {club} — hit a shot\n");
            }
            Some(Event::Shot { ball, club: c }) => {
                println!(
                    "SHOT  {:.2} m/s  launch {:.1}°  dir {:.1}°  spin {} rpm (axis {:.1}°)",
                    ball.speed, ball.launch_angle, ball.direction, ball.total_spin, ball.spin_axis
                );
                match c {
                    Some(c) => println!(
                        "      club {}  path {}  face {}  attack {}  loft {}\n      impact {} / {}  smash {}",
                        fmt(c.club_speed, "m/s"),
                        fmt(c.path, "°"),
                        fmt(c.face_angle, "°"),
                        fmt(c.attack_angle, "°"),
                        fmt(c.dynamic_loft, "°"),
                        fmt(c.impact_horizontal, "mm"),
                        fmt(c.impact_vertical, "mm"),
                        fmt(c.smash_factor, ""),
                    ),
                    None => println!("      (no club data — sticker not tracked)"),
                }
            }
            Some(Event::StateChanged(s)) => println!("state {s:?}"),
            Some(Event::Battery { percent, state }) => {
                println!("battery {percent}% {state:?}");
            }
            Some(Event::Sensor(s)) if s.detected => {
                println!("ball detected (ready={})", s.ready);
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn fmt(v: Option<f64>, unit: &str) -> String {
    v.map_or_else(|| "--".to_string(), |x| format!("{x:.2}{unit}"))
}
