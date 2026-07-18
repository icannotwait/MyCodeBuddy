"""Generate src-tauri/icons/tray-icon-template.png from Ikaros profile art.

This is a macOS menu-bar template image: the system reads only the alpha
channel and tints the opaque pixels to match light/dark/highlighted menu
bar appearance. RGB channels are black.

Re-run after replacing the source portrait:

    python src-tauri/icons/tray-icon-template.gen.py

Requires Pillow (pip install Pillow).
"""

from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw, ImageEnhance, ImageFilter, ImageOps

ROOT = Path(__file__).parent
# Prefer the original webp; fall back to the generated 512 app icon.
SOURCE_CANDIDATES = (
    ROOT / "Ikaros_Profile.webp",
    ROOT / "icon.png",
)
OUT = ROOT / "tray-icon-template.png"

# Square logical size for a circular portrait in the menu bar.
# Previous geometric mark was 52x44; 44x44 keeps the same height budget.
SIZE = 44
SCALE = 4  # supersample then Lanczos-downsample for smooth edges
S = SIZE * SCALE


def _load_source() -> Image.Image:
    for path in SOURCE_CANDIDATES:
        if path.exists():
            return Image.open(path).convert("RGBA")
    raise FileNotFoundError(
        "No source portrait found. Place Ikaros_Profile.webp next to this script."
    )


def _center_square(im: Image.Image) -> Image.Image:
    w, h = im.size
    side = min(w, h)
    left = (w - side) // 2
    top = (h - side) // 2
    return im.crop((left, top, left + side, top + side))


def _circular_mask(size: int) -> Image.Image:
    mask = Image.new("L", (size, size), 0)
    draw = ImageDraw.Draw(mask)
    # Small inset so the silhouette does not touch the menu-bar edge.
    pad = max(1, int(size * 0.04))
    draw.ellipse((pad, pad, size - pad - 1, size - pad - 1), fill=255)
    # Soft feather for retina-friendly edges.
    return mask.filter(ImageFilter.GaussianBlur(radius=max(1.0, size * 0.015)))


def _portrait_alpha(gray: Image.Image) -> Image.Image:
    """Map a light-subject portrait to menu-bar alpha.

    Bright face/hair becomes solid (high alpha); darker features (eyes,
    outlines) punch slightly so the face stays readable when the system
    tints the icon black/white.
    """
    # Punch up midtones so the face does not wash out at 22pt.
    g = ImageOps.autocontrast(gray, cutoff=1)
    g = ImageEnhance.Contrast(g).enhance(1.25)
    g = ImageEnhance.Brightness(g).enhance(1.05)

    # Floor keeps dark regions (eyes/hair tips) faintly present instead of
    # vanishing into transparent holes; ceiling keeps highlights solid.
    def map_px(p: int) -> int:
        # 28..255 — always a bit of ink inside the circle
        return 28 + (p * 227) // 255

    return g.point(map_px)


def main() -> None:
    src = _center_square(_load_source()).resize((S, S), Image.Resampling.LANCZOS)
    alpha = _portrait_alpha(src.convert("L"))
    alpha = Image.composite(alpha, Image.new("L", (S, S), 0), _circular_mask(S))

    zero = Image.new("L", (S, S), 0)
    img = Image.merge("RGBA", (zero, zero, zero, alpha)).resize(
        (SIZE, SIZE), Image.Resampling.LANCZOS
    )

    img.save(OUT, optimize=True)
    a = img.getchannel("A")
    nz = sum(1 for p in a.get_flattened_data() if p > 0)
    print(f"wrote {OUT} {img.size} nonzero_alpha={nz}/{SIZE * SIZE}")


if __name__ == "__main__":
    main()
