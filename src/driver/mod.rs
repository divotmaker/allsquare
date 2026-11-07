//! The Square Golf [`DeviceDriver`] (`driver` feature).
//!
//! [`Omni`] runs a Square Golf Omni for any host of flightrelay's driver
//! contract: flightrelay's `FrpServer`, the `allsquare-frp`
//! binary, or an application that embeds the driver. Connecting needs a BLE
//! backend (the `bluez` or `btleplug` feature).
//!
//! What a session does:
//!
//! - **Connect.** Scan for the device (or the one [`Config::address`] pins),
//!   connect without pairing and read its identity. The connect can be
//!   stopped at any point; a connection that completes after a stop is
//!   disconnected again, since the device takes one central at a time.
//! - **Arm** once, with the club and hand the host has latched, else the
//!   config's club and a right-handed player, and the configured spin mode.
//! - **Shots** become `shot_trigger` (stamped with the time the ball
//!   notification arrived), `launch`, then `club_path` and `face_impact` when
//!   the device tracked the club, then `shot_finished`. The device measures
//!   launch only, so there is no `flight_result`. A total spin of 0 is sent
//!   as 0, with no spin axis.
//! - **Zero-spin misreads** (a shot with no spin at all, unless the putter is
//!   in play) are dropped when [`Config::discard_non_putting_zero_spin`] is
//!   on: the driver raises a warning alert, counts the drop in the
//!   `zero_spin_discards` telemetry key and emits
//!   [`DeviceEvent::ShotDiscarded`](flightrelay::driver::DeviceEvent::ShotDiscarded).
//!   The dropped shot takes no shot number.
//! - **Club and hand** follow the host's commands. A club the device has no
//!   code for is armed as the nearest club of the same kind
//!   ([`crate::config::nearest_club`]). The device is told only
//!   when the club or the hand differs from what it has, with one club select
//!   carrying both. The detection mode does not apply: putting and chipping
//!   differ only by the club in play.
//! - **Telemetry:** `ready`, `device_state` (`waiting`, `ready`,
//!   `recording`, `processing`), `battery_pct`, `zero_spin_discards`.
//! - **Close** disarms the device and disconnects it on either backend,
//!   within 5 s. After a lost link it only lets go of the link.
//! - **Raw frames:** each notification (`"notify 11 02"`, labeled with its
//!   family and type bytes, or its family byte alone for the other families,
//!   e.g. `"notify 91"`) and each command (`"cmd 82"`, labeled with its type
//!   byte), once the identity has been read, stamped with the time it
//!   reached the BLE stack or was handed to it. Heartbeats, heartbeat acks,
//!   query replies and notifications this crate does not recognize never
//!   become raw frames; they go to the host's trace log.
//!
//! The device id is the device's advertised name, e.g. `SquareGolf(CD01)`.
//! The card label adds its BLE address, which pins the device in the config.

mod convert;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flightrelay::config::{ConfigError, DriverConfig};
use flightrelay::driver::{
    Backoff, ConnectCtx, DeviceCommand, DeviceDriver, DeviceIdentity, DeviceSession, DriverInfo,
    Events, Level, NoAction, RawProfile, SessionEnd, ShotIds, Stopped,
};
use flightrelay::{FrpEvent, RawDir, RawFrame, Severity, Source, Velocity};

use crate::client::{Client, Event, Transport};
use crate::club::{Club, Handed};
use crate::config::{Settings, nearest_club};
use crate::protocol::{self, BallMetrics, Command, DeviceState, Notification};

pub use crate::config::Config;
pub use convert::{club_data, face_impact, launch};

const MANUFACTURER: &str = "Invant";

/// How long a poll that read nothing waits before reading again.
const IDLE_SLEEP: Duration = Duration::from_millis(5);

/// How long close waits for the disarm write before giving up on it.
const DISARM_LIMIT: Duration = Duration::from_secs(2);

/// How long a btleplug disconnect may take.
#[cfg(feature = "btleplug")]
const RELEASE_LIMIT: Duration = Duration::from_secs(3);

/// Why a connect or a session ended.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The host stopped the driver.
    #[error("stopped")]
    Stopped,
    /// The device or the BLE stack failed.
    #[error(transparent)]
    Device(#[from] crate::Error),
}

impl From<Stopped> for Error {
    fn from(_: Stopped) -> Self {
        Self::Stopped
    }
}

/// A connected BLE link to the device, as the driver uses it: the
/// [`Transport`], plus what the driver needs to identify the device and to
/// let it go.
pub trait BleLink: Transport + Send {
    /// The device's advertised name, e.g. `SquareGolf(CD01)`: the FRP
    /// `device` id.
    fn name(&self) -> &str;

    /// The device's BLE address (on macOS, the peripheral UUID).
    fn address(&self) -> &str;

    /// Disconnect from the device, within 3 s. The device takes one central
    /// at a time, and a `BlueZ` connection outlives its transport, so a
    /// session that ends calls this.
    ///
    /// # Errors
    /// If the BLE stack refuses or does not answer.
    fn release(&mut self) -> crate::Result<()>;

    /// Give up on writes and characteristic reads once `cancel` is set, so a
    /// stop or a close is not held up by a device that does not answer.
    /// `None` detaches the flag. The default ignores it: the link's own
    /// timeouts bound its I/O.
    fn set_cancel(&mut self, cancel: Option<Arc<AtomicBool>>) {
        let _ = cancel;
    }

    /// When the notification the last [`read`](Transport::read) returned
    /// reached the BLE stack. `None` (the default) when the link does not
    /// know; the driver then uses the time of the read.
    fn received_at(&self) -> Option<SystemTime> {
        None
    }
}

#[cfg(feature = "bluez")]
impl BleLink for crate::bluez::BluezTransport {
    fn name(&self) -> &str {
        Self::name(self)
    }
    fn address(&self) -> &str {
        Self::address(self)
    }
    fn release(&mut self) -> crate::Result<()> {
        self.disconnect()
    }
    fn set_cancel(&mut self, cancel: Option<Arc<AtomicBool>>) {
        Self::set_cancel(self, cancel);
    }
    fn received_at(&self) -> Option<SystemTime> {
        Self::received_at(self)
    }
}

#[cfg(feature = "btleplug")]
impl BleLink for crate::btleplug_transport::BtleplugTransport {
    fn name(&self) -> &str {
        Self::name(self)
    }
    fn address(&self) -> &str {
        Self::address(self)
    }
    fn release(&mut self) -> crate::Result<()> {
        self.disconnect_within(RELEASE_LIMIT)
    }
    fn set_cancel(&mut self, cancel: Option<Arc<AtomicBool>>) {
        Self::set_cancel(self, cancel);
    }
    fn received_at(&self) -> Option<SystemTime> {
        Self::received_at(self)
    }
}

/// The Square Golf Omni driver. See the [module docs](self).
#[derive(Debug)]
pub struct Omni {
    config: Config,
    settings: Settings,
    /// Config values that were ignored, logged on the next connect.
    warnings: Vec<String>,
    ids: ShotIds,
}

impl Omni {
    fn with(config: Config) -> Self {
        let settings = config.settings();
        Self {
            warnings: settings.warnings.clone(),
            settings,
            config,
            ids: ShotIds::new(),
        }
    }

    /// Connect over `link` instead of scanning: the connect steps after the
    /// BLE link is up (identity, latched club and hand), so a test runs a
    /// whole session on a fake link.
    ///
    /// # Errors
    /// [`Error::Stopped`] after a stop, or the identity read failing.
    #[cfg(any(test, feature = "testing"))]
    pub fn connect_with(
        &mut self,
        ctx: &mut ConnectCtx<'_>,
        link: impl BleLink + 'static,
    ) -> Result<OmniSession, Error> {
        ctx.tick()?;
        self.log_warnings(ctx);
        let opened = Opened::new(Box::new(link), &self.settings)?;
        ctx.tick()?;
        Ok(self.session(ctx, opened))
    }

    fn log_warnings(&mut self, ctx: &mut ConnectCtx<'_>) {
        for warning in self.warnings.drain(..) {
            ctx.out.log(Level::Warn, warning);
        }
    }

    /// The session over a link whose identity has been read.
    fn session(&self, ctx: &mut ConnectCtx<'_>, mut opened: Opened) -> OmniSession {
        let latched = *ctx.latched();
        let club = latched.club.unwrap_or(self.settings.club);
        let hand = handed(latched.handed);
        opened.client.set_handed(hand);

        let Identified {
            name,
            address,
            firmware,
            mmi,
            hardware,
            model,
        } = opened.identified;
        ctx.out
            .log(Level::Info, format!("connected to {name} at {address}"));
        ctx.out.log(
            Level::Info,
            format!("model {model}, firmware lm {firmware} (mmi {mmi}), hw {hardware}"),
        );
        let label = if model.is_empty() {
            format!("Square Golf {name} ({address})")
        } else {
            format!("Square Golf {model} {name} ({address})")
        };
        let mut identity = DeviceIdentity::new(name)
            .with_manufacturer(MANUFACTURER)
            .with_firmware(firmware)
            .with_label(label);
        if !model.is_empty() {
            identity = identity.with_model(model);
        }

        // The identity exchange is never a raw frame.
        for wire in opened.log.take() {
            let (label, _) = classify(&wire);
            ctx.out.audit(wire.dir, &label, &wire.data);
        }

        OmniSession {
            client: opened.client,
            log: opened.log,
            identity,
            ids: self.ids.clone(),
            spin: self.settings.spin,
            discard_zero_spin: self.settings.discard_zero_spin,
            club,
            hand,
            armed: false,
            discards: 0,
            last_rx: Vec::new(),
            ball_at: None,
            pending: Vec::new(),
            link: LinkState::Up,
        }
    }

    /// Scan, connect and read the identity on a thread of its own, so the
    /// host keeps ticking and a stop is seen within 100 ms. `None` after a
    /// stop.
    #[cfg(any(feature = "bluez", feature = "btleplug"))]
    fn open_ble(&self, ctx: &mut ConnectCtx<'_>) -> Result<Option<Opened>, Error> {
        use std::sync::mpsc::{self, RecvTimeoutError};

        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<crate::Result<Option<Opened>>>();
        let address = self.settings.address.clone();
        let settings = self.settings.clone();
        let flag = Arc::clone(&cancel);
        std::thread::Builder::new()
            .name("allsquare-connect".into())
            .spawn(move || {
                let result = open_link(address.as_deref(), &settings, &flag);
                // Connect gave up waiting: let the device go.
                if let Err(mpsc::SendError(Ok(Some(mut opened)))) = tx.send(result) {
                    opened.release();
                }
            })
            .map_err(|e| crate::Error::Backend(format!("connect thread: {e}")))?;

        let mut stopped_at: Option<Instant> = None;
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(result) => {
                    if stopped_at.is_some() {
                        if let Ok(Some(mut opened)) = result {
                            opened.release();
                        }
                        return Err(Error::Stopped);
                    }
                    return Ok(result?);
                }
                Err(RecvTimeoutError::Timeout) => match stopped_at {
                    None => {
                        if ctx.tick().is_err() {
                            cancel.store(true, Ordering::Release);
                            stopped_at = Some(Instant::now());
                        }
                    }
                    // The connect thread lets go of what it finds later.
                    Some(at) if at.elapsed() >= Self::STOP_WAIT => return Err(Error::Stopped),
                    Some(_) => {}
                },
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(crate::Error::Backend("connect thread ended".into()).into());
                }
            }
        }
    }
}

/// Scan, connect and read the identity, giving up when `cancel` is set.
#[cfg(any(feature = "bluez", feature = "btleplug"))]
fn open_link(
    address: Option<&str>,
    settings: &Settings,
    cancel: &Arc<AtomicBool>,
) -> crate::Result<Option<Opened>> {
    let Some(transport) = crate::ble::connect_cancelable(address, cancel)? else {
        return Ok(None);
    };
    let mut link: Box<dyn BleLink> = Box::new(transport);
    if cancel.load(Ordering::Acquire) {
        let _ = link.release();
        return Ok(None);
    }
    // A cancel ends the identity reads early. The session does not keep the
    // flag.
    link.set_cancel(Some(Arc::clone(cancel)));
    let mut opened = Opened::new(link, settings)?;
    if cancel.load(Ordering::Acquire) {
        opened.release();
        return Ok(None);
    }
    opened.client.transport_mut().inner.set_cancel(None);
    Ok(Some(opened))
}

impl DeviceDriver for Omni {
    type Config = Config;
    type Error = Error;
    type Action = NoAction;
    type Session = OmniSession;

    /// A stop ends a scan or a connect within about a second on either
    /// backend, and a connection that completed meanwhile is disconnected:
    /// at most 5 s in all, since every `BlueZ` call gives up after 2.5 s
    /// and every btleplug step is raced against the stop. Close is a disarm
    /// write (given up after 2 s on btleplug, 2.5 s on `BlueZ`) and a
    /// disconnect (3 s, 2.5 s): at most 5 s.
    const STOP_WAIT: Duration = Duration::from_secs(6);

    fn info(_: &Config) -> DriverInfo {
        DriverInfo::new("allsquare", "Square Golf Omni", Source::Measured)
            .with_manufacturer(MANUFACTURER)
            .exclusive()
            .with_raw(RawProfile::new("square-ble", 64, 64, 0, no_bulk, false))
    }

    fn new(config: Config) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(Self::with(config))
    }

    fn config(&self) -> &Config {
        &self.config
    }

    fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<OmniSession, Error> {
        ctx.tick()?;
        self.log_warnings(ctx);
        ctx.out.phase("discovering");
        ctx.out
            .log(Level::Info, "searching for Square Golf device...");
        #[cfg(any(feature = "bluez", feature = "btleplug"))]
        {
            let Some(opened) = self.open_ble(ctx)? else {
                return Err(Error::Stopped);
            };
            Ok(self.session(ctx, opened))
        }
        #[cfg(not(any(feature = "bluez", feature = "btleplug")))]
        {
            Err(crate::Error::Backend(
                "built without a BLE backend: enable the `bluez` or `btleplug` feature".into(),
            )
            .into())
        }
    }

    fn retry_delay(
        &mut self,
        ended: &SessionEnd<'_, Error>,
        backoff: &mut Backoff,
        out: &mut Events<'_>,
    ) -> Option<Duration> {
        if let Some(error) = ended.error {
            out.alert(Severity::Warn, format!("Square Golf error: {error}"));
        }
        Some(backoff.delay(ended.connected_for))
    }

    fn apply_config(&mut self, new: &Config) {
        let ids = self.ids.clone();
        *self = Self {
            ids,
            ..Self::with(new.clone())
        };
    }
}

fn no_bulk(_: &RawFrame) -> bool {
    false
}

/// The device hand for the player's hand; right when unknown.
fn handed(handed: Option<flightrelay::Handedness>) -> Handed {
    if handed == Some(flightrelay::Handedness::Left) {
        Handed::Left
    } else {
        Handed::Right
    }
}

/// The club the device is armed with for an FRP club: the nearest one it
/// has a code for, or the 7-iron for a code this crate does not know.
fn square_club(club: flightrelay::Club) -> Club {
    nearest_club(club).unwrap_or(Club::Iron7)
}

/// Whether a shot is a spin misread to drop rather than report.
///
/// The device occasionally returns a shot with no spin at all, typically a
/// ball struck at the very front of the detection zone. A real strike always
/// imparts spin, and a spinless shot carries much further than it should in a
/// simulator. A putt has no airborne flight to measure spin over and reads
/// zero every time, so the putter is exempt.
fn is_zero_spin_misread(b: &BallMetrics, club: flightrelay::Club, enabled: bool) -> bool {
    enabled
        && club != flightrelay::Club::Putter
        && b.total_spin == 0
        && b.back_spin == 0
        && b.side_spin == 0
}

/// What the identity read found.
#[derive(Debug)]
struct Identified {
    name: String,
    address: String,
    firmware: String,
    mmi: String,
    hardware: String,
    model: String,
}

/// A link whose identity has been read.
struct Opened {
    client: Client<Tap>,
    log: WireLog,
    identified: Identified,
}

impl Opened {
    /// Read the identity over `link`. The link is released if that fails.
    fn new(link: Box<dyn BleLink>, settings: &Settings) -> crate::Result<Self> {
        let log = WireLog::default();
        let name = link.name().to_owned();
        let address = link.address().to_owned();
        let mut client = Client::new(Tap {
            inner: link,
            log: log.clone(),
        });
        client.set_impact_calibration(settings.calibration.clone());
        match client.poll() {
            Ok(Some(Event::Connected {
                firmware,
                hardware,
                model,
                ..
            })) => Ok(Self {
                client,
                log,
                identified: Identified {
                    name,
                    address,
                    firmware: firmware.lm,
                    mmi: firmware.mmi,
                    hardware,
                    model,
                },
            }),
            other => {
                let _ = client.transport_mut().inner.release();
                match other {
                    Err(e) => Err(e),
                    Ok(_) => Err(crate::Error::Backend("no identity from the device".into())),
                }
            }
        }
    }

    #[cfg_attr(not(any(feature = "bluez", feature = "btleplug")), allow(dead_code))] // read by the BLE connect
    fn release(&mut self) {
        let _ = self.client.transport_mut().inner.release();
    }
}

/// One message as it crossed the link.
#[derive(Debug)]
struct Wire {
    dir: RawDir,
    data: Vec<u8>,
    /// UTC milliseconds.
    at: Option<u64>,
}

/// UTC milliseconds.
fn utc_ms(at: SystemTime) -> Option<u64> {
    at.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
}

/// Messages the [`Tap`] saw, for the session to pass on.
#[derive(Debug, Clone, Default)]
struct WireLog(Arc<Mutex<Vec<Wire>>>);

impl WireLog {
    fn lock(&self) -> MutexGuard<'_, Vec<Wire>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn push(&self, dir: RawDir, data: &[u8], at: SystemTime) {
        self.lock().push(Wire {
            dir,
            data: data.to_vec(),
            at: utc_ms(at),
        });
    }

    fn take(&self) -> Vec<Wire> {
        std::mem::take(&mut *self.lock())
    }
}

/// The link, recording every notification and command. Characteristic
/// reads, where the device's identity lives, are not recorded.
struct Tap {
    inner: Box<dyn BleLink>,
    log: WireLog,
}

impl Transport for Tap {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            let at = self.inner.received_at().unwrap_or_else(SystemTime::now);
            self.log.push(RawDir::Rx, &buf[..n], at);
        }
        Ok(n)
    }

    fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        // Handed to the stack now; the write returns after the device's
        // response.
        let at = SystemTime::now();
        self.inner.write(data)?;
        self.log.push(RawDir::Tx, data, at);
        Ok(())
    }

    fn read_characteristic(&mut self, uuid: u128) -> std::io::Result<Vec<u8>> {
        self.inner.read_characteristic(uuid)
    }

    fn model_hint(&self) -> Option<String> {
        self.inner.model_hint()
    }
}

/// A message's raw-frame label, and whether it stays private: heartbeats,
/// heartbeat acks, query replies and notifications this crate does not
/// recognize.
fn classify(wire: &Wire) -> (String, bool) {
    match wire.dir {
        RawDir::Rx => {
            // In the 0x11 family the second byte is the notification type;
            // in the others it is already data.
            let label = match wire.data.as_slice() {
                [0x11, ty, ..] => format!("notify 11 {ty:02x}"),
                [family, ..] => format!("notify {family:02x}"),
                [] => "notify".to_owned(),
            };
            let private = !matches!(
                protocol::parse(&wire.data),
                Ok(Notification::Sensor(_)
                    | Notification::Ball(_)
                    | Notification::Club(_)
                    | Notification::Battery { .. }
                    | Notification::Clock(_)
                    | Notification::Alignment(_))
            );
            (label, private)
        }
        RawDir::Tx => {
            let ty = Command::type_byte_of(&wire.data);
            let label = format!("cmd {:02x}", ty.unwrap_or(0));
            let private = ty.is_none_or(|t| {
                t == Command::Heartbeat.type_byte() || t == Command::Query.type_byte()
            });
            (label, private)
        }
    }
}

/// A connection to a Square Golf Omni. See the [module docs](self).
pub struct OmniSession {
    client: Client<Tap>,
    log: WireLog,
    identity: DeviceIdentity,
    ids: ShotIds,
    spin: protocol::SpinMode,
    discard_zero_spin: bool,
    /// The club in play, as the host named it.
    club: flightrelay::Club,
    hand: Handed,
    armed: bool,
    /// Zero-spin misreads dropped in this session.
    discards: u32,
    /// The last notification, to recognize the device's repeat of it as the
    /// client does.
    last_rx: Vec<u8>,
    /// When the pending shot's ball notification arrived (UTC ms).
    ball_at: Option<u64>,
    /// Log lines from [`command`](DeviceSession::command), which has no
    /// sink; sent on the next poll.
    pending: Vec<(Level, String)>,
    link: LinkState,
}

/// Where the session's BLE link stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkState {
    Up,
    /// It failed and ended the session.
    Lost,
    /// [`close`](DeviceSession::close) let it go.
    Released,
}

impl std::fmt::Debug for OmniSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OmniSession")
            .field("identity", &self.identity)
            .field("club", &self.club)
            .field("hand", &self.hand)
            .field("armed", &self.armed)
            .finish_non_exhaustive()
    }
}

impl OmniSession {
    /// Disarm, giving up on the write after `limit` where the link can.
    fn disarm_within(&mut self, limit: Duration) -> crate::Result<()> {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let (done, wait) = mpsc::channel::<()>();
        // Ends as soon as the disarm does: `done` drops below.
        let timer = std::thread::Builder::new()
            .name("allsquare-close".into())
            .spawn(move || {
                if wait.recv_timeout(limit) == Err(mpsc::RecvTimeoutError::Timeout) {
                    flag.store(true, Ordering::Release);
                }
            });
        if timer.is_ok() {
            self.client.transport_mut().inner.set_cancel(Some(cancel));
        }
        let disarmed = self.client.disarm();
        drop(done);
        self.client.transport_mut().inner.set_cancel(None);
        disarmed
    }

    fn arm(&mut self, out: &mut Events<'_>) -> Result<(), Error> {
        out.ready(false);
        let club = square_club(self.club);
        let armed = self.client.arm(club, self.spin);
        self.drain(out);
        armed?;
        self.armed = true;
        out.log(Level::Info, format!("armed with {club}"));
        Ok(())
    }

    /// Pass on what crossed the link since the last call: raw frames for
    /// shot data, trace lines for the rest.
    fn drain(&mut self, out: &mut Events<'_>) {
        for wire in self.log.take() {
            // A ball notification that is not a repeat starts the shot the
            // client holds, as the client sees it.
            if wire.dir == RawDir::Rx {
                if wire.data != self.last_rx && wire.data.starts_with(&[0x11, 0x02]) {
                    self.ball_at = wire.at;
                }
                self.last_rx.clone_from(&wire.data);
            }
            let (label, private) = classify(&wire);
            if private {
                out.audit(wire.dir, &label, &wire.data);
            } else if out.wants_raw() {
                let frame = RawFrame {
                    timestamp: wire.at,
                    label: Some(label),
                    ..RawFrame::new(wire.dir, 0, wire.data)
                };
                out.frp(FrpEvent::raw_frame(frame));
            }
        }
    }

    fn handle(&mut self, event: &Event, out: &mut Events<'_>) {
        match event {
            &Event::StateChanged(state) => {
                out.log(Level::Debug, format!("device state: {state:?}"));
                let label = match state {
                    DeviceState::Detect => Some("waiting"),
                    DeviceState::Ready => Some("ready"),
                    DeviceState::Shot => Some("recording"),
                    DeviceState::Done => Some("processing"),
                    _ => None,
                };
                let ready = (
                    flightrelay::types::telemetry::READY,
                    (state == DeviceState::Ready).to_string(),
                );
                match label {
                    Some(label) => out.telemetry([ready, ("device_state", label.to_owned())]),
                    None => out.telemetry([ready]),
                }
            }
            &Event::Battery { percent, state } => {
                out.log(Level::Debug, format!("battery {percent}% {state:?}"));
                out.telemetry([(
                    flightrelay::types::telemetry::BATTERY_PCT,
                    percent.to_string(),
                )]);
            }
            Event::Shot { ball, club } => self.shot(ball, club.as_ref(), out),
            _ => {}
        }
    }

    fn shot(
        &mut self,
        ball: &BallMetrics,
        club: Option<&protocol::ClubMetrics>,
        out: &mut Events<'_>,
    ) {
        let ball_at = self.ball_at.take();
        // Dropped before a number is taken, so a discarded shot leaves no gap
        // in the numbering and no partial shot.
        if is_zero_spin_misread(ball, self.club, self.discard_zero_spin) {
            let mph = Velocity::MetersPerSecond(ball.speed).as_mph();
            self.discards += 1;
            out.log(
                Level::Warn,
                format!("discarded shot: {mph:.1}mph with zero spin (misread)"),
            );
            out.alert(
                Severity::Warn,
                format!(
                    "Square Golf: discarded a {mph:.0} mph shot that read zero spin — the ball \
                     was likely too far forward in the hitting zone. Re-hit it."
                ),
            );
            out.telemetry([("zero_spin_discards", self.discards.to_string())]);
            out.shot_discarded();
            return;
        }

        let key = self.ids.next_key();
        out.log(Level::Info, format!("shot #{}", key.shot_number));
        match ball_at {
            Some(ms) => out.shot_trigger_at(key.clone(), ms),
            None => out.shot_trigger(key.clone()),
        }
        let l = launch(ball);
        out.log(
            Level::Debug,
            format!(
                "  ball: {:.1}mph VLA={:.1} HLA={:.1} back={}rpm side={}rpm",
                Velocity::MetersPerSecond(ball.speed).as_mph(),
                ball.launch_angle,
                ball.direction,
                l.backspin_rpm.unwrap_or(0),
                l.sidespin_rpm.unwrap_or(0),
            ),
        );
        out.launch(key.clone(), l);
        match club {
            Some(c) => {
                out.club_path(key.clone(), club_data(c));
                if let Some(impact) = face_impact(c) {
                    out.face_impact(key.clone(), impact);
                }
            }
            None => out.log(Level::Debug, "  no club data (sticker not tracked)"),
        }
        out.shot_finished(key);
    }
}

impl DeviceSession for OmniSession {
    type Driver = Omni;

    fn poll(&mut self, timeout: Duration, out: &mut Events<'_>) -> Result<(), Error> {
        for (level, line) in self.pending.drain(..) {
            out.log(level, line);
        }
        if !self.armed {
            self.arm(out)?;
        }
        let deadline = Instant::now() + timeout;
        // A read that yields no event (a repeat, a club metrics request) is
        // not idleness; a few in a row are.
        let mut idle = 0u32;
        loop {
            let polled = self.client.poll();
            self.drain(out);
            match polled {
                Ok(Some(event)) => {
                    idle = 0;
                    self.handle(&event, out);
                }
                Ok(None) => {
                    idle += 1;
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Ok(());
                    }
                    if idle >= 3 {
                        std::thread::sleep(left.min(IDLE_SLEEP));
                    }
                }
                Err(e @ (crate::Error::Disconnected | crate::Error::Transport(_))) => {
                    out.log(Level::Info, format!("link lost: {e}"));
                    self.link = LinkState::Lost;
                    return Err(e.into());
                }
                Err(e) => {
                    out.log(Level::Warn, format!("protocol error: {e}"));
                    out.alert(Severity::Warn, format!("Square Golf: {e}"));
                }
            }
            if Instant::now() >= deadline {
                return Ok(());
            }
        }
    }

    fn command(&mut self, cmd: &DeviceCommand) -> Result<(), Error> {
        let mut reselect = false;
        if let Some(club) = cmd.club
            && club != self.club
        {
            if nearest_club(club).is_some() {
                self.club = club;
                reselect = true;
            } else {
                self.pending.push((
                    Level::Warn,
                    format!("no Square Golf club for {club}, keeping {}", self.club),
                ));
            }
        }
        if let Some(hand) = cmd.handed.map(|h| handed(Some(h)))
            && hand != self.hand
        {
            self.hand = hand;
            self.client.set_handed(hand);
            reselect = true;
        }
        if let Some(mode) = cmd.mode {
            self.pending.push((
                Level::Debug,
                format!(
                    "mode change to {mode} ignored: Square Golf has no mode control, select a club"
                ),
            ));
        }
        // Before the arm, the arm carries the club and the hand.
        if reselect && self.armed {
            let club = square_club(self.club);
            self.pending
                .push((Level::Info, format!("club -> {club}, hand {:?}", self.hand)));
            if let Err(e) = self.client.select_club(club) {
                self.pending
                    .push((Level::Warn, format!("club select failed: {e}")));
            }
        }
        Ok(())
    }

    fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    fn close(mut self, out: &mut Events<'_>) -> Result<(), Error> {
        // After a lost link there is nothing to disarm, and the disconnect
        // may fail for the same reason the link did.
        let lost = self.link == LinkState::Lost;
        let disarmed = if self.armed && !lost {
            self.disarm_within(DISARM_LIMIT)
        } else {
            Ok(())
        };
        self.drain(out);
        self.link = LinkState::Released;
        let released = self.client.transport_mut().inner.release();
        out.ready(false);
        if lost {
            if let Err(e) = released {
                out.log(
                    Level::Debug,
                    format!("disconnect after the link was lost: {e}"),
                );
            }
            return Ok(());
        }
        disarmed?;
        released?;
        Ok(())
    }
}

impl Drop for OmniSession {
    /// A session dropped without [`close`](DeviceSession::close) still lets
    /// the device go.
    fn drop(&mut self) {
        if self.link != LinkState::Released {
            let _ = self.client.transport_mut().inner.release();
        }
    }
}

#[cfg(test)]
mod tests;
