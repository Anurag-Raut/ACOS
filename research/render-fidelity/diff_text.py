#!/usr/bin/env python3
"""Compare candidate glyph *coverage* against the real ART screenshot.

Reference: "Hello ACOS" TextView at [42,317][231,368] in
fixtures/reference/hello-views/art-api34-x86_64/screenshot_initial.png.
The device screenshot carries a small colour cast, so coverage is recovered
from the green channel with detected background/foreground:
    a = (bg_g - g) / (bg_g - fg_g)
The candidate is placed into the reference frame by ink bbox, refined by an
integer shift search, then residual + optional per-candidate gamma calibration
are reported.

Usage:
  diff_text.py [--box x0,y0,x1,y1] [--gamma] candidate.png [candidate2.png ...]
"""
import sys
import numpy as np
from PIL import Image

REF = "fixtures/reference/hello-views/art-api34-x86_64/screenshot_initial.png"
REF_BOX = [30, 300, 300, 390]


def conv_gray(a):
    return a.astype(np.float64)


def detect_bg_fg(gray):
    flat = np.sort(gray.reshape(-1))
    bg = np.percentile(gray, 99.0)
    fg = np.percentile(gray, 0.3)
    return bg, fg


def cov_from_gray(gray, bg, fg):
    return np.clip((bg - gray) / max(bg - fg, 1e-6), 0.0, 1.0)


def cov_from_alpha(rgba):
    return rgba[:, :, 3].astype(np.float64) / 255.0


def ink_bbox(cov, thr=0.15):
    ys, xs = np.where(cov > thr)
    if len(xs) == 0:
        return None
    return (int(xs.min()), int(ys.min()), int(xs.max()) + 1, int(ys.max()) + 1)


def place(ref_shape, cand, ref_bbox, cand_bbox):
    H, W = ref_shape
    out = np.zeros(ref_shape)
    ox, oy = ref_bbox[0] - cand_bbox[0], ref_bbox[1] - cand_bbox[1]
    ch, cw = cand.shape
    xs0, xs1 = max(0, -ox), min(cw, W - ox)
    ys0, ys1 = max(0, -oy), min(ch, H - oy)
    if xs1 > xs0 and ys1 > ys0:
        out[ys0 + oy:ys1 + oy, xs0 + ox:xs1 + ox] = cand[ys0:ys1, xs0:xs1]
    return out


def best_shift(ref, cand, win=5):
    best = None
    H, W = ref.shape
    for dy in range(-win, win + 1):
        for dx in range(-win, win + 1):
            ys0, ys1 = max(0, dy), min(H, H + dy)
            xs0, xs1 = max(0, dx), min(W, W + dx)
            ry0, ry1 = max(0, -dy), min(H, H - dy)
            rx0, rx1 = max(0, -dx), min(W, W - dx)
            d = np.abs(ref[ys0:ys1, xs0:xs1] - cand[ry0:ry1, rx0:rx1]).mean()
            if best is None or d < best[0]:
                best = (d, dx, dy)
    return best


def shift_arr(a, dx, dy):
    H, W = a.shape
    out = np.zeros_like(a)
    ys0, ys1 = max(0, dy), min(H, H + dy)
    xs0, xs1 = max(0, dx), min(W, W + dx)
    cy0, cy1 = max(0, -dy), min(H, H - dy)
    cx0, cx1 = max(0, -dx), min(W, W - dx)
    if xs1 > xs0 and ys1 > ys0:
        out[ys0:ys1, xs0:xs1] = a[cy0:cy1, cx0:cx1]
    return out


def metrics(ref, cand):
    d = np.abs(ref - cand)
    return d.mean(), np.sqrt((d ** 2).mean()), d.max(), ref.sum(), cand.sum()


def compare(ref_cov, ref_bbox, cand_cov, label, do_gamma=False):
    H, W = ref_cov.shape
    cb = ink_bbox(cand_cov)
    if cb is None:
        print(f"{label:38s} EMPTY"); return None
    sc = place((H, W), cand_cov, ref_bbox, cb)
    _, dx, dy = best_shift(ref_cov, sc)
    sc = shift_arr(sc, dx, dy)
    m, rmse, mx, ink_r, ink_c = metrics(ref_cov, sc)
    extra = ""
    if do_gamma:
        best = None
        for g in np.arange(0.80, 1.81, 0.02):
            cg = np.clip(sc, 0, 1) ** g
            mm, rr, _, ir, ic = metrics(ref_cov, cg)
            if best is None or mm < best[0]:
                best = (mm, rr, g, ic)
        extra = f" | bestGamma={best[2]:.2f} mean|d|={best[0]:.5f} RMSE={best[1]:.5f} inkRatio={best[3]/max(ink_r,1):.3f}"
    pct = {t: 100.0 * (np.abs(ref_cov - sc) > t).sum() / ref_cov.size for t in (0.02, 0.05, 0.1, 0.2)}
    print(f"{label:38s} mean|d|={m:.5f} RMSE={rmse:.5f} max={mx:.3f} inkRatio={ink_c/max(ink_r,1):.3f} "
          f">.02={pct[0.02]:.2f}% >.05={pct[0.05]:.2f}% >.1={pct[0.1]:.2f}% >.2={pct[0.2]:.2f}% shift=({dx},{dy}){extra}")
    return (m, rmse, label)


def load_cov(path, bg, fg):
    im = Image.open(path)
    has_alpha = im.mode in ("RGBA", "LA") or (im.mode == "P" and "transparency" in im.info)
    if has_alpha:
        rgba = np.asarray(im.convert("RGBA"))
        if rgba[:, :, 3].min() < 255:
            return cov_from_alpha(rgba)
    g = np.asarray(im.convert("RGB"))[:, :, 1]
    if g.shape[0] > 200:
        g = g[REF_BOX[1]:REF_BOX[3], REF_BOX[0]:REF_BOX[2]]
    return cov_from_gray(g, bg, fg)


def main():
    args = sys.argv[1:]
    gamma = "--gamma" in args
    args = [a for a in args if a != "--gamma"]
    if args and args[0] == "--box":
        REF_BOX[:] = [int(x) for x in args[1].split(",")]
        args = args[2:]
    ref_img = np.asarray(Image.open(REF).convert("RGB"))
    reg = ref_img[REF_BOX[1]:REF_BOX[3], REF_BOX[0]:REF_BOX[2]]
    bg, fg = detect_bg_fg(reg[:, :, 1])
    ref_cov = cov_from_gray(reg[:, :, 1], bg, fg)
    print(f"reference box={REF_BOX} green bg={bg:.0f} fg={fg:.0f} "
          f"ink bbox={ink_bbox(ref_cov)} ink={ref_cov.sum():.0f}")
    results = []
    for p in args:
        r = compare(ref_cov, ink_bbox(ref_cov), load_cov(p, bg, fg), p, gamma)
        if r:
            results.append(r)
    if results:
        results.sort()
        print("\nranked by mean|d| (best first):")
        for m, rmse, lbl in results[:12]:
            print(f"  {m:.5f}  {rmse:.5f}  {lbl}")


if __name__ == "__main__":
    main()
