//! Gemeinsame HUD-Bausteine der Mission-Control-Module (Kopfzeile mit Status-Lampen, Datenzeilen).

use super::canvas::{Canvas, Layer};
use super::flight::{fit, DIM, HAIRLINE};
use super::text::{self, Color, Fonts, Line, Weight};

/// Kopfzeile: Titel links, rechts bündig Status-Lampen (● TEXT), dazwischen eine Haarlinie.
/// `x0 … x1` = verfügbare Breite, `y` = Oberkante.
pub fn header(c: &mut Canvas, fonts: &Fonts, s: f32, (x0, x1): (f32, f32), y: f32, title: (&str, Color), pills: &[(&str, Color)]) {
    let head = Line::new(14.0, Weight::Bold).tracking(0.12).span(title.0, title.1);
    let head_w = c.text(Layer::Fore, fonts, &head, s, x0, y);
    let mid = y + 9.0 * s;
    let mut right = x1;
    for (label, color) in pills {
        let t = Line::new(14.0, Weight::Bold).tracking(0.08).span(*label, *color);
        let tx = right - text::width(fonts, &t, s);
        c.text(Layer::Fore, fonts, &t, s, tx, y);
        c.disc(Layer::Fore, (tx - 9.0 * s, mid), 4.5 * s, *color);
        right = tx - 24.0 * s;
    }
    if right > x0 + head_w + 24.0 * s {
        c.line(Layer::Fore, (x0 + head_w + 12.0 * s, mid), (right, mid), 1.0, HAIRLINE);
    }
}

/// Kleine Beschriftung (Achsen, Spaltenköpfe).
pub fn tiny(t: &str, color: Color) -> Line {
    Line::new(9.0, Weight::Bold).tracking(0.1).span(t, color)
}

/// Datenzeile „LABEL   Wert“ wie bei Flight Control; der Wert wird bei Bedarf gekürzt.
#[allow(clippy::too_many_arguments)]
pub fn row(c: &mut Canvas, fonts: &Fonts, s: f32, x: f32, y: f32, label_w: f32, max_x: f32, label: &str, value: &str, color: Color) {
    c.text(Layer::Fore, fonts, &Line::new(10.0, Weight::Bold).tracking(0.14).span(label, DIM), s, x, y + 3.0 * s);
    let base = Line::new(15.0, Weight::Bold).tracking(0.05);
    let v = fit(fonts, &base, value, s, max_x - (x + label_w * s));
    c.text(Layer::Fore, fonts, &base.tabular().span(v, color), s, x + label_w * s, y);
}
