// Veyra – NV12 → RGB, Skalierung (cover/contain/stretch) und audio-reaktive Effekte.
// Alles in einem einzigen Durchlauf: ein Dreieck, wenige Texturzugriffe pro Pixel.

struct U {
    surf: vec4f,    // xy = Fenstergröße (px), zw = sichtbare Videogröße (px)
    crop: vec4f,    // xy = Versatz, zw = Skalierung des sichtbaren Bereichs in der Textur
    audio: vec4f,   // bass, mid, high, level  (0 … ~1.5)
    params: vec4f,  // fit (0 cover, 1 contain, 2 stretch), effect, intensity, time
    misc: vec4f,    // x = 1 → BT.709, sonst BT.601
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var ytex: texture_2d<f32>;
@group(0) @binding(2) var uvtex: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

struct VO {
    @builtin(position) pos: vec4f,
    @location(0) px: vec2f,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VO {
    // Ein bildschirmfüllendes Dreieck ohne Vertex-Buffer.
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    var o: VO;
    o.pos = vec4f(p * 2.0 - 1.0, 0.0, 1.0);
    o.px = vec2f(p.x, 1.0 - p.y);
    return o;
}

fn rgb(uv: vec2f) -> vec3f {
    let t = u.crop.xy + uv * u.crop.zw;
    let y = (textureSampleLevel(ytex, samp, t, 0.0).r - 16.0 / 255.0) * (255.0 / 219.0);
    let c = (textureSampleLevel(uvtex, samp, t, 0.0).rg - vec2f(0.5)) * (255.0 / 224.0);
    if (u.misc.x > 0.5) {
        return vec3f(y + 1.5748 * c.y, y - 0.1873 * c.x - 0.4681 * c.y, y + 1.8556 * c.x);
    }
    return vec3f(y + 1.402 * c.y, y - 0.344136 * c.x - 0.714136 * c.y, y + 1.772 * c.x);
}

@fragment
fn fs(in: VO) -> @location(0) vec4f {
    let s = u.surf.xy;
    let v = u.surf.zw;
    let fit = u32(u.params.x);
    let effect = u32(u.params.y);
    let k = u.params.z;
    let bass = u.audio.x;
    let mid = u.audio.y;
    let level = u.audio.w;

    // Anteil des Bildschirms, den das Video einnimmt (pro Achse).
    var cover = vec2f(1.0);
    let r = s / v;
    if (fit == 0u) {
        cover = v * max(r.x, r.y) / s;
    } else if (fit == 1u) {
        cover = v * min(r.x, r.y) / s;
    }

    var p = in.px - 0.5;
    if (effect == 1u) {
        p = p / (1.0 + bass * 0.05 * k); // Puls: leichter Zoom auf den Bass
    }
    let uv = p / cover + 0.5;
    if (any(uv < vec2f(0.0)) || any(uv > vec2f(1.0))) {
        return vec4f(0.0, 0.0, 0.0, 1.0); // Balken bei "contain"
    }

    var col: vec3f;
    if (effect == 3u) {
        // Chroma: RGB-Versatz im Takt
        let o = vec2f(bass * 0.008 * k, 0.0);
        col = vec3f(rgb(uv + o).r, rgb(uv).g, rgb(uv - o).b);
    } else {
        col = rgb(uv);
    }

    if (effect == 1u) {
        col = col * (1.0 + level * 0.2 * k);
    } else if (effect == 2u) {
        // Glow: Aufhellen + farbiger Rand-Schein
        let d = length(in.px - 0.5) * 1.4;
        let glow = smoothstep(0.4, 1.0, d) * (bass * 0.6 + mid * 0.3) * k;
        col = col + col * bass * 0.35 * k + vec3f(0.35, 0.2, 0.6) * glow;
    }
    return vec4f(clamp(col, vec3f(0.0), vec3f(1.0)), 1.0);
}
