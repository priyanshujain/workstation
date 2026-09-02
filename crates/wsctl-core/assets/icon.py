#!/usr/bin/env python3
"""Renders Workstation.icns, the icon of the app bundle that carries the launchd jobs.

    python3 crates/wsctl-core/assets/icon.py

Needs Pillow and macOS's iconutil. The drawing is a monitor with a shell prompt
on the standard macOS icon grid: an 824px rounded square centred on a 1024px
canvas, so it sits level with Apple's own icons in the Dock and Login Items.
"""
import shutil
import subprocess
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

S = 4  # supersampling factor
SIZE = 1024
OUT = Path(__file__).with_name("Workstation.icns")


def px(v):
    return int(round(v * S))


def rounded(draw, box, radius, fill):
    draw.rounded_rectangle([px(v) for v in box], radius=px(radius), fill=fill)


def gradient(box, top, bottom, radius):
    x0, y0, x1, y1 = (px(v) for v in box)
    w, h = x1 - x0, y1 - y0
    ramp = Image.linear_gradient("L").resize((w, h))
    layer = Image.composite(Image.new("RGB", (w, h), bottom), Image.new("RGB", (w, h), top), ramp)
    mask = Image.new("L", (w, h), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, w - 1, h - 1], radius=px(radius), fill=255)
    layer.putalpha(mask)
    return layer, (x0, y0)


def render():
    canvas = Image.new("RGBA", (px(SIZE), px(SIZE)), (0, 0, 0, 0))

    # Soft shadow under the tile, like Apple's template.
    shadow = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    rounded(ImageDraw.Draw(shadow), (100, 112, 924, 936), 185, (0, 0, 0, 70))
    shadow = shadow.filter(ImageFilter.GaussianBlur(px(14)))
    canvas.alpha_composite(shadow)

    tile, at = gradient((100, 100, 924, 924), (62, 80, 108), (24, 32, 44), 185)
    canvas.alpha_composite(tile, at)

    draw = ImageDraw.Draw(canvas)
    light = (236, 240, 245, 255)

    # Monitor: bezel, screen, neck and base.
    rounded(draw, (218, 262, 806, 640), 40, light)
    rounded(draw, (250, 294, 774, 608), 22, (13, 19, 28, 255))
    draw.rectangle([px(486), px(640), px(538), px(704)], fill=light)
    rounded(draw, (378, 704, 646, 740), 18, light)

    # Prompt chevron and cursor block on the screen.
    green = (74, 222, 128, 255)
    draw.line(
        [(px(318), px(398)), (px(374), px(451)), (px(318), px(504))],
        fill=green, width=px(30), joint="curve",
    )
    for end in ((318, 398), (318, 504), (374, 451)):
        draw.ellipse([px(end[0] - 15), px(end[1] - 15), px(end[0] + 15), px(end[1] + 15)], fill=green)
    rounded(draw, (410, 396, 470, 506), 8, light)

    return canvas.resize((SIZE, SIZE), Image.LANCZOS)


def main():
    master = render()
    with tempfile.TemporaryDirectory() as tmp:
        iconset = Path(tmp) / "Workstation.iconset"
        iconset.mkdir()
        for base in (16, 32, 128, 256, 512):
            master.resize((base, base), Image.LANCZOS).save(iconset / f"icon_{base}x{base}.png")
            master.resize((base * 2, base * 2), Image.LANCZOS).save(iconset / f"icon_{base}x{base}@2x.png")
        subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(OUT)], check=True)
        master.save(Path(tmp) / "preview.png")
        shutil.copy(Path(tmp) / "preview.png", "/tmp/workstation-icon-preview.png")
    print(f"wrote {OUT} ({OUT.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
