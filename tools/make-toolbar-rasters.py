#!/usr/bin/env python3
"""Rasterize assets/toolbar-icons/*.svg into 128x128 RGBA rasters.

The editable SVG sources are the Material icons the row draws. This script
pre-rasterizes each into a raw RGBA file of the same name (alpha carries the
shape; RGB is a neutral white that tint() replaces at runtime). The bundled
rasters are deliberately much larger than any on-screen size: the Rust side
area-averages them down to the exact pixel size the row needs, so glyphs stay
smooth at every DPI instead of being nearest-neighbour stretched.

Standard library only - scanline fill with fractional cell coverage and
nonzero winding, then a 4x supersample baked in by drawing at 4x and box
filtering down. Run from the crate root:

    python tools/make-toolbar-rasters.py
"""

import glob
import os
import re
import sys

# Source raster side; must match SRC_SIDE in src/toolbar_icons.rs.
SIDE = 128
# Supersample factor: fill at SIDE * SS, then box-filter down to SIDE.
SS = 4
# Quadratic bezier subdivision count per curve.
SEGMENTS = 16

TOKEN = re.compile(
    r"[MmZzLlHhVvQqTtCcSsAa]|[-+]?(?:\d*\.\d+|\d+\.?)(?:[eE][-+]?\d+)?"
)
VIEWBOX = re.compile(r'viewBox="([^"]+)"')
PATH_D = re.compile(r'\sd="([^"]+)"')


def tokenize(d):
    return TOKEN.findall(d)


def flatten_quad(p0, c, p1):
    pts = []
    for k in range(1, SEGMENTS + 1):
        t = k / SEGMENTS
        u = 1.0 - t
        pts.append(
            (
                u * u * p0[0] + 2 * u * t * c[0] + t * t * p1[0],
                u * u * p0[1] + 2 * u * t * c[1] + t * t * p1[1],
            )
        )
    return pts


def parse_path(d):
    """Parse SVG path data (M L H V Q T Z + relative forms) into contours."""
    toks = tokenize(d)
    i = 0
    cmd = None
    contours = []
    pts = None
    cur = (0.0, 0.0)
    start = (0.0, 0.0)
    prev_kind = None  # 'Q' if the previous curve was quadratic (for T)
    prev_ctrl = None

    def num():
        nonlocal i
        v = float(toks[i])
        i += 1
        return v

    while i < len(toks):
        if toks[i].isalpha():
            cmd = toks[i]
            i += 1
        if cmd is None:
            raise ValueError("path data does not start with a command")
        up = cmd.isupper()
        c = cmd.lower()
        if c == "m":
            x, y = num(), num()
            pt = (x, y) if up else (cur[0] + x, cur[1] + y)
            if pts:
                contours.append(pts)
            pts = [pt]
            cur = pt
            start = pt
            prev_kind = None
            cmd = "L" if up else "l"  # extra pairs after M are linetos
        elif c == "l":
            x, y = num(), num()
            pt = (x, y) if up else (cur[0] + x, cur[1] + y)
            if pts is None:
                raise ValueError("lineto without a current point")
            pts.append(pt)
            cur = pt
            prev_kind = None
        elif c == "h":
            x = num()
            pt = (x, cur[1]) if up else (cur[0] + x, cur[1])
            if pts is None:
                raise ValueError("h without a current point")
            pts.append(pt)
            cur = pt
            prev_kind = None
        elif c == "v":
            y = num()
            pt = (cur[0], y) if up else (cur[0], cur[1] + y)
            if pts is None:
                raise ValueError("v without a current point")
            pts.append(pt)
            cur = pt
            prev_kind = None
        elif c == "q":
            cx, cy, x, y = num(), num(), num(), num()
            ctrl = (cx, cy) if up else (cur[0] + cx, cur[1] + cy)
            pt = (x, y) if up else (cur[0] + x, cur[1] + y)
            if pts is None:
                raise ValueError("quadratic without a current point")
            pts.extend(flatten_quad(cur, ctrl, pt))
            cur = pt
            prev_ctrl = ctrl
            prev_kind = "Q"
        elif c == "t":
            x, y = num(), num()
            pt = (x, y) if up else (cur[0] + x, cur[1] + y)
            if pts is None:
                raise ValueError("smooth quadratic without a current point")
            if prev_kind in ("Q", "T") and prev_ctrl is not None:
                # Reflect the previous control point about the current point.
                ctrl = (2 * cur[0] - prev_ctrl[0], 2 * cur[1] - prev_ctrl[1])
            else:
                ctrl = cur
            pts.extend(flatten_quad(cur, ctrl, pt))
            cur = pt
            prev_ctrl = ctrl
            prev_kind = "T"
        elif c == "z":
            if pts:
                contours.append(pts)
            pts = None
            cur = start
            prev_kind = None
        else:
            raise ValueError("unsupported path command %r" % cmd)
    if pts:
        contours.append(pts)
    return contours


def render(contours, minx, miny, vbw, vbh):
    """Nonzero-winding scanline fill at SIDE*SS, box-filtered down to SIDE."""
    big = SIDE * SS
    scale = big / vbw
    yscale = big / vbh

    edges = []
    for pts in contours:
        if len(pts) < 2:
            continue
        tr = [
            ((p[0] - minx) * scale, (p[1] - miny) * yscale) for p in pts
        ]
        n = len(tr)
        for k in range(n):
            x0, y0 = tr[k]
            x1, y1 = tr[(k + 1) % n]
            if y0 != y1:
                edges.append((x0, y0, x1, y1))

    # dst[dy][dx] accumulates coverage in 0..1 units.
    dst = [[0.0] * SIDE for _ in range(SIDE)]
    by_row = {}
    for (x0, y0, x1, y1) in edges:
        lo, hi = (y0, y1) if y0 < y1 else (y1, y0)
        for row in range(int(lo), int(hi)):
            by_row.setdefault(row, []).append((x0, y0, x1, y1))

    for row, es in by_row.items():
        yc = row + 0.5
        xs = []
        for (x0, y0, x1, y1) in es:
            if y0 <= yc < y1 or y1 <= yc < y0:
                t = (yc - y0) / (y1 - y0)
                xs.append((x0 + t * (x1 - x0), 1 if y1 > y0 else -1))
        if not xs:
            continue
        xs.sort()
        winding = 0
        span_start = None
        spans = []
        for (x, d) in xs:
            was = winding
            winding += d
            if was == 0 and winding != 0:
                span_start = x
            elif was != 0 and winding == 0 and span_start is not None:
                spans.append((span_start, x))
                span_start = None
        if span_start is not None and xs:
            spans.append((span_start, xs[-1][0]))

        dy = row // SS
        if dy >= SIDE:
            continue
        for (a, b) in spans:
            if b <= 0 or a >= big:
                continue
            a = max(a, 0.0)
            b = min(b, float(big))
            c = int(a)
            while c < b:
                lo = max(a, float(c))
                hi = min(b, float(c + 1))
                if hi > lo:
                    dx = c // SS
                    if dx < SIDE:
                        dst[dy][dx] += (hi - lo) / (SS * SS)
                c += 1

    alpha = [
        min(255, int(v * 255 + 0.5)) for row in dst for v in row
    ]
    return alpha


def rasterize(svg_path):
    text = open(svg_path, encoding="utf-8").read()
    m = VIEWBOX.search(text)
    if not m:
        raise ValueError("%s has no viewBox" % svg_path)
    minx, miny, vbw, vbh = (float(v) for v in m.group(1).split())
    d = PATH_D.search(text)
    if not d:
        raise ValueError("%s has no path" % svg_path)
    contours = parse_path(d.group(1))
    return render(contours, minx, miny, vbw, vbh)


def box_down(src, src_side, dst_side):
    """Tiny box downsample of a 0..255 int list, for the sanity report."""
    k = src_side // dst_side
    out = []
    for dy in range(dst_side):
        for dx in range(dst_side):
            s = 0
            for yy in range(dy * k, (dy + 1) * k):
                base = yy * src_side
                for xx in range(dx * k, (dx + 1) * k):
                    s += src[base + xx]
            out.append(s // (k * k))
    return out


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    pattern = os.path.join(here, "..", "assets", "toolbar-icons", "*.svg")
    svgs = sorted(glob.glob(pattern))
    if not svgs:
        print("no SVGs found at %s" % pattern, file=sys.stderr)
        return 1
    for svg in svgs:
        alpha = rasterize(svg)
        rgba = bytearray()
        opaque = 0
        partial = 0
        for a in alpha:
            if a == 255:
                opaque += 1
            elif a > 0:
                partial += 1
            rgba += bytes((255, 255, 255, a))
        out = os.path.splitext(svg)[0] + ".rgba"
        old_note = ""
        if os.path.exists(out):
            old = open(out, "rb").read()
            if len(old) == 24 * 24 * 4:
                old_a = list(old[3::4])
                new_a = box_down(alpha, SIDE, 24)
                diff = sum(abs(x - y) for x, y in zip(old_a, new_a)) / (24 * 24)
                old_note = "; vs old 24px mean |dalpha|=%.1f" % diff
        with open(out, "wb") as f:
            f.write(rgba)
        print(
            "%s -> %dx%d (%.0f%% opaque, %.0f%% antialiased edges%s)"
            % (
                os.path.basename(out),
                SIDE,
                SIDE,
                100.0 * opaque / (SIDE * SIDE),
                100.0 * partial / (SIDE * SIDE),
                old_note,
            )
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
