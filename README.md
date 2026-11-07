# allsquare

[![CI](https://github.com/divotmaker/allsquare/actions/workflows/ci.yml/badge.svg)](https://github.com/divotmaker/allsquare/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/allsquare.svg)](https://crates.io/crates/allsquare)
[![docs.rs](https://docs.rs/allsquare/badge.svg)](https://docs.rs/allsquare)

## Disclaimer

This project is not affiliated with or endorsed by Invant Inc.
Square Golf and Square Omni are trademarks of Invant Inc.

## Description

Client library for the Square Golf Omni launch monitor. Decodes the Omni's
BLE protocol and exposes shot data through a synchronous, poll-based API:

- **`Client<T>`** — poll-based client over any `Transport`. Handles the connect
  sequence, heartbeats, notification deduplication, shot correlation and device
  state tracking automatically.
- **`ble::connect`** — platform-agnostic BLE transport. BlueZ on Linux
  (`bluez` feature), btleplug on Windows/macOS (`btleplug` feature).
- **`driver::Omni`** — a [flightrelay](https://github.com/flightrelay/sdk-rust)
  `DeviceDriver` (`driver` feature): connect, arm, follow the controller's club
  and hand, report shots, release the device. Any host of that contract can
  run it; its settings are `config::Config` (`config` feature, no BLE stack).
- **`allsquare-frp`** — a [Flight Relay Protocol](https://github.com/flightrelay/spec)
  (FRP 0.2.0) device binary (`frp` feature plus a BLE backend) that serves the
  driver to FRP controllers.

## Legal Basis — DMCA Section 1201(f)

This project is an exercise of the interoperability exception under
[17 U.S.C. § 1201(f)](https://www.law.cornell.edu/uscode/text/17/1201):

> (f) Reverse Engineering.—
>
> (1) Notwithstanding the provisions of subsection (a)(1)(A), a person who has
> lawfully obtained the right to use a copy of a computer program may
> circumvent a technological measure that effectively controls access to a
> particular portion of that program for the sole purpose of identifying and
> analyzing those elements of the program that are necessary to achieve
> interoperability of an independently created computer program with other
> programs, and that have not previously been readily made available to the
> person engaging in the circumvention, to the extent any such acts of
> identification and analysis do not constitute infringement under this title.
>
> (2) Notwithstanding the provisions of subsections (a)(2) and (b), a person
> may develop and employ technological means to circumvent a technological
> measure, or to circumvent protection afforded by a technological measure, in
> order to enable the identification and analysis described in paragraph (1),
> or for the purpose of enabling interoperability of an independently created
> computer program with other programs, if such means are necessary to achieve
> such interoperability, to the extent that doing so does not constitute
> infringement under this title.

The Square Golf Omni uses a proprietary protocol over Bluetooth Low Energy to
communicate shot data (ball speed, launch angle, spin, club data, impact
location, etc.) to companion software. Invant does not publish this protocol or
provide an SDK for third-party integration. The protocol was worked out from
the researcher's own lawfully purchased device and from analysis of the
official Square Golf app, solely to enable interoperability with third-party
golf simulation software.

No Invant code is reproduced here; the protocol is described and implemented in
our own words. No access controls were circumvented — the device uses no
pairing, bonding, or encryption of any kind, and all protocol data was captured
from the researcher's own device.

## Acceptable Use

This project exists solely to enable interoperability between the Square Golf
Omni and third-party golf simulation software.

**It must not be used to:**

- Circumvent licensing or subscription requirements on Invant products
- Unlock paid features without purchase
- Bypass any access controls on Invant software or services

Issues or discussions proposing circumvention of licensing will be closed and
the user blocked.

## License

Licensed under either of:

- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)

at your option.

## Status

Verified against a **Square Golf Omni** (hw 3.1, fw lm 1.5.8) on both Linux
(BlueZ) and Windows (btleplug/WinRT).

| | |
|---|---|
| Ball metrics | speed, launch, direction, total spin, spin axis, back/side spin |
| Club metrics | path, face, attack, dynamic loft, impact location, club speed, smash |
| Control | club selection, arm/disarm, units, green speed, carry adjustment |
| Telemetry | device state, battery, clock |

## Protocol Documentation

Detailed specs live in [`docs/`](docs/):

- **[WIRE.md](docs/WIRE.md)** — BLE GATT profile, packet format, command and
  notification catalog, data encoding, club codes, device states.
- **[SEQUENCE.md](docs/SEQUENCE.md)** — Connection lifecycle, session
  initialization, arming, shot flow, platform notes, minimum viable client.

## Things the device does that will surprise you

`Client` handles all of these; they matter only if you use `protocol` directly.

- **Never pair or bond.** The device does neither, and OS pairing dialogs fail.
  Connect as a plain GATT client.
- **Every notification is sent twice**, byte-identical. Deduplicate, or every
  shot counts twice.
- **Club metrics are pull-based and can be stale.** The device retains the last
  shot across disconnects, so a club response must be correlated with a
  preceding ball packet or you will re-report an old shot on reconnect.
- **The device is a GATT *client* too.** After connecting it sends ATT requests
  to the host; if nothing answers them it terminates the link at ~33s. Any
  normal OS GATT stack answers them for you — this only bites if you bypass one.
- **`0x11` is not a universal prefix.** Battery and clock frames use their own.
- **Units are display-only.** The device's unit setting changes its own screen;
  the wire is always m/s and native units.

### Linux specifics

BlueZ needs two workarounds, both applied automatically by the `bluez` backend:

1. `Trusted` must be set before connecting, or service discovery never completes
   and the device object is purged after ~4s. This is *not* pairing.
2. `Connect()` never sends a D-Bus reply for this device, so it is dispatched
   with `NO_REPLY_EXPECTED` and `ServicesResolved` is polled instead.

### Pinning a device

`ble::connect(None)` takes the first Square Golf device found. To pin one, pass
its advertised name, e.g. `ble::connect(Some("SquareGolf(CD01)"))` — the name is
the same on every OS. The BLE address also works, or on macOS, which hides
addresses, the peripheral UUID. Matching ignores case.

### macOS specifics

- macOS hides BLE addresses; `address()` returns the peripheral UUID, which is
  specific to that Mac. Pin by advertised name for a portable configuration.
- Dropping the transport disconnects the device. The Omni accepts one central at
  a time and stops advertising while connected, so a connection left open would
  hide it from later scans.
- Bluetooth permission belongs to the app that launches the program, e.g.
  Terminal. If its permission prompt is denied, that first run finds no device;
  later launches fail with "permission denied". Grant it in System Settings ›
  Privacy & Security › Bluetooth.
- A powered-off adapter is reported as an error from `connect`, and the next
  `connect` retries.

## Testing support

The `testing` feature (off by default) adds `allsquare::testing`: notification
fixtures and encoders for tests of code that consumes the device's
notifications, so those tests do not spell wire bytes by hand. Enable it as a
dev-dependency feature only:

```toml
[dev-dependencies]
allsquare = { version = "0.3", features = ["testing"] }
```

- `BALL_NOTIFICATION`, `QUERY_RESPONSE`, `CLUB_NOTIFICATION_NO_DATA` — fixed
  frames.
- `ball_notification(&BallMetrics)`, `club_notification(&ClubMetrics)`,
  `heartbeat_ack(DeviceState)`, `query_response(&[u8])` — encoders, each the
  inverse of `protocol::parse`.
- `TYPICAL_SHOT_TYPE` and `ball_metrics()` — the shot-type byte the Omni sends,
  and a zeroed `BallMetrics` carrying it.

The fixtures describe a synthetic device and carry no device identity.

With the `driver` feature too, `driver::Omni::connect_with` runs a driver
session over a link of the test's own (a `driver::BleLink`), so a test can feed
it fixtures and check the FRP events, the commands written and the release.

## FRP binary: `allsquare-frp`

```sh
cargo run --release --features frp,bluez --bin allsquare-frp      # Linux
cargo run --release --features frp,btleplug --bin allsquare-frp   # Windows, macOS
```

```text
allsquare-frp [--config <path>] [--target <listen-addr | ws://host:port/frp>]
              [--set <key>=<value>]... [--log <info|debug|trace>] [--help]
```

- `--target`: an address to listen on for controllers (default `0.0.0.0:5880`),
  or a `ws://` URL of a controller to dial, e.g. `ws://flighthook:5880/frp`.
  Several controllers can connect at once.
- `--config`: a TOML file holding the settings below at the top level.
- `--set key=value`: a setting over the file, e.g. `--set club=PW`,
  `--set address=SquareGolf(CD01)`, `--set dot_bottom_to_face_center_mm.7I=21`.
- `--help` lists every setting with its default. Ctrl-C disarms and
  disconnects the device before exiting.

| Setting | Default | |
|---|---|---|
| `address` | unset | BLE address, macOS peripheral UUID or advertised name to pin; unset or blank takes the first device found |
| `club` | `7I` | FRP club code armed until a controller names a club |
| `advanced_spin` | `true` | the device's advanced spin measurement |
| `discard_non_putting_zero_spin` | `true` | drop shots that read zero spin unless the putter is in play |
| `dot_bottom_to_face_center_mm` | built-in | per-club face impact calibration, keyed by club code (see Face impact) |

Clubs are FRP codes read leniently (`7i`, `I7`, `pw`, `52`); a code that names
no club is ignored with a warning.

What goes out over FRP 0.2.0:

- `device_telemetry` with `source: measured` and the identity (manufacturer
  Invant, model, launch monitor firmware) before the first shot; then `ready`,
  `device_state`, `battery_pct` and `zero_spin_discards`.
- Per shot: `shot_trigger` with a `timestamp` (when the ball notification
  arrived), `launch`, `club_path` and `face_impact` when the club was tracked,
  `shot_finished`. Launch only: no `flight_result`. A total spin of 0 (putts) is
  sent as `total_spin_rpm: 0` with no `spin_axis`.
- A zero-spin read with any club but the putter is a misread: it is dropped
  with a warning alert and counted in `zero_spin_discards`, and takes no shot
  number.
- `set_detection_mode`: `club` and `handed` select the club on the device,
  only when one of them changes. A club the device has no code for is armed as
  the nearest club of the same kind (MD → DR, 2W/4W → 3W, 1H/2H → 3H, 1I/2I →
  3I, …). `mode` is ignored: putting and chipping differ only by club.
- `raw_frame` on request: one per notification (`notify 11 02`, or the family
  byte alone outside the `0x11` family, e.g. `notify 91`) or command
  (`cmd 82`), stamped with the time it reached the BLE stack or was handed to
  it. Heartbeats, heartbeat acks, query replies and notifications this crate
  does not recognize are never sent.
- Stopping disarms the device and disconnects it within 5 s.

### Migrating from 0.3

This release moves to flightrelay 0.5 (FRP 0.2.0). Controllers move to FRP
0.2.0 too; a 0.1.0 peer cannot handshake.

| | 0.3 | This release |
|---|---|---|
| `allsquare-frp` arguments | `allsquare-frp [frp-target] [club]` | named flags only: `allsquare-frp --target <frp-target> --set club=PW` |
| FRP messages | `ball_flight`; telemetry without `source`; triggers without a time | `launch`; telemetry carries `source`; triggers carry a `timestamp`; raw frames on request |
| A club with no Square Golf code | the current club stays armed | the nearest club of its kind is armed |
| Club selection | also re-sent when nothing changed | sent only when the club or the hand changes |
| Total spin of 0 | left out | sent as 0 |
| Zero-spin misreads | reported as shots | discarded |
| Stop and Ctrl-C | not honored during the connect | stop the connect at any point; a running session disarms and disconnects the device |
| Lost link | `allsquare-frp` exits | reconnects with a backoff from 1 s to 15 s; shot numbers continue |
| Controllers | one | several at once |
| Notifications the crate does not recognize | exported as raw frames | written to the trace log only |
| `BlueZ` D-Bus calls | no timeout | each call gives up after 2.5 s |
| FRP device API | `FrpDevice`, `ControllerCommand`, the `frp` module | `driver` (a flightrelay `DeviceDriver`); conversions are `driver::{launch, club_data, face_impact}` |
| Connect API | no cancelable connect | adds `ble::connect_cancelable`, `BluezTransport::connect_cancelable`, `BtleplugTransport::connect_cancelable` and `BtleplugTransport::set_cancel` |

## Reading a command's type

`Command::type_byte_of(&[u8])` reads the command type from an encoded command
frame, for code that records or filters what a `Client` writes.

## Example

```sh
cargo run --example monitor --features bluez -- 7i
cargo run --example monitor --features btleplug -- putter
```

## Units

Speeds m/s, angles degrees, spin RPM. Impact location is millimeters from face
center, negative toward the toe and negative low on the face.

### Face impact (beta)

Impact location is calibrated against a reference launch monitor but still
needs tuning; see
[divotmaker/allsquare#1](https://github.com/divotmaker/allsquare/issues/1).

Horizontal impact is reported as the device measures it. The device measures
vertical impact from the bottom edge of the club sticker's dot, so `Client` adds
the armed club's distance from the bottom of the dot down to face center. Clubs
differ in shape and size, so that distance is configurable per club with
`ImpactCalibration` (`Client::set_impact_calibration`). The defaults:

| Club | Dot bottom to face center | Basis |
|---|---|---|
| Driver | 15 mm | fitted |
| Fairway woods | 9 mm | driver fit − 6.5 mm (measured difference) |
| Hybrids | 8 mm | fitted (4-hybrid) |
| Irons, pitching wedge | 19 mm | fitted (8-iron) |
| Gap and sand wedge | 21 mm | 8-iron fit + 2.5 mm (measured difference) |
| Lob wedge | 24 mm | fitted |
| Putter | — | none (vertical is `None`) |

"Fitted" values come from shots against a reference launch monitor. The others
add the measured difference in dot position to the nearest fitted club. All are
rounded to whole millimeters, toward direct measurement of the clubs, since the
calibration sample cannot support finer precision.

Caveats:

- Calibrated from a small sample of shots and one set of clubs.
- The defaults assume the sticker is in its recommended spot, with the dot center
  about 5 mm below the top of the club. A sticker placed higher or lower shifts
  vertical impact by the same amount, and other club models may differ: measure
  from the bottom edge of the dot down to face center on your own clubs and set
  it.
- Vertical impact needs the active club, so it is `None` until a club is armed.
- Wedges are the least certain, especially horizontal impact on high-lofted wedges.
