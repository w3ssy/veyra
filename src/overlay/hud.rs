//! Gemeinsame HUD-Bausteine der Mission-Control-Module: Panel (Glasplatte mit Rahmen), Kopfzeile mit
//! Status-Lampen, Datenzeilen. Alle Panels teilen Innenabstand, Schriftgrößen und Farben – so wirken
//! mehrere Widgets zusammen wie ein Dashboard.

use super::canvas::{Canvas, Layer};
use super::flight::{fit, CYAN, DIM, HAIRLINE};
use super::text::{self, Color, Fonts, Line, Weight};

/// Innenabstand eines Panels (bei Skalierung 1).
pub const PAD: f32 = 14.0;
/// Höhe der Kopfzeile samt Abstand zum Inhalt.
pub const HEAD_H: f32 = 24.0;

/// Glasplatte: dunkel und halbtransparent, damit das Video durchscheint und Schrift trotzdem lesbar bleibt.
const PLATE: Color = [6, 13, 24, 125];
const EDGE: Color = [255, 255, 255, 26];

/// Leere Panel-Fläche: `w` × `h` = Außenmaß bei Skalierung 1. Zeichenkoordinaten beginnen danach
/// innerhalb des Innenabstands; die Innenbreite ist [`inner`]`(w)`.
pub fn panel(w: f32, h: f32, s: f32) -> Canvas {
    let (pw, ph) = ((w * s).round(), (h * s).round());
    let mut c = Canvas::new(pw as u32, ph as u32, s);
    c.rect(Layer::Back, (0.0, 0.0, pw, ph), PLATE);
    let (x1, y1) = (pw - 0.5, ph - 0.5);
    for (a, b) in [((0.5, 0.5), (x1, 0.5)), ((0.5, y1), (x1, y1)), ((0.5, 0.5), (0.5, y1)), ((x1, 0.5), (x1, y1))] {
        c.line(Layer::Back, a, b, 1.0, EDGE);
    }
    c.corners(Layer::Fore, (0.5, 0.5, x1, y1), 10.0 * s, (1.5 * s).max(1.0), [CYAN[0], CYAN[1], CYAN[2], 200]);
    c.set_origin(PAD * s, PAD * s);
    c
}

/// Innenbreite eines Panels mit Außenbreite `w` (bei Skalierung 1).
pub fn inner(w: f32) -> f32 {
    w - 2.0 * PAD
}

/// Kopfzeile: Titel links, rechts bündig Status-Lampen (● TEXT), dazwischen eine Haarlinie.
/// `x0 … x1` = verfügbare Breite, `y` = Oberkante.
pub fn header(c: &mut Canvas, fonts: &Fonts, s: f32, (x0, x1): (f32, f32), y: f32, title: (&str, Color), pills: &[(&str, Color)]) {
    let head = Line::new(12.0, Weight::Bold).tracking(0.14).span(title.0, title.1);
    let head_w = c.text(Layer::Fore, fonts, &head, s, x0, y);
    let mid = y + 7.5 * s;
    let mut right = x1;
    for (label, color) in pills {
        let t = Line::new(11.0, Weight::Bold).tracking(0.1).span(*label, *color);
        let tx = right - text::width(fonts, &t, s);
        c.text(Layer::Fore, fonts, &t, s, tx, y + 1.0 * s);
        c.disc(Layer::Fore, (tx - 8.0 * s, mid), 3.5 * s, *color);
        right = tx - 20.0 * s;
    }
    if right > x0 + head_w + 20.0 * s {
        c.line(Layer::Fore, (x0 + head_w + 10.0 * s, mid), (right, mid), 1.0, HAIRLINE);
    }
}

/// Kleine Beschriftung (Achsen, Spaltenköpfe).
pub fn tiny(t: &str, color: Color) -> Line {
    Line::new(8.5, Weight::Bold).tracking(0.12).span(t, color)
}

/// Höhe einer Datenzeile.
pub const ROW_H: f32 = 19.0;

/// Datenzeile „LABEL   Wert“; der Wert wird bei Bedarf gekürzt.
#[allow(clippy::too_many_arguments)]
pub fn row(c: &mut Canvas, fonts: &Fonts, s: f32, x: f32, y: f32, label_w: f32, max_x: f32, label: &str, value: &str, color: Color) {
    c.text(Layer::Fore, fonts, &Line::new(9.0, Weight::Bold).tracking(0.14).span(label, DIM), s, x, y + 2.5 * s);
    let base = Line::new(13.0, Weight::Bold).tracking(0.05);
    let v = fit(fonts, &base, value, s, max_x - (x + label_w * s));
    c.text(Layer::Fore, fonts, &base.tabular().span(v, color), s, x + label_w * s, y);
}
