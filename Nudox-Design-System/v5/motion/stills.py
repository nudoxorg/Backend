"""v5/motion/stills.py: storyboard strips of the signature moves into ../shots/motion/.

python3 stills.py [demo ...] [--reduced]
Each frame is a headless-Chrome still of Signature.html?demo=<id>&t=<ms> (exact: the board is a
pure function of t). Frames are laid out two rows by N/2 with their times.
"""
import concurrent.futures as cf
import os
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(os.path.dirname(HERE), "shots", "motion")
CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
BASE = "http://127.0.0.1:47811/v5/motion/Signature.html"
FONT = os.path.join(os.path.dirname(os.path.dirname(HERE)), "fonts", "GeistMono[wght].ttf")

TIMES = {
    "open": [-1, 0, 40, 80, 120, 160, 240, 320],
    "close": [0, 40, 80, 120, 160, 240, 320, 700],
    "fold": [0, 40, 80, 120, 160, 240, 320, 480],
    "unfold": [0, 160, 240, 280, 320, 360, 400, 480],
    "peek": [0, 350, 390, 430, 470, 510, 590, 1020],
}
LABEL = {-1: "before (pointer on the row)"}


def snap(demo, t, extra, path):
    url = f"{BASE}?demo={demo}&t={t}{extra}"
    for _ in range(3):
        try:
            subprocess.run([CHROME, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-device-scale-factor=1",
                            "--window-size=1440,900", "--virtual-time-budget=2000", f"--screenshot={path}", url],
                           capture_output=True, timeout=45)
            if os.path.exists(path):
                return path
        except subprocess.TimeoutExpired:
            pass
    return path


def strip(frames, out, title):
    ims = [(t, Image.open(p).convert("RGB")) for t, p in frames]
    w, h = ims[0][1].size
    cols = 4
    scale = 0.5
    fw, fh = int(w * scale), int(h * scale)
    rows = (len(ims) + cols - 1) // cols
    pad, cap, head = 12, 26, 40
    sheet = Image.new("RGB", (cols * (fw + pad) + pad, head + rows * (fh + cap + pad) + pad), (3, 8, 20))
    draw = ImageDraw.Draw(sheet)
    font = ImageFont.truetype(FONT, 18)
    draw.text((pad, 10), title, fill=(210, 217, 229), font=font)
    for k, (t, im) in enumerate(ims):
        x = pad + (k % cols) * (fw + pad)
        y = head + (k // cols) * (fh + cap + pad)
        sheet.paste(im.resize((fw, fh), Image.LANCZOS), (x, y + cap))
        draw.text((x, y + 4), LABEL.get(t, f"t = {t} ms"), fill=(154, 166, 186), font=font)
    sheet.save(out)
    print(out, sheet.size)


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    reduced = "--reduced" in sys.argv
    demos = args or list(TIMES)
    os.makedirs(OUT, exist_ok=True)
    extra = "&reduced=1" if reduced else ""
    with tempfile.TemporaryDirectory() as tmp:
        jobs = {}
        with cf.ThreadPoolExecutor(6) as pool:
            for d in demos:
                for t in TIMES[d]:
                    path = os.path.join(tmp, f"{d}-{t}.png")
                    jobs[(d, t)] = pool.submit(snap, d, t, extra, path)
        for d in demos:
            frames = [(t, jobs[(d, t)].result()) for t in TIMES[d]]
            name = f"sig-{d}{'-reduced' if reduced else ''}.png"
            strip(frames, os.path.join(OUT, name), f"{d}{' (reduced motion)' if reduced else ''}")


if __name__ == "__main__":
    main()
