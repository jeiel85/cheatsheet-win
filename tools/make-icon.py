"""Builds assets/app.ico and the website icons from the 1024 px sources in assets/icon-source/.

The sources were generated with Codex CLI's image generation (see the PR that added them):
  icon-1024.png        detailed icon (keycap + cheat-sheet card), used at 48 px and up
  icon-small-1024.png  simplified flat keycap that stays legible at 16-40 px (tray, title bar)

Run: python tools/make-icon.py   (needs Pillow)"""
import os
from PIL import Image

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "assets", "icon-source")
big = Image.open(os.path.join(SRC, "icon-1024.png")).convert("RGBA")
small = Image.open(os.path.join(SRC, "icon-small-1024.png")).convert("RGBA")


def scaled(img, size):
    # Two-step downscale keeps edges crisp at tiny sizes.
    mid = img.resize((size * 4, size * 4), Image.LANCZOS) if size * 4 < img.width else img
    return mid.resize((size, size), Image.LANCZOS)


sizes = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
frames = [scaled(small if s <= 40 else big, s) for s in sizes]
ico = os.path.join(ROOT, "assets", "app.ico")
frames[-1].save(ico, format="ICO", sizes=[(s, s) for s in sizes], append_images=frames[:-1])

docs = os.path.join(ROOT, "docs", "assets")
os.makedirs(docs, exist_ok=True)
scaled(big, 512).save(os.path.join(docs, "icon-512.png"), optimize=True)
scaled(small, 64).save(os.path.join(docs, "favicon-64.png"), optimize=True)
scaled(big, 180).save(os.path.join(docs, "apple-touch-icon.png"), optimize=True)
print("wrote", ico)
