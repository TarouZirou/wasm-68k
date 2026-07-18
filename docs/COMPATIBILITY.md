# Compatibility policy

`compatibility/catalog.json` is the finite, reviewable release matrix. Every
registered entry must be `pass` before `main` can be published to GitHub Pages;
`cargo test --locked` enforces that rule. The initial deterministic matrix uses
the self-authored synthetic IPL/XDF. Separately, the browser gate verifies the
hashes and mounting of Sharp's officially authorized IPL/Human68k distribution.
No CGROM, commercial game or unlicensed asset is needed by CI.

This is not a claim that every commercial X68000 title, including titles nobody
has tested, works forever. A reproducible user report is added to the catalog as
`fail`, `partial` or `untested`, fixed, and kept as a regression entry. Formal
releases require every entry known at that time to return to `pass`.

## Reporting a result

Use the GitHub compatibility issue form and include the generated diagnostic
JSON. It records the build, machine model, rendering backend, frame/audio result,
and SHA-256 identifiers for user-supplied ROM/media. Never attach or link CGROM,
games, or user-supplied ROM/floppy/HDD images. For the bundled official Sharp
files, report only their hashes.

## Public diagnostic software

The browser generates `builtin-diagnostic.xdf`, `builtin-diagnostic.hdf` and
their matching IPL locally from `web/src/diagnostic.ts`. All are original
GPL-2.0-only project code. The media contain only a license notice and
deterministic bytes; the IPL exercises CPU, raster IRQ, DMA, FDC, SASI/SCSI,
video layers, sound and MIDI paths. They may be redistributed and are the only
GPL-covered software assets used by the initial public gate. Official Sharp
IPL/Human68k files are a separate noncommercial gate documented in
`SHARP_ASSETS.md`.
