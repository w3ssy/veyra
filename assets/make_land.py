"""Landmaske für den ISS Tracker erzeugen: assets/land-720x360.bin

Quelle: Natural Earth, "Land" 1:50m (gemeinfrei), z. B.
https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/geojson/ne_50m_land.geojson

Aufruf: python make_land.py ne_50m_land.geojson

Ausgabe: 720 × 360 Bit (0,5° je Zelle), zeilenweise von 90° N nach 90° S und von 180° W nach Osten,
das höchstwertige Bit eines Bytes zuerst; 1 = Land. Jede Zelle wird in ihrer Mitte geprüft
(gerade/ungerade Regel über alle Ringe eines Polygons, also mit Löchern).
"""

import json
import sys

W, H = 720, 360


def main(path):
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    polygons = []
    for feat in data["features"]:
        g = feat["geometry"]
        if g["type"] == "Polygon":
            polygons.append(g["coordinates"])
        elif g["type"] == "MultiPolygon":
            polygons.extend(g["coordinates"])
    grid = [[False] * W for _ in range(H)]
    for row in range(H):
        lat = 90.0 - (row + 0.5) * 180.0 / H
        for rings in polygons:
            xs = []
            for ring in rings:
                for (x0, y0), (x1, y1) in zip(ring, ring[1:] + ring[:1]):
                    if (y0 <= lat) != (y1 <= lat):
                        xs.append(x0 + (lat - y0) * (x1 - x0) / (y1 - y0))
            xs.sort()
            for a, b in zip(xs[0::2], xs[1::2]):
                c0 = max(0, int((a + 180.0) * W / 360.0 - 0.5) + 1)
                c1 = min(W - 1, int((b + 180.0) * W / 360.0 - 0.5))
                for col in range(c0, c1 + 1):
                    grid[row][col] = not grid[row][col]
    out = bytearray()
    for row in grid:
        for i in range(0, W, 8):
            byte = 0
            for bit in range(8):
                byte = (byte << 1) | (1 if row[i + bit] else 0)
            out.append(byte)
    with open("land-720x360.bin", "wb") as f:
        f.write(out)
    land = sum(sum(r) for r in grid)
    print(f"{len(out)} Bytes, Landanteil {land / (W * H):.1%}")


if __name__ == "__main__":
    main(sys.argv[1])
