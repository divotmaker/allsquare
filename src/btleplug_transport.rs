//! Cross-platform BLE transport via [`btleplug`].
//!
//! Verified on Windows (WinRT): connects in ~1.7s with no pairing and holds
//! indefinitely. Nothing special is required there, because the OS GATT server
//! answers the device's inbound ATT requests for us — see the crate docs.
//!
//! btleplug is async and this crate's API is not, so a Tokio runtime runs on
//! background threads and notifications are forwarded over a channel.
//!
//! # Lifecycle
//!
//! - **One runtime per process.** The Tokio runtime is created on first use
//!   and never dropped. If creating it fails, the next
//!   [`connect`](BtleplugTransport::connect) tries again.
//! - **A fresh OS Bluetooth session per connection.** Each btleplug adapter is
//!   one OS Bluetooth session. A session that has held a connection is
//!   retired: after a disconnect, btleplug's per-session bookkeeping can lose
//!   track of the device (a late disconnect event racing its rediscovery), and
//!   that session then never lists it again. A fresh session per connection
//!   avoids this.
//! - **Scans reuse a session that has not connected.** A scan that finds
//!   nothing, or an attempt whose connect fails, returns its session to a
//!   cache for the next attempt, so retrying while the device is absent or
//!   unreachable opens no new sessions. A session is taken out of the cache
//!   while it scans, so concurrent attempts never share one. If opening a
//!   session fails (no adapter, Bluetooth permission denied), or the scan finds
//!   Bluetooth powered off, nothing is cached and the next attempt opens a new
//!   one.
//! - **Each connection retires one session.** btleplug offers no way to close
//!   a session; on macOS each one keeps a parked Core Bluetooth thread for the
//!   life of the process. The cost is bounded by the number of connections
//!   made, not by scans or failed attempts.
//! - **Every BLE operation has a timeout**, so a lost link surfaces as an
//!   error rather than a hang.
//! - **Link loss ends the notification stream.** The transport watches both
//!   the notification stream and the adapter's disconnect events for this
//!   device; either one ending makes [`Transport::read`] return `Ok(0)`, and
//!   later writes fail fast with [`io::ErrorKind::NotConnected`].
//! - **Dropping the transport disconnects the device.** The Omni accepts one
//!   central at a time and stops advertising while connected, so a connection
//!   left open would hide it from every later scan.

use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use btleplug::api::{
    BDAddr, Central, CentralEvent, CentralState, Characteristic, Manager as _, Peripheral as _,
    ScanFilter, ValueNotification, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::future::ready;
use futures::stream::{self, BoxStream, StreamExt};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use uuid::Uuid;

use crate::ble::matches_pin;
use crate::client::Transport;
use crate::error::{Error, Result};
use crate::protocol::{advertised_name, is_square_golf, uuid as ids};

/// How long to scan before giving up.
const SCAN_TIMEOUT: Duration = Duration::from_secs(20);
/// The vendor's client waits this long after stopping the scan before
/// connecting, and after connecting before discovering services.
const SETTLE: Duration = Duration::from_millis(250);
/// Limit for creating the adapter, which can wait on the OS Bluetooth service.
const INIT_TIMEOUT: Duration = Duration::from_secs(10);
/// Limit for establishing the link.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Limit for every other BLE operation.
const OP_TIMEOUT: Duration = Duration::from_secs(5);
/// Limit for cancelling a connect that failed. The OS may never confirm
/// cancelling a peripheral that never connected.
const CANCEL_TIMEOUT: Duration = Duration::from_secs(1);
/// How long a scan match without its advertised name waits for one.
const NAME_WAIT: Duration = Duration::from_secs(2);
/// How often that wait re-reads the device's properties.
const NAME_POLL: Duration = Duration::from_millis(200);
/// How long an adapter reporting an unknown power state gets to settle before
/// scanning anyway. The state is briefly unknown right after start-up.
const STATE_GRACE: Duration = Duration::from_secs(2);

/// The process-wide runtime. Created on first use and never dropped: every
/// adapter's background tasks run on it.
static RUNTIME: Mutex<Option<&'static Runtime>> = Mutex::new(None);

/// An adapter that has scanned but never connected, kept for the next scan.
/// A connection attempt takes it out of the slot while it scans.
static SCANNER: Mutex<Option<Adapter>> = Mutex::new(None);

/// The shared runtime, creating it on first use. A failed attempt leaves
/// nothing behind, so the next call tries again.
fn runtime() -> Result<&'static Runtime> {
    let mut slot = RUNTIME.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(runtime) = *slot {
        return Ok(runtime);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("allsquare-btleplug")
        .enable_all()
        .build()
        .map_err(|e| Error::Backend(format!("Bluetooth runtime: {e}")))?;
    let runtime: &'static Runtime = Box::leak(Box::new(runtime));
    *slot = Some(runtime);
    Ok(runtime)
}

/// Take the cached scanning adapter for this attempt's exclusive use, opening
/// a new OS Bluetooth session if there is none. A failed attempt caches
/// nothing, so the next call tries again.
fn take_scanner(runtime: &'static Runtime) -> Result<Adapter> {
    let cached = SCANNER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    if let Some(adapter) = cached {
        return Ok(adapter);
    }
    // Adapters spawn their event tasks on the current runtime, so this must
    // run on the shared one.
    runtime.block_on(async {
        let manager = op("Bluetooth init", INIT_TIMEOUT, Manager::new()).await?;
        op("Bluetooth init", INIT_TIMEOUT, manager.adapters())
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| Error::Backend("no Bluetooth adapter found".into()))
    })
}

/// Return an adapter that has not connected to the slot, for the next scan.
fn park(adapter: Adapter) {
    let mut slot = SCANNER.lock().unwrap_or_else(PoisonError::into_inner);
    // A concurrent attempt may have parked its own adapter meanwhile. Keep
    // that one and drop this; on macOS that leaves one more parked session,
    // which only overlapping attempts can cause.
    if slot.is_none() {
        *slot = Some(adapter);
    }
}

/// Await a btleplug operation with a time limit, as a connect-time error.
async fn op<T>(
    name: &str,
    limit: Duration,
    fut: impl Future<Output = btleplug::Result<T>>,
) -> Result<T> {
    match timeout(limit, fut).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(btleplug::Error::PermissionDenied)) => Err(Error::Backend(format!(
            "{name}: Bluetooth permission denied for this application"
        ))),
        Ok(Err(e)) => Err(Error::Backend(format!("{name}: {e}"))),
        Err(_) => Err(Error::Backend(format!("{name}: timed out"))),
    }
}

/// Best-effort disconnect, ignoring the outcome.
async fn release(peripheral: &Peripheral, limit: Duration) {
    let _ = timeout(limit, peripheral.disconnect()).await;
}

/// Link state shared with the notification pump.
#[derive(Default)]
struct Link {
    /// The link is gone; reads and writes are pointless.
    down: AtomicBool,
    /// The BLE stack reported the device disconnected, so there is nothing
    /// left to disconnect.
    released: AtomicBool,
}

/// What the notification pump sees.
enum Pumped {
    Data(Vec<u8>),
    /// The link ended. `released` if the stack reported the disconnect.
    Closed {
        released: bool,
    },
}

/// A device found by scanning.
struct Found {
    peripheral: Peripheral,
    name: String,
    address: String,
}

/// Why a scan ended without a device.
struct ScanFailure {
    error: Error,
    /// The adapter may scan again. Not once Bluetooth has been seen powered
    /// off: Core Bluetooth invalidates a session's peripherals when it leaves
    /// the powered-on state.
    reusable: bool,
}

impl From<Error> for ScanFailure {
    fn from(error: Error) -> Self {
        Self {
            error,
            reusable: true,
        }
    }
}

/// A connected Square Golf device over btleplug.
///
/// Dropping it disconnects the device.
///
/// Its blocking methods ([`connect`](Self::connect),
/// [`disconnect`](Self::disconnect), [`is_connected`](Self::is_connected) and
/// the [`Transport`] I/O) drive an internal Tokio runtime and must not be
/// called from inside a Tokio runtime. Dropping it is safe anywhere.
pub struct BtleplugTransport {
    runtime: &'static Runtime,
    /// The adapter this connection was made through, owned by this transport
    /// alone. Kept alive for the pump's adapter events and for disconnecting.
    _adapter: Adapter,
    peripheral: Peripheral,
    notifications: Receiver<Vec<u8>>,
    link: Arc<Link>,
    pump: JoinHandle<()>,
    /// [`disconnect`](Self::disconnect) succeeded.
    closed: bool,
    name: String,
    address: String,
}

impl BtleplugTransport {
    /// Scan for a Square Golf device and connect to it.
    ///
    /// Pass `address` to select a specific device, or `None` to take the first
    /// Square Golf device found. A pinned value matches, ignoring case, either
    /// the device's [`address`](Self::address) or its advertised
    /// [`name`](Self::name), e.g. `SquareGolf(54E4)`. The advertised name is
    /// the same on every OS, so it is the portable way to pin a device. **No
    /// pairing is performed or required.**
    ///
    /// Auto-discovery accepts a device advertising the `SquareGolf` name
    /// prefix, the Omni's manufacturer data, or the Omni's GAP name — see
    /// [`is_square_golf`].
    ///
    /// # Errors
    /// [`Error::NotFound`] if no device appears within the scan window, or
    /// [`Error::Backend`] for BLE stack failures, including no adapter,
    /// Bluetooth permission denied, Bluetooth powered off, and timeouts.
    pub fn connect(address: Option<&str>) -> Result<Self> {
        let runtime = runtime()?;
        let adapter = take_scanner(runtime)?;
        let found = match runtime.block_on(find(&adapter, address)) {
            Ok(found) => found,
            Err(ScanFailure { error, reusable }) => {
                if reusable {
                    park(adapter);
                }
                return Err(error);
            }
        };
        let peripheral = found.peripheral;

        let connected = runtime.block_on(async {
            // Subscribe to adapter events before connecting, so a disconnect
            // at any point after this is seen by the pump.
            let events = op("events", OP_TIMEOUT, adapter.events()).await?;

            // Never connect straight out of a scan callback.
            tokio::time::sleep(SETTLE).await;
            if let Err(e) = op("connect", CONNECT_TIMEOUT, peripheral.connect()).await {
                // Also cancels a connection attempt still pending in the OS,
                // which would otherwise complete later and hold the device.
                release(&peripheral, CANCEL_TIMEOUT).await;
                return Err(e);
            }
            Ok(events)
        });
        let events = match connected {
            Ok(events) => events,
            Err(e) => {
                // The session never held a connection, so it may scan again.
                park(adapter);
                return Err(e);
            }
        };

        // From here on the session has held a connection and belongs to this
        // transport alone; see the module's Lifecycle notes.
        let link = Arc::new(Link::default());
        let (tx, rx) = std::sync::mpsc::channel();
        let pump = runtime.block_on(async {
            match subscribe(&peripheral).await {
                Ok(notes) => {
                    Ok(runtime.spawn(pump(notes, events, peripheral.id(), tx, Arc::clone(&link))))
                }
                Err(e) => {
                    release(&peripheral, OP_TIMEOUT).await;
                    Err(e)
                }
            }
        })?;

        // The scan may have matched before the advertised name arrived.
        let name = if advertised_name(&found.name).is_some() {
            found.name
        } else {
            runtime
                .block_on(read_advertised_name(&peripheral, Duration::ZERO))
                .unwrap_or(found.name)
        };

        Ok(Self {
            runtime,
            _adapter: adapter,
            peripheral,
            notifications: rx,
            link,
            pump,
            closed: false,
            name,
            address: found.address,
        })
    }

    /// Advertised name of the connected device, e.g. `SquareGolf(54E4)`.
    ///
    /// Falls back to the name the BLE stack reported if the advertised name
    /// was never seen.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Identifier of the connected device: the BLE address, or on macOS (which
    /// does not expose addresses) the peripheral UUID.
    ///
    /// Either this or the advertised [`name`](Self::name) pins a specific
    /// device in [`connect`](Self::connect). The UUID is specific to one Mac,
    /// whereas the advertised name is the same on every OS.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Whether the link is still up.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        if self.closed || self.link.down.load(Ordering::Acquire) {
            return false;
        }
        matches!(
            self.runtime
                .block_on(timeout(OP_TIMEOUT, self.peripheral.is_connected())),
            Ok(Ok(true))
        )
    }

    /// Disconnect. Calling it again, or dropping the transport afterwards, is
    /// harmless.
    ///
    /// # Errors
    /// [`Error::Backend`] if the BLE stack refuses or does not answer in time.
    /// The transport is closed either way; dropping it does not try again.
    pub fn disconnect(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        let result = if self.link.released.load(Ordering::Acquire) {
            Ok(())
        } else {
            self.runtime
                .block_on(op("disconnect", OP_TIMEOUT, self.peripheral.disconnect()))
        };
        self.closed = true;
        self.link.down.store(true, Ordering::Release);
        self.pump.abort();
        result
    }

    fn ensure_up(&self) -> io::Result<()> {
        if self.closed || self.link.down.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "device disconnected",
            ));
        }
        Ok(())
    }

    fn characteristic(&self, uuid: u128) -> io::Result<Characteristic> {
        let want = Uuid::from_u128(uuid);
        self.peripheral
            .characteristics()
            .into_iter()
            .find(|c| c.uuid == want)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "characteristic not found"))
    }

    /// Run a btleplug operation to completion with a time limit, as an I/O
    /// error.
    fn io_op<T>(
        &self,
        name: &str,
        fut: impl Future<Output = btleplug::Result<T>>,
    ) -> io::Result<T> {
        match self.runtime.block_on(timeout(OP_TIMEOUT, fut)) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(io::Error::other(format!("{name}: {e}"))),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{name}: timed out"),
            )),
        }
    }
}

impl Drop for BtleplugTransport {
    fn drop(&mut self) {
        self.pump.abort();
        if self.closed || self.link.released.load(Ordering::Acquire) {
            return;
        }
        let peripheral = self.peripheral.clone();
        let task = async move { release(&peripheral, OP_TIMEOUT).await };
        // Blocking inside an async context would panic, so there the
        // disconnect runs in the background instead. The runtime outlives
        // every transport, so it still completes.
        if tokio::runtime::Handle::try_current().is_ok() {
            drop(self.runtime.spawn(task));
        } else {
            self.runtime.block_on(task);
        }
    }
}

impl Transport for BtleplugTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.notifications.try_recv() {
            Ok(v) => {
                let n = v.len().min(buf.len());
                buf[..n].copy_from_slice(&v[..n]);
                Ok(n)
            }
            Err(TryRecvError::Empty) => {
                Err(io::Error::new(io::ErrorKind::WouldBlock, "no notification"))
            }
            // The pump task ended, which means the link is gone. A zero-length
            // read is how the client learns that.
            Err(TryRecvError::Disconnected) => Ok(0),
        }
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.ensure_up()?;
        let cmd = self.characteristic(ids::CMD)?;
        // CMD declares Write *with* response.
        self.io_op(
            "write",
            self.peripheral.write(&cmd, data, WriteType::WithResponse),
        )
    }

    fn read_characteristic(&mut self, uuid: u128) -> io::Result<Vec<u8>> {
        self.ensure_up()?;
        let ch = self.characteristic(uuid)?;
        self.io_op("read", self.peripheral.read(&ch))
    }
}

/// Scan until a matching device appears.
async fn find(adapter: &Adapter, address: Option<&str>) -> std::result::Result<Found, ScanFailure> {
    wait_powered(adapter).await?;
    op(
        "scan",
        OP_TIMEOUT,
        adapter.start_scan(ScanFilter::default()),
    )
    .await?;
    let found = scan(adapter, address).await;
    let _ = timeout(OP_TIMEOUT, adapter.stop_scan()).await;
    found.ok_or_else(|| ScanFailure::from(Error::NotFound))
}

/// Fail if Bluetooth is off. An unknown state gets a short grace period, then
/// scanning proceeds regardless.
async fn wait_powered(adapter: &Adapter) -> std::result::Result<(), ScanFailure> {
    let deadline = Instant::now() + STATE_GRACE;
    loop {
        match op("adapter state", OP_TIMEOUT, adapter.adapter_state()).await? {
            CentralState::PoweredOn => return Ok(()),
            CentralState::PoweredOff => {
                return Err(ScanFailure {
                    error: Error::Backend("Bluetooth is powered off".into()),
                    reusable: false,
                });
            }
            CentralState::Unknown if Instant::now() >= deadline => return Ok(()),
            CentralState::Unknown => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

async fn scan(adapter: &Adapter, address: Option<&str>) -> Option<Found> {
    let deadline = Instant::now() + SCAN_TIMEOUT;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let Ok(Ok(peripherals)) = timeout(OP_TIMEOUT, adapter.peripherals()).await else {
            continue;
        };
        for p in peripherals {
            let Ok(Ok(Some(props))) = timeout(OP_TIMEOUT, p.properties()).await else {
                continue;
            };
            let reported = props.local_name.unwrap_or_default();
            // macOS hides the MAC address and reports all zeros; the
            // peripheral identifier (a UUID there) is the stable handle.
            let addr = if props.address == BDAddr::default() {
                p.id().to_string()
            } else {
                props.address.to_string()
            };
            let matched = match address {
                Some(want) => matches_pin(want, &addr, &reported),
                None => is_square_golf(
                    &reported,
                    props
                        .manufacturer_data
                        .iter()
                        .map(|(id, data)| (*id, data.as_slice())),
                ),
            };
            if matched {
                let name = match advertised_name(&reported) {
                    Some(name) => name.to_string(),
                    // Matched on manufacturer data or the GAP name. The
                    // advertised name is the portable pin value, so give it a
                    // moment to arrive while the scan is still running.
                    None if address.is_none() => read_advertised_name(&p, NAME_WAIT)
                        .await
                        .unwrap_or(reported),
                    None => reported,
                };
                return Some(Found {
                    peripheral: p,
                    name,
                    address: addr,
                });
            }
        }
    }
    None
}

/// The device's advertised `SquareGolf…` name, re-reading its properties
/// until one is reported or `limit` has passed. A zero `limit` reads once.
async fn read_advertised_name(peripheral: &Peripheral, limit: Duration) -> Option<String> {
    let deadline = Instant::now() + limit;
    loop {
        if let Ok(Ok(Some(props))) = timeout(OP_TIMEOUT, peripheral.properties()).await
            && let Some(name) = props.local_name.as_deref().and_then(advertised_name)
        {
            return Some(name.to_string());
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(NAME_POLL).await;
    }
}

/// Discover services and subscribe to EVT. Returns the notification stream,
/// opened before subscribing so the first notifications are not missed.
async fn subscribe(peripheral: &Peripheral) -> Result<BoxStream<'static, ValueNotification>> {
    tokio::time::sleep(SETTLE).await;
    op("discover", OP_TIMEOUT, peripheral.discover_services()).await?;
    let evt = Uuid::from_u128(ids::EVT);
    let evt_char = peripheral
        .characteristics()
        .into_iter()
        .find(|c| c.uuid == evt)
        .ok_or_else(|| Error::Backend("EVT characteristic missing".into()))?;
    let notes = op("notifications", OP_TIMEOUT, peripheral.notifications()).await?;
    op("subscribe", OP_TIMEOUT, peripheral.subscribe(&evt_char)).await?;
    Ok(notes)
}

/// Forward EVT notifications to the transport until the link ends.
///
/// The link has ended when the notification stream ends or the adapter reports
/// this device disconnected — not every backend ends the stream on link loss.
/// Returning drops `tx`, which [`Transport::read`] reports as `Ok(0)`.
async fn pump(
    notes: BoxStream<'static, ValueNotification>,
    events: BoxStream<'static, CentralEvent>,
    id: btleplug::platform::PeripheralId,
    tx: Sender<Vec<u8>>,
    link: Arc<Link>,
) {
    let evt = Uuid::from_u128(ids::EVT);
    let data = notes
        .filter_map(move |n| ready((n.uuid == evt).then_some(Pumped::Data(n.value))))
        .chain(stream::once(ready(Pumped::Closed { released: false })));
    let gone = events
        .filter(move |e| ready(matches!(e, CentralEvent::DeviceDisconnected(d) if *d == id)))
        .map(|_| Pumped::Closed { released: true });
    let mut merged = stream::select(data, gone);

    while let Some(item) = merged.next().await {
        match item {
            Pumped::Data(v) => {
                if tx.send(v).is_err() {
                    return; // transport dropped
                }
            }
            Pumped::Closed { released } => {
                link.released.store(released, Ordering::Release);
                link.down.store(true, Ordering::Release);
                return;
            }
        }
    }
}
