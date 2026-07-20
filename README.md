# wasm-68k

Sharp X68000 emulator core written in Rust, with native and WebAssembly/WebGPU
frontends. WebGPU-unavailable browsers automatically use the WebGL2 backend.

Public site: <https://TarouZirou.github.io/wasm-68k/>

> **Release status:** Phase 0–7 implementation gates are complete. Compatibility
> is reported against the finite, hashed catalog in `compatibility/catalog.json`;
> new tester reports extend that catalog. This is not a claim that every
> unregistered commercial title is permanently 100% compatible. The gate and
> asset policy are listed in `AGENTS.md` and `docs/PHASE_AUDIT.md`.

## Implemented foundation

- X68000 10MHz, XVI 16MHz and X68030/68EC030 25MHz configurations
- `m68k` CPU integration behind a platform-independent API
- pinned SingleStepTests/m68000 JSON PR fixtures and scheduled full-corpus tests
- 1–12MiB RAM, reset-vector overlay, ROM/SRAM, graphics/text/sprite VRAM and
  device address ranges
- GRBi framebuffer composition and optional WebGPU/WebGL2 CRT shader
- XDF, DIM, D88 and HDF validation with copy-on-write mounting
- keyboard, mouse, gamepad and Web MIDI frontend paths
- AudioWorklet PCM queue without SharedArrayBuffer
- versioned, CRC-checked, LZ4-compressed save states that do not embed ROMs or
  immutable disk images; the header carries each loaded ROM/media SHA-256
- IndexedDB save-state slots, drag-and-drop loading and diagnostic reports
- GitHub Actions CI, license/advisory audit and GitHub Pages deployment
- self-authored synthetic IPL/XDF public diagnostic and an enforced finite
  compatibility catalog
- unmodified Sharp-authorized IPLs for all three profiles and Human68k 3.02,
  isolated under their separate noncommercial redistribution terms

## Build and test

Requirements: Rust 1.88+ (nightly recommended), wasm32 target, wasm-pack 0.13.1,
Node.js 24+.

```sh
cargo test --locked
cargo check --locked -p x68k-wasm --target wasm32-unknown-unknown
cargo deny check

cd web
npm ci
npm run dev
# 生成済みWasmを使ってViteだけを起動
npm run dev:vite
npm run build:pages
```

The native diagnostic runner is available with:

```sh
cargo run -p x68k-native
```

起動途中のPC/SR表示や`$0018`の切り分けは
[`docs/DIAGNOSTICS.md`](docs/DIAGNOSTICS.md)を参照してください。

## Browser use

1. Select X68000, XVI or X68030.
2. Use “公式Human68kを起動”, or load your own `iplrom.dat` and `cgrom.dat`.
3. Mount `.xdf`/`.2hd`, `.dim`, `.d88`/`.88d`, or `.hdf` images.
4. Click the screen to send keyboard input. Double-click it for mouse capture.
5. Audio and MIDI require an explicit button click because browsers require a
   user gesture/permission.

All images stay inside the browser and are never uploaded. User-selected IPL,
CGROM and SCSI ROM files are retained in IndexedDB for the next visit and can
be removed with “保存ROMを消去”; floppy/HDD software is never retained
automatically. IndexedDB also contains settings, SRAM and save states.
The core displays black until an IPL is loaded; the former Phase 0 test pattern
is no longer part of the emulator path.

利用者が別途用意した`CGROM.DAT`、SCSI ROM、関連DLLは
`local-assets/roms/`に置く。このディレクトリはGitとPagesから除外され、Webでは
CG ROM欄の「読込」から`CGROM.DAT`を選ぶ。DLL自体をブラウザへ読み込ませる必要は
ない。コアの手動起動トレースは同ディレクトリのCGROMと、X68030の場合だけ
`SCSIINROM.DAT`を自動検出する。

## Sharp-authorized assets

The unmodified X68000, XVI and X68030 IPL program ROMs and Human68k 3.02 XDF
published by Sharp are included under their own **noncommercial**, X-series/
emulator-only terms. They are not GPL assets. The original CP932 license and a
UTF-8 convenience copy are shipped beside them; see
[`docs/SHARP_ASSETS.md`](docs/SHARP_ASSETS.md). CGROM, games and other disk/HDD
images are not included. Do not attach copyrighted assets to compatibility
issues; use hashes and the generated diagnostic JSON instead.

## License and provenance

wasm-68k source is marked GPL-2.0-only with the project linking exception in
[`LICENSE-EXCEPTION`](LICENSE-EXCEPTION). The exception permits combining the
core with independent Apache-2.0-only dependencies while retaining the
GPL-2.0-only terms for wasm-68k itself. See
[`docs/PROVENANCE.md`](docs/PROVENANCE.md) and
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md). The release-matrix policy is
documented in [`docs/COMPATIBILITY.md`](docs/COMPATIBILITY.md).
