# TextCutForge

Videoschnitt in Rust, bei dem du **Text statt Video schneidest**: Jeder Clip wird
transkribiert und wie ein Textdokument dargestellt. Löschst du Wörter, wird das Video
an genau diesen Stellen geschnitten. **Stille wird automatisch erkannt und entfernt.**

## Zwei Modi

Beim Start wählst du den Projekttyp (später über *Datei → Neues Projekt …*):

- **🎬 Video-Projekt:** alles unten Beschriebene, inklusive Bild-in-Bild und Export in mehreren Größen.
  Zusätzlich lässt sich die Tonspur der Timeline als Audiodatei exportieren.
- **🎧 Audio-Projekt** für Podcasts, Interviews und Voiceover: nur Tonspuren, die Hauptspur A1 ist magnetisch,
  Schnitte rastern auf 10 ms. Videodateien liefern ihren Ton. Statt der Videovorschau zeigt ein
  Player den gerade gesprochenen Satz.

## Funktionen

**Hauptfunktionen**
- **Text-Schnitt:** Wörter markieren (klicken/ziehen, ⇧+Klick) → `Entf` schneidet sie heraus.
  Nochmal `Entf` stellt sie wieder her. Alles nicht-destruktiv und mit Undo/Redo.
- **Stille automatisch entfernen:** ffmpeg `silencedetect` findet Pausen. Ein Klick schneidet sie
  aus allen Clips, mit einstellbarer Schwelle (dB), Mindestdauer und Rand.
  Pausen erscheinen als `⏸ 1.2s`-Marken im Text und lassen sich auch einzeln löschen.
- Füllwörter („äh“, „ähm“ …) entfernen, Suchen & alle Treffer löschen. Whisper lässt Füllwörter normalerweise
  weg. Mit **„Füllwörter mit transkribieren (wortgetreu)“** (Einstellungen, standardmäßig an) bekommt es einen
  Prompt mit Füllwörtern und schreibt sie, Wiederholungen und Versprecher mit (de/en/fr/es/it/nl).
  Wörter mit kaum Ton darunter sind **gelb unterstrichen**: Whisper hat sie womöglich erfunden. „Füllwörter
  entfernen“ lässt diese stehen.
- Präzise Wortgrenzen: DTW-Zeitstempel von whisper.cpp plus Pegelanalyse. Beim Löschen wird nur der
  Wortkern geschnitten, leise Anlaute von Nachbarwörtern (z. B. das „S“ in „Sphäre“) bleiben erhalten.
- **Weiche Übergänge:** An jeder Schnittstelle wird der Ton kurz aus- und wieder eingeblendet.
  Länge (5–500 ms) und Kurve (Linear, Sinus, S-Kurve, Schnell, Langsam) sind einstellbar,
  getrennt für Text- und Stille-Schnitte, mit Kurvenvorschau. Die Länge der Timeline ändert sich nicht.
- Whisper-Zeitstempel werden mit der Stille-Analyse nachkorrigiert: Wörter beginnen und enden nie in
  einer Pause und überspannen keine. Wörter mit gleicher Startzeit werden verteilt. Kurze Geräusche ohne
  Wort zwischen zwei Pausen (Atmer, Schmatzen) zählen zur Pause und werden beim Stille-Entfernen mit entfernt.

**Schnitt**
- Timeline mit Haupt-Videospur (magnetisch, rückt automatisch nach), beliebig vielen
  weiteren Video- und Audiospuren
- Teilen, Trimmen (Clip-Ränder ziehen), Verschieben, Duplizieren, Löschen, Magnet/Snapping,
  Zoom, Wellenform, Schnittmarken
- Lautstärke, Stumm, Ein-/Ausblenden, Spuren stumm schalten oder ausblenden
- **Clip-Übergänge** (Video- und/oder Audiospuren), wo zwei Clips aneinanderstoßen: **Überblenden** (Bild löst sich auf,
  Ton wird übergeblendet; nutzt Material vor/nach den Clip-Grenzen, die Timeline-Länge bleibt gleich) oder
  **über Schwarz/Stille**. Länge 50–4000 ms, Kurve für den Ton einstellbar, Übergänge sind in der Timeline markiert.
- **Bild-in-Bild:** Clips auf V2+ mit Positions-Presets, Größe, X/Y, Deckkraft und Rahmen
- **Audiodateien** (mp3, wav, m4a, flac …) auf Audiospuren
- Vorschau mit Ton. Sie nutzt denselben Render-Graphen wie der Export.

**Export**
- Mehrere Größen in einem Durchgang: Original, 4K, 1440p, 1080p, 720p, 480p,
  9:16 (Reels/TikTok/Shorts), 1:1, 4:5, eigene Größe
- Andere Seitenverhältnisse: einpassen (Balken) oder füllen (zuschneiden)
- H.264, H.265, H.264-Hardware (VideoToolbox), ProRes 422
- **Audio-Export** (in beiden Modi): MP3, M4A (AAC), WAV, FLAC, Bitrate 128–320 kbit/s,
  optional **Lautheits-Normalisierung auf −16 LUFS**
- **Untertitel einbrennen** (💬 im Text-Schnitt): Text aus dem Transkript, folgt allen Schnitten. Vorlagen Klassisch,
  Social Media (große Großbuchstaben, aktuelles Wort farbig) und Box. Schrift, Größe, Position, Farben, Kontur,
  Hintergrundbox und Zeilenlänge sind einstellbar, Größen relativ zur Videohöhe (passt für 16:9 und 9:16 gleichermaßen).
  Gezeichnet wird in Rust; ffmpeg braucht dafür keine Textfilter (libass/drawtext).
- SRT-Untertitel, die den Schnitten folgen (dieselben Zeilen wie beim Einbrennen)

## Voraussetzungen

| Tool | Zweck | macOS |
|---|---|---|
| Rust ≥ 1.86 | Bauen | `rustup` |
| ffmpeg + ffprobe | Analyse, Vorschau, Export | `brew install ffmpeg` |
| whisper.cpp (`whisper-cli`) | Transkription (lokal, offline) | `brew install whisper-cpp` |

Das Whisper-Modell lädt TextCutForge unter **Einstellungen → Modell → Herunterladen**
nach `~/.textcutforge/models/`. `base` ist schnell, `small` oder `large-v3-turbo` sind genauer.

## Starten

```bash
cargo run --release                   # App starten
cargo run --release -- video.mp4      # direkt mit Video
cargo run --release -- projekt.tcf    # Projekt öffnen
cargo test                            # Tests (inkl. echter ffmpeg-Renderings)
```

## Ablauf

1. Video ins Fenster ziehen. Es landet auf der Hauptspur und wird transkribiert und auf Stille analysiert.
2. **🔇 Stille automatisch entfernen** klicken.
3. Im Text Versprecher markieren und mit `Entf` löschen.
4. Optional: zweites Video über **🖼 Bild-in-Bild** auf V2 legen und im Inspektor positionieren.
5. **⬆ Exportieren**, z. B. 1080p und 9:16 gleichzeitig.

## Tastenkürzel

`Leertaste` Abspielen · `Entf` Text/Clip löschen · `S` teilen · `⌘Z`/`⇧⌘Z` Undo/Redo ·
`⌘D` duplizieren · `←/→` Bild (mit ⇧: 1 s) · `J/K/L` · `⌘S/⌘O/⌘I/⌘E`

## Architektur

```
src/
├── project.rs     Datenmodell + Schnittlogik (Cuts, Split, Zeitabbildung, Tokens), getestet
├── render.rs      Timeline → ffmpeg filter_complex (geteilt von Vorschau & Export)
├── preview.rs     Scrub-Einzelbilder + Echtzeit-Wiedergabe (rawvideo/PCM-Pipes, rodio)
├── export.rs      Export-Presets, Codecs, Fortschritt, SRT
├── subtitles.rs   Untertitelzeilen, Text-Rendering (ab_glyph), Einbrennen per Bildfolge
├── media.rs       ffprobe, Wellenform, silencedetect
├── transcribe.rs  whisper-cli-Aufruf, JSON-Parser, Modell-Download
├── settings.rs    ~/.textcutforge/settings.json
├── app.rs         App-Zustand, Undo/Redo, Hintergrundjobs, Tastatur
└── ui/            Transkript-Editor, Timeline, Panels, Dialoge (egui)
```

Schnitte werden nicht-destruktiv als Quell-Zeitbereiche gespeichert. Die abgespielten
Bereiche sind Quellbereich minus Schnitte, gerastert auf das Frame-Raster. ffmpeg wählt sie
pro Clip in einem Durchlauf per `select`/`aselect` aus. Das bleibt auch bei Hunderten Text-Schnitten
schnell, und Bild und Ton bleiben synchron.
