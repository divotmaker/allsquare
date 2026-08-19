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
- **`FrpDevice`** — [Flight Relay Protocol](https://github.com/flightrelay/spec)
  device (`frp` feature). Bridges shot data to an FRP controller over WebSocket,
  either serving controllers on a local port (default 5880) or dialing a central
  controller such as flighthook.

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
provide an SDK for third-party integration. The protocol was reverse-engineered
from the researcher's own lawfully purchased hardware, solely to enable
interoperability with third-party golf simulation software.

No Invant code is reproduced here. No access controls were circumvented — the
device uses no pairing, bonding, or encryption of any kind, and all protocol
data was captured from the researcher's own device.

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
  initialisation, arming, shot flow, platform notes, minimum viable client.

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

## Example

```sh
cargo run --example monitor --features bluez -- 7i
cargo run --example monitor --features btleplug -- putter
```

## Units

Speeds m/s, angles degrees, spin RPM. Impact location is negative toward the toe
and negative low on the face; the scale is believed to be millimetres relative to
face centre, but that has not been verified against a reference launch monitor.
