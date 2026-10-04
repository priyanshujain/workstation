#!/usr/bin/env python3
"""Render the simple Margin Workstation monitor icon."""
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


def render():
    canvas = Image.new("RGBA", (px(SIZE), px(SIZE)), (0, 0, 0, 0))

    shadow = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    rounded(ImageDraw.Draw(shadow), (100, 112, 924, 936), 185, (0, 0, 0, 55))
    canvas.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(px(14))))
    rounded(ImageDraw.Draw(canvas), (100, 100, 924, 924), 185, (17, 17, 17, 255))
    draw = ImageDraw.Draw(canvas)
    light = (236, 230, 218, 255)
    rounded(draw, (228, 280, 796, 640), 48, light)
    rounded(draw, (260, 312, 764, 572), 16, (17, 17, 17, 255))
    rounded(draw, (474, 616, 550, 722), 16, light)
    rounded(draw, (418, 706, 606, 738), 16, light)

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
        master.save(OUT.with_name("MarginWorkstation.png"))
    print(f"wrote {OUT} ({OUT.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
