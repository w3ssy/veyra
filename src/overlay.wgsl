// Veyra – Widgets: ein fertig gerastertes Bild (vormultipliziertes Alpha) pixelgenau über das Video legen,
// plus eine Leucht-Ebene, die pro Bild mit `surf.z` eingeblendet wird (Countdown-Effekt).

struct R {
    rect: vec4f,  // x, y, Breite, Höhe in Pixeln
    surf: vec4f,  // xy = Fenstergröße in Pixeln, z = Stärke des Leuchtens
};

@group(0) @binding(0) var<uniform> r: R;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var glow: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    // Rechteck als Dreiecksstreifen aus 4 Ecken, ohne Vertex-Buffer.
    let c = vec2f(f32(i & 1u), f32((i >> 1u) & 1u));
    let px = r.rect.xy + c * r.rect.zw;
    return vec4f(px.x / r.surf.x * 2.0 - 1.0, 1.0 - px.y / r.surf.y * 2.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) p: vec4f) -> @location(0) vec4f {
    // 1:1 – kein Filtern, keine Unschärfe.
    let t = vec2i(floor(p.xy - r.rect.xy));
    let base = textureLoad(tex, t, 0);
    // Leuchten ist reines Licht: es hellt auf, deckt aber nichts ab (Alpha bleibt).
    let g = textureLoad(glow, t, 0).rgb * r.surf.z;
    return vec4f(base.rgb + g, base.a);
}
