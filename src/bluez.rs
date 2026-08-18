//! Linux BLE transport via `BlueZ`'s D-Bus GATT API.
//!
//! Two workarounds are mandatory, both discovered the hard way:
//!
//! 1. **`Trusted` must be set before connecting.** `BlueZ` treats an untrusted,
//!    unpaired device as temporary: service discovery never completes, the link
//!    is dropped after ~4s and the device object is removed. With `Trusted`,
//!    services resolve in ~1.4s and stay up indefinitely.
//! 2. **`Connect()` never returns a reply** for this device. Call it, swallow
//!    the resulting timeout, and poll `ServicesResolved` instead.
//!
//! Note this does *not* pair or bond the device — `Trusted` is a separate flag
//! and no SMP exchange occurs.
//!
//! Using bluetoothd's GATT API also means its GATT server answers the device's
//! inbound ATT requests, so the ~33s ATT-transaction disconnect never happens.
//! A raw L2CAP socket would bypass that and have to answer them itself.

use std::io;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use crate::client::Transport;
use crate::error::{Error, Result};
use crate::protocol::{NAME_PREFIX, uuid as ids};

const BLUEZ: &str = "org.bluez";
const DEVICE: &str = "org.bluez.Device1";
const ADAPTER: &str = "org.bluez.Adapter1";
const CHAR: &str = "org.bluez.GattCharacteristic1";
const PROPS: &str = "org.freedesktop.DBus.Properties";

const SCAN_TIMEOUT: Duration = Duration::from_secs(20);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(20);
/// The vendor's client waits this long after stopping a scan before connecting.
const SETTLE: Duration = Duration::from_millis(250);

type Managed = std::collections::HashMap<
    OwnedObjectPath,
    std::collections::HashMap<String, std::collections::HashMap<String, OwnedValue>>,
>;

/// A connected Square Golf device over `BlueZ`.
pub struct BluezTransport {
    conn: Connection,
    device_path: OwnedObjectPath,
    cmd_path: OwnedObjectPath,
    name: String,
    /// BLE address, e.g. `DC:0D:30:62:54:E4`.
    address: String,
    /// Advertised manufacturer payload, e.g. `0300A` — the only model
    /// identifier visible on Linux, since `BlueZ` hides the GAP service.
    model_hint: Option<String>,
    /// Notification payloads forwarded from the signal thread.
    notifications: Receiver<Vec<u8>>,
    chars: std::collections::HashMap<u128, OwnedObjectPath>,
}

fn backend<E: std::fmt::Display>(e: E) -> Error {
    Error::Backend(e.to_string())
}

impl BluezTransport {
    /// Scan for a Square Golf device and connect to it.
    ///
    /// # Errors
    /// [`Error::NotFound`] if nothing appears within the scan window, or
    /// [`Error::Backend`] for D-Bus/BlueZ failures.
    pub fn connect(address: Option<&str>, adapter: &str) -> Result<Self> {
        let conn = Connection::system().map_err(backend)?;
        let adapter_path =
            OwnedObjectPath::try_from(format!("/org/bluez/{adapter}")).map_err(backend)?;

        let (device_path, name, ble_address) = Self::find(&conn, &adapter_path, address)?;
        let model_hint = Self::manufacturer_payload(&conn, &device_path);

        // (1) Trusted first, or discovery silently fails and the device is
        // purged. This is not pairing.
        Self::prop_set(&conn, &device_path, DEVICE, "Trusted", Value::Bool(true))?;

        // (2) Connect() never sends a reply for this device, so an ordinary
        // method call would block forever. Build the message by hand with
        // NO_REPLY_EXPECTED and watch ServicesResolved instead.
        let connect_msg = zbus::message::Message::method_call(device_path.as_ref(), "Connect")
            .map_err(backend)?
            .destination(BLUEZ)
            .map_err(backend)?
            .interface(DEVICE)
            .map_err(backend)?
            .with_flags(zbus::message::Flags::NoReplyExpected)
            .map_err(backend)?
            .build(&())
            .map_err(backend)?;
        conn.send(&connect_msg).map_err(backend)?;

        let deadline = Instant::now() + RESOLVE_TIMEOUT;
        loop {
            if Self::prop_bool(&conn, &device_path, "ServicesResolved")? {
                break;
            }
            if Instant::now() > deadline {
                return Err(Error::Backend("services never resolved".into()));
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        let chars = Self::characteristics(&conn, &device_path)?;
        let cmd_path = chars
            .get(&ids::CMD)
            .ok_or_else(|| Error::Backend("CMD characteristic missing".into()))?
            .clone();
        let evt_path = chars
            .get(&ids::EVT)
            .ok_or_else(|| Error::Backend("EVT characteristic missing".into()))?
            .clone();

        // Subscribe to PropertiesChanged on the EVT characteristic before
        // enabling notifications, so nothing is missed.
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(BLUEZ)
            .map_err(backend)?
            .path(evt_path.as_ref())
            .map_err(backend)?
            .interface(PROPS)
            .map_err(backend)?
            .member("PropertiesChanged")
            .map_err(backend)?
            .build();
        let signals = zbus::blocking::MessageIterator::for_match_rule(rule, &conn, Some(64))
            .map_err(backend)?;

        let evt: Proxy<'_> = Proxy::new(&conn, BLUEZ, &evt_path, CHAR).map_err(backend)?;
        evt.call::<_, _, ()>("StartNotify", &()).map_err(backend)?;

        // The blocking MessageIterator has no non-blocking form, so it lives on
        // its own thread and forwards payloads over a channel. read() then just
        // does a try_recv.
        let (tx, notifications) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("allsquare-bluez".into())
            .spawn(move || {
                for msg in signals {
                    let Ok(msg) = msg else { continue };
                    let Ok((_iface, changed, _inv)) = msg.body().deserialize::<(
                        String,
                        std::collections::HashMap<String, OwnedValue>,
                        Vec<String>,
                    )>() else {
                        continue;
                    };
                    if let Some(v) = changed.get("Value")
                        && let Ok(bytes) = Vec::<u8>::try_from(v.clone())
                        && tx.send(bytes).is_err()
                    {
                        return; // transport dropped
                    }
                }
            })
            .map_err(backend)?;

        Ok(Self {
            conn,
            device_path,
            cmd_path,
            name,
            address: ble_address,
            model_hint,
            notifications,
            chars,
        })
    }

    fn find(
        conn: &Connection,
        adapter_path: &OwnedObjectPath,
        address: Option<&str>,
    ) -> Result<(OwnedObjectPath, String, String)> {
        let scan = |on: bool| -> Result<()> {
            let ad: Proxy<'_> = Proxy::new(conn, BLUEZ, adapter_path, ADAPTER).map_err(backend)?;
            let method = if on {
                "StartDiscovery"
            } else {
                "StopDiscovery"
            };
            let _: std::result::Result<(), _> = ad.call::<_, _, ()>(method, &());
            Ok(())
        };

        if let Some(hit) = Self::look(conn, address)? {
            return Ok(hit);
        }
        scan(true)?;
        let deadline = Instant::now() + SCAN_TIMEOUT;
        let mut hit = None;
        while Instant::now() < deadline && hit.is_none() {
            std::thread::sleep(Duration::from_millis(250));
            hit = Self::look(conn, address)?;
        }
        scan(false)?;
        // Connecting straight out of discovery is what hangs BlueZ.
        std::thread::sleep(SETTLE);
        hit.ok_or(Error::NotFound)
    }

    fn look(
        conn: &Connection,
        address: Option<&str>,
    ) -> Result<Option<(OwnedObjectPath, String, String)>> {
        for (path, ifaces) in Self::managed(conn)? {
            let Some(dev) = ifaces.get(DEVICE) else {
                continue;
            };
            let name = dev
                .get("Alias")
                .or_else(|| dev.get("Name"))
                .and_then(|v| String::try_from(v.clone()).ok())
                .unwrap_or_default();
            let addr = dev
                .get("Address")
                .and_then(|v| String::try_from(v.clone()).ok())
                .unwrap_or_default();
            let matched = match address {
                Some(want) => addr.eq_ignore_ascii_case(want),
                None => name.starts_with(NAME_PREFIX),
            };
            if matched {
                return Ok(Some((path, name, addr)));
            }
        }
        Ok(None)
    }

    fn managed(conn: &Connection) -> Result<Managed> {
        let om: Proxy<'_> = Proxy::new(
            conn,
            BLUEZ,
            ObjectPath::try_from("/").map_err(backend)?,
            "org.freedesktop.DBus.ObjectManager",
        )
        .map_err(backend)?;
        om.call("GetManagedObjects", &()).map_err(backend)
    }

    fn characteristics(
        conn: &Connection,
        device_path: &OwnedObjectPath,
    ) -> Result<std::collections::HashMap<u128, OwnedObjectPath>> {
        let mut out = std::collections::HashMap::new();
        let prefix = device_path.as_str();
        for (path, ifaces) in Self::managed(conn)? {
            if !path.as_str().starts_with(prefix) {
                continue;
            }
            let Some(ch) = ifaces.get(CHAR) else { continue };
            let Some(uuid) = ch
                .get("UUID")
                .and_then(|v| String::try_from(v.clone()).ok())
            else {
                continue;
            };
            if let Ok(parsed) = u128::from_str_radix(&uuid.replace('-', ""), 16) {
                out.insert(parsed, path);
            }
        }
        Ok(out)
    }

    fn prop_set(
        conn: &Connection,
        path: &OwnedObjectPath,
        iface: &str,
        name: &str,
        value: Value<'_>,
    ) -> Result<()> {
        let p: Proxy<'_> = Proxy::new(conn, BLUEZ, path, PROPS).map_err(backend)?;
        p.call::<_, _, ()>("Set", &(iface, name, value))
            .map_err(backend)
    }

    fn prop_bool(conn: &Connection, path: &OwnedObjectPath, name: &str) -> Result<bool> {
        let p: Proxy<'_> = Proxy::new(conn, BLUEZ, path, PROPS).map_err(backend)?;
        let v: OwnedValue = p.call("Get", &(DEVICE, name)).map_err(backend)?;
        Ok(bool::try_from(v).unwrap_or(false))
    }

    /// The advertised manufacturer payload, decoded as ASCII.
    fn manufacturer_payload(conn: &Connection, path: &OwnedObjectPath) -> Option<String> {
        let p: Proxy<'_> = Proxy::new(conn, BLUEZ, path, PROPS).ok()?;
        let v: OwnedValue = p.call("Get", &(DEVICE, "ManufacturerData")).ok()?;
        let map = std::collections::HashMap::<u16, OwnedValue>::try_from(v).ok()?;
        let (_, payload) = map.into_iter().next()?;
        let bytes = Vec::<u8>::try_from(payload).ok()?;
        let s = String::from_utf8(bytes).ok()?;
        (!s.is_empty()).then_some(s)
    }

    /// Advertised name of the connected device.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// BLE address of the connected device, e.g. `DC:0D:30:62:54:E4`.
    ///
    /// Useful for pinning a specific device in config once it has been found
    /// by auto-discovery.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Whether `BlueZ` still reports the device as connected.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        Self::prop_bool(&self.conn, &self.device_path, "Connected").unwrap_or(false)
    }

    /// Disconnect.
    ///
    /// # Errors
    /// [`Error::Backend`] if the D-Bus call fails.
    pub fn disconnect(&mut self) -> Result<()> {
        let dev: Proxy<'_> =
            Proxy::new(&self.conn, BLUEZ, &self.device_path, DEVICE).map_err(backend)?;
        dev.call::<_, _, ()>("Disconnect", &()).map_err(backend)
    }
}

impl Transport for BluezTransport {
    fn model_hint(&self) -> Option<String> {
        self.model_hint.clone()
    }

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
            // The signal thread ended, so the link is gone.
            Err(TryRecvError::Disconnected) => Ok(0),
        }
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let ch: Proxy<'_> = Proxy::new(&self.conn, BLUEZ, &self.cmd_path, CHAR)
            .map_err(|e| io::Error::other(e.to_string()))?;
        // CMD declares Write *with* response, so "request" not "command".
        let opts: std::collections::HashMap<&str, Value<'_>> =
            [("type", Value::from("request"))].into_iter().collect();
        ch.call::<_, _, ()>("WriteValue", &(data, opts))
            .map_err(|e| io::Error::other(e.to_string()))
    }

    fn read_characteristic(&mut self, uuid: u128) -> io::Result<Vec<u8>> {
        let path = self
            .chars
            .get(&uuid)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "characteristic not found"))?;
        let ch: Proxy<'_> = Proxy::new(&self.conn, BLUEZ, path, CHAR)
            .map_err(|e| io::Error::other(e.to_string()))?;
        let opts: std::collections::HashMap<&str, Value<'_>> = std::collections::HashMap::new();
        ch.call("ReadValue", &(opts))
            .map_err(|e| io::Error::other(e.to_string()))
    }
}
