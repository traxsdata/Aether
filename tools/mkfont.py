#!/usr/bin/env python3
"""Rasterize the NewOS "Facet" font into include/font_data.h.

Facet is an original stroke font designed for NewOS: every glyph is a set of
polylines on a 4 x 10 grid (x 0..4, cap height y 1..7, x-height y 3,
descender y 9). Curves are drawn as chamfered corners, which gives the
faceted, slightly futuristic look. Glyphs are rasterized with 8x8
supersampling into 8-bit alpha masks at several pixel sizes.

Usage: tools/mkfont.py > include/font_data.h
"""

GLYPHS = {
    " ": "",
    "!": "2,1 2,5|2,6.95 2,7",
    '"': "1,1 1,2.5|3,1 3,2.5",
    "#": "1,2 1,6.5|3,2 3,6.5|0,3.5 4,3.5|0,5 4,5",
    "$": "4,2 3,1.3 1,1.3 0,2 0,3 1,4 3,4 4,5 4,6 3,6.7 1,6.7 0,6|2,0.3 2,7.7",
    "%": "0,7 4,1|0,1 1,1 1,2 0,2 0,1|3,6 4,6 4,7 3,7 3,6",
    "&": "4,7 1,3.5 1,2 1.5,1 2.5,1 3,2 3,2.5 0,5 0,6 1,7 2.5,7 4,5",
    "'": "2,1 2,2.5",
    "(": "3,0.5 1.5,2 1.5,6 3,7.5",
    ")": "1,0.5 2.5,2 2.5,6 1,7.5",
    "*": "2,2 2,6|0.5,3 3.5,5|3.5,3 0.5,5",
    "+": "2,2.5 2,6.5|0,4.5 4,4.5",
    ",": "2,6.5 2,7.5 1.3,8.5",
    "-": "0.5,4.5 3.5,4.5",
    ".": "1.95,7 2.05,7",
    "/": "4,1 0,7",
    "0": "1,1 3,1 4,2 4,6 3,7 1,7 0,6 0,2 1,1|1.2,5.8 2.8,2.2",
    "1": "1,2 2,1 2,7|1,7 3,7",
    "2": "0,2 1,1 3,1 4,2 4,3 0,7 4,7",
    "3": "0,1 4,1 2,3.5 3,3.5 4,4.5 4,6 3,7 1,7 0,6",
    "4": "3,7 3,1 0,5 4,5",
    "5": "4,1 0,1 0,3.5 3,3.5 4,4.5 4,6 3,7 0,7",
    "6": "3,1 1,1 0,2 0,6 1,7 3,7 4,6 4,4.5 3,3.5 0,3.5",
    "7": "0,1 4,1 1.5,7",
    "8": "1,1 3,1 4,2 4,2.8 3,3.8 1,3.8 0,2.8 0,2 1,1|1,3.8 0,4.8 0,6 1,7 3,7 4,6 4,4.8 3,3.8",
    "9": "4,3.5 1,3.5 0,2.5 0,2 1,1 3,1 4,2 4,6 3,7 1,7",
    ":": "2,3.45 2,3.55|2,6.95 2,7",
    ";": "2,3.45 2,3.55|2,6.5 2,7.5 1.3,8.5",
    "<": "4,2 0,4.5 4,7",
    "=": "0,3.5 4,3.5|0,5.5 4,5.5",
    ">": "0,2 4,4.5 0,7",
    "?": "0,2 1,1 3,1 4,2 4,3 2,4.5 2,5|2,6.95 2,7",
    "@": "3,5 3,3 1.5,3 1,3.5 1,5 3,5 4,4.5 4,2 3,1 1,1 0,2 0,6 1,7 4,7",
    "A": "0,7 0,2 1,1 3,1 4,2 4,7|0,4.5 4,4.5",
    "B": "0,7 0,1 3,1 4,2 4,3 3,4 0,4|3,4 4,5 4,6 3,7 0,7",
    "C": "4,2 3,1 1,1 0,2 0,6 1,7 3,7 4,6",
    "D": "0,1 0,7 3,7 4,6 4,2 3,1 0,1",
    "E": "4,1 0,1 0,7 4,7|0,4 3,4",
    "F": "4,1 0,1 0,7|0,4 3,4",
    "G": "4,2 3,1 1,1 0,2 0,6 1,7 3,7 4,6 4,4 2,4",
    "H": "0,1 0,7|4,1 4,7|0,4 4,4",
    "I": "1,1 3,1|2,1 2,7|1,7 3,7",
    "J": "1,1 4,1|3,1 3,6 2,7 1,7 0,6",
    "K": "0,1 0,7|4,1 0,4.5|1.3,3.4 4,7",
    "L": "0,1 0,7 4,7",
    "M": "0,7 0,1 2,4 4,1 4,7",
    "N": "0,7 0,1 4,7 4,1",
    "O": "1,1 3,1 4,2 4,6 3,7 1,7 0,6 0,2 1,1",
    "P": "0,7 0,1 3,1 4,2 4,3 3,4 0,4",
    "Q": "1,1 3,1 4,2 4,6 3,7 1,7 0,6 0,2 1,1|2.5,5.5 4,7.5",
    "R": "0,7 0,1 3,1 4,2 4,3 3,4 0,4|2,4 4,7",
    "S": "4,2 3,1 1,1 0,2 0,3 1,4 3,4 4,5 4,6 3,7 1,7 0,6",
    "T": "0,1 4,1|2,1 2,7",
    "U": "0,1 0,6 1,7 3,7 4,6 4,1",
    "V": "0,1 2,7 4,1",
    "W": "0,1 1,7 2,3.5 3,7 4,1",
    "X": "0,1 4,7|4,1 0,7",
    "Y": "0,1 2,4 4,1|2,4 2,7",
    "Z": "0,1 4,1 0,7 4,7",
    "[": "3,0.5 1.5,0.5 1.5,7.5 3,7.5",
    "\\": "0,1 4,7",
    "]": "1,0.5 2.5,0.5 2.5,7.5 1,7.5",
    "^": "0,3 2,1 4,3",
    "_": "0,8.5 4,8.5",
    "`": "1.5,1 2.5,2",
    "a": "1,3 3,3 4,4 4,7|4,5 1,5 0,5.7 0,6.3 1,7 4,7",
    "b": "0,1 0,7 3,7 4,6 4,4 3,3 0,3",
    "c": "4,3 1,3 0,4 0,6 1,7 4,7",
    "d": "4,1 4,7 1,7 0,6 0,4 1,3 4,3",
    "e": "0,5 4,5 4,4 3,3 1,3 0,4 0,6 1,7 4,7",
    "f": "4,1 2.5,1 1.5,2 1.5,7|0,3 3.5,3",
    "g": "4,7 1,7 0,6 0,4 1,3 4,3 4,8 3,9 0,9",
    "h": "0,1 0,7|0,3 3,3 4,4 4,7",
    "i": "1,3 2,3 2,7|1,7 3,7|2,1.45 2,1.55",
    "j": "2,3 3,3 3,8 2,9 0,9|3,1.45 3,1.55",
    "k": "0,1 0,7|4,3 0,5.5|1.3,4.7 4,7",
    "l": "1,1 2,1 2,6 3,7 4,7",
    "m": "0,7 0,3 3,3 4,4 4,7|2,3 2,7",
    "n": "0,7 0,3 3,3 4,4 4,7",
    "o": "1,3 3,3 4,4 4,6 3,7 1,7 0,6 0,4 1,3",
    "p": "0,9 0,3 3,3 4,4 4,6 3,7 0,7",
    "q": "4,9 4,3 1,3 0,4 0,6 1,7 4,7",
    "r": "0,7 0,3|0,4 1,3 4,3",
    "s": "4,3 1,3 0,4 1,5 3,5 4,6 3,7 0,7",
    "t": "1,1 1,6 2,7 4,7|0,3 3,3",
    "u": "0,3 0,6 1,7 4,7 4,3",
    "v": "0,3 2,7 4,3",
    "w": "0,3 1,7 2,4.5 3,7 4,3",
    "x": "0,3 4,7|4,3 0,7",
    "y": "0,3 0,6 1,7 4,7|4,3 4,8 3,9 0,9",
    "z": "0,3 4,3 0,7 4,7",
    "{": "3,0.5 2,1 2,3.5 1,4 2,4.5 2,7 3,7.5",
    "|": "2,0.5 2,8.5",
    "}": "1,0.5 2,1 2,3.5 3,4 2,4.5 2,7 1,7.5",
    "~": "0,5 1,4 3,5 4,4",
    # Beyond ASCII: the letters in the OS name. Rendered from UTF-8 by gfx.c.
    "\u00e6": "0,3 1.6,3 2,3.6 2,6.4 1.6,7 0.5,7 0,6.4 0,5.6 0.5,5 2,5|2,5 4,5 4,3.6 3.5,3 2.5,3 2,3.6|2,6.4 2.5,7 4,7",
    "\u00c6": "0,7 0,2 1,1 4,1|0,4.5 2,4.5|2,1 2,7 4,7|2,4 3.5,4",
}
EXTRA = ["\u00e6", "\u00c6"]  # glyph indices FONT_COUNT and FONT_COUNT + 1

# (name, cell width, cell height, stroke width in px)
SIZES = [("small", 8, 16, 1.15), ("medium", 12, 24, 1.7), ("large", 20, 40, 2.8)]
SS = 8  # supersampling per axis


def parse(spec):
    lines = []
    for part in filter(None, spec.split("|")):
        pts = [tuple(map(float, p.split(","))) for p in part.split()]
        lines.append(pts)
    return lines


def seg_dist2(px, py, ax, ay, bx, by):
    dx, dy = bx - ax, by - ay
    l2 = dx * dx + dy * dy
    t = 0.0 if l2 == 0 else max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / l2))
    qx, qy = ax + t * dx - px, ay + t * dy - py
    return qx * qx + qy * qy


def rasterize(spec, w, h, stroke):
    pad = w * 0.19
    sx = (w - 2 * pad) / 4.0
    sy = h / 10.0 * 0.93
    oy = h * 0.035
    segs = []
    for pts in parse(spec):
        pts = [(pad + x * sx, oy + y * sy) for x, y in pts]
        if len(pts) == 1:
            pts = pts * 2
        segs += [(a[0], a[1], b[0], b[1]) for a, b in zip(pts, pts[1:])]
    r2 = (stroke / 2) ** 2
    out = []
    for y in range(h):
        row = []
        for x in range(w):
            hits = 0
            for j in range(SS):
                for i in range(SS):
                    px, py = x + (i + 0.5) / SS, y + (j + 0.5) / SS
                    if any(seg_dist2(px, py, *s) <= r2 for s in segs):
                        hits += 1
            row.append(min(255, round(hits * 255 * 1.15 / (SS * SS))))
        out.append(row)
    return out


def main():
    chars = [chr(c) for c in range(0x20, 0x7F)]
    missing = [c for c in chars + EXTRA if c not in GLYPHS]
    assert not missing, missing
    print("/* Generated by tools/mkfont.py: the NewOS \"Facet\" font, an original design. */")
    print("/* Do not edit by hand; change tools/mkfont.py and regenerate. */")
    print("#pragma once")
    print("#include <stdint.h>")
    print("#define FONT_FIRST 0x20")
    print("#define FONT_LAST 0x7E")
    print("#define FONT_COUNT (FONT_LAST - FONT_FIRST + 1)")
    print("#define FONT_GLYPH_AE_LOWER FONT_COUNT /* U+00E6 */")
    print("#define FONT_GLYPH_AE_UPPER (FONT_COUNT + 1) /* U+00C6 */")
    print("#define FONT_TOTAL (FONT_COUNT + 2)")
    for name, w, h, stroke in SIZES:
        print(f"#define FONT_{name.upper()}_W {w}")
        print(f"#define FONT_{name.upper()}_H {h}")
        print(f"static const uint8_t font_{name}[FONT_TOTAL][{h}][{w}] = {{")
        for c in chars + EXTRA:
            rows = rasterize(GLYPHS[c], w, h, stroke)
            body = ",".join("{" + ",".join(str(v) for v in r) + "}" for r in rows)
            label = {"\\": "backslash", "*": "star", "/": "slash"}.get(c, c if c.isascii() else f"U+{ord(c):04X}")
            print(f"    {{{body}}}, /* {label} */")
        print("};")


main()
