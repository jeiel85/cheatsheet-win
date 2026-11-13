"""Draws the app icon (rounded key cap with "Fn") into assets/app.ico and docs/assets/*.png.
Run: python tools/make-icon.py  (needs Pillow)."""
import os, sys
from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

def font(px):
    for name in ("segoeuib.ttf", "arialbd.ttf"):
        try:
            return ImageFont.truetype(os.path.join(os.environ.get("WINDIR", r"C:\Windows"), "Fonts", name), px)
        except OSError:
            pass
    return ImageFont.load_default()

def draw(size):
    s = size * 4  # supersample
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    pad = int(s * 0.06)
    r = int(s * 0.22)
    # key cap: darker base + lighter top face
    d.rounded_rectangle([pad, pad + int(s * 0.04), s - pad, s - pad], r, fill=(10, 60, 120, 255))
    d.rounded_rectangle([pad, pad, s - pad, s - pad - int(s * 0.06)], r, fill=(0, 120, 212, 255))
    f = font(int(s * 0.46))
    text = "Fn"
    box = d.textbbox((0, 0), text, font=f)
    w, h = box[2] - box[0], box[3] - box[1]
    cx, cy = s / 2, (pad + s - pad - int(s * 0.06)) / 2
    d.text((cx - w / 2 - box[0], cy - h / 2 - box[1]), text, font=f, fill=(255, 255, 255, 255))
    return img.resize((size, size), Image.LANCZOS)

sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
imgs = [draw(z) for z in sizes]
ico = os.path.join(ROOT, "assets", "app.ico")
imgs[-1].save(ico, format="ICO", sizes=[(z, z) for z in sizes], append_images=imgs[:-1])
docs = os.path.join(ROOT, "docs", "assets")
os.makedirs(docs, exist_ok=True)
draw(512).save(os.path.join(docs, "icon-512.png"))
draw(64).save(os.path.join(docs, "favicon-64.png"))
print("wrote", ico)
