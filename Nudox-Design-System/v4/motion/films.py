"""motion/films.py — film strips of every transition (8–12 frames each) into ../shots/motion/.

python3 films.py [demo ...] [--reduced] [--int]
Each frame is a headless-Chrome still of Motion.html?demo=<id>&t=<ms> (exact: the board is a pure function
of t). Frames are cropped to the demo's crop rect (if any) and laid out in a strip with their times.
"""
import concurrent.futures as cf
import html
import json
import os
import re
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(os.path.dirname(HERE), "shots", "motion")
CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
BASE = "http://127.0.0.1:47811/v4/motion/Motion.html"
FONT = os.path.join(os.path.dirname(os.path.dirname(HERE)), "fonts", "GeistMono[wght].ttf")


def demos():
    dom = subprocess.run([CHROME, "--headless=new", "--disable-gpu", "--virtual-time-budget=3000", "--dump-dom", BASE + "?list=1"],
                         capture_output=True, text=True).stdout
    m = re.search(r'<pre id="list">(.*?)</pre>', dom, re.S)
    return json.loads(html.unescape(m.group(1)))


def snap(d, t, extra, path):
    url = f"{BASE}?demo={d['id']}&t={t}{extra}"
    for _ in range(3):
        try:
            subprocess.run([CHROME, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-device-scale-factor=1",
                            f"--window-size={d['w']},{d['h']}", "--virtual-time-budget=3000", f"--screenshot={path}", url],
                           capture_output=True, timeout=45)
            if os.path.exists(path):
                return path
        except subprocess.TimeoutExpired:
            pass
    return path


def strip(d, frames, out, label):
    crop = d.get("crop")
    ims = []
    for t, p in frames:
        im = Image.open(p).convert("RGB")
        if crop:
            x, y, w, h = crop
            im = im.crop((x, y, x + w, y + h))
        ims.append((t, im))
    w, h = ims[0][1].size
    cols = 4 if w <= 520 else 3 if w <= 800 else 2
    scale = min(1.0, 1600 / (cols * w))
    fw, fh = int(w * scale), int(h * scale)
    rows = (len(ims) + cols - 1) // cols
    pad, cap = 10, 22
    sheet = Image.new("RGB", (cols * (fw + pad) + pad, 40 + rows * (fh + cap + pad) + pad), (2, 6, 14))
    dr = ImageDraw.Draw(sheet)
    f1 = ImageFont.truetype(FONT, 15)
    f2 = ImageFont.truetype(FONT, 12)
    dr.text((pad, 12), label, fill=(210, 217, 229), font=f1)
    for k, (t, im) in enumerate(ims):
        r, c = divmod(k, cols)
        x, y = pad + c * (fw + pad), 40 + r * (fh + cap + pad)
        sheet.paste(im.resize((fw, fh), Image.LANCZOS), (x, y))
        dr.text((x + 2, y + fh + 4), f"t={t} ms", fill=(116, 129, 154), font=f2)
    sheet.save(out)
    return out


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    reduced, inter = "--reduced" in sys.argv, "--int" in sys.argv
    ds = [d for d in demos() if not args or d["id"] in args]
    os.makedirs(OUT, exist_ok=True)
    tmp = tempfile.mkdtemp(prefix="films-")
    extra = ("&reduced=1" if reduced else "") + ("&int=1" if inter else "")
    suffix = ("-reduced" if reduced else "") + ("-int" if inter else "")
    jobs = []
    with cf.ThreadPoolExecutor(4) as ex:
        for d in ds:
            for t in d["film"]:
                p = os.path.join(tmp, f"{d['id']}{suffix}-{t:05d}.png")
                jobs.append((d, t, ex.submit(snap, d, t, extra, p)))
        cf.wait([j[2] for j in jobs])
    for d in ds:
        frames = [(t, f.result()) for dd, t, f in jobs if dd is d]
        label = f"{d['id']}{suffix} — {d['relation']}".replace("⌥", "alt").replace("⌘", "cmd")
        print(strip(d, frames, os.path.join(OUT, f"{d['id']}{suffix}.png"), label))


if __name__ == "__main__":
    main()
