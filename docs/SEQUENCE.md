# Connection Lifecycle and Message Sequencing

How a client connects to a Square Golf Omni, arms it, and receives shots.
Message and packet formats are specified in [WIRE.md](WIRE.md).

---

## 1. Overview

Five phases:

```
1. Discovery      scan for the SquareGolf name prefix
2. Connection     GATT connect, no pairing, subscribe to EVT
3. Session init   query, display settings, heartbeat
4. Arming         select club, enable ball detection
5. Shot flow      sensor → ball metrics → request club → club metrics
```

Phases 1–4 run once. Phase 5 repeats for every shot; the device stays armed.

There is no handshake to negotiate, no capability exchange, and no
authentication. A client that subscribes to notifications and sends heartbeats
is a functioning client.

---

## 2. Discovery

Scan for a device whose advertised name begins with `SquareGolf`. The full name
is `SquareGolf(XXXX)` where `XXXX` is the last two bytes of the BLE address.

The advertisement also carries manufacturer data under company ID `0xFFFF`
containing an ASCII model code (e.g. `0300A`). Do not filter on the company ID.

**Stop scanning before connecting.** Connecting while discovery is active is
unreliable on some stacks. A short settle delay (~250 ms) after stopping the
scan avoids it.

---

## 3. Connection

Connect as a plain GATT client. **Do not pair or bond** — the device supports
neither, and operating-system pairing dialogs will fail.

```
connect
  → discover services
  → enable notifications on EVT (86602102)
  → read firmware / hardware / device ID   (optional)
```

A ~250 ms pause between connecting and discovering services, and again before
subscribing, improves reliability. If service discovery completes but returns no
characteristics, retry it — up to four attempts is sufficient in practice.

### The host must answer the device

After connecting, the Omni acts as a **GATT client** against the host: it issues
an Exchange MTU Request, discovers the host's services and characteristics, and
reads the host's Device Name and Appearance.

If those requests go unanswered, the device terminates the link once the ATT
transaction timeout expires — a reproducible disconnect about 33 seconds after
connecting, immune to heartbeats or any other client activity.

Ordinary GATT stacks answer these automatically, so most clients need do
nothing. Implementations that bypass the system stack (for example a raw L2CAP
socket) must respond themselves; an ATT Error Response is sufficient, since it
completes the transaction.

### Platform notes

- **Windows / macOS** — no special handling. WinRT answers the device's requests
  and negotiates a larger MTU automatically.
- **Linux / BlueZ** — set the device's `Trusted` property before connecting.
  An untrusted, unpaired device is treated as temporary: service discovery never
  completes, the link drops after a few seconds, and the device object is
  removed. `Trusted` is not pairing and involves no key exchange. Additionally,
  BlueZ does not return a D-Bus reply to `Connect()` for this device — dispatch
  it without waiting for one and poll `ServicesResolved` instead. BlueZ also
  does not expose the GAP service, so the model must be read from the advertised
  manufacturer data rather than the GAP name characteristic.

---

## 4. Session Initialisation

```
0x86  Query                 → 0x06 response
0x88  SetUnits
0x8a  SetCarryAdjustment
0x89  SetGreenSpeed
0x83  Heartbeat             → 0x03 acknowledgement, every 5 s thereafter
```

The three setters configure the device's built-in display only and do not change
the wire format. They may be omitted entirely.

Send a heartbeat every 5 seconds for the life of the session. Each is answered
by a `0x03` carrying the current device state.

---

## 5. Arming

```
0x82  ClubSelect  {club_number} {category} {handed}
0x81  DetectBall  mode=0x01, spin=0x11
```

Select the club before enabling detection. The device uses the selected club to
classify the shot, so keep it in sync if the player changes clubs mid-session —
send `0x82` again at any time.

There is no separate putting or chipping mode. **Selecting the putter is
putting mode.**

**Arm once.** The device remains armed across shots and re-arms itself. Sending
`0x81` after every shot is harmless but unnecessary.

Once armed, `0x01` sensor notifications stream continuously with ball detection
state and position.

---

## 6. Shot Flow

```
device                                    host
  │
  │  0x01  detected=1, ready=0                    ball placed
  │─────────────────────────────────────────────▶
  │  0x01  detected=1, ready=1                    ready to hit
  │─────────────────────────────────────────────▶
  │  0x03  state=ready
  │─────────────────────────────────────────────▶
  │
  │              [ player hits ]
  │
  │  0x02  ball metrics
  │─────────────────────────────────────────────▶
  │                                    0x87  RequestClubMetrics
  │◀─────────────────────────────────────────────
  │  0x07  club metrics (or all-sentinel)
  │─────────────────────────────────────────────▶
  │  0x03  state=done / none
  │─────────────────────────────────────────────▶
```

Ball metrics are **pushed**; club metrics are **pulled**. A client that never
sends `0x87` never receives club data.

Every notification arrives **twice**, byte-identical. Deduplicate.

A shot the device could not track fully still produces a `0x02`, followed by a
`0x07` whose mask is `0x00` and whose fields are all sentinels. Shots the device
declines entirely produce no `0x02` at all.

### Correlating club data

The device retains the last shot's club metrics across disconnections. A `0x07`
that does not follow a `0x02` in the current session describes an **older shot**
and must not be reported as new. Track whether a `0x87` was issued for a ball
packet just seen, and discard unsolicited club responses.

---

## 7. Disconnection

Send `0x81` with mode `0x00` to stop detection before disconnecting. This is
courtesy, not a requirement.

A zero-length notification signals that the device is tearing the link down.

On reconnect, re-run phases 2–4. Re-select the club — the device does not
persist the client's selection in a way a new session can rely on.

---

## 8. Minimum Viable Implementation

To receive ball data only:

1. Scan for `SquareGolf`, stop scanning, connect
2. Subscribe to EVT notifications
3. Send `0x82` ClubSelect, then `0x81` DetectBall on
4. Send `0x83` Heartbeat every 5 seconds
5. Deduplicate notifications; parse `0x02`

That is roughly a dozen messages. Add step 6 for club data:

6. On each `0x02`, send `0x87` and parse the following `0x07`

Everything else — display settings, the clock, alignment, the query — is
optional.
