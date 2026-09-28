"""films.py — filmstrips of the hand's two motions into ../shots/hand/film-<name>.png.

take: the first card lands in the hand from where you held it (Take → hand, MOTION.md).
drop: letting go of the middle card re-routes the recipe.
Each frame is a real still of Hand.html at ?t=<ms>; the crop shows the part that moves.
Also before-after.png: today's titlebar and foot over the calmed ones.
Run with the v4 server on :47811: python3 films.py [take] [drop] [before-after]
"""
import os, subprocess, sys, tempfile
from concurrent.futures import ThreadPoolExecutor
from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
SHOTS = os.path.join(HERE, "..", "shots", "hand")
TMP = tempfile.mkdtemp(prefix="hand-frames-")
CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
FILMS = {
    "take": ("first", [0, 90, 180, 260, 340, 420, 600, 900], (258, 60, 760, 900), 0.62),
    "drop": ("drop", [0, 60, 120, 200, 300, 400, 520, 700], (270, 770, 1000, 870), 1.0),
}

def still(state, ms):
    out = os.path.join(TMP, f"{state}-{ms}.png")
    url = f"http://127.0.0.1:47811/v4/hand/Hand.html?w=1440&h=900&state={state}&t={ms}"
    subprocess.run([CHROME, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--window-size=1440,900",
                    "--virtual-time-budget=12000", f"--screenshot={out}", url], capture_output=True, timeout=150)
    return out

def film(name):
    state, times, box, scale = FILMS[name]
    with ThreadPoolExecutor(2) as ex:
        paths = list(ex.map(lambda t: still(state, t), times))
    frames = [Image.open(p).crop(box) for p in paths]
    fw, fh = int(frames[0].width * scale), int(frames[0].height * scale)
    cols = 4 if name == "take" else 2
    rows = (len(frames) + cols - 1) // cols
    pad, lab = 10, 22
    sheet = Image.new("RGB", (cols * (fw + pad) + pad, rows * (fh + lab + pad) + pad), (3, 8, 20))
    d = ImageDraw.Draw(sheet)
    try: font = ImageFont.truetype(os.path.join(HERE, "..", "..", "fonts", "GeistMono[wght].ttf"), 13)
    except Exception: font = None
    for k, (im, t) in enumerate(zip(frames, times)):
        x, y = pad + (k % cols) * (fw + pad), pad + (k // cols) * (fh + lab + pad)
        sheet.paste(im.resize((fw, fh), Image.LANCZOS), (x, y + lab))
        d.text((x, y + 3), f"t={t} ms", fill=(116, 129, 154), font=font)
    out = os.path.join(SHOTS, f"film-{name}.png"); sheet.save(out); print(out)

def before_after():
    """today's titlebar and foot (D-Page's board as it ships, with the trail) over the calmed window"""
    before = os.path.join(TMP, "before.png"); after = os.path.join(TMP, "after.png")
    subprocess.run([CHROME, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--window-size=1440,900", "--virtual-time-budget=8000",
                    f"--screenshot={before}", "http://127.0.0.1:47811/v4/page2/Page2.html?page=value"], capture_output=True)
    subprocess.run([CHROME, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--window-size=1440,900", "--virtual-time-budget=12000",
                    f"--screenshot={after}", "http://127.0.0.1:47811/v4/hand/Hand.html?w=1440&h=900&state=rest"], capture_output=True)
    font = ImageFont.truetype(os.path.join(HERE, "..", "..", "fonts", "GeistMono[wght].ttf"), 13)
    out = Image.new("RGB", (1440, 244), (3, 8, 20)); d = ImageDraw.Draw(out); y = 0
    for lab, p in (("today: the trail", before), ("instead: the jump bar and the hand", after)):
        im = Image.open(p); d.text((10, y + 4), lab, fill=(116, 129, 154), font=font); y += 22
        out.paste(im.crop((0, 0, 1440, 50)), (0, y)); y += 56
        out.paste(im.crop((0, 870, 1440, 900)), (0, y)); y += 44
    path = os.path.join(SHOTS, "before-after.png"); out.save(path); print(path)

if __name__ == "__main__":
    for n in sys.argv[1:] or [*FILMS, "before-after"]:
        before_after() if n == "before-after" else film(n)
