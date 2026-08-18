//! Cross-platform BLE transport via [`btleplug`].
//!
//! Verified on Windows (WinRT): connects in ~1.7s with no pairing and holds
//! indefinitely. Nothing special is required there, because the OS GATT server
//! answers the device's inbound ATT requests for us — see the crate docs.
//!
//! btleplug is async and this crate's API is not, so a Tokio runtime runs on a
//! background thread and notifications are forwarded over a channel. Same shape
//! as `10over`'s btleplug backend.

use std::io;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Manager, Peripheral};
use futures::StreamExt;
use tokio::runtime::Runtime;
use uuid::Uuid;

use crate::client::Transport;
use crate::error::{Error, Result};
use crate::protocol::{NAME_PREFIX, uuid as ids};

/// How long to scan before giving up.
const SCAN_TIMEOUT: Duration = Duration::from_secs(20);
/// The vendor's client waits this long after stopping the scan before
/// connecting, and after connecting before discovering services.
const SETTLE: Duration = Duration::from_millis(250);

/// A connected Square Golf device over btleplug.
pub struct BtleplugTransport {
    runtime: Runtime,
    peripheral: Peripheral,
    notifications: Receiver<Vec<u8>>,
    name: String,
    address: String,
}

impl BtleplugTransport {
    /// Scan for a Square Golf device and connect to it.
    ///
    /// Pass `address` to select a specific device, or `None` to take the first
    /// one advertising the `SquareGolf` name prefix. **No pairing is performed
    /// or required.**
    ///
    /// # Errors
    /// [`Error::NotFound`] if no device appears within the scan window, or
    /// [`Error::Backend`] for BLE stack failures.
    pub fn connect(address: Option<&str>) -> Result<Self> {
        let runtime = Runtime::new().map_err(|e| Error::Backend(e.to_string()))?;
        let (peripheral, name, ble_address) = runtime.block_on(Self::find(address))?;

        runtime.block_on(async {
            // Never connect straight out of a scan callback.
            tokio::time::sleep(SETTLE).await;
            peripheral
                .connect()
                .await
                .map_err(|e| Error::Backend(format!("connect: {e}")))?;
            tokio::time::sleep(SETTLE).await;
            peripheral
                .discover_services()
                .await
                .map_err(|e| Error::Backend(format!("discover: {e}")))
        })?;

        let evt = Uuid::from_u128(ids::EVT);
        let chars = peripheral.characteristics();
        let evt_char = chars
            .iter()
            .find(|c| c.uuid == evt)
            .ok_or_else(|| Error::Backend("EVT characteristic missing".into()))?
            .clone();

        let (tx, rx) = std::sync::mpsc::channel();
        runtime.block_on(async {
            peripheral
                .subscribe(&evt_char)
                .await
                .map_err(|e| Error::Backend(format!("subscribe: {e}")))
        })?;

        let pump = peripheral.clone();
        runtime.spawn(async move {
            let Ok(mut stream) = pump.notifications().await else {
                return;
            };
            while let Some(n) = stream.next().await {
                if tx.send(n.value).is_err() {
                    break; // receiver dropped
                }
            }
        });

        Ok(Self {
            runtime,
            peripheral,
            notifications: rx,
            name,
            address: ble_address,
        })
    }

    async fn find(address: Option<&str>) -> Result<(Peripheral, String, String)> {
        let manager = Manager::new()
            .await
            .map_err(|e| Error::Backend(e.to_string()))?;
        let adapter = manager
            .adapters()
            .await
            .map_err(|e| Error::Backend(e.to_string()))?
            .into_iter()
            .next()
            .ok_or_else(|| Error::Backend("no Bluetooth adapter".into()))?;

        adapter
            .start_scan(ScanFilter::default())
            .await
            .map_err(|e| Error::Backend(format!("scan: {e}")))?;

        let deadline = Instant::now() + SCAN_TIMEOUT;
        let mut found = None;
        while Instant::now() < deadline && found.is_none() {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let Ok(peripherals) = adapter.peripherals().await else {
                continue;
            };
            for p in peripherals {
                let Ok(Some(props)) = p.properties().await else {
                    continue;
                };
                let name = props.local_name.unwrap_or_default();
                let matched = match address {
                    Some(want) => props.address.to_string().eq_ignore_ascii_case(want),
                    None => name.starts_with(NAME_PREFIX),
                };
                if matched {
                    let addr = props.address.to_string();
                    found = Some((p, name, addr));
                    break;
                }
            }
        }
        let _ = adapter.stop_scan().await;
        found.ok_or(Error::NotFound)
    }

    /// Advertised name of the connected device, e.g. `SquareGolf(54E4)`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// BLE address of the connected device.
    ///
    /// Useful for pinning a specific device in config once it has been found
    /// by auto-discovery.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Whether the link is still up.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.runtime
            .block_on(self.peripheral.is_connected())
            .unwrap_or(false)
    }

    /// Disconnect.
    ///
    /// # Errors
    /// [`Error::Backend`] if the BLE stack refuses.
    pub fn disconnect(&mut self) -> Result<()> {
        self.runtime
            .block_on(self.peripheral.disconnect())
            .map_err(|e| Error::Backend(e.to_string()))
    }

    fn characteristic(&self, uuid: u128) -> io::Result<btleplug::api::Characteristic> {
        let want = Uuid::from_u128(uuid);
        self.peripheral
            .characteristics()
            .into_iter()
            .find(|c| c.uuid == want)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "characteristic not found"))
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
        let cmd = self.characteristic(ids::CMD)?;
        // CMD declares Write *with* response.
        self.runtime
            .block_on(self.peripheral.write(&cmd, data, WriteType::WithResponse))
            .map_err(|e| io::Error::other(e.to_string()))
    }

    fn read_characteristic(&mut self, uuid: u128) -> io::Result<Vec<u8>> {
        let ch = self.characteristic(uuid)?;
        self.runtime
            .block_on(self.peripheral.read(&ch))
            .map_err(|e| io::Error::other(e.to_string()))
    }
}
