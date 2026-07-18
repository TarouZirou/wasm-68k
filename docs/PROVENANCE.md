# Source provenance

wasm-68k source is marked GPL-2.0-only. MIT/BSD/Zlib dependencies are
GPLv2-compatible; Apache-2.0-only packages are covered by the project linking
exception in [`LICENSE-EXCEPTION`](../LICENSE-EXCEPTION). The exception applies
only to independent dependencies and does not alter the license of wasm-68k
source or separately licensed ROM/media data.
Sharp-authorized IPL program ROMs and Human68k 3.02 are kept as unmodified,
separately licensed data under `web/public/sharp/`; see `docs/SHARP_ASSETS.md`.
No CGROM, game, or other floppy/hard-disk image is included.

## Current implementation references

- `m68k` 0.2.1 (MIT), upstream commit
  `499c7a5deb8616a6f37d12c1f5e5fc932b81c4db`, crates.io archive SHA-256
  `40195cf4e80177329392fe5b02ef20772d8a359f9c9d681f501fbec0cfacdf61`:
  CPU execution core. The complete crate and original license are pinned at
  `vendor/m68k` and kept behind the `x68k-core` public API. The local patch
  changes the M68000 bus/address-error 14-byte stack frame from the incorrect
  `PC, SR, IR, address, status` stack layout to the hardware/RTE layout
  `SR, PC, IR, address, status`. It also adds an optional `AddressBus`
  supervisor-mode notification so an external bus controller can observe an
  exception's S-bit transition before the frame is written; buses that do not
  distinguish function codes retain the default no-op. The single-step APIs
  also check an accepted interrupt before returning `Stopped`, matching the
  existing batch executor's STOP wake-up ordering.
- MAME, commit `4e618a304e835f9891ad5836b2fdde4991682947` (BSD-3-Clause):
  `src/mame/sharp/x68k.cpp` is the memory-map/device-behaviour reference
  (copyright Barry Rodewald, Carl and MAME contributors), and
  `src/devices/machine/mb87030.cpp` plus `.h` are the MB89352 register/phase
  reference (copyright Sven Schnelle).
- SingleStepTests/m68000 (MIT), commit
  `64b253116a3de04aaac4346c43680960dc9b67e5`: CPU conformance corpus. Ten
  decoded representative JSON cases are bundled for PR checks; the complete
  corpus is fetched at that immutable revision by the scheduled workflow.
- m68k-rs test runner (MIT), commit
  `499c7a5deb8616a6f37d12c1f5e5fc932b81c4db`, file
  `tests/singlestep_m68000_v1_tests.rs`: reference for the fixture binary
  decoder, MAME `m_au` PC conversion and CPU state mapping in
  `crates/x68k-core/tests/singlestep_m68000.rs`. The repository implementation
  was rewritten to support both its vendored JSON subset and external corpus.
- PX68k/libretro (GPL-2.0 family): compatibility reference. No PX68k source file has
  been copied verbatim into the current implementation.

## PX68k-derived device behaviour

Repository: `https://github.com/libretro/px68k-libretro`, commit
`45dfd4005434d1199b01fb74a5371ec9bc513164`, GPL-2.0-or-later (see its
`COPYING`). The following Rust files are clean rewrites using the named C files
for register layout and behavioural comparison:

- `crates/x68k-core/src/devices/crtc.rs` — `x68k/crtc.c`, including register
  pairs, scroll values, visible dimensions and raster copy.
- `crates/x68k-core/src/devices/mfp.rs` — `x68k/mfp.c` and `x68k/mfp.h`,
  including reset values, interrupt priority/vectoring, timers and USART status.
- `crates/x68k-core/src/devices/fdc.rs` — `x68k/fdc.c`, including uPD72065
  command/parameter/result phases, main-status bits and drive commands.
- `crates/x68k-core/src/devices/dma.rs` — `x68k/dmac.c` and `x68k/dmac.h`,
  including HD63450 channel registers, address stepping and interrupt vectors.
- `crates/x68k-core/src/media.rs` — `x68k/disk_xdf.c`, `disk_dim.c` and
  `disk_d88.c` for CHRN-to-offset rules and D88 variable-sector headers.
- `crates/x68k-core/src/devices/gvram.rs` and `video.rs` — `x68k/gvram.c`,
  `palette.c`, `crtc.c` and `libretro/windraw.c` for CPU-window page mapping,
  16/256/65536-colour decoding, palettes and layer priority.
- `crates/x68k-core/src/devices/sprite.rs` — `x68k/bg.c` and `bg.h` for sprite
  control entries, pattern addressing, BG maps, flips and palette selection.
- `crates/x68k-core/src/devices/rtc.rs` and `system_port.rs` — `x68k/rtc.c`,
  `sysport.c` and their headers. The RTC was deliberately made deterministic
  instead of copying PX68k's host-local-time reads.
- `crates/x68k-core/src/bus.rs` printer data/strobe mapping — MAME's X68000
  base memory map and the X68k I/O map at
  `https://datacrystal.tcrf.net/wiki/X68k/IOMAP`; the two write-only ports are
  modelled locally without importing implementation code. New SRAM starts at
  `0x00`, matching the initialized reserved fields expected by the X68030 IPL;
  `$ED0008.w` is derived from installed RAM rather than persisted SRAM bytes.
- `crates/x68k-core/src/scheduler.rs` — timing constants and resolution mode
  comparison against `x68k/crtc.c` and `crtc.h`; the fractional event queue is
  an original Rust implementation.
- `crates/x68k-core/src/devices/audio.rs` — register/timer and ADPCM behaviour
  compared with `fmgen/opm.cpp`, `x68k/adpcm.c` and MAME YM2151/MSM6258 device
  wiring. No fmgen source or lookup table was copied.
- `crates/x68k-core/src/devices/hdc.rs` — SASI/SCSI phases and commands compared
  with `x68k/sasi.c`, `scsi.c` and MAME's `src/devices/machine/mb87030.cpp` and
  `.h` MB89352 register/phase implementation at the MAME commit recorded above.
- `crates/x68k-core/src/devices/midi.rs` — CZ-6BM1 register layout and baud/timer
  behaviour compared with `x68k/midi.c` and `midi.h`.
- `crates/x68k-core/src/devices/scc.rs` and `ppi.rs` — mouse packet and joystick/
  ADPCM control behaviour compared with `x68k/scc.c`, MAME's X68000 PPI wiring,
  and the libretro input adapters.

The original PX68k file headers do not name an individual copyright holder;
copyright remains with the PX68k contributors. No CPU core or copyrighted ROM
data was imported.

## Original code

All Rust, TypeScript, JavaScript, WGSL and workflow code currently present in
this repository was written for wasm-68k unless a file header says otherwise.
The generated diagnostic IPL/XDF contains only original project code and data.
It is independent of the separately licensed Sharp files.
