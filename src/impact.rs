//! Face impact calibration.

use std::collections::HashMap;

use crate::club::Club;

/// Per-club face impact calibration: the distance from the bottom edge of the
/// club sticker's dot down to face centre, in millimetres.
///
/// The Omni measures vertical impact from the bottom of the dot (to within
/// about a millimetre), so [`Client`](crate::Client) adds this distance to
/// report vertical impact as millimetres from face centre. Clubs differ in
/// shape and size, and stickers in placement, so measuring your own clubs
/// gives the best results. A club with no value reports no vertical impact.
///
/// [`Default`] holds values for one set of clubs with the sticker in its
/// recommended spot (dot centre about 5 mm below the top of the club). The
/// driver, hybrid, iron and lob wedge values are fitted against a reference
/// launch monitor; the fairway wood and gap/sand wedge values add the measured
/// difference in dot position to the driver and iron values respectively. All
/// are rounded to whole millimetres, toward direct measurement of the clubs.
#[derive(Debug, Clone, PartialEq)]
pub struct ImpactCalibration {
    dot_bottom_to_face_centre_mm: HashMap<Club, f64>,
}

impl ImpactCalibration {
    /// A calibration with no clubs: vertical impact is reported for none.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            dot_bottom_to_face_centre_mm: HashMap::new(),
        }
    }

    /// Distance from the bottom of the dot down to face centre for `club`, in mm.
    #[must_use]
    pub fn dot_bottom_to_face_centre_mm(&self, club: Club) -> Option<f64> {
        self.dot_bottom_to_face_centre_mm.get(&club).copied()
    }

    /// Set (or, with `None`, clear) the distance from the bottom of the dot
    /// down to face centre for `club`, in mm.
    pub fn set_dot_bottom_to_face_centre_mm(&mut self, club: Club, mm: Option<f64>) {
        match mm {
            Some(mm) => self.dot_bottom_to_face_centre_mm.insert(club, mm),
            None => self.dot_bottom_to_face_centre_mm.remove(&club),
        };
    }
}

impl Default for ImpactCalibration {
    fn default() -> Self {
        let mut c = Self::empty();
        let table: [(&[Club], f64); 6] = [
            (&[Club::Driver], 15.0),
            (&[Club::Wood3, Club::Wood5, Club::Wood7], 9.0),
            (&[Club::Hybrid3, Club::Hybrid4, Club::Hybrid5], 8.0),
            (
                &[
                    Club::Iron3,
                    Club::Iron4,
                    Club::Iron5,
                    Club::Iron6,
                    Club::Iron7,
                    Club::Iron8,
                    Club::Iron9,
                    Club::PitchingWedge,
                ],
                19.0,
            ),
            (&[Club::GapWedge, Club::SandWedge], 21.0),
            (&[Club::LobWedge], 24.0),
        ];
        for (clubs, mm) in table {
            for &club in clubs {
                c.set_dot_bottom_to_face_centre_mm(club, Some(mm));
            }
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_offsets() {
        let c = ImpactCalibration::default();
        let offset = |club| c.dot_bottom_to_face_centre_mm(club).expect("estimate");
        assert!((offset(Club::Driver) - 15.0).abs() < 1e-9);
        assert!((offset(Club::Iron8) - 19.0).abs() < 1e-9);
        assert!((offset(Club::LobWedge) - 24.0).abs() < 1e-9);
        assert_eq!(c.dot_bottom_to_face_centre_mm(Club::Putter), None);
    }

    #[test]
    fn set_and_clear() {
        let mut c = ImpactCalibration::default();
        c.set_dot_bottom_to_face_centre_mm(Club::Iron7, Some(30.0));
        assert_eq!(c.dot_bottom_to_face_centre_mm(Club::Iron7), Some(30.0));
        c.set_dot_bottom_to_face_centre_mm(Club::Iron7, None);
        assert_eq!(c.dot_bottom_to_face_centre_mm(Club::Iron7), None);
        assert_eq!(
            ImpactCalibration::empty().dot_bottom_to_face_centre_mm(Club::Driver),
            None
        );
    }
}
