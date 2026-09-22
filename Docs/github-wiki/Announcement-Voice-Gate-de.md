<!--
SPDX-FileCopyrightText: 2026 Chris YO3TCO / Nexus-BS Project
SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
-->

# Ankündigungs-TG (Live-Audio-Sprecher)

Sprachen: **Deutsch** | [English](Announcement-Voice-Gate)

Der Ankündigungs-Voicegate lässt die Station auf einer eigenen, abonnierbaren
Gruppentalk-Gruppe „sprechen": Live-Audio aus einem HTTP-Stream (z. B.
Radiosender, Broadcastify-Kanal) wird ins TETRA-Netz eingespeist. Sobald
Audio erkannt wird (VAD), leitet die Station einen Gruppenruf auf der
konfigurierten Gruppe ein und verteilt das Live-Audio wie gewöhnliche
TETRA-Gruppensprache. Der Ruf endet nach anhaltender Stille, Stream-Verlust
oder der maximalen Rufdauer.

## Config

Laufende Config: `/etc/nexus-bs/config.toml`
(Repo-Beispiel: `example_config/config.toml`, Sektion `[announcement]`.)

```toml
[announcement]
enabled = true
gssi = 100              # dedizierte Ankündigungs-Gruppe (muss frei sein!)
issi = 255              # anrufende ISSI, die das Terminal anzeigt
active_stream = "main"

# Optional — Defaults sind eingeblendet
# vad_start_dbfs = -40.0        # Pegel, der eine Ankündigung startet
# vad_stop_dbfs = -50.0         # Hysterese-Pegel, der sie beendet
# vad_start_ms = 150            # Sprachzeit, bis der Ruf startet
# silence_timeout_ms = 4000     # Stille, bis der Ruf endet
# stream_loss_grace_ms = 15000  # Geduld bei stockendem Stream, bis Ruf-Ende
# max_call_duration_secs = 600  # harte Obergrenze pro Ankuendigungsruf

[[announcement.streams]]
label = "main"
url = "https://example.org/stream.mp3"
```

Pflichtfelder: `gssi`, `issi`, `streams` (mind. einer), `active_stream`.
`enabled` ist standardmäßig `false`.

## Stream-Links einfügen

Jeder öffentliche HTTP(S)-Audio-Stream funktioniert. Unterstützte Formate:
MP3, AAC, FLAC, Vorbis/OGG, M4A, WAV/PCM — beliebiges Abtastintervall und
beliebige Kanalzahl, intern wird auf 8 kHz Mono umgerechnet.

### Öffentlicher Radiosender

Die meisten Radiosender veröffentlichen einen Live-Stream-Link auf ihrer
Website (nach „Live" / „Stream" / „Radio hören" suchen). Diese URL (oder
die MP3/AAC-URL dahinter) ist das `url`. Erreichbarkeit vorher prüfen:

```sh
curl -sI "https://.../stream.mp3" | head -5
```

(Erwartet: 200 + `Content-Type: audio/...`.)

### Geschützte Streams (z. B. Broadcastify Premium)

Die echte Stream-URL aus der eingeloggten Browser-Session mitfangen:

1. Einloggen, Kanal öffnen, Play drücken
2. DevTools (`F12`) → Reiter **Network** → Filter **Media**
3. Der neue Request ist der Stream — Rechtsklick → *Kopieren* →
   *Request-Header kopieren*
4. Zwei Fälle:
   - Die URL enthält schon ein Token → direkt als `url` verwenden
   - Ein `Cookie`- (oder `Authorization`-)Header ist nötig → Auth (unten)

```toml
[[announcement.streams]]
label = "premium"
url = "https://.../gemeldete-stream-url"
auth_kind = "header"
auth_name = "Cookie"
auth_value = "<kopierter Cookie-Wert>"
```

Auth-Arten:

| `auth_kind` | Verhalten |
|---|---|
| `bearer` | sendet `Authorization: Bearer <auth_value>` |
| `header` + `auth_name` | sendet `<auth_name>: <auth_value>` (z. B. `Cookie`) |
| `query` + `auth_name` | hängt `?auth_name=<auth_value>` an die URL |

**Achtung:** Tokens/Cookies von Premium-Diensten laufen meist ab. Wenn der
Stream nicht mehr geht: neu mitschneiden, `url`/`auth_value` aktualisieren,
Service neu starten.

## Mehrere Streams

Weitere `[[announcement.streams]]`-Blöcke mit eindeutigem `label` ergänzen.
Der aktive Stream ist `active_stream`. Der Wechsel geht zur Laufzeit ohne
Neustart (siehe Steuerung unten).

## Aktivieren

1. Sektion in die laufende Config eintragen
2. `sudo systemctl restart nexus-bs`
3. Am Terminal: die Gruppe (`gssi`) **abonnieren**

Danach starten die Rufe von allein, sobald der Stream Audio enthält (VAD).

## Steuerung

- **Dashboard:** `http://<bs-ip>:8080` → Seite **Announcement** (State,
  Stream-Label, Start/Stop-Buttons, Stream-Wechsel)
- **HTTP-API:**

```sh
curl -X POST http://<bs-ip>:8080/api/voicegate/start
curl -X POST http://<bs-ip>:8080/api/voicegate/stop
curl -X POST -d '{"label":"premium"}' http://<bs-ip>:8080/api/voicegate/stream
```

`start` umgeht die VAD (sofortiger Ruf), `stop` beendet die aktuelle
Sprachphase.

## Erwartetes Verhalten

- Stream hat Audio → Gruppenruf auf `gssi` von ISSI `issi`, Live-Audio
- Stille länger als `silence_timeout_ms` (Default 4 s) → Ruf endet; neues
  Audio startet einen neuen Ruf
- Stockender/verloren gegangener Stream länger als
  `stream_loss_grace_ms` (Default 15 s) → Ruf endet, der Worker verbindet
  erneut und es geht von vorn los
- Ruf länger als `max_call_duration_secs` (Default 10 min) → Ruf endet
- State ist im Dashboard (Idle/Starting/Speaking/Stopping) und in der
  Telemetrie (`voicegate_state`) sichtbar

## Fehlerbehebung

| Symptom | Prüfen |
|---|---|
| Gar kein Ruf | Terminal abonniert `gssi`? `gssi` frei (keine Kollision mit Trunking/anderen Gruppen)? |
| Ruf kommt, kein Audio | `curl -s <url> \| head -c 200000 > /tmp/a.mp3` — ist das gültiges, durchgehendes Audio? |
| Rufe zu kurz / zu lang / nie | `vad_start_dbfs` / `vad_stop_dbfs` anpassen (leise Streams: beide erhöhen, z. B. `-30` / `-35`) |
| Premium-Stream geht nach einer Weile nicht mehr | Token/Cookie abgelaufen — neu mitschneiden, Config aktualisieren, neu starten |
| Stream erreichbar, aber nichts hörbar | Format unterstützt? (nur MP3/AAC/FLAC/OGG/M4A/WAV) |
