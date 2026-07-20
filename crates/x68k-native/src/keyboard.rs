//! ホストの物理キーをX68000キーボードのmake/break codeへ変換する。

use std::collections::{HashMap, HashSet};

use winit::keyboard::KeyCode;

#[derive(Default)]
pub(crate) struct KeyboardState {
    pressed: HashMap<KeyCode, u8>,
}

impl KeyboardState {
    /// make codeを返し、同じ物理キーのOS repeatもmakeとして通す。
    pub(crate) fn press(&mut self, code: KeyCode) -> Option<u8> {
        let scancode = x68k_scancode(code)?;
        self.pressed.insert(code, scancode);
        Some(scancode)
    }

    /// 同じX68000キーへ割り当てた物理キーがすべて離れた時だけbreak対象を返す。
    pub(crate) fn release(&mut self, code: KeyCode) -> Option<u8> {
        let scancode = self.pressed.remove(&code)?;
        (!self.pressed.values().any(|value| *value == scancode)).then_some(scancode)
    }

    /// 蓄積済みの状態またはデータを取り出し、処理済みとして整理する。
    pub(crate) fn drain(&mut self) -> HashSet<u8> {
        self.pressed.drain().map(|(_, scan)| scan).collect()
    }
}

/// ホストの物理キーコードをX68000キーボードのスキャンコードへ変換する。
fn x68k_scancode(code: KeyCode) -> Option<u8> {
    Some(match code {
        KeyCode::Escape => 0x01,
        KeyCode::Digit1 => 0x02,
        KeyCode::Digit2 => 0x03,
        KeyCode::Digit3 => 0x04,
        KeyCode::Digit4 => 0x05,
        KeyCode::Digit5 => 0x06,
        KeyCode::Digit6 => 0x07,
        KeyCode::Digit7 => 0x08,
        KeyCode::Digit8 => 0x09,
        KeyCode::Digit9 => 0x0a,
        KeyCode::Digit0 => 0x0b,
        KeyCode::Minus => 0x0c,
        KeyCode::Equal => 0x0d,
        KeyCode::Backquote | KeyCode::IntlYen => 0x0e,
        KeyCode::Backspace => 0x0f,
        KeyCode::Tab => 0x10,
        KeyCode::KeyQ => 0x11,
        KeyCode::KeyW => 0x12,
        KeyCode::KeyE => 0x13,
        KeyCode::KeyR => 0x14,
        KeyCode::KeyT => 0x15,
        KeyCode::KeyY => 0x16,
        KeyCode::KeyU => 0x17,
        KeyCode::KeyI => 0x18,
        KeyCode::KeyO => 0x19,
        KeyCode::KeyP => 0x1a,
        KeyCode::BracketLeft => 0x1b,
        KeyCode::BracketRight => 0x1c,
        KeyCode::Enter => 0x1d,
        KeyCode::KeyA => 0x1e,
        KeyCode::KeyS => 0x1f,
        KeyCode::KeyD => 0x20,
        KeyCode::KeyF => 0x21,
        KeyCode::KeyG => 0x22,
        KeyCode::KeyH => 0x23,
        KeyCode::KeyJ => 0x24,
        KeyCode::KeyK => 0x25,
        KeyCode::KeyL => 0x26,
        KeyCode::Semicolon => 0x27,
        KeyCode::Quote => 0x28,
        KeyCode::Backslash => 0x29,
        KeyCode::KeyZ => 0x2a,
        KeyCode::KeyX => 0x2b,
        KeyCode::KeyC => 0x2c,
        KeyCode::KeyV => 0x2d,
        KeyCode::KeyB => 0x2e,
        KeyCode::KeyN => 0x2f,
        KeyCode::KeyM => 0x30,
        KeyCode::Comma => 0x31,
        KeyCode::Period => 0x32,
        KeyCode::Slash => 0x33,
        KeyCode::IntlRo => 0x34,
        KeyCode::Space => 0x35,
        KeyCode::Home => 0x36,
        KeyCode::Delete => 0x37,
        KeyCode::PageUp => 0x38,
        KeyCode::PageDown => 0x39,
        KeyCode::End => 0x3a,
        KeyCode::ArrowLeft => 0x3b,
        KeyCode::ArrowUp => 0x3c,
        KeyCode::ArrowRight => 0x3d,
        KeyCode::ArrowDown => 0x3e,
        KeyCode::NumLock => 0x3f,
        KeyCode::NumpadDivide => 0x40,
        KeyCode::NumpadMultiply => 0x41,
        KeyCode::NumpadSubtract => 0x42,
        KeyCode::Numpad7 => 0x43,
        KeyCode::Numpad8 => 0x44,
        KeyCode::Numpad9 => 0x45,
        KeyCode::NumpadAdd => 0x46,
        KeyCode::Numpad4 => 0x47,
        KeyCode::Numpad5 => 0x48,
        KeyCode::Numpad6 => 0x49,
        KeyCode::NumpadEqual => 0x4a,
        KeyCode::Numpad1 => 0x4b,
        KeyCode::Numpad2 => 0x4c,
        KeyCode::Numpad3 => 0x4d,
        KeyCode::NumpadEnter => 0x4e,
        KeyCode::Numpad0 => 0x4f,
        KeyCode::NumpadComma => 0x50,
        KeyCode::NumpadDecimal => 0x51,
        KeyCode::Help => 0x54,
        KeyCode::F11 => 0x55,
        KeyCode::F12 => 0x56,
        KeyCode::F13 => 0x57,
        KeyCode::F14 => 0x58,
        KeyCode::F15 => 0x59,
        KeyCode::KanaMode | KeyCode::Lang3 => 0x5a,
        KeyCode::CapsLock => 0x5d,
        KeyCode::Insert => 0x5e,
        KeyCode::Hiragana | KeyCode::Lang4 => 0x5f,
        KeyCode::Lang5 => 0x60,
        KeyCode::F1 => 0x63,
        KeyCode::F2 => 0x64,
        KeyCode::F3 => 0x65,
        KeyCode::F4 => 0x66,
        KeyCode::F5 => 0x67,
        KeyCode::F6 => 0x68,
        KeyCode::F7 => 0x69,
        KeyCode::F8 => 0x6a,
        KeyCode::F9 => 0x6b,
        KeyCode::F10 => 0x6c,
        KeyCode::ShiftLeft | KeyCode::ShiftRight => 0x70,
        KeyCode::ControlLeft | KeyCode::ControlRight => 0x71,
        KeyCode::PrintScreen | KeyCode::AltLeft => 0x72,
        KeyCode::Pause | KeyCode::AltRight => 0x73,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// `maps_host_keys_to_x68000_matrix_codes` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn maps_host_keys_to_x68000_matrix_codes() {
        for (key, scan) in [
            (KeyCode::Enter, 0x1d),
            (KeyCode::KeyA, 0x1e),
            (KeyCode::KeyZ, 0x2a),
            (KeyCode::KeyX, 0x2b),
            (KeyCode::Space, 0x35),
            (KeyCode::ArrowLeft, 0x3b),
            (KeyCode::ArrowUp, 0x3c),
            (KeyCode::ArrowRight, 0x3d),
            (KeyCode::ArrowDown, 0x3e),
            (KeyCode::F1, 0x63),
            (KeyCode::F10, 0x6c),
            (KeyCode::ShiftLeft, 0x70),
            (KeyCode::ControlLeft, 0x71),
        ] {
            assert_eq!(x68k_scancode(key), Some(scan), "{key:?}");
        }
    }

    #[test]
    /// `shared_modifier_emits_break_only_after_both_host_keys_are_released` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn shared_modifier_emits_break_only_after_both_host_keys_are_released() {
        let mut keyboard = KeyboardState::default();
        assert_eq!(keyboard.press(KeyCode::ShiftLeft), Some(0x70));
        assert_eq!(keyboard.press(KeyCode::ShiftRight), Some(0x70));
        assert_eq!(keyboard.release(KeyCode::ShiftLeft), None);
        assert_eq!(keyboard.release(KeyCode::ShiftRight), Some(0x70));
    }

    #[test]
    /// `drain_deduplicates_shared_hardware_keys` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn drain_deduplicates_shared_hardware_keys() {
        let mut keyboard = KeyboardState::default();
        keyboard.press(KeyCode::ControlLeft);
        keyboard.press(KeyCode::ControlRight);
        assert_eq!(keyboard.drain(), HashSet::from([0x71]));
        assert!(keyboard.drain().is_empty());
    }
}
