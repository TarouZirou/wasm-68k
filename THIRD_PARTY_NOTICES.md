# Third-party notices

The application links to dependencies recorded in `Cargo.lock` and
`web/package-lock.json`. Their license texts and source locations are available
from the corresponding crates.io and npm package metadata.

Notable runtime components:

- m68k 0.2.1 — MIT, copyright Ben Letchford. The complete crate is pinned in
  `vendor/m68k`; wasm-68k corrects the M68000 bus/address-error stack-frame
  field order and adds an optional bus supervisor-mode notification used by
  the X68000 Area Set controller, plus consistent interrupt wake-up for the
  crate's single-step APIs. Its original `LICENSE` is included unchanged.
- SingleStepTests/m68000 representative test data — MIT
- wgpu, winit, wasm-bindgen and web-sys — MIT OR Apache-2.0 (used under MIT)
- Rust-SDL2 — MIT; system SDL2 — Zlib (native audio output)
- midir — MIT (native MIDI output)
- Vite and TypeScript — MIT
- Playwright — Apache-2.0 (development/browser tests only)

Dual-licensed dependencies are used under their MIT/BSD option where
available. Apache-2.0-only packages are covered by the project linking
exception in [`LICENSE-EXCEPTION`](LICENSE-EXCEPTION); the exception permits
combining those independent components with this GPL-2.0-only project without
changing their separate notices.

Reference implementations and test-data sources are documented in
`docs/PROVENANCE.md`.

Unmodified Sharp X68000/XVI/X68030 IPL program ROMs and Human68k 3.02 are
distributed as separate data under Sharp's noncommercial X-series/emulator
permission. They are not covered by GPL-2.0-only. The mandatory original
license accompanies them at `web/public/sharp/SHARP_LICENSE_CP932.txt`; hashes,
sources and scope are recorded in `docs/SHARP_ASSETS.md`. CGROM, games and other
ROM/media images remain end-user supplied.
