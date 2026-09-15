#!/usr/bin/env python3
"""Generate the plugin's icons from one set of glyph definitions.

Two outputs, so the shapes can never drift apart:

  com.biviale.qbzconnector.sdPlugin/icons/*.svg   full tiles, referenced by the
                                               manifest and shown when the
                                               album tint is off
  assets/glyphs/*.png                          white-on-transparent masks,
                                               embedded in the binary and
                                               composited over the album
                                               colour when the tint is on

Run after changing any glyph:  python3 scripts/make_icons.py
Needs rsvg-convert (librsvg) for the PNG masks.
"""

import pathlib
import shutil
import subprocess
import sys

BASE = "#0F3D5C"  # deep Qobuz blue, the family colour
ACCENT = "#B3123C"  # crimson, reserved for an active favourite
MUTED = "#2A2E35"  # slate, for "unavailable" and muted

ROOT = pathlib.Path(__file__).resolve().parent.parent
TILES = ROOT / "com.biviale.qbzconnector.sdPlugin" / "icons"
GLYPHS = ROOT / "assets" / "glyphs"

# Every shape is drawn on a 128x128 canvas, centred on (64, 64).
PLAY = '<path d="M48 34 L48 94 L96 64 Z"/>'
PAUSE = '<rect x="44" y="36" width="14" height="56" rx="3"/><rect x="70" y="36" width="14" height="56" rx="3"/>'
STOP = '<rect x="42" y="42" width="44" height="44" rx="5"/>'
NEXT = '<path d="M38 36 L38 92 L76 64 Z"/><rect x="80" y="36" width="12" height="56" rx="3"/>'
PREV = '<path d="M90 36 L90 92 L52 64 Z"/><rect x="36" y="36" width="12" height="56" rx="3"/>'

CONE = '<path d="M30 52 L46 52 L66 32 L66 96 L46 76 L30 76 Z"/>'
WAVE_1 = '<path d="M76 54 a14 14 0 0 1 0 20" fill="none" stroke="#ffffff" stroke-width="7" stroke-linecap="round"/>'
WAVE_2 = '<path d="M86 44 a26 26 0 0 1 0 40" fill="none" stroke="#ffffff" stroke-width="7" stroke-linecap="round"/>'
CROSS = (
    '<path d="M78 50 L104 78" stroke="#ffffff" stroke-width="8" stroke-linecap="round"/>'
    '<path d="M104 50 L78 78" stroke="#ffffff" stroke-width="8" stroke-linecap="round"/>'
)
PLUS = (
    '<path d="M91 48 L91 80" stroke="#ffffff" stroke-width="8" stroke-linecap="round"/>'
    '<path d="M75 64 L107 64" stroke="#ffffff" stroke-width="8" stroke-linecap="round"/>'
)
MINUS = '<path d="M75 64 L107 64" stroke="#ffffff" stroke-width="8" stroke-linecap="round"/>'

HEART_PATH = (
    "M64 98 C64 98 22 74 22 48 C22 34 33 25 45 25 C54 25 61 30 64 36 "
    "C67 30 74 25 83 25 C95 25 106 34 106 48 C106 74 64 98 64 98 Z"
)
HEART_FULL = f'<path d="{HEART_PATH}"/>'
HEART_LINE = f'<path d="{HEART_PATH}" fill="none" stroke="#ffffff" stroke-width="8" stroke-linejoin="round"/>'

NOTE = (
    '<path d="M54 84 a13 13 0 1 0 13 13 L67 46 L96 38 L96 72 a13 13 0 1 0 13 13 L109 22 '
    'a4 4 0 0 0-5-4 L60 30 a4 4 0 0 0-3 4 Z" transform="translate(-16,2)"/>'
)
SEEK_FWD = (
    '<path d="M44 40 L44 88 L72 64 Z"/><path d="M72 40 L72 88 L100 64 Z"/>'
    '<rect x="28" y="40" width="10" height="48" rx="3"/>'
)
SEEK_BACK = (
    '<path d="M84 40 L84 88 L56 64 Z"/><path d="M56 40 L56 88 L28 64 Z"/>'
    '<rect x="90" y="40" width="10" height="48" rx="3"/>'
)

# name -> (glyph markup, tile background)
ICONS = {
    "play": (PLAY, BASE),
    "pause": (PAUSE, BASE),
    "stop": (STOP, BASE),
    "next": (NEXT, BASE),
    "previous": (PREV, BASE),
    "playpause": (PLAY, BASE),
    "icon": (NOTE, BASE),
    "nowplaying": (NOTE, BASE),
    "offline": (NOTE, MUTED),
    "seek": (SEEK_FWD, BASE),
    "seek_backwards": (SEEK_BACK, BASE),
    "volume": (CONE + WAVE_1 + WAVE_2, BASE),
    "volume_up": (CONE + PLUS, BASE),
    "volume_down": (CONE + MINUS, BASE),
    "volume_set": (CONE + WAVE_1, BASE),
    "muted": (CONE + CROSS, MUTED),
    "favorite_off": (HEART_LINE, BASE),
    "favorite_on": (HEART_FULL, ACCENT),
    "favorite_disabled": (HEART_LINE, MUTED),
}

# The masks the binary embeds. Tiles that differ only by background colour
# share one mask, because the tint supplies the background. `favorite_on` has
# none: the favourited state keeps its crimson tile, because that colour means
# something the album must not overrule.
MASKS = [
    "play", "pause", "stop", "next", "previous", "nowplaying",
    "seek", "seek_backwards", "volume", "volume_up", "volume_down",
    "volume_set", "muted", "favorite_off",
]


def svg(glyph: str, background: str | None) -> str:
    fill = f'<rect width="128" height="128" fill="{background}"/>' if background else ""
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" '
        f'width="128" height="128">{fill}'
        f'<g fill="#ffffff" stroke="none">{glyph}</g></svg>'
    )


def main() -> int:
    if not shutil.which("rsvg-convert"):
        print("rsvg-convert not found; install librsvg", file=sys.stderr)
        return 1

    TILES.mkdir(parents=True, exist_ok=True)
    GLYPHS.mkdir(parents=True, exist_ok=True)

    for name, (glyph, background) in ICONS.items():
        (TILES / f"{name}.svg").write_text(svg(glyph, background))

    for name in MASKS:
        glyph, _ = ICONS[name]
        source = GLYPHS / f"{name}.svg"
        source.write_text(svg(glyph, None))
        # Rendered at 2x the key size, matching the cover art path.
        subprocess.run(
            ["rsvg-convert", "-w", "144", "-h", "144", str(source),
             "-o", str(GLYPHS / f"{name}.png")],
            check=True,
        )
        source.unlink()

    print(f"{len(ICONS)} tiles -> {TILES}")
    print(f"{len(MASKS)} masks -> {GLYPHS}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
