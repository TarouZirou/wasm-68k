// X68000 フレームバッファ描画シェーダ。
// R16Uint テクスチャにアップロードされた GRBi ピクセルを RGB に変換して表示する。

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
// フルスクリーン三角形の頂点位置とテクスチャ座標を生成する。
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    // 画面全体を覆う三角形 (fullscreen triangle)
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    // 画面上端がテクスチャの先頭行 (row 0) に対応するようにする
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var out: VsOut;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    out.uv = uvs[vertex_index];
    return out;
}

@group(0) @binding(0)
var frame_tex: texture_2d<u32>;

// x: enabled, y: scanline, z: RGB mask, w: curvature
@group(0) @binding(1)
var<uniform> crt: vec4<f32>;

// X68000 の 16bit GRBi ピクセルを、表示用の 8bit RGB へ変換する。
fn grbi_to_rgb8(pixel: u32) -> vec3<u32> {
    let i = pixel & 1u;
    let g6 = ((pixel >> 10u) & 0x3eu) | i;
    let r6 = ((pixel >> 5u) & 0x3eu) | i;
    let b6 = (pixel & 0x3eu) | i;
    let g8 = (g6 << 2u) | (g6 >> 4u);
    let r8 = (r6 << 2u) | (r6 >> 4u);
    let b8 = (b6 << 2u) | (b6 >> 4u);
    return vec3<u32>(r8, g8, b8);
}

@fragment
// フレームバッファを読み、任意の CRT 効果を適用して最終色を出力する。
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let dims = textureDimensions(frame_tex);
    let centered = in.uv * 2.0 - vec2<f32>(1.0);
    let warped = centered * (1.0 + crt.w * dot(centered, centered));
    let raw_uv = select(in.uv, warped * 0.5 + vec2<f32>(0.5), crt.x > 0.5);
    if (any(raw_uv < vec2<f32>(0.0)) || any(raw_uv > vec2<f32>(1.0))) {
        discard;
    }
    let uv = clamp(raw_uv, vec2<f32>(0.0), vec2<f32>(1.0));
    let coord = min(vec2<u32>(uv * vec2<f32>(dims)), dims - vec2<u32>(1u, 1u));
    let pixel = textureLoad(frame_tex, vec2<i32>(coord), 0).r;

    // X68000 GRBi フォーマット: G(15-11) R(10-6) B(5-1) I(0)
    // 各チャネルは (5bit << 1) | I の 6bit としてデコードし、8bit に伸長する
    let rgb8 = grbi_to_rgb8(pixel);
    var rgb = vec3<f32>(rgb8) / 255.0;
    if (crt.x > 0.5) {
        let scan = select(1.0 - crt.y, 1.0, (coord.y & 1u) == 0u);
        let lane = coord.x % 3u;
        let mask = vec3<f32>(
            select(1.0 - crt.z, 1.0, lane == 0u),
            select(1.0 - crt.z, 1.0, lane == 1u),
            select(1.0 - crt.z, 1.0, lane == 2u),
        );
        rgb *= scan * mask;
    }
    return vec4<f32>(rgb, 1.0);
}
