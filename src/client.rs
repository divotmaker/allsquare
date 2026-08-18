//! Poll-based client.
//!
//! [`Client`] owns the sequence counter, heartbeat timing, duplicate
//! suppression and shot correlation. It is synchronous and caller-driven — call
//! [`Client::poll`] in a loop; it never blocks longer than the transport does.

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use crate::club::{Club, Handed};
use crate::error::{Error, Result};
use crate::protocol::{
    self, BallMetrics, ChargingState, ClubMetrics, Command, DeviceState, DistanceUnit,
    HEARTBEAT_SECS, Notification, Sensor, SpeedUnit, SpinMode,
};

/// A byte transport to the device's CMD/EVT characteristics.
///
/// Implementations must not require pairing — the device never bonds.
pub trait Transport {
    /// Read one notification payload. Return [`io::ErrorKind::WouldBlock`] when
    /// nothing is available.
    ///
    /// # Errors
    /// Transport-specific I/O failures.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;

    /// Write one command to the CMD characteristic.
    ///
    /// The characteristic declares Write *with* response.
    ///
    /// # Errors
    /// Transport-specific I/O failures.
    fn write(&mut self, data: &[u8]) -> io::Result<()>;

    /// Read a characteristic by 128-bit UUID.
    ///
    /// # Errors
    /// Transport-specific I/O failures.
    fn read_characteristic(&mut self, uuid: u128) -> io::Result<Vec<u8>>;

    /// A model identifier the transport learned out-of-band, if any.
    ///
    /// Used as a fallback when the GAP name characteristic is unreadable.
    /// `BlueZ` handles the GAP service internally and does not expose `0x2a00`
    /// over D-Bus at all, so on Linux the advertised manufacturer payload is
    /// the only way to see the model. Returns `None` by default.
    fn model_hint(&self) -> Option<String> {
        None
    }
}

/// Firmware versions reported by the device.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Firmware {
    /// Launcher firmware version.
    pub launcher: String,
    /// MMI firmware version.
    pub mmi: String,
    /// Launch monitor firmware — the interesting one.
    pub lm: String,
}

impl Firmware {
    /// Parse the JSON the firmware characteristic returns, without pulling in a
    /// JSON dependency for three flat string fields.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        let field = |key: &str| -> String {
            let pat = format!("\"{key}\"");
            let Some(start) = s.find(&pat) else {
                return String::new();
            };
            let rest = &s[start + pat.len()..];
            let Some(colon) = rest.find(':') else {
                return String::new();
            };
            let rest = &rest[colon + 1..];
            let Some(open) = rest.find('"') else {
                return String::new();
            };
            let rest = &rest[open + 1..];
            rest.find('"')
                .map_or_else(String::new, |close| rest[..close].to_string())
        };
        Firmware {
            launcher: field("launcher"),
            mmi: field("mmi"),
            lm: field("lm"),
        }
    }
}

/// Events surfaced to the caller.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// Emitted once, after identity has been read.
    Connected {
        /// Firmware versions.
        firmware: Firmware,
        /// Hardware revision.
        hardware: String,
        /// Device serial.
        device_id: String,
        /// Model identifier for the device, e.g. `SGO300A` from the GAP name,
        /// or `0300A` from the advertised manufacturer data on `BlueZ` (which
        /// does not expose the GAP service). Empty if neither is available.
        model: String,
    },
    /// Device state machine changed.
    StateChanged(DeviceState),
    /// Ball detection state.
    Sensor(Sensor),
    /// A completed shot. `club` is present once the pull-based club metrics
    /// arrive; it is `None` if the device had nothing to report.
    Shot {
        /// Ball launch data.
        ball: BallMetrics,
        /// Club data, if the device tracked it.
        club: Option<ClubMetrics>,
    },
    /// Battery level, unsolicited.
    Battery {
        /// Charge level, 0-100.
        percent: u8,
        /// Charging status.
        state: ChargingState,
    },
    /// Device clock, unsolicited.
    Clock(u64),
    /// Alignment aim angle in degrees. Original Square only.
    Alignment(f64),
}

/// Connection phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Identity not yet read.
    Connecting,
    /// Running.
    Active,
}

/// Poll-based client over a [`Transport`].
pub struct Client<T: Transport> {
    transport: T,
    phase: Phase,
    seq: u8,
    last_rx: Vec<u8>,
    queue: VecDeque<Event>,
    last_heartbeat: Instant,
    heartbeat_interval: Duration,
    state: Option<DeviceState>,
    /// Ball metrics awaiting their club packet.
    pending_shot: Option<BallMetrics>,
    /// Set once we have requested club metrics for `pending_shot`. Guards
    /// against attributing a stale club packet to a shot we never saw.
    awaiting_club: bool,
    handed: Handed,
}

impl<T: Transport> Client<T> {
    /// Wrap a connected transport.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            phase: Phase::Connecting,
            seq: 0,
            last_rx: Vec::new(),
            queue: VecDeque::new(),
            last_heartbeat: Instant::now(),
            heartbeat_interval: Duration::from_secs(HEARTBEAT_SECS),
            state: None,
            pending_shot: None,
            awaiting_club: false,
            handed: Handed::Right,
        }
    }

    /// Set player handedness, used by [`Self::select_club`].
    pub fn set_handed(&mut self, handed: Handed) {
        self.handed = handed;
    }

    /// Borrow the transport.
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    fn send(&mut self, cmd: Command) -> Result<()> {
        self.seq = self.seq.wrapping_add(1);
        let bytes = cmd.encode(self.seq);
        self.transport.write(&bytes)?;
        Ok(())
    }

    /// Arm ball detection for `club`.
    ///
    /// Arming once is sufficient — the device stays armed across shots. The
    /// official app re-arms after each shot, but it is not required.
    ///
    /// # Errors
    /// If the transport fails.
    pub fn arm(&mut self, club: Club, spin: SpinMode) -> Result<()> {
        self.send(Command::SelectClub {
            club,
            handed: self.handed,
        })?;
        self.send(Command::DetectBall { on: true, spin })
    }

    /// Stop ball detection.
    ///
    /// # Errors
    /// If the transport fails.
    pub fn disarm(&mut self) -> Result<()> {
        self.send(Command::DetectBall {
            on: false,
            spin: SpinMode::Advanced,
        })
    }

    /// Change the active club without touching detection.
    ///
    /// # Errors
    /// If the transport fails.
    pub fn select_club(&mut self, club: Club) -> Result<()> {
        self.send(Command::SelectClub {
            club,
            handed: self.handed,
        })
    }

    /// Set the units shown on the device's own display.
    ///
    /// This has **no effect on the wire format** — values are always m/s and
    /// native units regardless. It only changes the device's screen.
    ///
    /// # Errors
    /// If the transport fails.
    pub fn set_units(&mut self, speed: SpeedUnit, distance: DistanceUnit) -> Result<()> {
        self.send(Command::SetUnits { speed, distance })
    }

    /// Set green speed, `0..=5` for stimp 8..13.
    ///
    /// # Errors
    /// If the transport fails.
    pub fn set_green_speed(&mut self, stimp_index: u8) -> Result<()> {
        self.send(Command::SetGreenSpeed(stimp_index.min(5)))
    }

    /// Set carry distance adjustment, as a percentage.
    ///
    /// # Errors
    /// If the transport fails.
    pub fn set_carry_adjustment(&mut self, percent: u8) -> Result<()> {
        self.send(Command::SetCarryAdjustment(percent))
    }

    /// Most recent device state, if a heartbeat ack has been seen.
    #[must_use]
    pub fn state(&self) -> Option<DeviceState> {
        self.state
    }

    /// Advance the client. Returns the next event, if any.
    ///
    /// Call this in a loop. It reads at most one notification per call and
    /// sends the heartbeat when due.
    ///
    /// # Errors
    /// [`Error::Disconnected`] if the device closed the link, or a transport
    /// error. Malformed frames are skipped rather than surfaced.
    pub fn poll(&mut self) -> Result<Option<Event>> {
        if let Some(ev) = self.queue.pop_front() {
            return Ok(Some(ev));
        }

        if self.phase == Phase::Connecting {
            let ev = self.read_identity()?;
            self.phase = Phase::Active;
            return Ok(Some(ev));
        }

        if self.last_heartbeat.elapsed() >= self.heartbeat_interval {
            self.send(Command::Heartbeat)?;
            self.last_heartbeat = Instant::now();
        }

        let mut buf = [0u8; 64];
        let n = match self.transport.read(&mut buf) {
            Ok(0) => return Err(Error::Disconnected),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let data = &buf[..n];

        // The device sends every notification twice, byte-identical. Without
        // this every shot would be reported twice.
        if data == self.last_rx.as_slice() {
            return Ok(None);
        }
        self.last_rx.clear();
        self.last_rx.extend_from_slice(data);

        match protocol::parse(data) {
            Ok(note) => self.handle(&note),
            Err(Error::Disconnected) => Err(Error::Disconnected),
            // Unknown or truncated frames are not fatal; the device emits
            // families we have not characterised.
            Err(_) => Ok(None),
        }
    }

    fn read_identity(&mut self) -> Result<Event> {
        let fw = self
            .transport
            .read_characteristic(protocol::uuid::FW_VERSION)?;
        let hw = self
            .transport
            .read_characteristic(protocol::uuid::HW_VERSION)?;
        let id = self
            .transport
            .read_characteristic(protocol::uuid::DEVICE_ID)?;
        // Report what the device calls itself rather than assuming a model.
        // BlueZ never exposes the GAP name, so fall back to the transport's
        // out-of-band hint (the advertised manufacturer payload).
        let model = self
            .transport
            .read_characteristic(protocol::uuid::GAP_NAME)
            .ok()
            .map(|v| String::from_utf8_lossy(&v).into_owned())
            .filter(|s| !s.is_empty())
            .or_else(|| self.transport.model_hint())
            .unwrap_or_default();
        // The app sends this first; the device answers with a 0x06.
        self.send(Command::Query)?;
        self.last_heartbeat = Instant::now();
        self.send(Command::Heartbeat)?;
        Ok(Event::Connected {
            firmware: Firmware::parse(&String::from_utf8_lossy(&fw)),
            hardware: String::from_utf8_lossy(&hw).into_owned(),
            device_id: String::from_utf8_lossy(&id).into_owned(),
            model,
        })
    }

    fn handle(&mut self, note: &Notification) -> Result<Option<Event>> {
        match *note {
            Notification::Ball(ball) => {
                // Club data is pull-based: ask for it now and hold the ball
                // metrics until it lands.
                self.pending_shot = Some(ball);
                self.awaiting_club = true;
                self.send(Command::RequestClubMetrics)?;
                Ok(None)
            }
            Notification::Club(club) => {
                // A club packet is only meaningful if we asked for it after a
                // ball packet. The device retains the last shot across
                // disconnects and will happily serve a stale one otherwise.
                if !self.awaiting_club {
                    return Ok(None);
                }
                self.awaiting_club = false;
                let Some(ball) = self.pending_shot.take() else {
                    return Ok(None);
                };
                Ok(Some(Event::Shot {
                    ball,
                    club: if club.is_empty() { None } else { Some(club) },
                }))
            }
            Notification::HeartbeatAck(state) => {
                if self.state == Some(state) {
                    return Ok(None);
                }
                self.state = Some(state);
                Ok(Some(Event::StateChanged(state)))
            }
            Notification::Sensor(s) => Ok(Some(Event::Sensor(s))),
            Notification::Battery { percent, state } => Ok(Some(Event::Battery { percent, state })),
            Notification::Clock(c) => Ok(Some(Event::Clock(c))),
            Notification::Alignment(a) => Ok(Some(Event::Alignment(a))),
            Notification::QueryResponse(_) => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Transport that replays a scripted set of notifications and records
    /// everything written.
    #[derive(Default)]
    struct Mock {
        incoming: VecDeque<Vec<u8>>,
        pub written: Vec<Vec<u8>>,
    }

    impl Mock {
        fn with(frames: &[&str]) -> Self {
            Mock {
                incoming: frames.iter().map(|f| hex(f)).collect(),
                written: Vec::new(),
            }
        }
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    impl Transport for Mock {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.incoming.pop_front() {
                Some(v) => {
                    buf[..v.len()].copy_from_slice(&v);
                    Ok(v.len())
                }
                None => Err(io::Error::new(io::ErrorKind::WouldBlock, "empty")),
            }
        }
        fn write(&mut self, data: &[u8]) -> io::Result<()> {
            self.written.push(data.to_vec());
            Ok(())
        }
        fn read_characteristic(&mut self, uuid: u128) -> io::Result<Vec<u8>> {
            let s = match uuid {
                protocol::uuid::FW_VERSION => {
                    r#"{"launcher": "1.0.1", "mmi": "1.4.0", "lm": "1.5.8"}"#
                }
                protocol::uuid::HW_VERSION => "3.1",
                protocol::uuid::GAP_NAME => "SGO300A",
                _ => "9c001051b20147b1950",
            };
            Ok(s.as_bytes().to_vec())
        }
    }

    fn drain<T: Transport>(c: &mut Client<T>) -> Vec<Event> {
        let mut out = Vec::new();
        for _ in 0..64 {
            match c.poll() {
                Ok(Some(e)) => out.push(e),
                Ok(None) => {}
                Err(_) => break,
            }
        }
        out
    }

    #[test]
    fn reads_identity_on_first_poll() {
        let mut c = Client::new(Mock::with(&[]));
        let Some(Event::Connected {
            firmware, hardware, ..
        }) = c.poll().expect("poll succeeds")
        else {
            panic!("expected Connected");
        };
        assert_eq!(firmware.lm, "1.5.8");
        assert_eq!(firmware.launcher, "1.0.1");
        assert_eq!(hardware, "3.1");
    }

    #[test]
    fn correlates_ball_then_club_into_one_shot() {
        let mut c = Client::new(Mock::with(&[
            "110237a50039002e000000000000000000",
            "1107ffbd012d03d600e90f6df331f946016500",
        ]));
        let events = drain(&mut c);
        let shot = events
            .iter()
            .find_map(|e| match e {
                Event::Shot { ball, club } => Some((ball, club)),
                _ => None,
            })
            .expect("one shot");
        assert!((shot.0.speed - 1.65).abs() < 1e-9);
        let club = shot.1.expect("club metrics present");
        assert_eq!(club.mask, 0xff);
        assert!(club.club_speed.is_some());
    }

    #[test]
    fn requests_club_metrics_after_a_ball_packet() {
        let mut c = Client::new(Mock::with(&["110237a50039002e000000000000000000"]));
        drain(&mut c);
        let sent = &c.transport_mut().written;
        assert!(
            sent.iter().any(|w| w[1] == 0x87),
            "expected a RequestClubMetrics, got {sent:02x?}"
        );
    }

    #[test]
    fn ignores_a_stale_club_packet_with_no_preceding_shot() {
        // The device retains the last shot and will serve it to a fresh client.
        // Reporting that as a new shot would be wrong.
        let mut c = Client::new(Mock::with(&["1107ffbd012d03d600e90f6df331f946016500"]));
        let events = drain(&mut c);
        assert!(!events.iter().any(|e| matches!(e, Event::Shot { .. })));
    }

    #[test]
    fn suppresses_duplicate_notifications() {
        // The device sends everything twice; a shot must be reported once.
        let mut c = Client::new(Mock::with(&[
            "110237a50039002e000000000000000000",
            "110237a50039002e000000000000000000",
            "110700ffffffffffffffffffffffffffffffff",
            "110700ffffffffffffffffffffffffffffffff",
        ]));
        let events = drain(&mut c);
        let shots = events
            .iter()
            .filter(|e| matches!(e, Event::Shot { .. }))
            .count();
        assert_eq!(shots, 1, "duplicate frames produced {shots} shots");
    }

    #[test]
    fn untracked_shot_reports_no_club_data() {
        let mut c = Client::new(Mock::with(&[
            "110237a50039002e000000000000000000",
            "110700ffffffffffffffffffffffffffffffff",
        ]));
        let events = drain(&mut c);
        let club = events
            .iter()
            .find_map(|e| match e {
                Event::Shot { club, .. } => Some(*club),
                _ => None,
            })
            .expect("a shot");
        assert!(club.is_none(), "all-sentinel club data should be None");
    }

    #[test]
    fn state_changes_are_emitted_once_per_transition() {
        let mut c = Client::new(Mock::with(&[
            "110316030300000000",
            "110318030300000000",
            "110385040403000000",
        ]));
        let events = drain(&mut c);
        let states: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::StateChanged(s) => Some(*s),
                _ => None,
            })
            .collect();
        assert_eq!(states, vec![DeviceState::Detect, DeviceState::Ready]);
    }

    #[test]
    fn arm_sends_club_then_detect() {
        let mut c = Client::new(Mock::with(&[]));
        let _ = c.poll();
        c.arm(Club::Iron7, SpinMode::Advanced).expect("arms");
        let sent: Vec<u8> = c.transport_mut().written.iter().map(|w| w[1]).collect();
        let club_at = sent.iter().position(|&t| t == 0x82).expect("club select");
        let detect_at = sent.iter().position(|&t| t == 0x81).expect("detect ball");
        assert!(club_at < detect_at, "club must be selected before arming");
    }

    #[test]
    fn firmware_parses_the_device_json() {
        let f = Firmware::parse(r#"{"launcher": "1.0.1", "mmi": "1.4.0", "lm": "1.5.8"}"#);
        assert_eq!(f.launcher, "1.0.1");
        assert_eq!(f.mmi, "1.4.0");
        assert_eq!(f.lm, "1.5.8");
        // Compact form, as the original Square reportedly emits.
        let f = Firmware::parse(r#"{"launcher":"1.0.0","mmi":"1.2.0","lm":"1.9.27"}"#);
        assert_eq!(f.lm, "1.9.27");
        // Garbage must not panic.
        assert_eq!(Firmware::parse("not json").lm, "");
    }
}
