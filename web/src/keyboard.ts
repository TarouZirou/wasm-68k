// Sharp X68000キーボードが送る7bit make code。
// 行列位置 (row << 3 | column) はMAME x68k_kbd.cppの実機定義と合わせる。
export const KEYMAP_VERSION = 2;

export const defaultKeyMap: Readonly<Record<string, number>> = {
  Escape: 0x01,
  Digit1: 0x02, Digit2: 0x03, Digit3: 0x04, Digit4: 0x05,
  Digit5: 0x06, Digit6: 0x07, Digit7: 0x08, Digit8: 0x09,
  Digit9: 0x0a, Digit0: 0x0b, Minus: 0x0c, Equal: 0x0d,
  Backquote: 0x0e, IntlYen: 0x0e, Backspace: 0x0f, Tab: 0x10,
  KeyQ: 0x11, KeyW: 0x12, KeyE: 0x13, KeyR: 0x14,
  KeyT: 0x15, KeyY: 0x16, KeyU: 0x17, KeyI: 0x18,
  KeyO: 0x19, KeyP: 0x1a, BracketLeft: 0x1b, BracketRight: 0x1c,
  Enter: 0x1d, KeyA: 0x1e, KeyS: 0x1f, KeyD: 0x20,
  KeyF: 0x21, KeyG: 0x22, KeyH: 0x23, KeyJ: 0x24,
  KeyK: 0x25, KeyL: 0x26, Semicolon: 0x27, Quote: 0x28,
  Backslash: 0x29, KeyZ: 0x2a, KeyX: 0x2b, KeyC: 0x2c,
  KeyV: 0x2d, KeyB: 0x2e, KeyN: 0x2f, KeyM: 0x30,
  Comma: 0x31, Period: 0x32, Slash: 0x33, IntlRo: 0x34,
  Space: 0x35, Home: 0x36, Delete: 0x37, PageUp: 0x38,
  PageDown: 0x39, End: 0x3a, ArrowLeft: 0x3b, ArrowUp: 0x3c,
  ArrowRight: 0x3d, ArrowDown: 0x3e, NumLock: 0x3f,
  NumpadDivide: 0x40, NumpadMultiply: 0x41, NumpadSubtract: 0x42,
  Numpad7: 0x43, Numpad8: 0x44, Numpad9: 0x45, NumpadAdd: 0x46,
  Numpad4: 0x47, Numpad5: 0x48, Numpad6: 0x49, NumpadEqual: 0x4a,
  Numpad1: 0x4b, Numpad2: 0x4c, Numpad3: 0x4d, NumpadEnter: 0x4e,
  Numpad0: 0x4f, NumpadComma: 0x50, NumpadDecimal: 0x51,
  Help: 0x54, F11: 0x55, F12: 0x56, F13: 0x57, F14: 0x58, F15: 0x59,
  KanaMode: 0x5a, Lang3: 0x5a, CapsLock: 0x5d, Insert: 0x5e,
  Hiragana: 0x5f, Lang4: 0x5f, Lang5: 0x60,
  F1: 0x63, F2: 0x64, F3: 0x65, F4: 0x66, F5: 0x67,
  F6: 0x68, F7: 0x69, F8: 0x6a, F9: 0x6b, F10: 0x6c,
  ShiftLeft: 0x70, ShiftRight: 0x70,
  ControlLeft: 0x71, ControlRight: 0x71,
  PrintScreen: 0x72, AltLeft: 0x72, Pause: 0x73, AltRight: 0x73,
};

export function isKeyMap(value: unknown): value is Record<string, number> {
  return typeof value === "object" && value !== null && !Array.isArray(value) &&
    Object.values(value).every((scan) => Number.isInteger(scan) && scan >= 0 && scan <= 0x7f);
}

export function encodeKeyMap(keyMap: Record<string, number>): Uint8Array {
  return new TextEncoder().encode(JSON.stringify({ version: KEYMAP_VERSION, keyMap }));
}

export function decodeKeyMap(value: unknown): { keyMap: Record<string, number>; migrated: boolean } {
  if (typeof value === "object" && value !== null && !Array.isArray(value)) {
    const stored = value as Partial<{ version: unknown; keyMap: unknown }>;
    if (stored.version === KEYMAP_VERSION && isKeyMap(stored.keyMap)) {
      return { keyMap: stored.keyMap, migrated: false };
    }
  }
  if (!isKeyMap(value)) throw new Error("invalid persisted keymap");

  // v1にはPC/ATコード表と初期の誤った簡易表が存在した。既知表だけを更新し、
  // それ以外のユーザー定義はそのままv2コンテナへ移行する。
  const pcAtMap = value.Enter === 0x1c && value.Space === 0x39 &&
    value.ArrowUp === 0x48 && value.F1 === 0x3b;
  const earlyMap = value.Enter === 0x1d && value.Space === 0x35 &&
    value.KeyX === 0x1b && value.ArrowUp === 0x3c;
  return {
    keyMap: pcAtMap || earlyMap ? { ...defaultKeyMap } : value,
    migrated: true,
  };
}
