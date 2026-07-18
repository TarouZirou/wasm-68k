# AGENTS.md — wasm-68k

Sharp X68000 エミュレータ。Rust 製コアを Wasm に変換し、Web サイト (WebGPU) で動かすプロジェクト。

## リポジトリ構成

```
crates/
  x68k-core/    エミュレーションコア (プラットフォーム非依存。m68k クレートで CPU を駆動)
  x68k-render/  wgpu ベースの描画。ネイティブ/Wasm 共通。GRBi → RGB 変換は WGSL シェーダ
  x68k-native/  デバッグ用ネイティブランナー (winit + wgpu)
  x68k-wasm/    wasm-bindgen バインディング (wasm32 ターゲット専用)
web/            Vite + TypeScript のサイト (wasm-pack の生成物を import)
```

## ビルド / テスト手順

前提: Rust (nightly 推奨。stable 1.88+ でも可)、`rustup target add wasm32-unknown-unknown`、
wasm-pack、Node 24+

```sh
# ホスト向けビルド & テスト (x68k-wasm は default-members から外してある)
cargo build
cargo test

# ネイティブランナー (ウィンドウが開く)
cargo run -p x68k-native

# Wasm 単体チェック (.cargo/config.toml で web_sys_unstable_apis 設定済み)
cargo check -p x68k-wasm --target wasm32-unknown-unknown

# Web サイト (wasm-pack ビルド → Vite)
cd web
npm install
npm run dev      # 開発サーバ
npm run build    # dist/ に本番ビルド
```

## 重要な設計上の決まり事

- **CPU はサードパーティ製 `m68k` クレート (MIT) を使用**。自前実装ではない。
  不具合時に差し替えられるよう、コア内で直接 m68k 型を露出させすぎないこと。
- **ピクセルフォーマットは X68000 ネイティブの 16bit GRBi** (G=15-11, R=10-6, B=5-1, I=0)。
  コアは GRBi の `u16` フレームバッファを出力し、RGB 変換はシェーダ側で行う。
  変換ロジックは `x68k-core/src/color.rs` と `x68k-render/src/shader.wgsl` の 2 箇所に
  あるので、変更時は必ず両方を同期させること。
- **wgpu は v30**。旧版と API が異なる点に注意:
  - プレゼントは `queue.present(surface_texture)` (`SurfaceTexture::present()` ではない)
  - `surface.get_current_texture()` は `CurrentSurfaceTexture` enum を返す
  - `PipelineLayoutDescriptor.immediate_size` (旧 `push_constant_ranges`)
  - `InstanceDescriptor` は `new_without_display_handle()` 等で構築
- **WebGL2 フォールバック**: `wgpu::util::new_instance_with_webgpu_detection` が
  WebGPU 非対応ブラウザ (Linux Firefox 等) では自動で WebGL2 を使う。
  limits は `Limits::downlevel_webgl2_defaults()` 固定で両対応している。
- **ネイティブの GLES バックエンドではディスプレイハンドルが必須**。
  `Renderer::new_with_display` に winit の `OwnedDisplayHandle` を渡すこと
  (Vulkan が無い環境ではこちらが使われる)。
- **公式許諾済み資産は例外を限定する**。`web/public/sharp/`には
  `docs/SHARP_ASSETS.md`でhashを固定した未改変IPL 3種、Human68k 3.02 XDF、
  添付許諾文書だけを置ける。これらはGPLではなく非商用の別許諾である。
  CGROM、ゲーム、その他のROM／媒体はコミットせず、ユーザーがブラウザ上で
  ロードする。

## コーディング規約

- コメント・ドキュメントは日本語でよい (本プロジェクトの慣例)。
- `x68k-core` はプラットフォーム非依存を維持する (std のみ。wgpu/winit/web-sys 禁止)。
- unsafe は原則使わない。
- 変更後は `cargo test` と `cargo check -p x68k-wasm --target wasm32-unknown-unknown`
  の両方が通ることを確認する。

## ロードマップ (フェーズ計画)

- [x] Phase 0: スキャフォールド (本構成 + テストパターン描画)
- [x] Phase 1: CPU 統合検証 (SingleStepTests/m68000 の JSON テスト導入)
- [x] Phase 2: IPL 起動画面 (バス/メモリマップ、CRTC 最小、テキスト VRAM + CGROM)
- [x] Phase 3: Human68k 起動 (FDC + DMA + XDF/DIM、キーボード)
- [x] Phase 4: ゲーム起動 (グラフィックプレーン、スプライト、ラスタ割り込み)
- [x] Phase 5: サウンド (YM2151 + MSM6258、Web は AudioWorklet)
- [x] Phase 6: Web 統合・仕上げ (D&D ロード、SRAM 永続化、セーブステート、Pages デプロイ、CRT シェーダ)
- [x] Phase 7 (任意): HDD、D88、X68030、MIDI
