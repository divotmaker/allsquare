//! FRP device adapter — bridges Square Golf to the
//! [Flight Relay Protocol](https://github.com/flightrelay/spec).
//!
//! Maps [`Event`]s from a connected device to FRP envelopes and streams them to
//! an FRP controller. The adapter always plays the FRP [`Role::Device`]; the
//! transport direction is the caller's choice:
//!
//! - [`FrpDevice::serve`] accepts controllers on a local port (default 5880)
//! - [`FrpDevice::bridge`] dials a central controller such as flighthook
//!
//! Connections are established on a background thread so the caller's poll loop
//! never blocks, and a dropped connection is re-established automatically.
//!
//! Requires the `frp` feature.

mod convert;

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use flightrelay::{
    EndpointConfig, FrpConnection, FrpEndpoint, FrpEnvelope, FrpEvent, FrpMessage,
    FrpProtocolMessage, Role, SPEC_VERSION, ShotKey, Transport,
};

use crate::client::Event;
use crate::protocol::DeviceState;

pub use convert::{ball_flight, club_data, face_impact};

const MANUFACTURER: &str = "Invant";

/// Backoff between failed connection attempts.
const RETRY_DELAY: Duration = Duration::from_secs(2);

/// An FRP device backed by a Square Golf connection.
///
/// The caller drives both the [`Client`](crate::Client) poll loop and this
/// adapter from the same thread.
pub struct FrpDevice {
    conn: Option<FrpConnection>,
    /// Signals the acceptor thread to establish a connection.
    request: Sender<()>,
    /// Receives established connections from the acceptor thread.
    incoming: Receiver<FrpConnection>,
    /// True while the acceptor thread is working on a connection.
    pending: bool,
    /// Last telemetry envelope, re-sent to each newly connected controller.
    telemetry: Option<FrpEnvelope>,
    device: String,
    firmware: Option<String>,
    model: Option<String>,
    shot_number: u32,
    /// Last `ready` state sent, so identical telemetry is not resent.
    last_ready: Option<bool>,
}

impl FrpDevice {
    /// Accept controllers on `addr`, e.g. `"0.0.0.0:5880"`.
    ///
    /// # Errors
    /// If the listener cannot bind.
    pub fn serve(addr: &str) -> Result<Self, flightrelay::FrpError> {
        Self::spawn(EndpointConfig::new(Role::Device, Transport::listen(addr)))
    }

    /// Dial a central controller at `url`, e.g. `"ws://flighthook:5880/frp"`,
    /// identifying as `name`.
    ///
    /// # Errors
    /// If the endpoint cannot be opened.
    pub fn bridge(url: &str, name: &str) -> Result<Self, flightrelay::FrpError> {
        Self::spawn(
            EndpointConfig::new(Role::Device, Transport::connect(url))
                .with_name(name)
                .with_versions(&[SPEC_VERSION]),
        )
    }

    fn spawn(config: EndpointConfig) -> Result<Self, flightrelay::FrpError> {
        let mut endpoint = FrpEndpoint::open(config)?;
        let (request, request_rx) = mpsc::channel::<()>();
        let (conn_tx, incoming) = mpsc::channel::<FrpConnection>();

        thread::spawn(move || {
            while request_rx.recv().is_ok() {
                // Retry until connected — one request yields one connection.
                loop {
                    match endpoint.establish() {
                        Ok(conn) if conn.set_nonblocking(true).is_ok() => {
                            if conn_tx.send(conn).is_err() {
                                return;
                            }
                            break;
                        }
                        // Back off so a refused dial or rejected handshake
                        // does not spin the thread.
                        _ => thread::sleep(RETRY_DELAY),
                    }
                }
            }
        });

        let mut device = Self {
            conn: None,
            request,
            incoming,
            pending: false,
            telemetry: None,
            device: String::new(),
            firmware: None,
            model: None,
            shot_number: 0,
            last_ready: None,
        };
        device.request_connection();
        Ok(device)
    }

    /// Ask the acceptor thread for a connection, unless one is already pending.
    fn request_connection(&mut self) {
        if !self.pending && self.request.send(()).is_ok() {
            self.pending = true;
        }
    }

    /// Adopt a newly established connection, if one is ready.
    ///
    /// Call once per poll-loop iteration. Re-sends the cached telemetry
    /// envelope to each newly connected controller, as the spec requires.
    ///
    /// Returns `true` when a connection was adopted.
    ///
    /// # Errors
    /// If the telemetry re-send fails.
    pub fn poll_connection(&mut self) -> Result<bool, flightrelay::FrpError> {
        if self.conn.is_some() {
            return Ok(false);
        }
        match self.incoming.try_recv() {
            Ok(conn) => {
                self.pending = false;
                self.conn = Some(conn);
                if let Some(env) = self.telemetry.clone() {
                    self.send_envelope(&env)?;
                }
                Ok(true)
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => Ok(false),
        }
    }

    /// Whether a controller is currently connected.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.conn.is_some()
    }

    /// Set the device name, e.g. `"SquareGolf(54E4)"`.
    pub fn set_device_name(&mut self, name: &str) {
        name.clone_into(&mut self.device);
    }

    /// Set the firmware string reported in telemetry.
    pub fn set_firmware(&mut self, fw: &str) {
        self.firmware = Some(fw.to_owned());
    }

    /// Set the model the device reports for itself, e.g. `SGO300A`.
    pub fn set_model(&mut self, model: &str) {
        if !model.is_empty() {
            self.model = Some(model.to_owned());
        }
    }

    /// Poll for controller commands (non-blocking).
    ///
    /// Returns a [`DetectionMode`](flightrelay::DetectionMode) if the controller
    /// sent `set_detection_mode`. Square Golf has no device-side shot mode —
    /// putting and chipping differ only by club selection — so a caller can map
    /// the mode onto a club or ignore it.
    pub fn check_controller(&mut self) -> Option<flightrelay::DetectionMode> {
        let conn = self.conn.as_mut()?;
        match conn.try_recv() {
            Ok(Some(FrpMessage::Protocol(FrpProtocolMessage::SetDetectionMode {
                mode, ..
            }))) => mode,
            Err(_) => {
                self.drop_connection();
                None
            }
            _ => None,
        }
    }

    /// Send the initial device-info envelope.
    ///
    /// # Errors
    /// If the send fails.
    pub fn send_device_info(&mut self) -> Result<(), flightrelay::FrpError> {
        self.send_telemetry(None, None)
    }

    /// Drop the current connection and ask for a replacement.
    fn drop_connection(&mut self) {
        self.conn = None;
        self.request_connection();
    }

    /// Send one envelope, dropping the connection if the peer has gone away.
    fn send_envelope(&mut self, env: &FrpEnvelope) -> Result<(), flightrelay::FrpError> {
        let Some(conn) = self.conn.as_mut() else {
            return Ok(());
        };
        match conn.send_envelope(env) {
            Ok(()) => Ok(()),
            Err(flightrelay::FrpError::Closed) => {
                self.drop_connection();
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Process a client [`Event`] and emit the resulting FRP envelopes.
    ///
    /// Shot data is delivered as one [`Event::Shot`] with ball metrics always
    /// present and club metrics optional, so this emits
    /// `ShotTrigger → BallFlight → [ClubPath] → [FaceImpact] → ShotFinished`.
    ///
    /// # Errors
    /// If a send fails for a reason other than the controller disconnecting.
    pub fn handle_event(&mut self, event: &Event) -> Result<(), flightrelay::FrpError> {
        match event {
            // The heartbeat ack carries the device state machine; Ready means a
            // ball is sitting there waiting to be hit.
            Event::StateChanged(state) => {
                return self.send_telemetry(Some(*state == DeviceState::Ready), None);
            }
            Event::Battery { percent, .. } => {
                return self.send_telemetry(None, Some(*percent));
            }
            Event::Shot { ball, club } => {
                self.send_telemetry(Some(false), None)?;

                self.shot_number += 1;
                let key = ShotKey {
                    shot_id: uuid_v4(),
                    shot_number: self.shot_number,
                };

                let mut events = vec![
                    FrpEvent::ShotTrigger { key: key.clone() },
                    FrpEvent::BallFlight {
                        key: key.clone(),
                        ball: convert::ball_flight(ball),
                    },
                ];

                if let Some(club) = club {
                    events.push(FrpEvent::ClubPath {
                        key: key.clone(),
                        club: convert::club_data(club),
                    });
                    if let Some(impact) = convert::face_impact(club) {
                        events.push(FrpEvent::FaceImpact {
                            key: key.clone(),
                            impact,
                        });
                    }
                }

                events.push(FrpEvent::ShotFinished { key });
                return self.send_events(&events);
            }
            _ => {}
        }
        Ok(())
    }

    fn send_telemetry(
        &mut self,
        ready: Option<bool>,
        battery: Option<u8>,
    ) -> Result<(), flightrelay::FrpError> {
        // Battery arrives unprompted every few seconds; suppress telemetry that
        // would say nothing new.
        if battery.is_none() && ready.is_some() && ready == self.last_ready {
            return Ok(());
        }
        if let Some(r) = ready {
            self.last_ready = Some(r);
        }

        let mut telemetry = std::collections::HashMap::new();
        telemetry.insert(
            flightrelay::types::telemetry::READY.to_owned(),
            self.last_ready.unwrap_or(false).to_string(),
        );
        if let Some(pct) = battery {
            telemetry.insert(
                flightrelay::types::telemetry::BATTERY_PCT.to_owned(),
                pct.to_string(),
            );
        }

        let env = FrpEnvelope {
            device: self.device.clone(),
            event: FrpEvent::DeviceTelemetry {
                manufacturer: Some(MANUFACTURER.to_owned()),
                model: self.model.clone(),
                firmware: self.firmware.clone(),
                telemetry: Some(telemetry),
            },
        };

        self.telemetry = Some(env.clone());
        self.send_envelope(&env)
    }

    fn send_events(&mut self, events: &[FrpEvent]) -> Result<(), flightrelay::FrpError> {
        for event in events {
            if self.conn.is_none() {
                return Ok(());
            }
            let env = FrpEnvelope {
                device: self.device.clone(),
                event: event.clone(),
            };
            self.send_envelope(&env)?;
        }
        Ok(())
    }
}

/// Generate a UUID v4 string without pulling in the `uuid` crate.
///
/// Same approach as 10over's FRP server: xorshift128+ seeded from the clock.
fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    #[allow(clippy::cast_possible_truncation)]
    let mut s0 = seed as u64;
    #[allow(clippy::cast_possible_truncation)]
    let mut s1 = seed.wrapping_mul(6_364_136_223_846_793_005) as u64;
    if s0 == 0 {
        s0 = 0x1234_5678_9abc_def0;
    }
    if s1 == 0 {
        s1 = 0xfedc_ba98_7654_3210;
    }

    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_exact_mut(8) {
        let mut x = s0;
        let y = s1;
        s0 = y;
        x ^= x << 23;
        x ^= x >> 17;
        x ^= y;
        x ^= y >> 26;
        s1 = x;
        chunk.copy_from_slice(&s0.wrapping_add(s1).to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_v4_is_well_formed_and_unique() {
        let a = uuid_v4();
        assert_eq!(a.len(), 36);
        assert_eq!(a.as_bytes()[14], b'4', "version nibble");
        assert!(matches!(a.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
        assert_ne!(a, uuid_v4());
    }
}
