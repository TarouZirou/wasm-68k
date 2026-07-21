# Phase 0–7 completion audit

All implementation gates in `AGENTS.md` are complete. Release compatibility is
deliberately finite: every entry registered in `compatibility/catalog.json` must
be `pass`; unregistered commercial software is not a promise of permanent
100% compatibility. New tester reports are added as hashed, non-copyrighted
diagnostic records.

| Phase | Current evidence | Release-gate note |
| --- | --- | --- |
| 0 | Workspace, native/Wasm renderer and shared GRBi vectors; the old test-pattern path is removed. | Complete. |
| 1 | Private `CpuCore` adapter, three CPU/clock/address profiles, bus/address faults, vectored IRQ, event scheduler, representative pinned SST fixtures and scheduled pinned full-corpus job. The fixed revision `64b253116a3de04aaac4346c43680960dc9b67e5` full 127-file corpus passed locally; TAS/TRAPV are explicit expected failures. | The same command remains scheduled in CI for regression. |
| 2 | 1–12 MiB RAM, reset overlay, IPL/CGROM/SRAM, TVRAM, CRTC, video, MFP, RTC, DMA, system port and supervisor-area mapping. Synthetic IPL and hash-pinned official IPL tests cover deterministic startup and GRBi output. | CGROM remains user-supplied under Sharp's permission; the UI documents the blank-font/horizontal-band behavior. |
| 3 | uPD72065 phases, HD63450 transfers, IOC, XDF/DIM copy-on-write, keyboard, mouse and joystick paths; hash-pinned Human68k XDF boot test; model-specific SCSI ROM rejection. | Human68k interactive testing uses the user's legally supplied CGROM and never uploads it. |
| 4 | GVRAM 16/256/65536 modes, palettes, text/graphics/sprite composition, scroll, raster timing, priorities, special priority and half transparency are covered by unit and synthetic diagnostic tests. | Public compatibility entries must record their own license and content hashes. |
| 5 | Integer YM2151 register/timer/IRQ/LFO/noise/detune/key-scaling at the X68000 4 MHz clock, MSM6258 ADPCM, deterministic resampling, fixed-rate stereo PCM, native output and AudioWorklet MessagePort output. The redistributable register trace at `crates/x68k-core/tests/fixtures/audio/ym2151_register_trace.json` has a deterministic test. | Tester traces can be appended without distributing copyrighted audio. |
| 6 | Stable core/Wasm APIs, X68S v10 state manifest/CRC/LZ4, immutable-media exclusion, IndexedDB, D&D, CRT shader, diagnostics, issue form, Pages workflow, base-path Chromium tests, GPL exception and `cargo deny` gate. | User-selected ROMs are retained only in browser-local IndexedDB with an explicit clear action. Game media is never automatically saved, sent, or committed. |
| 7 | D88 variable sectors, SASI/SCSI HDF access, MB89352 path, overlays, M68EC030/25 MHz/32-bit waits and DMA, internal/external SCSI mapping, native/Web MIDI fallback. | X68030 and MIDI reports are accepted through the same finite compatibility catalog. |

The built-in diagnostic is the deterministic GPL regression fixture and is
registered as passing for all three profiles. Sharp's explicitly authorized
IPL/Human68k files are separate, unmodified noncommercial data under the
license shipped in `web/public/sharp/`; CGROM, games and other copyrighted
images must not be committed, uploaded to CI, or attached to issues.
