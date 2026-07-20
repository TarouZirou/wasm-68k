//! X68000 の 16bit カラーフォーマット (GRBi) ヘルパー。
//!
//! ビット配置は以下の通り:
//! ```text
//! Bit: 15 14 13 12 11 10  9  8  7  6  5  4  3  2  1  0
//!      G4 G3 G2 G1 G0 R4 R3 R2 R1 R0 B4 B3 B2 B1 B0  I
//! ```
//! I (インテンシティ) ビットは 3 チャネル共通の最下位ビットで、
//! 各チャネルは `(5bit値 << 1) | I` の 6bit としてデコードされる。

/// RGB888 を X68000 の 16bit GRBi フォーマットにパックする。
///
/// インテンシティビットは使用しない (0 固定)。
pub fn rgb_to_grbi(r: u8, g: u8, b: u8) -> u16 {
    let r5 = u16::from(r >> 3);
    let g5 = u16::from(g >> 3);
    let b5 = u16::from(b >> 3);
    (g5 << 11) | (r5 << 6) | (b5 << 1)
}

/// GRBi ピクセルを RGB888 にデコードする (テスト・検証用)。
pub fn grbi_to_rgb(pixel: u16) -> (u8, u8, u8) {
    let i = pixel & 1;
    let g6 = ((pixel >> 10) & 0x3e) | i;
    let r6 = ((pixel >> 5) & 0x3e) | i;
    let b6 = (pixel & 0x3e) | i;
    (expand6to8(r6), expand6to8(g6), expand6to8(b6))
}

/// 特殊合成の半透明用に2画素を各RGBチャネルで平均する。
pub fn blend_half(a: u16, b: u16) -> u16 {
    let (ar, ag, ab) = grbi_to_rgb(a);
    let (br, bg, bb) = grbi_to_rgb(b);
    rgb_to_grbi(
        ((u16::from(ar) + u16::from(br)) / 2) as u8,
        ((u16::from(ag) + u16::from(bg)) / 2) as u8,
        ((u16::from(ab) + u16::from(bb)) / 2) as u8,
    )
}

/// 6bit 値を 8bit に伸長する (MAME の pal6bit と同じ変換)。
fn expand6to8(v: u16) -> u8 {
    ((v << 2) | (v >> 4)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Vector {
        grbi: u16,
        rgb: [u8; 3],
    }

    #[test]
    /// `pack_black_and_white` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn pack_black_and_white() {
        assert_eq!(rgb_to_grbi(0, 0, 0), 0x0000);
        assert_eq!(rgb_to_grbi(255, 255, 255), 0xfffe);
    }

    #[test]
    /// `pack_primary_colors` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn pack_primary_colors() {
        assert_eq!(rgb_to_grbi(255, 0, 0), 0x07c0); // 赤
        assert_eq!(rgb_to_grbi(0, 255, 0), 0xf800); // 緑
        assert_eq!(rgb_to_grbi(0, 0, 255), 0x003e); // 青
    }

    #[test]
    /// `roundtrip` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn roundtrip() {
        let packed = rgb_to_grbi(255, 128, 0);
        let (r, g, b) = grbi_to_rgb(packed);
        // 5bit 最大値 31 は I=0 の場合 6bit で 62 となり、8bit では 251 になる
        // (フル輝度 255 には I=1 が必要。これが X68000 GRBi の仕様)
        assert_eq!(r, 251);
        assert_eq!(g, 130); // 128>>3=16 -> 6bit: 32 -> 8bit: 130
        assert_eq!(b, 0);
    }

    #[test]
    /// `half_blend_preserves_channel_order` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn half_blend_preserves_channel_order() {
        assert_eq!(blend_half(0x07c0, 0x003e), rgb_to_grbi(125, 0, 125));
    }

    #[test]
    /// `shared_grbi_vectors_match_rust_conversion` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn shared_grbi_vectors_match_rust_conversion() {
        let vectors: Vec<Vector> =
            serde_json::from_str(include_str!("../tests/fixtures/grbi_vectors.json")).unwrap();
        for vector in vectors {
            assert_eq!(grbi_to_rgb(vector.grbi), vector.rgb.into());
        }
    }
}
