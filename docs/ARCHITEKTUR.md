# Veyra – Architektur

## Ziele

1. **Sicher**: so wenig Angriffsfläche wie möglich.
2. **Sparsam**: nur rechnen, wenn jemand den Desktop sieht – und dann so wenig wie möglich.
3. **Portabel**: Windows zuerst, Linux ohne Neuschreiben ergänzbar.

## Warum Wallpaper Engine schwer und riskant ist – und was Veyra anders macht

| Wallpaper Engine | Veyra |
|---|---|
| Eingebetteter Chromium (CEF) für Web-Wallpaper: ~100–300 MB RAM pro Instanz, großer Angriffsvektor | Kein Browser. Nur Videodateien. |
| JavaScript/„SceneScript“ aus dem Workshop führt fremden Code aus | Kein Skripting, keine Plugins, keine Workshop-Downloads |
| Netzwerkzugriff (Steam, Workshop, Web-Wallpaper) | Kein Netzwerk – einzige, abschaltbare Ausnahmen: Flight Control und DSN Now (feste HTTPS-Adressen, nur lesend) |
| Große Codebasis, viele Drittbibliotheken | ~9.300 Zeilen Rust (inkl. Kommentare/Tests), 11 direkte Abhängigkeiten |
| Rendert oft dauerhaft mit voller Bildrate | Zeichnet nur, wenn ein neues Videobild fällig ist |

## Aufbau

```
                ┌────────────────────────── Haupt-Thread ───────────────────────────┐
config.toml ──► │ App (app.rs)                                                       │
                │  ├─ Nachrichtenschleife: schläft per MsgWaitForMultipleObjects     │
                │  ├─ Pause-Logik (alle 500 ms)                                      │
                │  ├─ Gpu (render.rs, wgpu/DX12) ── 1 Gerät, Video- + Widget-Pipeline│
                │  │    └─ Target pro Monitor (Fenster-Oberfläche)                   │
                │  ├─ Widgets (overlay/): Text → Bild, max. 1×/s, nur bei Änderung   │
                │  └─ Player pro Videodatei (video/mod.rs: Uhr + Bildauswahl)        │
                └──────────▲───────────────────────────────▲─────────────────────────┘
                           │ NV12-Bilder (max. 3 gepuffert) │ Pegel bass/mid/high/level
                ┌──────────┴──────────┐         ┌──────────┴──────────┐
                │ Dekoder-Thread       │         │ Audio-Thread         │
                │ video/mf.rs          │         │ audio/wasapi.rs      │
                │ Media Foundation +   │         │ WASAPI-Loopback +    │
                │ DXVA (Hardware)      │         │ FFT (rustfft)        │
                └─────────────────────┘         └─────────────────────┘
```

Das **Einstellungsfenster** (`veyra.exe --settings`, `settings.rs`) ist ein eigener, kurzlebiger Prozess mit egui
(auf demselben wgpu). Es spricht nicht mit dem Hintergrund-Prozess, sondern schreibt nur `config.toml`
(atomar über eine temporäre Datei); der Hintergrund-Prozess prüft die Datei jede Sekunde.
Vorteile: kein IPC, keine Kosten solange das Fenster zu ist, und ein Absturz der Oberfläche lässt das Wallpaper weiterlaufen.

Beim Neuladen der Konfiguration werden Fenster und Oberflächen **nur dann** neu angelegt, wenn sich ändert,
welcher Monitor welches Video zeigt. Darstellung, Effekte, Pausen-Regeln oder Autostart werden direkt übernommen.

| Modul | Aufgabe | plattformabhängig? |
|---|---|---|
| `config.rs` | TOML laden/prüfen/kommentiert schreiben, Monitor-Zuordnung, Video-Bibliothek | nein |
| `app.rs` | Lebenszyklus, Pause-Entscheidung, Bildtakt | nein (nutzt `platform::sys`) |
| `render.rs` + `shader.wgsl` | NV12→RGB, Skalierung, Effekte in **einem** Shader-Durchlauf | nein (wgpu) |
| `video/mod.rs` | Wiedergabe-Uhr, Bildauswahl, Puffer-Recycling | nein |
| `video/mf.rs` | Dekoder | **ja** (Windows) |
| `audio/mod.rs` | FFT-Analyse, Auto-Gain, Glättung | nein |
| `audio/scope.rs` | Messdaten für den Signal Analyzer: Stereo-Samples, 48-Band-Spektrum in dBFS, Spitzenwerte | nein |
| `audio/wasapi.rs` | System-Loopback | **ja** (Windows) |
| `platform/windows.rs` | Desktop-Einbettung + Wächter, Monitore, Tray, Autostart, Pause-Signale, Dateidialoge | **ja** |
| `settings.rs` | Einstellungsfenster (egui), Vorschaubilder über `video/mf.rs` | **ja** (Windows) |
| `overlay/mod.rs` + `overlay.wgsl` | Widgets: Inhalt bestimmen, platzieren (Arbeitsbereich ohne Taskleiste), pixelgenau überblenden | nein |
| `overlay/canvas.rs` | Zeichenfläche: Ebenen Hintergrund/Vordergrund/Leuchten, kantengeglättete Formen, Schatten | nein |
| `overlay/text.rs` | Orbitron (eingebettet) mit `ab_glyph` setzen, gleich breite Ziffern | nein |
| `overlay/flight.rs` | Mission-Control-Modul: Layout, Raketen-Scan (Kantenbild), Patch-Ring, Countdown-Phasen | nein |
| `overlay/launch.rs` | Flight Control: Launch Library 2 abfragen, Bilder laden, zwischenspeichern, nächsten Start wählen | nein (HTTPS und Bild-Dekodierung über `platform::sys`) |
| `overlay/hud.rs` | Gemeinsame HUD-Bausteine: Kopfzeile mit Status-Lampen, Datenzeilen | nein |
| `overlay/astro.rs` | Julianisches Datum, Sternzeit (IAU 1982), Mars-Zeit nach Allison & McEwen (Mars24) | nein |
| `overlay/spaceclock.rs` | Deep Space Clock: Layout, Zifferblatt Erdtag/Marssol | nein |
| `overlay/analyzer.rs` | Signal Analyzer: Rahmen/Raster (1×/s) und Phosphor-Schirm (pro Bild, mit Nachleuchten) | nein |
| `overlay/dsn.rs` | DSN Now: Antennenstatus abfragen, eigener Mini-XML-Leser, Kürzel → Sondennamen | nein (HTTPS über `platform::sys`) |
| `overlay/dsnnow.rs` | DSN Now: Tafel mit Link-Richtung, Datenrate, Laufzeit, logarithmischer Entfernungsskala | nein |

**Widgets** kosten fast nichts: Der Inhalt wird höchstens einmal pro Sekunde bestimmt und nur bei einer Änderung
neu gerastert (Uhr ohne Sekunden: einmal pro Minute). Das fertige Bild liegt als kleine Textur auf der GPU und wird
im selben Render-Durchlauf wie das Video mit einem Rechteck überblendet. Pausierte Monitore aktualisieren nichts.
Der Countdown-Effekt (Aufleuchten in den letzten 10 Sekunden, Blitz bei T-0) steckt in einer zweiten, gleich großen
Textur; der Shader mischt sie mit einer Stärke dazu, die pro Bild berechnet wird – nur dann wird jedes Bild gezeichnet.

Der **Signal Analyzer** ist das einzige Widget mit Bewegung in jedem Bild – und nutzt dafür dieselbe Leucht-Ebene:
Rahmen, Raster und Beschriftung entstehen wie üblich höchstens einmal pro Sekunde; die Leuchtspur rechnet
`Phosphor` pro Bild auf der CPU (Strahl mit gleicher Lichtmenge pro Sample, exponentielles Nachleuchten,
weicher Schein) und lädt nur den Ausschnitt des Schirms in die Leucht-Textur (`Gpu::overlay_glow`). Als reines
Licht addiert sie sich zum Video. Die Daten liefert der Audio-Thread (`audio/scope.rs`) alle ~16 ms; er läuft nur,
solange ein sichtbarer Monitor einen Audio-Effekt oder den Analyzer zeigt. Ist nichts zu hören und die Spur
verblasst, wird wieder nur bei neuen Videobildern gezeichnet.

## Desktop-Einbettung (Windows)

Das Wallpaper ist ein normales Kindfenster *hinter* den Desktop-Symbolen:

- **Windows 10 / Windows 11 bis 23H2**: Nachricht `0x052C` an `Progman` lässt Explorer die Symbole in ein
  eigenes `WorkerW` verschieben; das *nächste* `WorkerW` ist die Hintergrundebene → dort wird das Fenster eingehängt.
- **Windows 11 24H2+ („raised desktop“)**: Symbole (`SHELLDLL_DefView`) und `WorkerW` sind Kinder von `Progman`.
  Veyra hängt sich als *Layered*-Kindfenster (Alpha 255) an `Progman` und sortiert sich direkt unter die Symbole –
  dasselbe Verfahren, das Lively Wallpaper nutzt.
- Explorer-Neustart (`TaskbarCreated`) und Monitoränderungen (`WM_DISPLAYCHANGE`) bauen alles automatisch neu auf.
- Beim Beenden wird das normale Hintergrundbild neu gezeichnet.

Das Verfahren ist undokumentiert; es ist deshalb komplett in `DesktopHost` gekapselt.

**Wächter**: Alle 500 ms prüft `DesktopHost::check()`, ob Elternfenster und Symbol-Ebene noch dieselben sind und
kein `WorkerW` (statisches Hintergrundbild) zwischen Symbolen und Wallpaper gerutscht ist. Eine verschobene
Reihenfolge wird sofort korrigiert, eine verlorene Einbettung führt zum Neuaufbau.
**Autostart** startet mit `--autostart`: Veyra wartet dann, bis Explorer Taskleiste und Desktop aufgebaut hat,
bevor es sich einbettet – direkt nach der Anmeldung baut Explorer die Ebenen sonst noch einmal um.

## Performance-Strategie

1. **Hardware-Dekodierung** (DXVA über ein eigenes D3D11-Gerät auf derselben GPU wie die Ausgabe). Die CPU dekodiert nichts.
2. **Zero-Copy**: Nutzt der Renderer DX12, kopiert der Dekoder jedes Bild GPU-intern in einen Ring aus 6 geteilten
   NV12-Texturen (`SharedRing`), die wgpu direkt als Y-/UV-Ebene liest. Zwei geteilte Fences synchronisieren:
   *ready* (Dekoder → Renderer: Bild fertig) und *released* (Renderer → Dekoder: Slot darf überschrieben werden).
   Die Pixel verlassen nie den Grafikspeicher; skaliert wird beim Zeichnen im Shader.
   Rückfall (Vulkan, andere GPU, Fehler beim Öffnen): Bild in den Arbeitsspeicher kopieren und hochladen – dann
   verkleinert der Videoprozessor große Videos vorher auf Monitorgröße (4K → 1080p = ¼ der Daten).
3. **NV12 statt RGBA**: 1,5 statt 4 Byte pro Pixel; Farbumrechnung im Shader (praktisch kostenlos).
4. **Nur zeichnen, wenn nötig**: ein Monitor wird nur neu gezeichnet, wenn ein neues Videobild fällig ist
   (30-fps-Video = 30 Zeichnungen/s, auch auf einem 144-Hz-Monitor) oder ein Audio-Effekt bei hörbarem Ton läuft.
5. **Geteilte Dekoder**: zeigen 3 Monitore dasselbe Video, wird es einmal dekodiert und einmal hochgeladen.
6. **Gegendruck statt Polling**: Pausiert → Bild-Warteschlange voll → Dekoder-Thread blockiert → 0 % CPU.
   Der Haupt-Thread schläft dann 500 ms am Stück; Audio-Mitschnitt wird komplett gestoppt.
   Pausiert wird u. a. auch, wenn der Bildschirm ausgeschaltet ist (`GUID_CONSOLE_DISPLAY_STATE`).
7. **Puffer-/Slot-Recycling**: Bildpuffer wandern zurück zum Dekoder, keine Allokation pro Bild.
8. **`gpu = "low"`**: bevorzugt die integrierte GPU, die dedizierte kann schlafen.
9. **Kleine Warteschlangen**: max. 3 Bilder vorgepuffert, `desired_maximum_frame_latency = 1`.

### Bekannte Optimierungsreserve (Roadmap)

- **Präsentieren**: Jeder Monitor wird einzeln übergeben (bei 3 Monitoren × 60 fps = 180 Übergaben/s); das ist
  inzwischen der größte CPU-Anteil. Ein gemeinsamer Submit für alle Monitore würde ihn verringern.
- **GPU-Wahl**: Hängen die Monitore an der dedizierten GPU, ist `gpu = "high"` trotz des Namens oft sparsamer
  (sonst kopiert Windows jedes Bild zwischen den Grafikkarten). Ein `gpu = "auto"` nach Monitor-Anschluss wäre sinnvoll.
- Bildrate des Videos auf `max_fps` drosseln, *bevor* dekodiert wird (heute wird jedes Bild dekodiert,
  überzählige werden verworfen). Am einfachsten: Video passend kodieren (siehe README).
- Mehrere Monitore präsentieren nacheinander mit VSync; bei sehr unterschiedlichen Bildwiederholraten
  wäre ein Render-Thread pro Monitor sauberer.

## Sicherheitsmodell

- **Keine Codeausführung aus Inhalten**: Es werden ausschließlich Videodateien gelesen. Kein HTML, JS, Lua, Shader-Upload.
- **Parser = Windows-Komponente**: Container/Codecs parst Media Foundation, das Microsoft per Windows Update patcht.
  Veyra selbst enthält keinen Medienparser.
- **Kein Netzwerk**: weder Updates noch Telemetrie noch Downloads. `MFSTARTUP_NOSOCKET` verhindert zusätzlich,
  dass Media Foundation Netzwerkquellen öffnet.
  **Einzige Ausnahmen: Flight Control und DSN Now** (beide standardmäßig aus). Nur solange Flight Control aktiv ist, läuft ein Thread, der
  alle 15 Minuten (in der Stunde vor einem Start alle 6 Minuten) die Liste der nächsten Starts von
  `https://ll.thespacedevs.com/2.3.0/…` abfragt, einmal pro neuem Start dessen Mission-Patch, und Fotos/Patches der
  nächsten zwei Starts aus der Bildablage `https://thespacedevs-prod.nyc3.digitaloceanspaces.com/media/…` lädt.
  Andere Adressen werden abgelehnt, auch wenn sie in den Daten stehen.
  Umsetzung über WinHTTP aus Windows (Systemproxy, Zertifikatsprüfung von Windows, keine eigene TLS-Bibliothek),
  nur `GET`, Antworten auf 4 MB (JSON) bzw. 8 MB (Bild) begrenzt. JSON wird nur in Textfelder übernommen – nie
  ausgeführt. Bilder müssen JPEG oder PNG sein (Signatur geprüft) und werden ausschließlich mit den eingebauten
  Decodern der Windows Imaging Component gelesen – wie bei den Videos parst also eine von Windows Update gepflegte
  Komponente, nachinstallierte Codecs kommen nie zum Zug. Zwischenspeicher: `%APPDATA%\Veyra\cache` (nach 14 Tagen
  ohne Gebrauch gelöscht). Gesendet wird nur die Anfrage mit `User-Agent: Veyra/<Version>`.
  **DSN Now** fragt, solange es aktiv ist, einmal pro Minute `https://eyes.nasa.gov/dsn/data/dsn.xml` ab (Antwort auf
  1 MB begrenzt). Gelesen werden nur Elemente und Attribute mit einem eigenen, minimalen Leser – keine DTDs, keine
  Entitäten, keine externen Verweise; die Werte landen ausschließlich in Text- und Zahlenfeldern.
  Prüfbar: `grep -rn "https_get" src/` – aufgerufen wird es nur in `overlay/launch.rs` und `overlay/dsn.rs`.
- **Keine Adminrechte**, schreibt nur nach `%APPDATA%\Veyra`, `HKCU\…\Run` (Autostart) und – nur wenn in den
  Einstellungen aktiviert – eine Verknüpfung ins eigene Startmenü (`%APPDATA%\Microsoft\Windows\Start Menu\Programs`).
- **Speichersicher**: Rust; `unsafe` nur an den Grenzen zu Win32/COM, jeweils gekapselt.
- **Wenige Abhängigkeiten**: `wgpu`, `windows`, `rustfft`, `serde`, `toml`, `anyhow`, `log`, `pollster`,
  `ab_glyph` (Schrift-Rasterung, reines Rust), `serde_json` (nur Flight Control) –
  plus `eframe`/egui (nur für das Einstellungsfenster, ohne Netzwerk-/Link-Features).
  Alle weit verbreitet und aktiv gepflegt. `cargo audit` regelmäßig ausführen.
- **Audio bleibt lokal**: Der Loopback-Pegel wird nur in 4 Zahlen zerlegt; nichts wird gespeichert oder versendet.
  Ist kein Effekt aktiv, wird gar nicht mitgeschnitten.
- **Strenge Konfiguration**: unbekannte Schlüssel werden abgelehnt, Werte begrenzt; bei Fehlern läuft die alte Konfiguration weiter.

Mögliche Härtung später: Dekoder in einen separaten Prozess mit *Low Integrity Level* / AppContainer auslagern,
sodass selbst eine Codec-Lücke keinen Zugriff auf Benutzerdaten hätte.

## Linux-Pfad

Plattformunabhängig (läuft unverändert): Konfiguration, Renderer (wgpu → Vulkan), Shader, Wiedergabe-Uhr, Audio-Analyse,
Widgets (nur Ortszeit und HTTPS-Abruf kommen aus `platform::sys`).

Neu zu schreiben – mit derselben Schnittstelle wie `platform/windows.rs`:

| Baustein | Linux-Umsetzung |
|---|---|
| Hintergrundfenster | **Wayland**: `wlr-layer-shell`, Ebene `background` (KDE, Sway, Hyprland; GNOME braucht eine Extension). **X11**: Fenster mit `_NET_WM_WINDOW_TYPE_DESKTOP` |
| Dekoder | GStreamer mit VA-API (`vah264dec` …) → NV12, oder `ffmpeg` + VA-API |
| Audio-Loopback | PipeWire/PulseAudio-Monitor-Quelle (`*.monitor`) |
| Tray | StatusNotifierItem (D-Bus), z. B. Crate `ksni` |
| Autostart | `~/.config/autostart/veyra.desktop` |
| Pause-Signale | Vollbild über Compositor-Protokolle (eingeschränkt), Akku über UPower (D-Bus), Sperre über logind |

## Roadmap

1. **v0.1**: Video, Multi-Monitor, Auto-Pause, Tray, Autostart, Audio-Effekte.
2. **v0.2**: Praxistest auf Win10/Win11 23H2/24H2, Fehlerkorrekturen; signierte Release-Builds.
   *(Erledigt: Einstellungsfenster mit Monitor-Übersicht und Video-Bibliothek, Pause bei ausgeschaltetem Bildschirm,
   Wächter für die Desktop-Einbettung, Release-Builds per GitHub Actions.)* *(Erledigt: eigenes Programm- und Tray-Symbol + Versionsinfo, von `build.rs`
   ohne `rc.exe` als `.res` eingebettet; Quellen und Generator in `assets/`.)*
3. **v0.3 (dieser Stand)**: Zero-Copy-Pfad *(erledigt)*; Widgets Uhr und Flight Control (Mission-Control-Modul mit
   Countdown-Effekten) *(erledigt)*; Deep Space Clock, Signal Analyzer und DSN Now *(erledigt)*;
   offen: `gpu = "auto"` nach Monitor-Anschluss.
4. **v0.4**: Linux (Wayland zuerst).
