//! Driver sessions over a fake link, fed with `testing` notification
//! fixtures.

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::sync::{Arc, Mutex};

use flightrelay::driver::{ConnectHost, DeviceEvent, HostFlags, Stop};
use flightrelay::{ClubData, Distance, Handedness, Launch};

use super::*;
use crate::protocol::{ClubMetrics, uuid};
use crate::testing;

/// What the fake link has seen, shared with the test.
#[derive(Debug, Default)]
struct Wires {
    /// Notifications to deliver, with when each reached the stack.
    incoming: VecDeque<(Vec<u8>, Option<SystemTime>)>,
    /// When the notification read last arrived.
    received: Option<SystemTime>,
    written: Vec<Vec<u8>>,
    released: u32,
    /// The next read fails as a lost link.
    lost: bool,
}

#[derive(Debug, Clone, Default)]
struct FakeLink(Arc<Mutex<Wires>>);

impl FakeLink {
    fn wires(&self) -> std::sync::MutexGuard<'_, Wires> {
        self.0.lock().expect("lock")
    }

    fn feed(&self, frame: &[u8]) {
        self.feed_at(frame, None);
    }

    /// Feed a notification that reached the stack at `at`.
    fn feed_at(&self, frame: &[u8], at: Option<SystemTime>) {
        // The device sends every notification twice.
        let mut w = self.wires();
        w.incoming.push_back((frame.to_vec(), at));
        w.incoming.push_back((frame.to_vec(), at));
    }

    /// The type bytes of the commands written so far.
    fn commands(&self) -> Vec<u8> {
        self.wires().written.iter().map(|w| w[1]).collect()
    }

    /// The `SelectClub` frames written so far.
    fn selects(&self) -> Vec<Vec<u8>> {
        self.wires()
            .written
            .iter()
            .filter(|w| {
                w[1] == Command::SelectClub {
                    club: Club::Iron7,
                    handed: Handed::Right,
                }
                .type_byte()
            })
            .cloned()
            .collect()
    }
}

impl Transport for FakeLink {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut w = self.wires();
        if w.lost {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "link lost"));
        }
        match w.incoming.pop_front() {
            Some((frame, at)) => {
                w.received = at;
                buf[..frame.len()].copy_from_slice(&frame);
                Ok(frame.len())
            }
            None => Err(io::Error::new(io::ErrorKind::WouldBlock, "empty")),
        }
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.wires().written.push(data.to_vec());
        Ok(())
    }

    fn read_characteristic(&mut self, id: u128) -> io::Result<Vec<u8>> {
        let s = match id {
            uuid::FW_VERSION => r#"{"launcher": "1.0.1", "mmi": "1.4.0", "lm": "1.5.8"}"#,
            uuid::HW_VERSION => "3.1",
            uuid::GAP_NAME => "SGO300A",
            _ => "SERIAL-0001",
        };
        Ok(s.as_bytes().to_vec())
    }
}

impl BleLink for FakeLink {
    fn name(&self) -> &'static str {
        "SquareGolf(CD01)"
    }
    fn address(&self) -> &'static str {
        "02:00:00:AB:CD:01"
    }
    fn release(&mut self) -> crate::Result<()> {
        self.wires().released += 1;
        Ok(())
    }
    fn received_at(&self) -> Option<SystemTime> {
        self.wires().received
    }
}

struct Host(DeviceCommand);

impl ConnectHost for Host {
    fn tick(&mut self) {}
    fn latched(&self) -> &DeviceCommand {
        &self.0
    }
}

/// A driver and session over a fresh fake link, plus what connecting emitted.
struct Rig {
    flags: HostFlags,
    driver: Omni,
    session: Option<OmniSession>,
    link: FakeLink,
    connect_events: Vec<DeviceEvent>,
}

impl Rig {
    fn new(config: Config, latched: DeviceCommand) -> Self {
        let flags = HostFlags::new();
        let mut driver = Omni::new(config).expect("valid config");
        let link = FakeLink::default();
        let stop = Stop::new();
        let mut host = Host(latched);
        let mut seen = Vec::new();
        let session = {
            let mut sink = |e| seen.push(e);
            let mut ctx = ConnectCtx::new(&stop, Events::new(&mut sink, &flags), &mut host);
            driver
                .connect_with(&mut ctx, link.clone())
                .expect("connects")
        };
        Self {
            flags,
            driver,
            session: Some(session),
            link,
            connect_events: seen,
        }
    }

    fn session(&mut self) -> &mut OmniSession {
        self.session.as_mut().expect("open")
    }

    fn poll(&mut self) -> Vec<DeviceEvent> {
        let mut seen = Vec::new();
        let mut sink = |e| seen.push(e);
        let mut out = Events::new(&mut sink, &self.flags);
        self.session
            .as_mut()
            .expect("open")
            .poll(Duration::from_millis(20), &mut out)
            .expect("poll");
        seen
    }

    fn close(&mut self) -> Vec<DeviceEvent> {
        let mut seen = Vec::new();
        let mut sink = |e| seen.push(e);
        let mut out = Events::new(&mut sink, &self.flags);
        self.session
            .take()
            .expect("open")
            .close(&mut out)
            .expect("close");
        seen
    }

    /// Feed a shot (ball, then the club metrics the client asks for) and poll
    /// it through.
    fn shot(&mut self, ball: &BallMetrics, club: &ClubMetrics) -> Vec<DeviceEvent> {
        self.link.feed(&testing::ball_notification(ball));
        let mut seen = self.poll();
        self.link.feed(&testing::club_notification(club));
        seen.extend(self.poll());
        seen
    }
}

fn frp(events: &[DeviceEvent]) -> Vec<&FrpEvent> {
    events
        .iter()
        .filter_map(|e| match e {
            DeviceEvent::Frp(f) => Some(f),
            _ => None,
        })
        .collect()
}

fn kinds(events: &[DeviceEvent]) -> Vec<&'static str> {
    frp(events)
        .into_iter()
        .filter_map(|e| match e {
            FrpEvent::ShotTrigger { .. } => Some("trigger"),
            FrpEvent::Launch { .. } => Some("launch"),
            FrpEvent::FlightResult { .. } => Some("flight"),
            FrpEvent::ClubPath { .. } => Some("club"),
            FrpEvent::FaceImpact { .. } => Some("impact"),
            FrpEvent::ShotFinished { .. } => Some("finished"),
            _ => None,
        })
        .collect()
}

fn launch_of(events: &[DeviceEvent]) -> Launch {
    frp(events)
        .into_iter()
        .find_map(|e| match e {
            FrpEvent::Launch { launch, .. } => Some(launch.clone()),
            _ => None,
        })
        .expect("a launch")
}

fn telemetry(events: &[DeviceEvent], key: &str) -> Vec<String> {
    frp(events)
        .into_iter()
        .filter_map(|e| match e {
            FrpEvent::DeviceTelemetry {
                telemetry: Some(t), ..
            } => t.get(key).cloned(),
            _ => None,
        })
        .collect()
}

fn logs(events: &[DeviceEvent]) -> Vec<(Level, &str)> {
    events
        .iter()
        .filter_map(|e| match e {
            DeviceEvent::Log(level, line) => Some((*level, line.as_str())),
            _ => None,
        })
        .collect()
}

/// A tracked 7-iron: real spin, every club field.
fn iron() -> (BallMetrics, ClubMetrics) {
    let ball = BallMetrics {
        speed: 50.0,
        launch_angle: 17.0,
        direction: 1.5,
        total_spin: 6000,
        spin_axis: -3.0,
        back_spin: 5990,
        side_spin: -310,
        ..testing::ball_metrics()
    };
    let club = ClubMetrics {
        mask: 0xff,
        path: Some(1.0),
        face_angle: Some(0.5),
        attack_angle: Some(-4.0),
        dynamic_loft: Some(22.0),
        impact_horizontal: Some(-5.0),
        impact_vertical: Some(-10.0),
        club_speed: Some(37.0),
        smash_factor: Some(1.35),
    };
    (ball, club)
}

/// A putt with no sticker: every spin value zero, no club data.
fn putt() -> (BallMetrics, ClubMetrics) {
    let ball = BallMetrics {
        speed: 1.6,
        launch_angle: 0.7,
        direction: -0.4,
        ..testing::ball_metrics()
    };
    (ball, ClubMetrics::default())
}

#[test]
fn connect_reports_identity_without_publishing_the_address() {
    let rig = Rig::new(Config::default(), DeviceCommand::default());
    let id = rig.session.as_ref().expect("open").identity();
    assert_eq!(id.device, "SquareGolf(CD01)");
    assert_eq!(id.manufacturer.as_deref(), Some("Invant"));
    assert_eq!(id.model.as_deref(), Some("SGO300A"));
    assert_eq!(id.firmware.as_deref(), Some("1.5.8"));
    assert_eq!(
        id.label(),
        "Square Golf SGO300A SquareGolf(CD01) (02:00:00:AB:CD:01)"
    );
    // The identity exchange (query, heartbeat) reaches only the trace log.
    assert!(frp(&rig.connect_events).is_empty());
    assert!(
        logs(&rig.connect_events)
            .iter()
            .any(|(l, line)| *l == Level::Trace && line.starts_with("tx cmd 86"))
    );
    let info = Omni::info(rig.driver.config());
    assert_eq!(info.source, Source::Measured);
    assert!(info.exclusive);
    assert_eq!(Omni::STOP_WAIT, Duration::from_secs(6));
}

#[test]
fn the_first_poll_arms_with_the_config_club() {
    let mut rig = Rig::new(Config::default(), DeviceCommand::default());
    let events = rig.poll();
    assert_eq!(telemetry(&events, "ready"), ["false"]);
    let selects = rig.link.selects();
    assert_eq!(selects.len(), 1);
    // 7-iron (07, irons 02), right-handed (00).
    assert_eq!(&selects[0][3..6], &[0x07, 0x02, 0x00]);
    let detect = rig.link.wires().written.last().cloned().expect("written");
    assert_eq!(&detect[1..2], &[0x81]);
    assert_eq!(&detect[3..5], &[0x01, 0x11], "on, advanced spin");
}

#[test]
fn the_latched_club_and_hand_win_over_the_config() {
    let latched = DeviceCommand::default()
        .with_club(flightrelay::Club::Putter)
        .with_handed(Handedness::Left);
    let config = Config {
        club: "DR".into(),
        advanced_spin: false,
        ..Config::default()
    };
    let mut rig = Rig::new(config, latched);
    rig.poll();
    let selects = rig.link.selects();
    assert_eq!(
        &selects[0][3..6],
        &[0x01, 0x03, 0x01],
        "putter, left-handed"
    );
    let detect = rig.link.wires().written.last().cloned().expect("written");
    assert_eq!(detect[4], 0x10, "standard spin");
}

#[test]
fn a_shot_is_sent_once_complete_with_a_timestamp() {
    let mut rig = Rig::new(Config::default(), DeviceCommand::default());
    rig.poll();
    let (ball, club) = iron();
    let events = rig.shot(&ball, &club);
    assert_eq!(
        kinds(&events),
        ["trigger", "launch", "club", "impact", "finished"]
    );
    let FrpEvent::ShotTrigger { key, timestamp } = frp(&events)[0] else {
        panic!("{events:?}");
    };
    assert_eq!(key.shot_number, 1);
    assert!(timestamp.is_some_and(|t| t > 1_600_000_000_000));
    let l = launch_of(&events);
    assert_eq!(l.launch_speed, Some(Velocity::MetersPerSecond(50.0)));
    assert_eq!(l.total_spin_rpm, Some(6000));
    assert_eq!(l.sidespin_rpm, Some(310));
    assert_eq!(l.spin_axis, Some(3.0));
    let club_data = frp(&events)
        .into_iter()
        .find_map(|e| match e {
            FrpEvent::ClubPath { club, .. } => Some(club.clone()),
            _ => None,
        })
        .expect("club");
    assert_eq!(
        club_data,
        ClubData {
            club_speed: Some(Velocity::MetersPerSecond(37.0)),
            path: Some(1.0),
            attack_angle: Some(-4.0),
            face_angle: Some(0.5),
            dynamic_loft: Some(22.0),
            smash_factor: Some(1.35),
            ..ClubData::default()
        }
    );
    let impact = frp(&events)
        .into_iter()
        .find_map(|e| match e {
            FrpEvent::FaceImpact { impact, .. } => Some(impact.clone()),
            _ => None,
        })
        .expect("impact");
    if cfg!(not(feature = "raw-face-impact")) {
        // Toe-positive lateral; vertical from the dot plus the 7-iron's offset.
        assert_eq!(impact.lateral, Some(Distance::Millimeters(5.0)));
        assert_eq!(impact.vertical, Some(Distance::Millimeters(9.0)));
    }

    // The next shot takes the next number.
    let events = rig.shot(&ball, &club);
    let FrpEvent::ShotTrigger { key, .. } = frp(&events)[0] else {
        panic!("{events:?}");
    };
    assert_eq!(key.shot_number, 2);
}

#[test]
fn the_trigger_and_frames_carry_the_arrival_time() {
    let mut rig = Rig::new(Config::default(), DeviceCommand::default());
    rig.flags.set_wants_raw(true);
    rig.poll();
    let (ball, club) = iron();
    let first = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
    let second = first + Duration::from_secs(10);
    // A ball notification whose club notification never comes, then a heartbeat
    // ack, then the same ball bytes for the next strike, which completes.
    rig.link
        .feed_at(&testing::ball_notification(&ball), Some(first));
    rig.poll();
    rig.link.feed(&testing::heartbeat_ack(DeviceState::Ready));
    rig.poll();
    rig.link
        .feed_at(&testing::ball_notification(&ball), Some(second));
    let mut events = rig.poll();
    rig.link.feed(&testing::club_notification(&club));
    events.extend(rig.poll());
    let FrpEvent::ShotTrigger { key, timestamp } = frp(&events)
        .into_iter()
        .find(|e| matches!(e, FrpEvent::ShotTrigger { .. }))
        .expect("trigger")
    else {
        unreachable!()
    };
    assert_eq!(key.shot_number, 1);
    assert_eq!(*timestamp, Some(1_700_000_010_000));
    let ball_frames: Vec<Option<u64>> = frp(&events)
        .into_iter()
        .filter_map(|e| match e {
            FrpEvent::RawFrame(f) if f.label.as_deref() == Some("notify 11 02") => {
                Some(f.timestamp)
            }
            _ => None,
        })
        .collect();
    assert_eq!(ball_frames, [Some(1_700_000_010_000); 2]);
}

#[test]
fn a_poll_returns_near_its_timeout() {
    let mut rig = Rig::new(Config::default(), DeviceCommand::default());
    rig.poll();
    for timeout in [Duration::ZERO, Duration::from_millis(20)] {
        let started = Instant::now();
        let mut sink = |_| {};
        let mut out = Events::new(&mut sink, &rig.flags);
        rig.session
            .as_mut()
            .expect("open")
            .poll(timeout, &mut out)
            .expect("poll");
        let took = started.elapsed();
        assert!(took >= timeout, "{timeout:?}: returned after {took:?}");
        assert!(
            took < timeout + Duration::from_millis(40),
            "{timeout:?}: returned after {took:?}"
        );
    }
}

#[test]
fn a_zero_spin_putt_sends_total_zero_and_no_axis() {
    let latched = DeviceCommand::default().with_club(flightrelay::Club::Putter);
    let mut rig = Rig::new(Config::default(), latched);
    rig.poll();
    let (ball, club) = putt();
    let events = rig.shot(&ball, &club);
    assert_eq!(kinds(&events), ["trigger", "launch", "finished"]);
    let l = launch_of(&events);
    assert_eq!(l.total_spin_rpm, Some(0));
    assert_eq!(l.spin_axis, None);
    assert_eq!(l.backspin_rpm, Some(0));
    assert_eq!(l.sidespin_rpm, Some(0));
}

#[test]
fn a_zero_spin_misread_is_discarded() {
    let mut rig = Rig::new(Config::default(), DeviceCommand::default());
    rig.poll();
    let (ball, _) = putt();
    let (_, club) = iron();
    let events = rig.shot(&ball, &club);
    assert!(kinds(&events).is_empty(), "{events:?}");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DeviceEvent::ShotDiscarded))
    );
    assert_eq!(telemetry(&events, "zero_spin_discards"), ["1"]);
    let alert = frp(&events)
        .into_iter()
        .find_map(|e| match e {
            FrpEvent::Alert { severity, message } => Some((*severity, message.clone())),
            _ => None,
        })
        .expect("alert");
    assert_eq!(alert.0, Severity::Warn);
    assert!(
        alert.1.starts_with("Square Golf: discarded a 4 mph shot"),
        "{}",
        alert.1
    );

    // The discarded shot took no number; the count keeps going.
    let events = rig.shot(&ball, &club);
    assert_eq!(telemetry(&events, "zero_spin_discards"), ["2"]);
    let (ball, club) = iron();
    let events = rig.shot(&ball, &club);
    let FrpEvent::ShotTrigger { key, .. } = frp(&events)[0] else {
        panic!("{events:?}");
    };
    assert_eq!(key.shot_number, 1);
}

#[test]
fn the_discard_can_be_turned_off() {
    let config = Config {
        discard_non_putting_zero_spin: false,
        ..Config::default()
    };
    let mut rig = Rig::new(config, DeviceCommand::default());
    rig.poll();
    let (ball, club) = putt();
    let events = rig.shot(&ball, &club);
    assert_eq!(kinds(&events), ["trigger", "launch", "finished"]);
}

#[test]
fn one_club_change_after_the_arm_is_one_select() {
    let mut rig = Rig::new(Config::default(), DeviceCommand::default());
    rig.poll();
    assert_eq!(rig.link.selects().len(), 1);
    // A host sends the club twice for one change (its club and its
    // detection mode messages), and the hand once more.
    let pw = DeviceCommand::default().with_club(flightrelay::Club::PitchingWedge);
    rig.session().command(&pw).expect("command");
    rig.session()
        .command(&pw.with_mode(flightrelay::DetectionMode::Full))
        .expect("command");
    rig.session()
        .command(&DeviceCommand::default().with_handed(Handedness::Right))
        .expect("command");
    let selects = rig.link.selects();
    assert_eq!(selects.len(), 2, "{selects:02x?}");
    assert_eq!(&selects[1][3..6], &[0x0a, 0x02, 0x00]);

    // A hand change reselects the same club with the new hand.
    rig.session()
        .command(&DeviceCommand::default().with_handed(Handedness::Left))
        .expect("command");
    let selects = rig.link.selects();
    assert_eq!(selects.len(), 3);
    assert_eq!(&selects[2][3..6], &[0x0a, 0x02, 0x01]);
    let events = rig.poll();
    assert!(
        logs(&events)
            .iter()
            .any(|(l, line)| *l == Level::Info && line.contains("club -> PW"))
    );
}

#[test]
fn a_club_without_a_code_selects_the_nearest() {
    let mut rig = Rig::new(Config::default(), DeviceCommand::default());
    rig.poll();
    rig.session()
        .command(&DeviceCommand::default().with_club(flightrelay::Club::MiniDriver))
        .expect("command");
    rig.session()
        .command(&DeviceCommand::default().with_club(flightrelay::Club::Iron2))
        .expect("command");
    let selects = rig.link.selects();
    assert_eq!(&selects[1][3..5], &[0x01, 0x00], "MD -> driver");
    assert_eq!(&selects[2][3..5], &[0x03, 0x02], "2I -> 3I");
}

#[test]
fn a_command_before_the_arm_is_only_recorded() {
    let mut rig = Rig::new(Config::default(), DeviceCommand::default());
    rig.session()
        .command(&DeviceCommand::default().with_club(flightrelay::Club::SandWedge))
        .expect("command");
    assert!(rig.link.selects().is_empty());
    rig.poll();
    let selects = rig.link.selects();
    assert_eq!(selects.len(), 1);
    assert_eq!(&selects[0][3..5], &[0x0c, 0x02], "armed with the SW");
}

#[test]
fn the_putter_exemption_follows_the_club_in_play() {
    let mut rig = Rig::new(Config::default(), DeviceCommand::default());
    rig.poll();
    rig.session()
        .command(&DeviceCommand::default().with_club(flightrelay::Club::Putter))
        .expect("command");
    let (ball, club) = putt();
    assert_eq!(
        kinds(&rig.shot(&ball, &club)),
        ["trigger", "launch", "finished"]
    );
}

#[test]
fn raw_frames_only_when_wanted_and_never_private_ones() {
    let mut rig = Rig::new(Config::default(), DeviceCommand::default());
    rig.poll();
    let (ball, club) = iron();
    let quiet = rig.shot(&ball, &club);
    assert!(
        !frp(&quiet)
            .iter()
            .any(|e| matches!(e, FrpEvent::RawFrame(_)))
    );

    rig.flags.set_wants_raw(true);
    rig.link.feed(&testing::heartbeat_ack(DeviceState::Ready));
    rig.link.feed(&testing::QUERY_RESPONSE);
    let mut events = rig.poll();
    events.extend(rig.shot(&ball, &club));
    let frames: Vec<&RawFrame> = frp(&events)
        .into_iter()
        .filter_map(|e| match e {
            FrpEvent::RawFrame(f) => Some(f),
            _ => None,
        })
        .collect();
    let labels: Vec<&str> = frames
        .iter()
        .map(|f| f.label.as_deref().unwrap_or(""))
        .collect();
    // The ball notification, the club metrics request the client sends at
    // once, the device's repeat of the ball notification, then the club
    // notification and its repeat.
    assert_eq!(
        labels,
        [
            "notify 11 02",
            "cmd 87",
            "notify 11 02",
            "notify 11 07",
            "notify 11 07"
        ]
    );
    assert!(frames.iter().all(|f| f.seq == 0 && f.timestamp.is_some()));
    assert_eq!(frames[0].data, testing::ball_notification(&ball));
    assert_eq!(frames[1].dir, RawDir::Tx);
    // The shot's frames precede its trigger; only the device's repeat of
    // the club notification follows it.
    let trigger = events
        .iter()
        .position(|e| matches!(e, DeviceEvent::Frp(FrpEvent::ShotTrigger { .. })))
        .expect("trigger");
    let before = events[..trigger]
        .iter()
        .filter(|e| matches!(e, DeviceEvent::Frp(FrpEvent::RawFrame(_))))
        .count();
    assert_eq!(before, 4);
    // The heartbeat ack and the query reply went to the trace log.
    let traces: Vec<&str> = logs(&events)
        .into_iter()
        .filter(|(l, _)| *l == Level::Trace)
        .map(|(_, line)| line)
        .collect();
    assert_eq!(traces.len(), 4, "{traces:?}");
}

#[test]
fn state_and_battery_become_telemetry() {
    let mut rig = Rig::new(Config::default(), DeviceCommand::default());
    rig.poll();
    rig.link.feed(&testing::heartbeat_ack(DeviceState::Ready));
    rig.link.feed(&[0x91, 87, 0x00]);
    let events = rig.poll();
    assert_eq!(telemetry(&events, "ready"), ["true"]);
    assert_eq!(telemetry(&events, "device_state"), ["ready"]);
    assert_eq!(telemetry(&events, "battery_pct"), ["87"]);
}

#[test]
fn close_disarms_then_releases() {
    let mut rig = Rig::new(Config::default(), DeviceCommand::default());
    rig.poll();
    let events = rig.close();
    let commands = rig.link.commands();
    let detect = rig.link.wires().written.last().cloned().expect("written");
    assert_eq!(commands.last(), Some(&0x81));
    assert_eq!(detect[3], 0x00, "detection off");
    assert_eq!(rig.link.wires().released, 1);
    assert_eq!(telemetry(&events, "ready"), ["false"]);
}

#[test]
fn a_dropped_session_still_releases_the_link() {
    let mut rig = Rig::new(Config::default(), DeviceCommand::default());
    rig.poll();
    drop(rig.session.take());
    assert_eq!(rig.link.wires().released, 1);
}

#[test]
fn a_lost_link_ends_the_session() {
    let mut rig = Rig::new(Config::default(), DeviceCommand::default());
    rig.poll();
    rig.link.wires().lost = true;
    let mut seen = Vec::new();
    let mut sink = |e| seen.push(e);
    let mut out = Events::new(&mut sink, &rig.flags);
    let result = rig
        .session
        .as_mut()
        .expect("open")
        .poll(Duration::from_millis(20), &mut out);
    assert!(
        matches!(result, Err(Error::Device(crate::Error::Transport(_)))),
        "{result:?}"
    );
}

#[test]
fn close_after_a_lost_link_writes_nothing_and_succeeds() {
    let mut rig = Rig::new(Config::default(), DeviceCommand::default());
    rig.poll();
    let written = rig.link.wires().written.len();
    rig.link.wires().lost = true;
    {
        let mut sink = |_| {};
        let mut out = Events::new(&mut sink, &rig.flags);
        assert!(
            rig.session
                .as_mut()
                .expect("open")
                .poll(Duration::from_millis(20), &mut out)
                .is_err()
        );
    }
    let events = rig.close();
    assert_eq!(rig.link.wires().written.len(), written, "no disarm");
    assert_eq!(rig.link.wires().released, 1);
    assert_eq!(telemetry(&events, "ready"), ["false"]);
}

#[test]
fn config_warnings_are_logged_on_the_first_connect_only() {
    let config = Config {
        club: "ZZ".into(),
        dot_bottom_to_face_center_mm: BTreeMap::from([("QQ".to_owned(), 1.0)]),
        ..Config::default()
    };
    let mut rig = Rig::new(config, DeviceCommand::default());
    let warnings: Vec<_> = logs(&rig.connect_events)
        .into_iter()
        .filter(|(l, _)| *l == Level::Warn)
        .collect();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    // The unknown club arms the 7-iron.
    rig.poll();
    assert_eq!(&rig.link.selects()[0][3..5], &[0x07, 0x02]);

    // Reconnecting does not repeat them.
    let stop = Stop::new();
    let mut host = Host(DeviceCommand::default());
    let mut seen = Vec::new();
    {
        let mut sink = |e| seen.push(e);
        let mut ctx = ConnectCtx::new(&stop, Events::new(&mut sink, &rig.flags), &mut host);
        let _session = rig
            .driver
            .connect_with(&mut ctx, FakeLink::default())
            .expect("connects");
    }
    assert!(!logs(&seen).iter().any(|(l, _)| *l == Level::Warn));
}

#[test]
fn shot_numbers_keep_counting_across_sessions() {
    let mut rig = Rig::new(Config::default(), DeviceCommand::default());
    rig.poll();
    let (ball, club) = iron();
    rig.shot(&ball, &club);
    rig.close();

    let stop = Stop::new();
    let mut host = Host(DeviceCommand::default());
    let link = FakeLink::default();
    let mut session = {
        let mut sink = |_| {};
        let mut ctx = ConnectCtx::new(&stop, Events::new(&mut sink, &rig.flags), &mut host);
        rig.driver
            .connect_with(&mut ctx, link.clone())
            .expect("connects")
    };
    let mut seen = Vec::new();
    {
        let mut sink = |e| seen.push(e);
        let mut out = Events::new(&mut sink, &rig.flags);
        session
            .poll(Duration::from_millis(5), &mut out)
            .expect("poll");
        link.feed(&testing::ball_notification(&ball));
        session
            .poll(Duration::from_millis(5), &mut out)
            .expect("poll");
        link.feed(&testing::club_notification(&club));
        session
            .poll(Duration::from_millis(5), &mut out)
            .expect("poll");
    }
    let FrpEvent::ShotTrigger { key, .. } = frp(&seen)
        .into_iter()
        .find(|e| matches!(e, FrpEvent::ShotTrigger { .. }))
        .expect("trigger")
    else {
        unreachable!()
    };
    assert_eq!(key.shot_number, 2);
}

#[test]
fn a_stopped_connect_returns_stopped() {
    let flags = HostFlags::new();
    let mut driver = Omni::new(Config::default()).expect("valid");
    let stop = Stop::new();
    stop.stop();
    let mut host = Host(DeviceCommand::default());
    let mut sink = |_| {};
    let mut ctx = ConnectCtx::new(&stop, Events::new(&mut sink, &flags), &mut host);
    let link = FakeLink::default();
    let result = driver.connect_with(&mut ctx, link.clone());
    assert!(matches!(result, Err(Error::Stopped)), "{result:?}");
    assert!(link.wires().written.is_empty());
}

#[test]
fn a_failed_attempt_alerts_and_backs_off() {
    let flags = HostFlags::new();
    let mut driver = Omni::new(Config::default()).expect("valid");
    let mut backoff = Backoff::default();
    let mut alerts = Vec::new();
    let mut sink = |e| {
        if let DeviceEvent::Frp(FrpEvent::Alert { message, .. }) = e {
            alerts.push(message);
        }
    };
    let error = Error::Device(crate::Error::NotFound);
    let ended = SessionEnd {
        error: Some(&error),
        connected_for: None,
        attempts: 1,
    };
    {
        let mut out = Events::new(&mut sink, &flags);
        assert_eq!(
            driver.retry_delay(&ended, &mut backoff, &mut out),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            driver.retry_delay(&ended, &mut backoff, &mut out),
            Some(Duration::from_secs(2))
        );
    }
    assert_eq!(alerts[0], "Square Golf error: no Square Golf device found");
}

#[test]
fn a_config_change_restarts_and_is_kept() {
    let mut driver = Omni::new(Config::default()).expect("valid");
    let new = Config {
        club: "PW".into(),
        ..Config::default()
    };
    assert!(Config::restart_needed(driver.config(), &new));
    for field in [
        Config {
            advanced_spin: false,
            ..Config::default()
        },
        Config {
            discard_non_putting_zero_spin: false,
            ..Config::default()
        },
    ] {
        assert!(Config::restart_needed(&Config::default(), &field));
    }
    driver.apply_config(&new);
    assert_eq!(driver.config(), &new);
    assert_eq!(driver.settings.club, flightrelay::Club::PitchingWedge);
}

#[test]
fn a_config_change_keeps_the_shot_numbers() {
    let mut rig = Rig::new(Config::default(), DeviceCommand::default());
    rig.poll();
    let (ball, club) = iron();
    rig.shot(&ball, &club);
    rig.close();
    rig.driver.apply_config(&Config {
        club: "PW".into(),
        ..Config::default()
    });

    let stop = Stop::new();
    let mut host = Host(DeviceCommand::default());
    let link = FakeLink::default();
    let mut session = {
        let mut sink = |_| {};
        let mut ctx = ConnectCtx::new(&stop, Events::new(&mut sink, &rig.flags), &mut host);
        rig.driver
            .connect_with(&mut ctx, link.clone())
            .expect("connects")
    };
    let mut seen = Vec::new();
    {
        let mut sink = |e| seen.push(e);
        let mut out = Events::new(&mut sink, &rig.flags);
        session
            .poll(Duration::from_millis(5), &mut out)
            .expect("poll");
        link.feed(&testing::ball_notification(&ball));
        session
            .poll(Duration::from_millis(5), &mut out)
            .expect("poll");
        link.feed(&testing::club_notification(&club));
        session
            .poll(Duration::from_millis(5), &mut out)
            .expect("poll");
    }
    // The new config is in play: the session armed the pitching wedge.
    assert_eq!(&link.selects()[0][3..5], &[0x0a, 0x02]);
    let FrpEvent::ShotTrigger { key, .. } = frp(&seen)
        .into_iter()
        .find(|e| matches!(e, FrpEvent::ShotTrigger { .. }))
        .expect("trigger")
    else {
        unreachable!()
    };
    assert_eq!(key.shot_number, 2);
}
