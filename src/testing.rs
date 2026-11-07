//! Notification fixtures and encoders for tests (`testing` feature).
//!
//! [`protocol::parse`] decodes what the device sends; this module goes the
//! other way, so a test of code that consumes notifications (a transport
//! double, a capture filter, an event mapper) can build them without writing
//! wire bytes. Every function here is the inverse of the matching `parse`
//! branch for any value the device can send: `parse(&encode(x))` yields `x`,
//! and the tests pin the exact bytes.
//!
//! The fixtures describe a synthetic device: they carry no device identity.
//! The feature is meant for `dev-dependencies`; nothing in the crate's own
//! [`Client`](crate::Client) depends on it.
//!
//! ```
//! use allsquare::protocol::{self, DeviceState, Notification};
//! use allsquare::testing;
//!
//! let ack = testing::heartbeat_ack(DeviceState::Ready);
//! assert_eq!(
//!     protocol::parse(&ack).unwrap(),
//!     Notification::HeartbeatAck(DeviceState::Ready)
//! );
//!
//! let ball = testing::BallMetrics {
//!     speed: 40.0,
//!     ..testing::ball_metrics()
//! };
//! let frame = testing::ball_notification(&ball);
//! assert_eq!(protocol::parse(&frame).unwrap(), Notification::Ball(ball));
//! ```
//!
//! [`protocol::parse`]: crate::protocol::parse

pub use crate::protocol::{BallMetrics, ClubMetrics, DeviceState};

use crate::protocol::{CLUB_SCALE, CLUB_SENTINEL, IMPACT_SCALE};

/// The shot-type byte the Omni sends with every ball notification, putts
/// included.
pub const TYPICAL_SHOT_TYPE: u8 = 0x37;

/// A ball notification: 40 m/s, 15° launch, 1° right, 5000 rpm total spin,
/// 2° spin axis, 4990 rpm backspin, 300 rpm sidespin. Its shot-type byte is
/// `0x01`, not [`TYPICAL_SHOT_TYPE`]; [`ball_notification`] encodes any other.
pub const BALL_NOTIFICATION: [u8; 17] = [
    0x11, 0x02, 0x01, 0xa0, 0x0f, 0xdc, 0x05, 0x64, 0x00, 0x88, 0x13, 0xc8, 0x00, 0x7e, 0x13, 0x2c,
    0x01,
];

/// A query reply with a three-byte payload.
pub const QUERY_RESPONSE: [u8; 5] = [0x11, 0x06, 0x17, 0x01, 0x00];

/// The short club notification with an empty validity mask and no field
/// values. The Omni sends the full-length form ([`club_notification`] of
/// [`ClubMetrics::default`]); the parser accepts both.
pub const CLUB_NOTIFICATION_NO_DATA: [u8; 3] = [0x11, 0x07, 0x00];

/// Ball metrics for a shot that measured nothing: every value zero, with the
/// [`TYPICAL_SHOT_TYPE`]. Override the fields a test cares about with struct
/// update syntax.
#[must_use]
pub fn ball_metrics() -> BallMetrics {
    BallMetrics {
        shot_type: TYPICAL_SHOT_TYPE,
        speed: 0.0,
        launch_angle: 0.0,
        direction: 0.0,
        total_spin: 0,
        spin_axis: 0.0,
        back_spin: 0,
        side_spin: 0,
    }
}

/// A wire value: `value * scale`, rounded and clamped to `i16`.
fn wire(value: f64, scale: f64) -> i16 {
    let v = (value * scale)
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX));
    // The clamp keeps `v` inside `i16`; a NaN becomes 0.
    #[allow(clippy::cast_possible_truncation)]
    {
        v as i16
    }
}

/// Encode a ball notification (`0x11 0x02`, 17 bytes).
///
/// Speed and angles are rounded to the device's 1/100 resolution and clamped
/// to the `i16` wire range; NaN encodes as 0. The spin fields are written
/// unchanged.
#[must_use]
pub fn ball_notification(ball: &BallMetrics) -> [u8; 17] {
    let mut b = [0u8; 17];
    b[0] = 0x11;
    b[1] = 0x02;
    b[2] = ball.shot_type;
    b[3..5].copy_from_slice(&wire(ball.speed, 100.0).to_le_bytes());
    b[5..7].copy_from_slice(&wire(ball.launch_angle, 100.0).to_le_bytes());
    b[7..9].copy_from_slice(&wire(ball.direction, 100.0).to_le_bytes());
    b[9..11].copy_from_slice(&ball.total_spin.to_le_bytes());
    b[11..13].copy_from_slice(&wire(ball.spin_axis, 100.0).to_le_bytes());
    b[13..15].copy_from_slice(&ball.back_spin.to_le_bytes());
    b[15..17].copy_from_slice(&ball.side_spin.to_le_bytes());
    b
}

/// Encode a club notification in the Omni's full form (`0x11 0x07`, 19 bytes,
/// eight fields).
///
/// A field that is `None` is written as the `0xffff` sentinel. The validity
/// mask is `club.mask` widened by one bit for every `Some` field, so every
/// measured field parses back; the result equals `club` whenever `club.mask`
/// already covers its measured fields, as it does for parsed metrics.
///
/// Values are rounded to the wire resolution (1/100, or 1 for the two impact
/// fields under the `raw-face-impact` feature, which carry unscaled wire
/// values) and clamped to the `i16` range; NaN encodes as 0.
///
/// The device marks an unmeasured field with the wire value `-1`, so a
/// measured value that rounds to it, `-0.01` (`-1.0` for the impact fields
/// under `raw-face-impact`), is indistinguishable from an unmeasured field and
/// parses back as `None`.
#[must_use]
pub fn club_notification(club: &ClubMetrics) -> [u8; 19] {
    let fields = [
        (club.path, CLUB_SCALE),
        (club.face_angle, CLUB_SCALE),
        (club.attack_angle, CLUB_SCALE),
        (club.dynamic_loft, CLUB_SCALE),
        (club.impact_horizontal, IMPACT_SCALE),
        (club.impact_vertical, IMPACT_SCALE),
        (club.club_speed, CLUB_SCALE),
        (club.smash_factor, CLUB_SCALE),
    ];
    let mut b = [0u8; 19];
    b[0] = 0x11;
    b[1] = 0x07;
    let mut mask = club.mask;
    for (i, (value, scale)) in fields.into_iter().enumerate() {
        let raw = match value {
            Some(v) => {
                mask |= 1 << i;
                wire(v, scale)
            }
            None => CLUB_SENTINEL,
        };
        b[3 + 2 * i..5 + 2 * i].copy_from_slice(&raw.to_le_bytes());
    }
    b[2] = mask;
    b
}

fn state_byte(state: DeviceState) -> u8 {
    match state {
        DeviceState::None => 0,
        DeviceState::Idle => 1,
        DeviceState::Init => 2,
        DeviceState::Detect => 3,
        DeviceState::Ready => 4,
        DeviceState::Shot => 5,
        DeviceState::Done => 6,
        DeviceState::Unknown(v) => v,
    }
}

/// Encode a heartbeat acknowledgment (`0x11 0x03`, 9 bytes) carrying
/// `state`.
///
/// Bytes 2, 4 and 5 carry fixed values; `parse` ignores them (see `WIRE.md`
/// §6.3).
#[must_use]
pub fn heartbeat_ack(state: DeviceState) -> [u8; 9] {
    [
        0x11,
        0x03,
        0x85,
        state_byte(state),
        0x04,
        0x03,
        0x00,
        0x00,
        0x00,
    ]
}

/// Encode a query reply (`0x11 0x06`) with the given payload.
#[must_use]
pub fn query_response(payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(2 + payload.len());
    v.extend_from_slice(&[0x11, 0x06]);
    v.extend_from_slice(payload);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Notification, parse};

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    fn ball_of(frame: &[u8]) -> BallMetrics {
        match parse(frame).expect("parses") {
            Notification::Ball(b) => b,
            other => panic!("expected ball, got {other:?}"),
        }
    }

    fn club_of(frame: &[u8]) -> ClubMetrics {
        match parse(frame).expect("parses") {
            Notification::Club(c) => c,
            other => panic!("expected club, got {other:?}"),
        }
    }

    // The byte literals below are written out independently of the encoders:
    // they pin the exact wire bytes.

    #[test]
    fn ball_fixture_parses_to_its_documented_values() {
        let expected = BallMetrics {
            shot_type: 0x01,
            speed: 40.0,
            launch_angle: 15.0,
            direction: 1.0,
            total_spin: 5000,
            spin_axis: 2.0,
            back_spin: 4990,
            side_spin: 300,
        };
        assert_eq!(
            BALL_NOTIFICATION,
            [
                0x11, 0x02, 0x01, 0xa0, 0x0f, 0xdc, 0x05, 0x64, 0x00, 0x88, 0x13, 0xc8, 0x00, 0x7e,
                0x13, 0x2c, 0x01,
            ]
        );
        assert_eq!(ball_of(&BALL_NOTIFICATION), expected);
        assert_eq!(ball_notification(&expected), BALL_NOTIFICATION);
    }

    #[test]
    fn typical_ball_round_trips() {
        assert_eq!(TYPICAL_SHOT_TYPE, 0x37);
        let zero = ball_metrics();
        assert_eq!(zero.shot_type, 0x37);
        assert_eq!(ball_of(&ball_notification(&zero)), zero);

        let fade = BallMetrics {
            speed: 3.30,
            launch_angle: 32.51,
            direction: 7.39,
            total_spin: 627,
            spin_axis: -8.00,
            back_spin: 621,
            side_spin: -87,
            ..ball_metrics()
        };
        assert_eq!(ball_of(&ball_notification(&fade)), fade);
    }

    #[test]
    fn ball_encoding_matches_a_putt_as_the_device_sent_it() {
        let frame = hex("1102379f004800dcff0000000000000000");
        assert_eq!(ball_notification(&ball_of(&frame)).to_vec(), frame);
    }

    #[test]
    fn ball_encoding_clamps_to_the_wire_range() {
        let wild = BallMetrics {
            speed: 1.0e9,
            launch_angle: -1.0e9,
            direction: f64::NAN,
            ..ball_metrics()
        };
        let b = ball_of(&ball_notification(&wild));
        assert!((b.speed - 327.67).abs() < 1e-9);
        assert!((b.launch_angle - -327.68).abs() < 1e-9);
        assert!(b.direction.abs() < f64::EPSILON);
    }

    #[test]
    fn heartbeat_ack_matches_the_pinned_frame() {
        assert_eq!(
            heartbeat_ack(DeviceState::Ready),
            [0x11, 0x03, 0x85, 0x04, 0x04, 0x03, 0, 0, 0]
        );
    }

    #[test]
    fn heartbeat_ack_round_trips_every_state() {
        for state in [
            DeviceState::None,
            DeviceState::Idle,
            DeviceState::Init,
            DeviceState::Detect,
            DeviceState::Ready,
            DeviceState::Shot,
            DeviceState::Done,
            DeviceState::Unknown(0x7f),
        ] {
            assert_eq!(
                parse(&heartbeat_ack(state)).expect("parses"),
                Notification::HeartbeatAck(state)
            );
        }
    }

    #[test]
    fn query_response_matches_the_pinned_frames() {
        assert_eq!(QUERY_RESPONSE, [0x11, 0x06, 0x17, 0x01, 0x00]);
        assert_eq!(query_response(&[0x17, 0x01, 0x00]), QUERY_RESPONSE);
        assert_eq!(query_response(&[0xaa, 0xbb]), [0x11, 0x06, 0xaa, 0xbb]);
        assert_eq!(
            parse(&QUERY_RESPONSE).expect("parses"),
            Notification::QueryResponse(vec![0x17, 0x01, 0x00])
        );
        assert_eq!(
            parse(&query_response(&[0xaa, 0xbb])).expect("parses"),
            Notification::QueryResponse(vec![0xaa, 0xbb])
        );
        assert_eq!(
            parse(&query_response(&[])).expect("parses"),
            Notification::QueryResponse(Vec::new())
        );
    }

    #[test]
    fn no_data_club_fixture_parses_to_empty_metrics() {
        assert_eq!(CLUB_NOTIFICATION_NO_DATA, [0x11, 0x07, 0x00]);
        let c = club_of(&CLUB_NOTIFICATION_NO_DATA);
        assert_eq!(c, ClubMetrics::default());
        assert!(c.is_empty());
    }

    #[test]
    fn default_club_encodes_the_omni_untracked_form() {
        assert_eq!(
            club_notification(&ClubMetrics::default()).to_vec(),
            hex("110700ffffffffffffffffffffffffffffffff")
        );
        assert_eq!(
            club_of(&club_notification(&ClubMetrics::default())),
            ClubMetrics::default()
        );
    }

    #[test]
    fn club_encoding_reproduces_a_tracked_frame() {
        let frame = hex("1107ffbd012d03d600e90f6df331f946016500");
        let c = club_of(&frame);
        assert_eq!(c.mask, 0xff);
        assert_eq!(club_notification(&c).to_vec(), frame);
    }

    #[test]
    fn club_round_trips_a_partial_mask() {
        // Bits 0, 2 and 6 set; the other fields carry the sentinel.
        let c = ClubMetrics {
            mask: 0b0100_0101,
            path: Some(-2.5),
            attack_angle: Some(-4.0),
            club_speed: Some(38.5),
            ..ClubMetrics::default()
        };
        assert_eq!(club_of(&club_notification(&c)), c);
    }

    #[test]
    fn club_value_that_rounds_to_the_sentinel_parses_as_none() {
        let c = ClubMetrics {
            path: Some(-0.01),
            ..ClubMetrics::default()
        };
        let frame = club_notification(&c);
        assert_eq!(frame[3..5], [0xff, 0xff]);
        assert_eq!(frame[2], 0b1);
        assert!(club_of(&frame).path.is_none());
    }

    #[test]
    fn club_mask_widens_to_cover_measured_fields() {
        let c = ClubMetrics {
            club_speed: Some(38.5),
            smash_factor: Some(1.31),
            ..ClubMetrics::default()
        };
        let parsed = club_of(&club_notification(&c));
        assert_eq!(parsed.mask, 0b1100_0000);
        assert_eq!(parsed.club_speed, c.club_speed);
        assert_eq!(parsed.smash_factor, c.smash_factor);
        assert!(parsed.path.is_none());
    }
}
