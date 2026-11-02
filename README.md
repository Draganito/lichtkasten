# Lichtkasten

Ordnerbasierter Foto- und Video-Betrachter. Der Ordner ist die Wahrheit — keine Import-Datenbank.

Miniaturen liegen versteckt im angeschauten Verzeichnis (`.lichtkasten`). Videos spielt GStreamer: was das System dekodieren kann, spielt die App.

## Debian-Paket

Jedes Push auf `main` und jedes Tag `v*` baut auf GitHub ein `.deb`. Releases hängen das Paket an.

Lokal:

```bash
cargo deb
sudo dpkg -i target/debian/lichtkasten_*.deb
```

Abhängigkeiten für Video: die GStreamer-Plugins des Systems (`gstreamer1.0-libav`, `good`/`bad`/`ugly`).

## Bedienung

- Ordner öffnen oder hereinziehen
- Raster, Sortierung, RAW ein/aus
- Doppelklick: Foto oder Video
- Alben stehen in den Metadaten der Dateien (XMP / Sidecar)
- Backspace oder Escape: zurück ins Raster
