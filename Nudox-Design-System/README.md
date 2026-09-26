# Nudox Design System — HTML archives

This folder collects four standalone HTML design references exported from the artifact:

- `artifacts/descent/Descent.dc.html`
- `artifacts/colour-with-a-job/Color.dc.html`
- `artifacts/one-mark-one-language/Brand.dc.html`
- `artifacts/the-information-language/Language.dc.html`

The original ZIP files are preserved in `archives/`. Each artifact folder is self-contained and includes its own runtime and vendor files.

## Setup and local viewing

The exports use JavaScript modules/runtime files, so serve the folder over HTTP instead of opening the HTML files directly with `file://`.

1. Open Terminal.
2. Change into this folder:

   ```sh
   cd ~/Downloads/Nudox-Design-System
   ```

3. Start a local server:

   ```sh
   python3 -m http.server 8766 --bind 127.0.0.1
   ```

4. Open one of these URLs in a browser:

   - <http://127.0.0.1:8766/artifacts/descent/Descent.dc.html>
   - <http://127.0.0.1:8766/artifacts/colour-with-a-job/Color.dc.html>
   - <http://127.0.0.1:8766/artifacts/one-mark-one-language/Brand.dc.html>
   - <http://127.0.0.1:8766/artifacts/the-information-language/Language.dc.html>

Stop the server with `Ctrl-C` when finished.

The pages load some fonts from Google Fonts. If you need a fully offline render, allow the fonts to finish loading while online first, or replace the font links in the HTML with local font files.

## Render at full resolution

These are design references, not responsive production pages. For the most faithful result:

- Use browser zoom at `100%`.
- Do not use the browser's “fit to width” or automatic page scaling.
- Use the page's natural CSS pixel dimensions. If the design extends beyond the viewport, scroll rather than zooming out.
- For a clean capture, hide browser chrome with the browser's normal full-screen mode.

In Chrome or Edge, open DevTools (`Cmd-Option-I`), turn on Device Toolbar (`Cmd-Shift-M`), and set a custom viewport large enough for the artboard. Keep device scale factor at `1` for CSS-pixel screenshots, or choose a higher scale factor when you need a larger raster image.

## Capture screenshots

### Full page

In Chrome/Edge DevTools:

1. Open the Command Menu with `Cmd-Shift-P`.
2. Run **Capture full size screenshot**.
3. Save the PNG outside the project or in a new `screenshots/` folder.

### A portion of the page

For a specific component or panel:

1. Open DevTools and use the element picker to select the portion.
2. In the Elements panel, right-click the selected node.
3. Choose **Capture node screenshot**.

For an arbitrary rectangular crop, use the DevTools Command Menu and choose **Capture area screenshot**, then drag over the region. Keep the browser at `100%` before selecting the area so the crop maps cleanly to the rendered design.

### Repeatable command-line capture (optional)

If you already have Playwright installed, start the server as above and run a script like this from the project folder:

```js
// save as capture.mjs
import { chromium } from 'playwright';

const browser = await chromium.launch({ headless: true });
const page = await browser.newPage({
  viewport: { width: 1600, height: 1000 },
  deviceScaleFactor: 2,
});

await page.goto('http://127.0.0.1:8766/artifacts/descent/Descent.dc.html', {
  waitUntil: 'networkidle',
});
await page.screenshot({ path: 'descent-full.png', fullPage: true });
await page.screenshot({
  path: 'descent-detail.png',
  clip: { x: 0, y: 0, width: 900, height: 700 },
});
await browser.close();
```

If the `playwright` package is already installed, run it with:

```sh
node capture.mjs
```

If it is not installed, install it in this folder first with `npm install --no-save playwright`, then run `node capture.mjs`. You may also need `npx playwright install chromium` once to install the browser binary.

Adjust the URL, viewport, device scale factor, and `clip` rectangle for the artifact or region you need. The `clip` coordinates are CSS pixels; a device scale factor of `2` produces a 2× raster output.

## Notes

- The per-artifact `README.md` files are the original export notes.
- Do not edit the HTML exports if you want to preserve the source references; make implementation copies instead.
