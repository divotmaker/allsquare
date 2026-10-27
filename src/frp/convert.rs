//! Convert Square Golf shot data into FRP domain types.

use flightrelay::types::{BallFlight, ClubData as FrpClubData, FaceImpact};
use flightrelay::units::{Distance, Velocity};

use crate::protocol::{BallMetrics, ClubMetrics};

/// Convert [`BallMetrics`] to an FRP [`BallFlight`].
///
/// The device measures launch only — carry, landing, total, roll, apex and
/// flight time are computed by the vendor's app, not the hardware, so they stay
/// `None`. It reports both spin pairs (backspin + sidespin, total + axis), and
/// all four are sent. A total spin of 0, which the device reports for putts,
/// sends neither total spin nor axis.
///
/// Sign conventions:
/// - `launch_azimuth` is "positive = right of target"; the device's `direction`
///   is already positive-right, so it passes through.
/// - `sidespin_rpm` is "positive = curves rightward"; the device is the
///   opposite, so it is **negated**. Verified on course: without this, fades
///   draw and draws fade downstream.
/// - `spin_axis` is "positive = tilted right, curves rightward"; the device's
///   axis has the same inverted polarity as its sidespin, so it is **negated**
///   too.
#[must_use]
pub fn ball_flight(b: &BallMetrics) -> BallFlight {
    let spin_measured = b.total_spin > 0;
    BallFlight {
        launch_speed: Some(Velocity::MetersPerSecond(b.speed)),
        launch_azimuth: Some(b.direction),
        launch_elevation: Some(b.launch_angle),
        backspin_rpm: Some(i32::from(b.back_spin)),
        sidespin_rpm: Some(-i32::from(b.side_spin)),
        total_spin_rpm: spin_measured.then_some(i32::from(b.total_spin)),
        spin_axis: spin_measured.then_some(-b.spin_axis),
        ..BallFlight::default()
    }
}

/// Convert [`ClubMetrics`] to FRP [`ClubData`](FrpClubData).
///
/// Every field is optional on both sides, so an untracked shot maps cleanly to
/// all-`None`. Swing plane, post-impact club speed, spin loft, low point, and
/// the club offset/height pair are not measured by this device — impact
/// location is reported through [`face_impact`] instead, which is its proper
/// home.
#[must_use]
pub fn club_data(c: &ClubMetrics) -> FrpClubData {
    FrpClubData {
        club_speed: c.club_speed.map(Velocity::MetersPerSecond),
        path: c.path,
        attack_angle: c.attack_angle,
        face_angle: c.face_angle,
        dynamic_loft: c.dynamic_loft,
        smash_factor: c.smash_factor,
        ..FrpClubData::default()
    }
}

/// Convert [`ClubMetrics`] impact location to an FRP [`FaceImpact`], if present.
///
/// **The lateral sign is inverted.** FRP defines `lateral` as "positive =
/// toward toe"; the device reports negative toward the toe (verified against
/// the vendor app's own impact display for a right-handed player). `vertical`
/// needs no adjustment — both call positive "above centre".
///
/// Both are millimetres from face centre (beta calibration; see
/// [`ClubMetrics::impact_vertical`]). `vertical` may be `None` even when
/// `lateral` is present.
///
/// With the `raw-face-impact` feature, both values are the unscaled wire
/// values with the device's signs, labelled millimetres.
#[must_use]
pub fn face_impact(c: &ClubMetrics) -> Option<FaceImpact> {
    if c.impact_horizontal.is_none() && c.impact_vertical.is_none() {
        return None;
    }
    let lateral_sign = if cfg!(feature = "raw-face-impact") {
        1.0
    } else {
        -1.0
    };
    Some(FaceImpact {
        lateral: c
            .impact_horizontal
            .map(|v| Distance::Millimeters(lateral_sign * v)),
        vertical: c.impact_vertical.map(Distance::Millimeters),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real tracked wedge chip, from an Omni capture.
    fn tracked_chip() -> (BallMetrics, ClubMetrics) {
        let ball = BallMetrics {
            shot_type: 0x37,
            speed: 3.30,
            launch_angle: 32.51,
            direction: 7.39,
            total_spin: 627,
            spin_axis: -8.00,
            back_spin: 621,
            side_spin: -87,
        };
        let club = ClubMetrics {
            mask: 0xff,
            path: Some(4.45),
            face_angle: Some(8.13),
            attack_angle: Some(2.14),
            dynamic_loft: Some(40.73),
            impact_horizontal: Some(-32.19),
            impact_vertical: Some(-17.43),
            club_speed: Some(3.26),
            smash_factor: Some(1.01),
        };
        (ball, club)
    }

    #[test]
    fn ball_flight_conversion() {
        let (ball, _) = tracked_chip();
        let frp = ball_flight(&ball);
        assert_eq!(frp.launch_speed, Some(Velocity::MetersPerSecond(3.30)));
        assert_eq!(frp.launch_elevation, Some(32.51));
        assert_eq!(frp.launch_azimuth, Some(7.39));
        assert_eq!(frp.backspin_rpm, Some(621));
        // Wire -87 (device: negative = curves right) -> FRP +87 (positive =
        // curves right). The sign is inverted, not passed through.
        assert_eq!(frp.sidespin_rpm, Some(87));
        assert_eq!(frp.total_spin_rpm, Some(627));
        // Wire -8.00 (device: negative = tilts right) -> FRP +8.0, the same
        // sense as the negated sidespin.
        assert_eq!(frp.spin_axis, Some(8.0));
        // Measured at launch only; the app computes the rest.
        assert_eq!(frp.carry_distance, None);
        assert_eq!(frp.carry_offline, None);
        assert_eq!(frp.total_distance, None);
        assert_eq!(frp.total_offline, None);
        assert_eq!(frp.flight_time, None);
        assert_eq!(frp.descent_angle, None);
    }

    #[test]
    fn spin_axis_and_sidespin_share_a_sign() {
        let (ball, _) = tracked_chip();
        // A draw for a right-hander: device sidespin and axis both positive.
        let draw = BallMetrics {
            spin_axis: 6.5,
            side_spin: 340,
            ..ball
        };
        let frp = ball_flight(&draw);
        assert_eq!(frp.sidespin_rpm, Some(-340));
        assert_eq!(frp.spin_axis, Some(-6.5));
    }

    #[test]
    fn spin_pairs_agree() {
        let (ball, _) = tracked_chip();
        let frp = ball_flight(&ball);
        let mut components = BallFlight {
            backspin_rpm: frp.backspin_rpm,
            sidespin_rpm: frp.sidespin_rpm,
            ..BallFlight::default()
        };
        components.complete_spin();
        assert_eq!(components.total_spin_rpm, frp.total_spin_rpm);
        let axis = components.spin_axis.expect("axis");
        assert!((axis - frp.spin_axis.expect("axis")).abs() < 0.1);
    }

    #[test]
    fn zero_spin_sends_neither_total_nor_axis() {
        // Captured 1.59 m/s putt: every spin field zero.
        let putt = BallMetrics {
            shot_type: 0x37,
            speed: 1.59,
            launch_angle: 0.72,
            direction: -0.36,
            total_spin: 0,
            spin_axis: 0.0,
            back_spin: 0,
            side_spin: 0,
        };
        let frp = ball_flight(&putt);
        assert_eq!(frp.total_spin_rpm, None);
        assert_eq!(frp.spin_axis, None);
        assert_eq!(frp.backspin_rpm, Some(0));
        assert_eq!(frp.sidespin_rpm, Some(0));
    }

    #[test]
    fn club_data_conversion() {
        let (_, club) = tracked_chip();
        let frp = club_data(&club);
        assert_eq!(frp.club_speed, Some(Velocity::MetersPerSecond(3.26)));
        assert_eq!(frp.face_angle, Some(8.13));
        assert_eq!(frp.path, Some(4.45));
        assert_eq!(frp.attack_angle, Some(2.14));
        // The Omni reports these two, unlike the R10.
        assert_eq!(frp.dynamic_loft, Some(40.73));
        assert_eq!(frp.smash_factor, Some(1.01));
        // Impact goes to face_impact, not here.
        assert_eq!(frp.club_offset, None);
        assert_eq!(frp.club_height, None);
        // Not measured by this device.
        assert_eq!(frp.spin_loft, None);
        assert_eq!(frp.low_point_distance, None);
    }

    #[cfg(not(feature = "raw-face-impact"))]
    #[test]
    fn face_impact_lateral_sign_is_inverted() {
        let (_, club) = tracked_chip();
        let impact = face_impact(&club).expect("tracked shot has impact");
        // Device says -32.19 = toe. FRP says positive = toe.
        assert_eq!(impact.lateral, Some(Distance::Millimeters(32.19)));
        // Both agree that negative is below centre.
        assert_eq!(impact.vertical, Some(Distance::Millimeters(-17.43)));
    }

    #[cfg(feature = "raw-face-impact")]
    #[test]
    fn raw_face_impact_passes_through() {
        let club = ClubMetrics {
            impact_horizontal: Some(-3219.0),
            impact_vertical: Some(-1743.0),
            ..tracked_chip().1
        };
        let impact = face_impact(&club).expect("tracked shot has impact");
        assert_eq!(impact.lateral, Some(Distance::Millimeters(-3219.0)));
        assert_eq!(impact.vertical, Some(Distance::Millimeters(-1743.0)));
    }

    #[test]
    fn untracked_shot_maps_to_nothing() {
        // A putt with no sticker: mask 0, every field the 0xffff sentinel.
        let club = ClubMetrics::default();
        let frp = club_data(&club);
        assert_eq!(frp.club_speed, None);
        assert_eq!(frp.smash_factor, None);
        assert_eq!(face_impact(&club), None);
    }
}
