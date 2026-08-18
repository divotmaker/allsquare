# Wire Protocol Reference

Interoperability specification for the Square Golf Omni launch monitor protocol
over BLE GATT.

Verified against hardware revision 3.1, launch monitor firmware 1.5.8. The
original Square / Square Home shares the GATT profile but uses different club
codes and reports battery differently; this document covers the Omni only.

---

## 1. Transport Overview

The Omni communicates over **Bluetooth Low Energy**. Each GATT write or
notification is one complete, self-contained message — there is no stream to
reassemble, no framing layer, no checksum, and no handshake negotiation.

```
┌──────────────────────────────────────────┐
│  Message (type byte + fixed payload)     │  Application
├──────────────────────────────────────────┤
│  BLE GATT (write / notify)               │  Transport
└──────────────────────────────────────────┘
```

Every message fits in a single notification. All multi-byte integers are
**little-endian** unless noted.

**The device is never paired or bonded.** No characteristic requires encryption
or authentication. Attempting to pair through an operating system's Bluetooth
settings will fail; connect as a plain GATT client instead.

---

## 2. BLE GATT

### Discovery

| Property | Value |
|---|---|
| Advertised name | `SquareGolf(XXXX)` — `XXXX` is the last two bytes of the BLE address |
| Manufacturer ID | `0xFFFF` |
| Manufacturer payload | ASCII model code, e.g. `0300A` |
| Address type | public |

Match on the `SquareGolf` name prefix. Do not filter on a specific manufacturer
company ID.

### Service

All custom UUIDs share the base `8660xxxx-6b7e-439a-bdd1-489a3213e9bb`.

| UUID | Name | Properties | Notes |
|---|---|---|---|
| `86601001-…` | EXGOLF service | — | primary service |
| `86602001-…` | Device ID | read | ASCII serial |
| `86602002-…` | Hardware version | read | e.g. `3.1` |
| `86602003-…` | Firmware version | read | JSON, see below |
| `86602004-…` | Clock | read/write | LE `u64`, see §7 |
| `86602101-…` | CMD | **write with response** | commands to the device |
| `86602102-…` | EVT | notify | messages from the device |
| `86602201-…` | Firmware update | write | not documented here |
| `86602202-…` | — | write | purpose unknown |
| `86608001-…` | — | write without response | purpose unknown |

The standard Battery Service (`0x2a19`) is present but **not implemented** — it
always reads `0`. Battery level arrives as a `0x91` notification instead (§6.7).

`86602003` returns JSON, not raw bytes:

```json
{"launcher": "1.0.1", "mmi": "1.4.0", "lm": "1.5.8"}
```

`lm` is the launch monitor firmware — the version that governs protocol
behaviour.

### ATT MTU

The device does not require an MTU exchange, and every protocol message fits in
the 23-byte default (20-byte payload). If the MTU is left at the default, the
51-byte firmware JSON must be retrieved with **Read Blob** requests.

---

## 3. Packet Format

### Commands (host → device)

```
[0x00] [type] [seq] [payload…]        always 9 bytes
```

- `type` — command ID, see §5
- `seq` — sequence counter, incremented per command, wraps at 256
- `payload` — command-specific, zero-padded to 9 bytes total

Byte 0 is not validated by the device; it behaves as a direction field.

### Notifications (device → host)

```
[family] [type] [payload…]
```

**`0x11` is not a universal prefix.** Three message families exist:

| Family | Meaning |
|---|---|
| `0x11` | shot, sensor, status and response messages (§6.1–6.6) |
| `0x71` | clock tick, unsolicited (§6.8) |
| `0x91` | battery, unsolicited (§6.7) |

Parse byte 0 as a family, not a magic number.

For the `0x11` family, **byte 2 is a device-side message counter** — a `u8` that
increments once per notification and is shared across all `0x11` message types.
It is not per-type, and it does not echo the host's command sequence.

---

## 4. Data Encoding

| Quantity | Type | Scale | Unit |
|---|---|---|---|
| Ball speed, club speed | `i16` | ÷ 100 | m/s |
| Launch, direction, spin axis | `i16` | ÷ 100 | degrees |
| Club path, face, attack, dynamic loft | `i16` | ÷ 100 | degrees |
| Total spin, backspin, sidespin | `i16` | raw | RPM |
| Smash factor | `i16` | ÷ 100 | unitless |
| Impact location | `i16` | ÷ 100 | millimetres (see §6.6) |
| Ball position | `i32` | raw | units undetermined |

All signed integers are two's complement, little-endian.

---

## 5. Commands

| ID | Name | Payload | Response |
|---|---|---|---|
| `0x81` | DetectBall | `{mode} {spin}` | `0x01` stream |
| `0x82` | ClubSelect | `{club_number} {category} {handed}` | — |
| `0x83` | Heartbeat | — | `0x03` |
| `0x85` | Alignment | original Square only | `0x04` |
| `0x86` | Query | — | `0x06` |
| `0x87` | RequestClubMetrics | — | `0x07` |
| `0x88` | SetUnits | `{?} {speedUnit} {distanceUnit}` | — |
| `0x89` | SetGreenSpeed | `{0..5}` | — |
| `0x8a` | SetCarryAdjustment | `{percent}` | — |

A response's type is the command's low nibble: `0x81`→`0x01`, `0x83`→`0x03`,
`0x86`→`0x06`, `0x87`→`0x07`. The three setters are fire-and-forget.

`0x92` (GetOSVersion) exists but the device does not respond to it. Read the
firmware characteristic instead.

### 0x81 — DetectBall

| Byte | Field | Values |
|---|---|---|
| 3 | Mode | `0x00` off, `0x01` on, `0x02` alignment |
| 4 | Spin | `0x10` standard, `0x11` advanced |

Arming once is sufficient — the device remains armed across shots.

### 0x82 — ClubSelect

| Byte | Field |
|---|---|
| 3 | club number |
| 4 | category |
| 5 | handedness — `0x00` right, `0x01` left |

See §8 for codes. There is **no shot-mode field**: putting, chipping and normal
play differ only in which club is selected.

### 0x88 / 0x89 / 0x8a — Display settings

These configure the device's own built-in display and **have no effect on the
wire format**. Values transmitted are always m/s and native units regardless of
the unit setting.

- `0x88` SetUnits — speed `0` m/s, `1` mph; distance `0` metres, `1` yards/feet,
  `2` yards. Byte 3 is a third setting of unknown meaning; `0` is safe.
- `0x89` SetGreenSpeed — `0`–`5`, mapping to stimp 8–13.
- `0x8a` SetCarryAdjustment — a percentage, e.g. `100`.

---

## 6. Notifications

### 6.1 `0x01` — Sensor

17 bytes. Emitted continuously while detection is active.

| Offset | Size | Field |
|---|---|---|
| 0–1 | 2 | `11 01` |
| 2 | 1 | message counter |
| 3 | 1 | ball ready (`0x01`/`0x02` = ready) |
| 4 | 1 | ball detected (`0x01` = detected) |
| 5–8 | 4 | position X, `i32` |
| 9–12 | 4 | position Y, `i32` |
| 13–16 | 4 | position Z, `i32` |

### 6.2 `0x02` — Ball metrics

17 bytes. Pushed on every tracked shot.

| Offset | Size | Field | Scale |
|---|---|---|---|
| 0–1 | 2 | `11 02` | — |
| 2 | 1 | shot type | — |
| 3–4 | 2 | ball speed | ÷100 m/s |
| 5–6 | 2 | launch angle | ÷100 deg |
| 7–8 | 2 | direction | ÷100 deg, positive right |
| 9–10 | 2 | total spin | RPM |
| 11–12 | 2 | spin axis | ÷100 deg |
| 13–14 | 2 | backspin | RPM |
| 15–16 | 2 | sidespin | RPM |

The shot type byte reads `0x37` for every shot, including putts. It does **not**
distinguish shot modes on this device.

Carry, total distance, roll, apex and flight time are **not transmitted**. The
device measures launch only; any flight model is the consumer's responsibility.

### 6.3 `0x03` — Heartbeat acknowledgement

9 bytes. Byte 3 carries the device state (§9). Bytes 4–5 are related state
values that are not fully characterised.

### 6.4 `0x04` — Alignment

Aim angle, `i16` ÷ 100 degrees at offset 5. Original Square only.

### 6.5 `0x06` — Query response

5 bytes, answering `0x86`. Payload meaning is not documented.

### 6.6 `0x07` — Club metrics

19 bytes, answering `0x87`.

```
11 07 {mask} {path} {face} {attack} {loft} {impactH} {impactV} {clubSpeed} {smash}
```

Eight `i16` fields in that order. Byte 2 is a **per-field validity bitmask** —
bit *n* corresponds to field *n*. A field the device did not measure is also set
to the sentinel `0xFFFF` (−1). Check **both** the mask bit and the sentinel.

A fully tracked shot reports `mask = 0xFF`. An untracked shot (no club sticker,
or a strike the device declined) reports `mask = 0x00` with all eight fields set
to the sentinel — **the Omni still sends all 19 bytes**. Do not key on length.

`smash` equals ball speed ÷ club speed.

Impact location sign conventions, for a right-handed player:

| Field | Negative | Positive |
|---|---|---|
| `impactH` | toe | heel |
| `impactV` | low on the face | high |

The scale appears to be millimetres relative to face centre. This has not been
validated against a reference launch monitor, and left-handed behaviour is
unverified.

### 6.7 `0x91` — Battery

3 bytes, unsolicited.

```
91 {level} {chargingState}
```

`level` is 0–100. `chargingState`: `0` not charging, `1` discharging,
`2` charging, `3` full, `4` no battery, `5` unknown.

### 6.8 `0x71` — Clock tick

9 bytes, unsolicited, roughly every 3 seconds. Carries the same value as the
`86602004` characteristic — see §7.

---

## 7. Device Clock

`86602004` is a settable real-time clock. It reads seconds since power-on until
a client writes to it, after which it holds and advances the written value.

The characteristic encodes the value **little-endian**; the `0x71` notification
carries the same value **big-endian**.

Setting the clock is not required for normal operation.

---

## 8. Club Codes

`ClubSelect` carries `{club_number} {category}`. The category disambiguates
numbers that would otherwise collide — a 5-wood and a 5-iron are both number 5.

| Category | Meaning |
|---|---|
| `0x00` | driver |
| `0x01` | wood / hybrid |
| `0x02` | iron / wedge |
| `0x03` | putter |

| Club | Code | Club | Code |
|---|---|---|---|
| Driver | `01 00` | 7-iron | `07 02` |
| 3-wood | `03 01` | 8-iron | `08 02` |
| 5-wood | `05 01` \* | 9-iron | `09 02` |
| 7-wood | `07 01` \* | Pitching wedge | `0a 02` |
| 3-hybrid | `0d 01` | Gap wedge | `0b 02` |
| 4-hybrid | `0e 01` \* | Sand wedge | `0c 02` |
| 5-hybrid | `0f 01` | Lob wedge | `0d 02` \* |
| 3-iron | `03 02` \* | Putter | `01 03` |
| 4-iron | `04 02` | | |
| 5-iron | `05 02` | | |
| 6-iron | `06 02` | | |

\* Inferred from the numbering scheme; unverified.

Irons use their own number, wedges continue the run (PW 10, GW 11, SW 12,
LW 13), hybrids start at 13 within their own category, and woods use their own
number. `00 63` is sent once at connect and is not a club.

These codes do **not** apply to the original Square, which uses a different
scheme.

---

## 9. Device State

Reported in byte 3 of every heartbeat acknowledgement.

| Value | State |
|---|---|
| `0` | none |
| `1` | idle |
| `2` | initialising |
| `3` | detecting |
| `4` | ready — a ball is in position |
| `5` | shot |
| `6` | done |

---

## 10. Implementation Requirements

Four device behaviours will cause incorrect results if not handled.

**Notifications are sent twice.** Every notification is transmitted twice,
byte-identical. Deduplicate against the immediately preceding payload or every
shot will be counted twice.

**Club metrics can be stale.** The device retains the last shot's club data
across disconnections and will serve it to a freshly connected client. A `0x07`
response must be correlated with a preceding `0x02`, or an old shot will be
re-reported on reconnect.

**A zero-length notification means disconnect.** Treat it as link teardown.

**The device is also a GATT client.** After connecting it issues ATT *requests
to the host* — an Exchange MTU Request, service and characteristic discovery,
and reads of the host's Device Name and Appearance. If these go unanswered, the
device terminates the connection at the ATT transaction timeout (30 seconds),
which presents as a reproducible disconnect roughly 33 seconds after connecting.
Any ordinary operating-system GATT stack answers them automatically; this only
requires attention in implementations that bypass one, such as a raw L2CAP
socket.
