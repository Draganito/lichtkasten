# Lichtkasten

Folder-based photo and video viewer for Linux. The folder is the source of truth — there is no import database.

*Ordnerbasierter Foto- und Video-Betrachter. Der Ordner ist die Wahrheit — keine Import-Datenbank.*

## What it does

- Opens a folder and shows every photo, RAW file and video in it. Virtualised grid, smooth thumbnail size slider.
- Thumbnails are stored **inside the folder you open** (`<folder>/.lichtkasten/miniaturen/`). Plug the same drive into another machine and the thumbnails are already there. Nothing is cached on the local disk.
- RAW: embedded JPEG preview via `rawler` (Canon, Nikon, Sony, Fuji, DNG, …). Toggle RAW files on/off.
- Albums are playlists stored in the files' metadata (XMP `lichtkasten:albums`, embedded in JPEG or as `.xmp` sidecar). Reinstall the OS, albums survive. Covers too.
- Multi-select like a file manager: click, Shift-range, Ctrl-toggle, Ctrl+A, drag onto an album.
- Video playback through the system's GStreamer — whatever your system decodes, the app plays.
- Right-click → *Open with* lists every installed program that handles the file type, with its icon. No default is enforced.
- EXIF / IPTC / XMP panel, sort by name, date taken, modified, size.
- Full-text search (Ctrl+F) over file name, folder, albums and metadata (camera, lens, keywords, captions, dates). A small SQLite FTS5 index lives next to the files (`<root>/.lichtkasten/index.sqlite`) and is rebuilt from the files whenever they change; search already works while indexing runs.
- Trash: Del marks photos for deletion and moves them to `<folder>/.lichtkasten/papierkorb/` (sidecars travel along). Restore single files or everything, or empty the trash after a confirmation.
- The GUI thread never blocks: decoding, scanning, metadata and album reads run on worker threads with a per-frame apply budget.
- Interface follows the system language: English (US) and German.

## What is stored where

| What | Where |
|---|---|
| Thumbnail size, last folder, RAW toggle, sort order, window geometry | `~/.local/share/lichtkasten/app.ron` |
| Thumbnails | `<folder>/.lichtkasten/miniaturen/*.jpg` |
| Album membership and covers | XMP in the image (JPEG) or `<file>.xmp` sidecar (RAW, TIFF, PNG, video, …) |
| Search index (rebuildable cache) | `<root>/.lichtkasten/index.sqlite`; in-memory only if the root is read-only |
| Trash | `<folder>/.lichtkasten/papierkorb/` |

## Install (Debian / Ubuntu)

Download the `.deb` from [Releases](https://github.com/Draganito/lichtkasten/releases) and:

```bash
sudo apt install ./lichtkasten_*.deb
```

Video needs the system GStreamer plugins (`gstreamer1.0-libav`, `gstreamer1.0-plugins-good/-bad`). They are listed as recommends.

Build locally:

```bash
cargo install cargo-deb
cargo deb
sudo apt install ./target/debian/lichtkasten_*.deb
```

Every push to `main` and every `v*` tag builds the package on GitHub Actions; tags publish a release.

## Keyboard

| Key | Action |
|---|---|
| Enter / double-click | Open photo or video |
| Backspace / Escape | Back to grid |
| ← → | Previous / next (viewer) or move selection (grid) |
| Space | Next photo, or play/pause for video |
| Ctrl+A | Select all |
| Ctrl+F | Search field; Escape clears the search |
| Del | Move selection (or open photo) to the trash |
| Shift / Ctrl + click | Range / toggle selection |
| Scroll wheel on photo | Zoom; double-click toggles fit / 100 % |

## Language

The UI picks the language from `LC_ALL`, `LC_MESSAGES`, `LANG`, `LANGUAGE`. German for `de*`, English otherwise. Force it with e.g. `LANG=en_US.UTF-8 lichtkasten`.

## License

MIT or Apache-2.0, at your option.
