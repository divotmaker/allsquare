//! Wire protocol: command builders and notification parsing.
//!
//! Everything is little-endian. Angles and speeds are `i16` scaled by 100;
//! spin rates are raw RPM.
//!
//! Commands are `[0x00] [type] [seq] [payload…]`, always **9 bytes**. Byte 0
//! reads as a direction field rather than a magic number — the device also
//! accepts `0x11` there, which is what the original Square's clients send.
//!
//! Notifications mostly begin `0x11`, but not always: `0x91` (battery) and
//! `0x71` (clock) arrive unprompted. Parse byte 0 as a message family.

use crate::club::{Club, Handed};
use crate::error::{Error, Result};

/// BLE service and characteristic UUIDs. All custom UUIDs share the base
/// `8660xxxx-6b7e-439a-bdd1-489a3213e9bb`.
pub mod uuid {
    /// EXGOLF service.
    pub const SERVICE: u128 = 0x8660_1001_6b7e_439a_bdd1_489a_3213_e9bb;
    /// Device ID (read).
    pub const DEVICE_ID: u128 = 0x8660_2001_6b7e_439a_bdd1_489a_3213_e9bb;
    /// Hardware version (read).
    pub const HW_VERSION: u128 = 0x8660_2002_6b7e_439a_bdd1_489a_3213_e9bb;
    /// Firmware version (read) — returns JSON.
    pub const FW_VERSION: u128 = 0x8660_2003_6b7e_439a_bdd1_489a_3213_e9bb;
    /// Real-time clock (read/write, LE u64). Reads seconds-since-boot until set.
    pub const TIMESTAMP: u128 = 0x8660_2004_6b7e_439a_bdd1_489a_3213_e9bb;
    /// Commands to the device. Declares Write *with* response.
    pub const CMD: u128 = 0x8660_2101_6b7e_439a_bdd1_489a_3213_e9bb;
    /// Notifications from the device.
    pub const EVT: u128 = 0x8660_2102_6b7e_439a_bdd1_489a_3213_e9bb;
    /// Standard GAP device name. Reports a model identifier, e.g. `SGO300A`
    /// on an Omni — "SGO" for Square Golf Omni, then the model code that also
    /// appears in the advertised manufacturer data (`0300A`).
    pub const GAP_NAME: u128 = 0x0000_2a00_0000_1000_8000_0080_5f9b_34fb;
    /// Standard battery service. Present but **unimplemented on the Omni**,
    /// which reports battery via the `0x91` notification instead.
    pub const BATTERY: u16 = 0x2a19;
}

/// Name prefix advertised by both the Home and the Omni.
pub const NAME_PREFIX: &str = "SquareGolf";

/// Every command is exactly this long.
pub const COMMAND_LEN: usize = 9;

/// Recommended heartbeat interval.
pub const HEARTBEAT_SECS: u64 = 5;

/// Spin measurement mode, the second parameter of `DetectBall`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpinMode {
    /// `0x10`.
    Standard,
    /// `0x11`. What the official app uses.
    #[default]
    Advanced,
}

/// Speed unit for the device's own display. Does not affect the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpeedUnit {
    /// Metres per second.
    #[default]
    MetersPerSecond = 0,
    /// Miles per hour.
    MilesPerHour = 1,
}

/// Distance unit for the device's own display. Does not affect the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DistanceUnit {
    /// Metres.
    #[default]
    Meter = 0,
    /// Yards for long distances, feet for short.
    YardFeet = 1,
    /// Yards throughout.
    YardYard = 2,
}

/// A command to the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Start or stop ball detection.
    DetectBall {
        /// Enable or disable detection.
        on: bool,
        /// Spin measurement mode.
        spin: SpinMode,
    },
    /// Select the active club.
    SelectClub {
        /// Club to select.
        club: Club,
        /// Player handedness.
        handed: Handed,
    },
    /// Keepalive, every 5s.
    Heartbeat,
    /// Status query. The app sends this first; answered by a `0x06`.
    Query,
    /// Request club metrics for the last shot.
    RequestClubMetrics,
    /// Set the units shown on the device's own display.
    SetUnits {
        /// Speed unit for the device display.
        speed: SpeedUnit,
        /// Distance unit for the device display.
        distance: DistanceUnit,
    },
    /// Green speed, `0..=5` mapping to stimp 8..13.
    SetGreenSpeed(u8),
    /// Carry distance adjustment, as a percentage.
    SetCarryAdjustment(u8),
}

impl Command {
    /// Command type byte.
    #[must_use]
    pub const fn type_byte(self) -> u8 {
        match self {
            Command::DetectBall { .. } => 0x81,
            Command::SelectClub { .. } => 0x82,
            Command::Heartbeat => 0x83,
            Command::Query => 0x86,
            Command::RequestClubMetrics => 0x87,
            Command::SetUnits { .. } => 0x88,
            Command::SetGreenSpeed(_) => 0x89,
            Command::SetCarryAdjustment(_) => 0x8A,
        }
    }

    /// Encode into the 9-byte wire form.
    #[must_use]
    pub fn encode(self, seq: u8) -> [u8; COMMAND_LEN] {
        let mut b = [0u8; COMMAND_LEN];
        b[0] = 0x00;
        b[1] = self.type_byte();
        b[2] = seq;
        match self {
            Command::DetectBall { on, spin } => {
                b[3] = u8::from(on);
                b[4] = match spin {
                    SpinMode::Standard => 0x10,
                    SpinMode::Advanced => 0x11,
                };
            }
            Command::SelectClub { club, handed } => {
                let (number, category) = club.code();
                b[3] = number;
                b[4] = category as u8;
                b[5] = handed.code();
            }
            Command::SetUnits { speed, distance } => {
                // Byte 3 is a third setting whose meaning is not yet identified;
                // the app sends both 0 and 1. Zero is safe.
                b[4] = speed as u8;
                b[5] = distance as u8;
            }
            Command::SetGreenSpeed(v) | Command::SetCarryAdjustment(v) => b[3] = v,
            Command::Heartbeat | Command::Query | Command::RequestClubMetrics => {}
        }
        b
    }
}

/// Device state machine, reported in every heartbeat ack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DeviceState {
    /// No club/idle state.
    None,
    /// Idle.
    Idle,
    /// Initialising.
    Init,
    /// Looking for a ball.
    Detect,
    /// Ball detected and ready.
    Ready,
    /// Shot in progress.
    Shot,
    /// Shot complete.
    Done,
    /// A value outside the documented range.
    Unknown(u8),
}

impl From<u8> for DeviceState {
    fn from(v: u8) -> Self {
        match v {
            0 => DeviceState::None,
            1 => DeviceState::Idle,
            2 => DeviceState::Init,
            3 => DeviceState::Detect,
            4 => DeviceState::Ready,
            5 => DeviceState::Shot,
            6 => DeviceState::Done,
            other => DeviceState::Unknown(other),
        }
    }
}

/// Charging status accompanying a battery notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ChargingState {
    /// Not charging.
    NotCharging,
    /// Running on battery.
    Discharging,
    /// Charging.
    Charging,
    /// Charged.
    Full,
    /// No battery present.
    NoBattery,
    /// Unrecognised value.
    Unknown,
}

impl From<u8> for ChargingState {
    fn from(v: u8) -> Self {
        match v {
            0 => ChargingState::NotCharging,
            1 => ChargingState::Discharging,
            2 => ChargingState::Charging,
            3 => ChargingState::Full,
            4 => ChargingState::NoBattery,
            _ => ChargingState::Unknown,
        }
    }
}

/// Ball launch data. Identical on the Home and the Omni.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BallMetrics {
    /// Raw shot-type byte. Always `0x37` on the Omni, including for putts —
    /// it does **not** distinguish putt from full swing on this device.
    pub shot_type: u8,
    /// Ball speed, m/s.
    pub speed: f64,
    /// Launch angle, degrees.
    pub launch_angle: f64,
    /// Launch direction, degrees. Positive is right for a right-hander.
    pub direction: f64,
    /// Total spin, RPM.
    pub total_spin: i16,
    /// Spin axis, degrees, as reported by the device: **negative tilts the
    /// ball right** (a fade for a right-hander). This is the opposite of the
    /// usual convention — see [`side_spin`](Self::side_spin).
    pub spin_axis: f64,
    /// Backspin, RPM.
    pub back_spin: i16,
    /// Sidespin, RPM, as reported by the device: **negative curves the ball
    /// right** (a fade for a right-hander), the same polarity the device uses
    /// for [`spin_axis`](Self::spin_axis). Consumers expecting the common
    /// "positive = rightward" convention (including FRP and `GSPro`) must negate
    /// this; [`frp::ball_flight`](crate::frp::ball_flight) already does.
    pub side_spin: i16,
}

/// Club data for the last shot. Pull-based: request it with
/// [`Command::RequestClubMetrics`] after a ball packet.
///
/// Fields are `None` when the device did not measure them. Both the validity
/// bitmask *and* the per-field `0xffff` sentinel are checked.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ClubMetrics {
    /// Raw validity bitmask; bit *n* corresponds to field *n*.
    pub mask: u8,
    /// Club path, degrees.
    pub path: Option<f64>,
    /// Face angle, degrees.
    pub face_angle: Option<f64>,
    /// Attack angle, degrees. Negative is descending.
    pub attack_angle: Option<f64>,
    /// Dynamic loft, degrees.
    pub dynamic_loft: Option<f64>,
    /// Horizontal impact position. Negative is toward the toe (right-handed).
    ///
    /// Scale is believed to be millimetres, and zero is *assumed* to be face
    /// centre; neither has been verified against a reference launch monitor.
    pub impact_horizontal: Option<f64>,
    /// Vertical impact position. Negative is low on the face. Same caveats as
    /// [`Self::impact_horizontal`].
    pub impact_vertical: Option<f64>,
    /// Club head speed, m/s.
    pub club_speed: Option<f64>,
    /// Smash factor. Equals ball speed / club speed.
    pub smash_factor: Option<f64>,
}

impl ClubMetrics {
    /// Whether the device measured anything at all for this shot.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.club_speed.is_none() && self.path.is_none() && self.face_angle.is_none()
    }
}

/// Ball position and detection state, emitted while detection is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Sensor {
    /// Device is ready for a shot.
    pub ready: bool,
    /// A ball is present.
    pub detected: bool,
    /// Ball position; units undetermined.
    pub position: (i32, i32, i32),
}

/// A decoded notification.
#[derive(Debug, Clone, PartialEq)]
pub enum Notification {
    /// Ball detection state.
    Sensor(Sensor),
    /// Ball launch data.
    Ball(BallMetrics),
    /// Heartbeat ack, carrying the device state machine.
    HeartbeatAck(DeviceState),
    /// Alignment aim angle, degrees. Home only.
    Alignment(f64),
    /// Response to [`Command::Query`]. Payload meaning not yet identified.
    QueryResponse(Vec<u8>),
    /// Club data for the last shot.
    Club(ClubMetrics),
    /// Battery level and charging state. Unsolicited, `0x91`.
    Battery {
        /// Charge level, 0-100.
        percent: u8,
        /// Charging status.
        state: ChargingState,
    },
    /// Device clock, matching the timestamp characteristic. Unsolicited, `0x71`.
    Clock(u64),
}

/// The sentinel a club field carries when the device has no value for it.
const CLUB_SENTINEL: i16 = -1;

fn i16_at(b: &[u8], off: usize) -> i16 {
    i16::from_le_bytes([b[off], b[off + 1]])
}

fn i32_at(b: &[u8], off: usize) -> i32 {
    i32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn need(kind: &'static str, data: &[u8], n: usize) -> Result<()> {
    if data.len() < n {
        return Err(Error::Truncated {
            kind,
            len: data.len(),
            need: n,
        });
    }
    Ok(())
}

/// Scale a club field, mapping the sentinel to `None`.
fn club_field(raw: i16, mask: u8, bit: u8) -> Option<f64> {
    if raw == CLUB_SENTINEL || mask & (1 << bit) == 0 {
        None
    } else {
        Some(f64::from(raw) / 100.0)
    }
}

/// Parse one notification payload.
///
/// # Errors
/// Returns [`Error::Disconnected`] for an empty payload — the device signals
/// teardown that way — or a parse error for malformed or unknown frames.
pub fn parse(data: &[u8]) -> Result<Notification> {
    if data.is_empty() {
        return Err(Error::Disconnected);
    }

    match data[0] {
        // Battery, unsolicited.
        0x91 => {
            need("battery", data, 3)?;
            Ok(Notification::Battery {
                percent: data[1],
                state: ChargingState::from(data[2]),
            })
        }
        // Device clock, unsolicited. Big-endian here, unlike everything else,
        // even though the matching characteristic is little-endian.
        0x71 => {
            need("clock", data, 9)?;
            let mut v = [0u8; 8];
            v.copy_from_slice(&data[1..9]);
            Ok(Notification::Clock(u64::from_be_bytes(v)))
        }
        0x11 => parse_11(data),
        other => Err(Error::UnknownFamily(other)),
    }
}

fn parse_11(data: &[u8]) -> Result<Notification> {
    need("notification", data, 2)?;
    match data[1] {
        0x01 => {
            need("sensor", data, 17)?;
            Ok(Notification::Sensor(Sensor {
                // Observed as 0x01/0x02 for ready.
                ready: data[3] == 0x01 || data[3] == 0x02,
                detected: data[4] == 0x01,
                position: (i32_at(data, 5), i32_at(data, 9), i32_at(data, 13)),
            }))
        }
        0x02 => {
            need("ball", data, 17)?;
            Ok(Notification::Ball(BallMetrics {
                shot_type: data[2],
                speed: f64::from(i16_at(data, 3)) / 100.0,
                launch_angle: f64::from(i16_at(data, 5)) / 100.0,
                direction: f64::from(i16_at(data, 7)) / 100.0,
                total_spin: i16_at(data, 9),
                spin_axis: f64::from(i16_at(data, 11)) / 100.0,
                back_spin: i16_at(data, 13),
                side_spin: i16_at(data, 15),
            }))
        }
        0x03 => {
            need("heartbeat ack", data, 4)?;
            Ok(Notification::HeartbeatAck(DeviceState::from(data[3])))
        }
        0x04 => {
            need("alignment", data, 7)?;
            Ok(Notification::Alignment(f64::from(i16_at(data, 5)) / 100.0))
        }
        0x06 => Ok(Notification::QueryResponse(data[2..].to_vec())),
        0x07 => {
            need("club", data, 3)?;
            let mask = data[2];
            let body = &data[3..];
            // The Home answers "no data" with a bare 3-byte frame; the Omni
            // always sends all eight fields using sentinels. Handle both.
            let get = |i: usize, bit: u8| -> Option<f64> {
                if body.len() < (i + 1) * 2 {
                    return None;
                }
                club_field(i16_at(body, i * 2), mask, bit)
            };
            Ok(Notification::Club(ClubMetrics {
                mask,
                path: get(0, 0),
                face_angle: get(1, 1),
                attack_angle: get(2, 2),
                dynamic_loft: get(3, 3),
                impact_horizontal: get(4, 4),
                impact_vertical: get(5, 5),
                club_speed: get(6, 6),
                smash_factor: get(7, 7),
            }))
        }
        other => Err(Error::UnknownNotification(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    #[test]
    fn commands_are_nine_bytes_and_zero_prefixed() {
        for cmd in [
            Command::Heartbeat,
            Command::Query,
            Command::RequestClubMetrics,
            Command::SetGreenSpeed(3),
        ] {
            let b = cmd.encode(0x42);
            assert_eq!(b.len(), COMMAND_LEN);
            assert_eq!(b[0], 0x00);
            assert_eq!(b[2], 0x42);
        }
    }

    #[test]
    fn encodes_commands_captured_from_the_official_app() {
        // 00 83 1c 00...  heartbeat
        assert_eq!(
            Command::Heartbeat.encode(0x1c).to_vec(),
            hex("00831c000000000000")
        );
        // 00 81 0a 01 11  detect on, advanced spin
        assert_eq!(
            Command::DetectBall {
                on: true,
                spin: SpinMode::Advanced
            }
            .encode(0x0a)
            .to_vec(),
            hex("00810a011100000000")
        );
        // 00 82 06 0a 02 00  select PW, right handed
        assert_eq!(
            Command::SelectClub {
                club: Club::PitchingWedge,
                handed: Handed::Right
            }
            .encode(0x06)
            .to_vec(),
            hex("0082060a0200000000")
        );
        // 00 87 10  request club metrics
        assert_eq!(
            Command::RequestClubMetrics.encode(0x10).to_vec(),
            hex("008710000000000000")
        );
    }

    #[test]
    fn parses_a_real_putt() {
        // Captured: 1.59 m/s putt.
        let n = parse(&hex("1102379f004800dcff0000000000000000")).expect("parses");
        let Notification::Ball(b) = n else {
            panic!("expected ball")
        };
        assert_eq!(b.shot_type, 0x37);
        assert!((b.speed - 1.59).abs() < 1e-9);
        assert!((b.launch_angle - 0.72).abs() < 1e-9);
        assert!((b.direction - -0.36).abs() < 1e-9);
        assert_eq!(b.total_spin, 0);
    }

    #[test]
    fn parses_a_tracked_club_packet() {
        // Captured wedge chip, mask 0xff, all eight fields valid.
        let n = parse(&hex("1107ffbd012d03d600e90f6df331f946016500")).expect("parses");
        let Notification::Club(c) = n else {
            panic!("expected club")
        };
        assert_eq!(c.mask, 0xff);
        assert!((c.path.expect("path") - 4.45).abs() < 1e-9);
        assert!((c.face_angle.expect("face") - 8.13).abs() < 1e-9);
        assert!((c.dynamic_loft.expect("loft") - 40.73).abs() < 1e-9);
        assert!((c.impact_horizontal.expect("impact h") - -32.19).abs() < 1e-9);
        assert!(!c.is_empty());

        // smash == ball speed / club speed is what pins both scalings.
        let club_speed = c.club_speed.expect("club speed");
        let smash = c.smash_factor.expect("smash");
        let ball_speed = 3.30_f64;
        assert!((ball_speed / club_speed - smash).abs() < 0.01);
    }

    #[test]
    fn untracked_club_packet_yields_no_fields() {
        // Omni form: full length, mask 0, every field the 0xffff sentinel.
        let n = parse(&hex("110700ffffffffffffffffffffffffffffffff")).expect("parses");
        let Notification::Club(c) = n else {
            panic!("expected club")
        };
        assert_eq!(c.mask, 0x00);
        assert!(c.is_empty());
        assert!(c.path.is_none() && c.smash_factor.is_none());

        // Home form: bare 3-byte frame. Must not panic on the short body.
        let n = parse(&hex("110700")).expect("parses");
        let Notification::Club(c) = n else {
            panic!("expected club")
        };
        assert!(c.is_empty());
    }

    #[test]
    fn parses_unsolicited_families() {
        let n = parse(&hex("916403")).expect("parses");
        assert_eq!(
            n,
            Notification::Battery {
                percent: 100,
                state: ChargingState::Full
            }
        );

        let n = parse(&hex("710000000000000551")).expect("parses");
        assert_eq!(n, Notification::Clock(0x0551));
    }

    #[test]
    fn heartbeat_ack_carries_device_state() {
        let n = parse(&hex("110385040403000000")).expect("parses");
        assert_eq!(n, Notification::HeartbeatAck(DeviceState::Ready));
        let n = parse(&hex("11032f000003000000")).expect("parses");
        assert_eq!(n, Notification::HeartbeatAck(DeviceState::None));
    }

    #[test]
    fn parses_sensor_with_ball_present() {
        let n = parse(&hex("1101270101b8220000a4fbffff36ffffff")).expect("parses");
        let Notification::Sensor(s) = n else {
            panic!("expected sensor")
        };
        assert!(s.ready && s.detected);
        assert_eq!(s.position, (8888, -1116, -202));
    }

    #[test]
    fn empty_payload_means_disconnect() {
        assert!(matches!(parse(&[]), Err(Error::Disconnected)));
    }

    #[test]
    fn truncated_frames_error_rather_than_panic() {
        assert!(matches!(parse(&hex("1102")), Err(Error::Truncated { .. })));
        assert!(matches!(parse(&hex("9164")), Err(Error::Truncated { .. })));
        assert!(matches!(
            parse(&hex("55aa")),
            Err(Error::UnknownFamily(0x55))
        ));
    }
}
