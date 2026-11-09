<div align="center">

<img src="assets/veyra-logo.png" alt="Veyra" width="440">

**Motion for your universe.**

Schlanke, sichere Video-Hintergründe für den Windows-Desktop.<br>
Kein Browser, keine Skripte, kein Netzwerk – nur dein Video, hardwarebeschleunigt hinter deinen Desktop-Symbolen.

![Plattform](https://img.shields.io/badge/Plattform-Windows%2010%20%7C%2011-0078D6?logo=windows)
![Rust](https://img.shields.io/badge/Rust-2021-000000?logo=rust)
![Lizenz](https://img.shields.io/badge/Lizenz-MIT-green)
![Status](https://img.shields.io/badge/Status-MVP-orange)

</div>

---

## Features

- 🎬 **Video-Wallpaper** – MP4/H.264, H.265/HEVC, VP9, AV1 (alles, was Windows' Media Foundation kann), in Endlosschleife
- ⚡ **Hardware-Dekodierung** über die GPU – Ton wird nie dekodiert
- 🖥️ **Multi-Monitor** – pro Bildschirm ein eigenes Video oder ein Video für alle (wird dann nur einmal dekodiert)
- ⏸️ **Auto-Pause** bei Vollbild-Spielen/-Videos, maximierten Fenstern (optional), Akkubetrieb und Sperrbildschirm
- 🔊 **Audio-reaktiv** – Effekte `pulse`, `glow` und `chroma` reagieren auf den Systemsound (WASAPI-Loopback)
- 🧭 **Tray-Icon** mit Menü (passt sich heller/dunkler Taskleiste an) und **Autostart** mit Windows
- 🔒 **Sicher by Design** – kein Browser, keine Skripte, kein Netzwerk, keine Adminrechte
- 🪶 **Eine einzige EXE** – keine DLLs, keine Installation

Linux ist architektonisch vorbereitet (siehe [docs/ARCHITEKTUR.md](docs/ARCHITEKTUR.md)).

## Warum Veyra?

Viele Wallpaper-Programme bringen einen kompletten Browser, Skript-Engines und Online-Workshops mit –
das kostet RAM, CPU und vergrößert die Angriffsfläche. Veyra macht genau eine Sache:
Videos effizient als Hintergrund abspielen.

| | Veyra |
|---|---|
| Laufzeit | Kein Browser, nur Videodateien |
| Fremder Code | Kein Skripting, keine Plugins, keine Downloads |
| Netzwerk | Kein einziger Netzwerkaufruf im Code |
| Codebasis | ~2.500 Zeilen Rust, 8 direkte Abhängigkeiten |
| Rechenlast | Zeichnet nur, wenn ein neues Videobild fällig ist – und pausiert, wenn niemand hinsieht |

## Bauen (Windows)

1. Rust installieren: <https://rustup.rs> (Standard-Toolchain `x86_64-pc-windows-msvc`).
   Dafür werden die *Visual Studio Build Tools* mit „Desktopentwicklung mit C++“ benötigt.
2. Repository klonen und bauen:

   ```powershell
   git clone https://github.com/w3ssy/veyra.git
   cd veyra
   cargo build --release
   ```

3. Die fertige Datei liegt unter `target\release\veyra.exe` (eine einzige EXE, keine DLLs).

Zum Entwickeln mit Konsolenausgabe: `cargo run` (Debug-Build zeigt Logs im Terminal).
Ausführlichere Logs: Umgebungsvariable `VEYRA_LOG=debug` setzen.

Logo oder Symbol geändert? Die Icons in `assets/` neu erzeugen mit `python assets/make_icons.py`
(benötigt Pillow, NumPy, SciPy).

## Benutzen

1. `veyra.exe` starten – es erscheint ein Symbol im Infobereich der Taskleiste.
2. Rechtsklick → **Konfiguration bearbeiten…** öffnet `%APPDATA%\Veyra\config.toml`.
3. Bei `video = '…'` den Pfad zu deinem Video eintragen und speichern – die Änderung wird nach ~2 s automatisch übernommen.

```toml
[[wallpaper]]
monitor = "all"
video = 'C:\Users\<name>\Videos\regen.mp4'
fit = "cover"
effect = "pulse"
intensity = 1.0

[[wallpaper]]            # Monitor 2 bekommt ein eigenes Video
monitor = 2
video = 'D:\Wallpaper\stadt-nacht.mp4'
effect = "none"
```

Monitore werden **von links nach rechts** ab 1 gezählt; die Zuordnung steht beim Start im Log
(Tray → **Ordner mit Log öffnen** → `veyra.log`). Doppelklick aufs Tray-Symbol pausiert/setzt fort.

> **Hinweis:** Veyra liefert keine Videos mit. Verwende nur Videos, an denen du die nötigen Rechte hast.

### Tipps für minimale Last

- Videos in der **Auflösung des Monitors** und mit **30 fps** kodieren – das spart mehr als jede Einstellung.
  Beispiel mit ffmpeg: `ffmpeg -i in.mp4 -an -vf scale=2560:-2,fps=30 -c:v libx264 -crf 20 out.mp4`
  (`-an` entfernt die Tonspur).
- H.264 oder HEVC werden praktisch überall in Hardware dekodiert; AV1 nur auf neueren GPUs.
- `max_fps = 30` und `gpu = "low"` (integrierte GPU) sind auf Laptops die sparsamste Wahl.

## Sicherheit prüfen

```powershell
cargo install cargo-audit
cargo audit          # bekannte Schwachstellen in Abhängigkeiten
```

Siehe Abschnitt „Sicherheitsmodell“ in [docs/ARCHITEKTUR.md](docs/ARCHITEKTUR.md).

## Stand

Veyra ist ein **MVP**. Der Code ist gegen das Windows-Ziel (`x86_64-pc-windows-msvc`) gebaut und
Clippy-sauber, die plattformunabhängigen Teile sind mit Unit-Tests abgedeckt.
Je nach Windows-Version und Grafiktreiber kann es noch Ecken geben – v. a. bei der Desktop-Einbettung
(Windows 11 24H2+) und der NV12-Pufferaufteilung einzelner Treiber.
Das Log (`%APPDATA%\Veyra\veyra.log`) zeigt, wo es hakt – Issues mit Log-Auszug sind willkommen.

## Mitmachen

Bug-Reports, Ideen und Pull Requests sind willkommen! Bitte vor einem PR `cargo clippy` und `cargo test` ausführen.

## Lizenz

[MIT](LICENSE) © w3ssy
