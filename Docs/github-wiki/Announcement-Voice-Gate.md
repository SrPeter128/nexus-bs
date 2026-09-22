<!--
SPDX-FileCopyrightText: 2026 Chris YO3TCO / Nexus-BS Project
SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
-->

# Announcement Voice Gate (Live Audio Talk Group)

Languages: **English** | [Deutsch](Announcement-Voice-Gate-de)

The announcement voice gate makes the base station "talk" on a dedicated
subscribable talk group. It plays live external audio from an HTTP stream
(e.g. a radio stream, a Broadcastify channel) into the TETRA network: when
audio is detected (VAD), the BS originates a group call on the configured
talk group and streams the live audio as ordinary TETRA group speech. The
call ends after sustained silence, stream loss, or the maximum call duration.

## Config

Live config: `/etc/nexus-bs/config.toml`
(Repo example: `example_config/config.toml`, `[announcement]` section.)

```toml
[announcement]
enabled = true
gssi = 100              # dedicated announcement talk group (must be free!)
issi = 255              # calling party ISSI shown on subscriber terminals
active_stream = "main"

# Optional — defaults shown
# vad_start_dbfs = -40.0        # RMS level that starts an announcement
# vad_stop_dbfs = -50.0         # hysteresis level that ends it
# vad_start_ms = 150            # voiced time before the call starts
# silence_timeout_ms = 4000     # silence before the call ends
# stream_loss_grace_ms = 15000  # stalled stream tolerated before call end
# max_call_duration_secs = 600  # hard cap per announcement call

[[announcement.streams]]
label = "main"
url = "https://example.org/stream.mp3"
```

Required fields: `gssi`, `issi`, `streams` (at least one), `active_stream`.
`enabled` defaults to `false`.

## Adding Stream Links

Any public HTTP(S) audio stream works. Supported formats: MP3, AAC, FLAC,
Vorbis/OGG, M4A, WAV/PCM — any sample rate or channel layout, resampled to
8 kHz mono internally.

### Public radio stream

Most radio stations publish a live stream link on their website (look for
"Live" / "Stream" / "Radio hören"). That URL (or the MP3/AAC URL behind it)
is the `url`. Check reachability first:

```sh
curl -sI "https://.../stream.mp3" | head -5
```

(Expect 200 + `Content-Type: audio/...`.)

### Protected streams (e.g. Broadcastify Premium)

Capture the real stream URL from a logged-in browser session:

1. Log in, open the channel, press play
2. Dev tools (`F12`) → **Network** tab → filter **Media**
3. The new request is the stream — right-click it → *Copy* →
   *Copy request headers*
4. Two cases:
   - The URL already contains a token → use it directly as `url`
   - A `Cookie` (or `Authorization`) header is required → use auth (below)

```toml
[[announcement.streams]]
label = "premium"
url = "https://.../captured-stream-url"
auth_kind = "header"
auth_name = "Cookie"
auth_value = "<copied cookie value>"
```

Auth kinds:

| `auth_kind` | Behaviour |
|---|---|
| `bearer` | sends `Authorization: Bearer <auth_value>` |
| `header` + `auth_name` | sends `<auth_name>: <auth_value>` (e.g. `Cookie`) |
| `query` + `auth_name` | appends `?auth_name=<auth_value>` to the URL |
| `basic` + `auth_username` + `auth_password` | HTTP Basic auth (e.g. Broadcastify) |

**Broadcastify example** (premium account, feed `32602`):

```toml
[[announcement.streams]]
label = "dispatch"
url = "https://audio.broadcastify.com/32602.mp3"
auth_kind = "basic"
auth_username = "<your-broadcastify-user>"
auth_password = "<your-broadcastify-password>"
```

The static audio URL per feed is `https://audio.broadcastify.com/<feed-id>.mp3`
(the feed page shows it under *Static Audio URL* and requires a premium
account to play).

**Note:** tokens/cookies of premium services usually expire. When the stream
stops working, capture again, update `url`/`auth_value`, and restart the
service.

## Multiple Streams

Add more `[[announcement.streams]]` blocks with unique `label`s. The active
one is `active_stream`. You can switch at runtime without restart (see
Control below).

## Activate

1. Add the section to the live config
2. `sudo systemctl restart nexus-bs`
3. On the subscriber terminal: **subscribe** to the talk group (`gssi`)

Calls then start by themselves as soon as the stream carries audio (VAD).

## Control

- **Dashboard:** `http://<bs-ip>:8080` → **Announcement** page (state,
  stream label, Start/Stop buttons, stream switch)
- **HTTP API:**

```sh
curl -X POST http://<bs-ip>:8080/api/voicegate/start
curl -X POST http://<bs-ip>:8080/api/voicegate/stop
curl -X POST -d '{"label":"premium"}' http://<bs-ip>:8080/api/voicegate/stream
```

`start` bypasses VAD (immediate call), `stop` ends the current speech period.

## Expected Behaviour

- Stream carries audio → group call on `gssi` from ISSI `issi`, live audio
- Silence longer than `silence_timeout_ms` (default 4 s) → call ends; new
  audio starts a new call
- Stalled/lost stream longer than `stream_loss_grace_ms` (default 15 s) →
  call ends, the worker reconnects and tries again
- Call longer than `max_call_duration_secs` (default 10 min) → call ends
- State is visible in the dashboard (Idle/Starting/Speaking/Stopping) and in
  telemetry (`voicegate_state`)

## Troubleshooting

| Symptom | Check |
|---|---|
| No call at all | Terminal subscribed to `gssi`? `gssi` free (no collision with trunking/other groups)? |
| Call rings but no audio | `curl -s <url> \| head -c 200000 > /tmp/a.mp3` — is it valid, continuous audio? |
| Calls too short / too long / never start | Adjust `vad_start_dbfs` / `vad_stop_dbfs` (quiet streams: raise both, e.g. `-30` / `-35`) |
| Premium stream dies after a while | Token/cookie expired — re-capture, update config, restart |
| Stream reachable but nothing plays | Format supported? (MP3/AAC/FLAC/OGG/M4A/WAV only) |
