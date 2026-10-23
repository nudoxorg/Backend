"""Observe the published desktop through X11 input and pixels, against real source."""
from __future__ import annotations

import hashlib
import base64
import json
import os
from pathlib import Path
import re
import select
import shutil
import subprocess
import process_custody
import time
from collections.abc import Callable

from smoke import drew_a_frame


def require_visible(text: str, expected: list[str]) -> None:
    """Do not count a typed search query as proof that its source page opened."""
    if len(text.encode('utf-8')) > 32768:
        raise RuntimeError('GUI OCR response exceeds its bound')
    normalized = ' '.join(text.casefold().split())
    missing = [value for value in expected if ' '.join(value.casefold().split()) not in normalized]
    if missing:
        raise RuntimeError('GUI window is missing visible source: ' + ', '.join(missing))


_RETAINED_RESULT_MARKERS = ('earlier result', 'index changed', 'links unavailable')


def require_current_result(text: str, query: str, filename: str) -> None:
    """A retained row can repeat both the query and its filename; reject it."""
    require_visible(text, [query, filename])
    normalized = ' '.join(text.casefold().split())
    if any(marker in normalized for marker in _RETAINED_RESULT_MARKERS):
        raise RuntimeError('GUI search result is retained from an earlier index')


def require_ask_dismissed(window_text: str, ask_plate_text: str, query: str) -> None:
    """Prove Return closed Ask before sending any plain shell key."""
    if len(ask_plate_text.encode('utf-8')) > 32768:
        raise RuntimeError('GUI Ask plate OCR response exceeds its bound')
    normalized_plate = ' '.join(ask_plate_text.casefold().split())
    normalized_query = ' '.join(query.casefold().split())
    if normalized_query in normalized_plate:
        raise RuntimeError('GUI Ask input is still visible after choosing a result')
    normalized_window = ' '.join(window_text.casefold().split())
    if any(marker in normalized_window for marker in _RETAINED_RESULT_MARKERS):
        raise RuntimeError('GUI Ask retained an earlier-index result after Return')


def open_current_result(
    capture: Callable[[], str],
    ask_plate: Callable[[], str],
    press_return: Callable[[], None],
    query: str,
    filename: str,
    deadline: float,
    *,
    on_ready: Callable[[], None] | None = None,
    now: Callable[[], float] = time.monotonic,
    pause: Callable[[float], None] = time.sleep,
) -> str:
    """Retry stale Ask rows within the original GUI deadline, returning only after dismissal."""
    last_error = None
    while now() < deadline:
        try:
            require_current_result(capture(), query, filename)
        except RuntimeError as error:
            last_error = error
            pause(min(0.5, max(0, deadline - now())))
            continue

        if on_ready is not None:
            on_ready()
        press_return()
        # Keep the existing settle interval, then check the whole window and
        # the top-center Ask input plate independently. A stale Return leaves
        # the query plate open, so no plain `s` can be typed into it.
        pause(min(0.5, max(0, deadline - now())))
        opened = capture()
        try:
            require_ask_dismissed(opened, ask_plate(), query)
            return opened
        except RuntimeError as error:
            last_error = error
            pause(min(0.5, max(0, deadline - now())))
    raise RuntimeError(f'GUI did not open a current search result before its deadline: {last_error}')


def emit_failure_evidence(root: Path, error: Exception) -> None:
    """Retain the already observed window without extending the GUI deadline."""
    try:
        packet = {"observer": "owned native X11 window pixels, English OCR", "error": str(error)[:2048], "captures": []}
        for name in ("search-result.png", "opened-result.png", "window.png"):
            path = root / name
            if not path.is_file():
                continue
            with path.open("rb") as image:
                data = image.read(512 * 1024 + 1)
            if len(data) <= 512 * 1024:
                packet["captures"].append({"name": name, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest(), "png_base64": base64.b64encode(data).decode("ascii")})
        text = root / "window.txt"
        if text.is_file():
            with text.open("rb") as ocr:
                contents = ocr.read(32769)
            if len(contents) <= 32768:
                packet["last_ocr"] = contents.decode("utf-8", errors="replace")
        print("CI-GUI-FAILURE " + json.dumps(packet, sort_keys=True), flush=True)
    except Exception as evidence_error:
        # Evidence cannot replace the original product/harness failure.
        try:
            print("CI-GUI-FAILURE evidence unavailable: " + str(evidence_error)[:512], flush=True)
        except Exception:
            pass


def product_usage(pid: int) -> dict:
    status = Path(f'/proc/{pid}/status').read_text()
    fields = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
    return {'gui_peak_rss_kib': int(re.search(r'VmHWM:\s+(\d+) kB', status).group(1)),
            'gui_cpu_seconds': round((int(fields[11]) + int(fields[12])) / os.sysconf('SC_CLK_TCK'), 2)}


def run_gui(owner, project: Path, spec: dict, budget: float = 120) -> tuple[dict, dict, dict]:
    tools = {name: shutil.which(name) for name in ('Xvfb', 'xdotool', 'magick', 'tesseract')}
    if not all(tools.values()):
        raise RuntimeError('GUI integration requires Xvfb, xdotool, ImageMagick and English Tesseract in the surface shell')
    if owner.process.poll() is not None:
        raise RuntimeError(f'GUI owner retired before desktop launch: exit={owner.process.returncode}')
    root = project.parent / 'gui'
    root.mkdir(mode=0o700)
    env = dict(owner.env)
    env.update(BACKEND_PROJECT=str(project), BACKEND_LOCALD_ENDPOINT=str(owner.endpoint),
               NUDOX_TRACE=str(root / 'trace.jsonl'))
    reader, writer = os.pipe()
    display = process_custody.popen(
        [tools['Xvfb'], '-displayfd', str(writer), '-screen', '0', '1600x1000x24', '-nolisten', 'tcp'],
        env=env, pass_fds=(writer,), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        start_new_session=True)
    os.close(writer)
    app = None
    started = time.monotonic()
    deadline = started + min(budget, 180)
    log = open(root / 'desktop.log', 'w')
    try:
        if not select.select([reader], [], [], 10)[0]:
            raise RuntimeError('GUI Xvfb did not publish a display')
        number = os.read(reader, 64).decode().strip()
        if not number.isdecimal() or display.poll() is not None:
            raise RuntimeError('GUI Xvfb did not start')
        env['DISPLAY'] = ':' + number
        app = process_custody.popen([owner.bin_dir / 'backend-desktop'], env=env, cwd=project,
                               stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                               start_new_session=True)

        def command(name, *args):
            remaining = deadline - time.monotonic()
            if remaining <= 0 or app.poll() is not None:
                raise RuntimeError('GUI deadline exceeded or desktop exited before content proof')
            return process_custody.run([tools[name], *map(str, args)], env=env, check=True,
                                  capture_output=True, text=True, timeout=min(10, remaining))

        window = None
        while time.monotonic() < deadline:
            if app.poll() is not None:
                raise RuntimeError('desktop exited before opening the real project')
            if drew_a_frame(root / 'trace.jsonl'):
                found = process_custody.run([tools['xdotool'], 'search', '--onlyvisible', '--pid', str(app.pid)],
                                       env=env, capture_output=True, text=True, timeout=5)
                if found.returncode == 0 and found.stdout.splitlines():
                    windows = found.stdout.splitlines()
                    if len(windows) != 1:
                        raise RuntimeError('GUI content proof requires one visible owned window')
                    window = windows[0]
                    break
            time.sleep(0.1)
        if window is None:
            raise RuntimeError('desktop never painted a visible owned window')
        command('xdotool', 'windowfocus', '--sync', window)

        def pixels():
            image = root / 'window.png'
            command('magick', 'import', '-silent', '-window', window, image)
            if image.stat().st_size > 4 * 1024 * 1024:
                raise RuntimeError('GUI capture exceeds four MiB')
            result = command('tesseract', image, 'stdout', '-l', 'eng', '--psm', '11').stdout
            if len(result.encode('utf-8')) > 32768:
                raise RuntimeError('GUI OCR exceeds32KiB')
            (root / 'window.txt').write_text(result)
            return result

        def ask_plate_pixels():
            # Ask's input is centered at the top of the owned window. OCR only
            # this region so a matching declaration in the opened source body
            # cannot be mistaken for an Ask field that stayed open.
            window_width = int(width.group(1))
            crop_width = max(1, int(window_width * 0.6))
            crop_x = (window_width - crop_width) // 2
            crop = root / 'ask-plate.png'
            command('magick', 'import', '-silent', '-window', window, '-crop',
                    f'{crop_width}x64+{crop_x}+0', '+repage', crop)
            if crop.stat().st_size > 512 * 1024:
                raise RuntimeError('GUI Ask plate capture exceeds512KiB')
            result = command('tesseract', crop, 'stdout', '-l', 'eng', '--psm', '11').stdout
            if len(result.encode('utf-8')) > 32768:
                raise RuntimeError('GUI Ask plate OCR exceeds32KiB')
            return result

        # The project is selected through the desktop's normal launch contract,
        # on the same native owner used by this case's CLI and MCP checks.
        last_error = None
        while time.monotonic() < deadline:
            try:
                require_visible(pixels(), [spec['project_name']])
                break
            except RuntimeError as error:
                last_error = error
                time.sleep(1)
        else:
            raise RuntimeError(f'GUI did not show the real project: {last_error}')
        if owner.process.poll() is not None:
            raise RuntimeError(f'GUI owner retired before project observation: exit={owner.process.returncode}')
        geometry = command('xdotool', 'getwindowgeometry', '--shell', window).stdout
        width = re.search(r'^WIDTH=(\d+)$', geometry, re.MULTILINE)
        if width is None or not 800 <= int(width.group(1)) <= 1600:
            raise RuntimeError('GUI window geometry is outside the observed test viewport')
        # Give the real Ask field native pointer focus. Merely focusing an X11
        # top-level window does not focus GPUI's text input without a WM.
        command('xdotool', 'mousemove', '--window', window, int(width.group(1)) // 2, 24, 'click', '1')
        time.sleep(0.2)
        command('xdotool', 'type', '--clearmodifiers', '--delay', '30', '--', spec['query'])
        input_deadline = min(deadline, time.monotonic() + 10)
        while time.monotonic() < input_deadline:
            try:
                require_visible(pixels(), [spec['query']])
                break
            except RuntimeError:
                time.sleep(0.2)
        else:
            raise RuntimeError('GUI Ask field did not display the typed query')
        # A retained result can still show both the query and source filename.
        # Retry until the row is current, then verify Ask actually dismissed
        # after Return before sending the shell's plain `s` binding.
        open_current_result(
            pixels, ask_plate_pixels,
            lambda: command('xdotool', 'key', '--clearmodifiers', 'Return'),
            spec['query'], spec['result_filename'], deadline,
            on_ready=lambda: shutil.copyfile(root / 'window.png', root / 'search-result.png'))
        shutil.copyfile(root / 'window.png', root / 'opened-result.png')
        # Ask dismissal above is the positive readiness signal. PeelSource uses
        # the shell's plain `s` binding (shell/keys.rs), revealing source.
        command('xdotool', 'key', '--clearmodifiers', 's')
        command('xdotool', 'key', '--clearmodifiers', 'ctrl+equal', 'ctrl+equal')
        expected = [spec['project_name'], *spec['visible_source']]
        while time.monotonic() < deadline:
            try:
                require_visible(pixels(), expected)
                break
            except RuntimeError as error:
                last_error = error
                time.sleep(1)
        else:
            raise RuntimeError(f'GUI source page did not match original source: {last_error}')
        usage = product_usage(app.pid)
        image = root / 'window.png'
        evidence = {'screenshot': str(image), 'screenshot_sha256': hashlib.sha256(image.read_bytes()).hexdigest(),
                    'observer': 'owned native X11 window pixels, English OCR', 'project': spec['project_name'],
                    'source_filename': spec['result_filename'], 'visible_source': spec['visible_source']}
        command('xdotool', 'key', '--clearmodifiers', 'super+q')
        if app.wait(timeout=10) != 0:
            raise RuntimeError('desktop did not quit cleanly after content proof')
        usage['seconds'] = {'gui read project': round(time.monotonic() - started, 2)}
        return ({'project': spec['project_name'], 'symbol': spec['query'],
                 'visible_source': spec['visible_source'], 'quit_exit': 0}, usage, evidence)
    except Exception as error:
        emit_failure_evidence(root, error)
        raise
    finally:
        if app is not None:
            process_custody.stop(app)
        process_custody.stop(display)
        os.close(reader)
        log.close()
