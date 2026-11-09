"""Erzeugt die Icons aus den Logo-Grafiken (nur nötig, wenn sich die Grafiken ändern).

    python assets/make_icons.py

Benötigt Pillow, NumPy und SciPy. Ergebnis (wird eingecheckt und von build.rs in die EXE eingebettet):
  assets/veyra.ico            – Programm-Icon (farbiges V aus dem Logo), 16–256 px
  assets/tray-light.ico       – Tray-Icon, weiß  (für dunkle Taskleiste)
  assets/tray-dark.ico        – Tray-Icon, dunkel (für helle Taskleiste)
Bereinigt außerdem veyra-logo.png / veyra-symbol.png (Rausch-Pixel, Alpha-Reste) – idempotent.
"""

import io
import struct
from pathlib import Path

import numpy as np
from PIL import Image
from scipy import ndimage

DIR = Path(__file__).resolve().parent
APP_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
TRAY_SIZES = [16, 20, 24, 32, 40, 48, 64]


def clean(img: Image.Image, min_area: int) -> Image.Image:
    """Alpha-Rauschen entfernen und nur zusammenhängende Flächen ab `min_area` Pixeln behalten."""
    a = np.array(img.convert("RGBA"))
    alpha = a[..., 3]
    alpha[alpha < 8] = 0
    labels, n = ndimage.label(alpha > 96)
    areas = ndimage.sum(np.ones_like(alpha), labels, range(1, n + 1))
    keep = np.isin(labels, [i + 1 for i, s in enumerate(areas) if s >= min_area])
    # Etwas weiten, damit die geglätteten Kanten erhalten bleiben.
    keep = ndimage.binary_dilation(keep, iterations=3)
    alpha[~keep] = 0
    a[..., :3][alpha == 0] = 0
    return Image.fromarray(a, "RGBA")


def square(img: Image.Image, margin: float = 0.04) -> Image.Image:
    img = img.crop(img.getbbox())
    side = int(max(img.size) * (1 + 2 * margin))
    out = Image.new("RGBA", (side, side), (0, 0, 0, 0))
    out.paste(img, ((side - img.width) // 2, (side - img.height) // 2))
    return out


def tint(img: Image.Image, rgb) -> Image.Image:
    a = np.array(img)
    a[..., :3] = rgb
    a[..., :3][a[..., 3] == 0] = 0
    return Image.fromarray(a, "RGBA")


def bmp_entry(img: Image.Image) -> bytes:
    """32-bit-BGRA-DIB (bottom-up) + leere AND-Maske – das klassische ICO-Format."""
    w, h = img.size
    a = np.array(img)[::-1]
    bgra = a[..., [2, 1, 0, 3]].tobytes()
    mask_row = ((w + 31) // 32) * 4
    header = struct.pack("<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0, len(bgra), 0, 0, 0, 0)
    return header + bgra + bytes(mask_row * h)


def write_ico(path: Path, master: Image.Image, sizes):
    images = []
    for s in sizes:
        im = master.resize((s, s), Image.LANCZOS)
        if s >= 256:
            buf = io.BytesIO()
            im.save(buf, "PNG", optimize=True)
            images.append((s, buf.getvalue()))
        else:
            images.append((s, bmp_entry(im)))
    out = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    for s, data in images:
        b = s if s < 256 else 0
        out += struct.pack("<BBBBHHII", b, b, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    out += b"".join(d for _, d in images)
    path.write_bytes(out)
    print(f"{path.name}: {', '.join(str(s) for s in sizes)} px")


def main():
    logo = clean(Image.open(DIR / "veyra-logo.png"), min_area=1500)
    logo.save(DIR / "veyra-logo.png", optimize=True)
    symbol = clean(Image.open(DIR / "veyra-symbol.png"), min_area=1500)
    symbol = tint(symbol, (255, 255, 255))
    symbol.save(DIR / "veyra-symbol.png", optimize=True)

    # Farbiges V = linker Teil des Logos (bis vor das "V" des Schriftzugs).
    h = logo.height
    v_part = logo.crop((0, 0, int(logo.width * 0.39), h))
    write_ico(DIR / "veyra.ico", square(v_part), APP_SIZES)

    sym = square(symbol, margin=0.02)
    write_ico(DIR / "tray-light.ico", sym, TRAY_SIZES)
    write_ico(DIR / "tray-dark.ico", tint(sym, (32, 32, 32)), TRAY_SIZES)


if __name__ == "__main__":
    main()
