"""Draws the Oxyde icon (the splash screen's mark: an isometric part with a bore) and
writes it in the sizes the bundle needs.

    python packaging/make_icon.py

-> packaging/oxyde.ico (16-256 px, for the exe and the installer), packaging/oxyde.png
(512 px) and crates/oxyde-app/assets/icon-64.rgba (the window icon, raw RGBA).
Needs Pillow. The outputs are committed; run this again only to change the icon.
"""

import math
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
ACCENT = (79, 195, 247, 255)
FACE = (150, 168, 186)
BACKGROUND = ((34, 48, 63), (13, 20, 28))


def shade(color, k):
    return tuple(int(c * k) for c in color) + (255,)


def draw(size):
    # Drawn 4x larger, then reduced: smooth edges at every size.
    big = size * 4
    image = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    d = ImageDraw.Draw(image)
    radius = big * 0.2
    # A rounded navy tile with a vertical gradient.
    tile = Image.new("RGBA", (big, big))
    for y in range(big):
        t = y / (big - 1)
        c = tuple(int(a + (b - a) * t) for a, b in zip(*BACKGROUND)) + (255,)
        ImageDraw.Draw(tile).line([(0, y), (big, y)], fill=c)
    mask = Image.new("L", (big, big), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, big - 1, big - 1], radius, fill=255)
    image.paste(tile, (0, 0), mask)

    cx, cy = big / 2, big / 2 + big * 0.03
    s = big * 0.34
    dx, dy = s * 0.866, s * 0.5
    p = lambda x, y: (cx + x, cy + y)
    top = p(0, -s)
    width = max(2, int(big / 40))
    upper = [top, p(dx, -dy), p(0, 0), p(-dx, -dy)]
    left = [p(-dx, -dy), p(0, 0), p(0, s), p(-dx, dy)]
    right = [p(0, 0), p(dx, -dy), p(dx, dy), p(0, s)]
    for face, k in ((upper, 1.0), (left, 0.72), (right, 0.55)):
        d.polygon(face, fill=shade(FACE, k))
    for face in (upper, left, right):
        d.line(face + [face[0]], fill=ACCENT, width=width, joint="curve")
        # Round the corners, where the line joins leave notches.
        for x, y in face:
            r = width / 2
            d.ellipse([x - r, y - r, x + r, y + r], fill=ACCENT)
    # The bore on the top face.
    hx, hy = top[0], top[1] + s * 0.5
    ring = [
        (hx + math.cos(a) * dx * 0.42, hy + math.sin(a) * dy * 0.42)
        for a in (i / 64 * math.tau for i in range(64))
    ]
    d.polygon(ring, fill=shade(FACE, 0.3))
    d.line(ring + [ring[0]], fill=ACCENT, width=width)
    return image.resize((size, size), Image.LANCZOS)


def main():
    draw(512).save(ROOT / "packaging" / "oxyde.png")
    draw(256).save(
        ROOT / "packaging" / "oxyde.ico",
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    (ROOT / "crates" / "oxyde-app" / "assets" / "icon-64.rgba").write_bytes(draw(64).tobytes())


if __name__ == "__main__":
    main()
