//! Convert Square Golf shot data into FRP domain types.

use flightrelay::types::{BallFlight, ClubData as FrpClubData, FaceImpact};
use flightrelay::units::{Distance, Velocity};

use crate::protocol::{BallMetrics, ClubMetrics};

/// Convert [`BallMetrics`] to an FRP [`BallFlight`].
///
/// The device measures launch only — carry, total, roll, apex and flight time
/// are computed by the vendor's app, not the hardware, so they stay `None`.
///
/// Sign conventions line up with the spec without adjustment:
/// - `launch_azimuth` is "positive = right of target"; the device's `direction`
///   is already positive-right.
/// - `sidespin_rpm` is "positive = curves rightward"; the device pairs negative
///   sidespin with a negative spin axis (a draw for a right-hander), so its
///   polarity already matches. Note this differs from `GSPro`, which wants both
///   sidespin and spin axis negated.
#[must_use]
pub fn ball_flight(b: &BallMetrics) -> BallFlight {
    BallFlight {
        launch_speed: Some(Velocity::MetersPerSecond(b.speed)),
        launch_azimuth: Some(b.direction),
        launch_elevation: Some(b.launch_angle),
        carry_distance: None,
        total_distance: None,
        roll_distance: None,
        max_height: None,
        flight_time: None,
        backspin_rpm: Some(i32::from(b.back_spin)),
        sidespin_rpm: Some(i32::from(b.side_spin)),
    }
}

/// Convert [`ClubMetrics`] to FRP [`ClubData`](FrpClubData).
///
/// Every field is optional on both sides, so an untracked shot maps cleanly to
/// all-`None`. Swing plane, post-impact club speed, and the club offset/height
/// pair are not measured by this device — impact location is reported through
/// [`face_impact`] instead, which is its proper home.
#[must_use]
pub fn club_data(c: &ClubMetrics) -> FrpClubData {
    FrpClubData {
        club_speed: c.club_speed.map(Velocity::MetersPerSecond),
        club_speed_post: None,
        path: c.path,
        attack_angle: c.attack_angle,
        face_angle: c.face_angle,
        dynamic_loft: c.dynamic_loft,
        smash_factor: c.smash_factor,
        swing_plane_horizontal: None,
        swing_plane_vertical: None,
        club_offset: None,
        club_height: None,
    }
}

/// Convert [`ClubMetrics`] impact location to an FRP [`FaceImpact`], if present.
///
/// **The lateral sign is inverted.** FRP defines `lateral` as "positive =
/// toward toe"; the device reports negative toward the toe (verified against
/// the vendor app's own impact display for a right-handed player). `vertical`
/// needs no adjustment — both call positive "above centre".
///
/// Units are reported as millimetres. That is the best current reading of the
/// device's scale but it has **not** been checked against a reference launch
/// monitor, and zero is *assumed* to be face centre. If that turns out wrong,
/// this is the single place to fix it.
#[must_use]
pub fn face_impact(c: &ClubMetrics) -> Option<FaceImpact> {
    if c.impact_horizontal.is_none() && c.impact_vertical.is_none() {
        return None;
    }
    Some(FaceImpact {
        lateral: c.impact_horizontal.map(|v| Distance::Millimeters(-v)),
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
        assert_eq!(frp.sidespin_rpm, Some(-87));
        // Measured at launch only; the app computes the rest.
        assert_eq!(frp.carry_distance, None);
        assert_eq!(frp.total_distance, None);
        assert_eq!(frp.flight_time, None);
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
    }

    #[test]
    fn face_impact_lateral_sign_is_inverted() {
        let (_, club) = tracked_chip();
        let impact = face_impact(&club).expect("tracked shot has impact");
        // Device says -32.19 = toe. FRP says positive = toe.
        assert_eq!(impact.lateral, Some(Distance::Millimeters(32.19)));
        // Both agree that negative is below centre.
        assert_eq!(impact.vertical, Some(Distance::Millimeters(-17.43)));
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
