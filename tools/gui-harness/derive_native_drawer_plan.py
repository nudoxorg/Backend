#!/usr/bin/env python3
"""Derive a drawer Down→drag→Up film from a verified native AX survey.

The survey is a real capture of the same window at the same size/text scale.
This tool never launches or controls the product; it refuses absent/ambiguous
AX bounds rather than guessing a hit point or a pixel crop.
"""
from __future__ import annotations
import argparse
import json
import math
from pathlib import Path

from native_motion import require_plan, sha256


def rect(value: object, name: str) -> tuple[float, float, float, float]:
    if not isinstance(value, dict) or any(not isinstance(value.get(k), (int, float)) or
            not math.isfinite(value[k]) for k in ("x", "y", "width", "height")):
        raise ValueError(f"{name}: finite AX bounds required")
    x, y, width, height = (float(value[k]) for k in ("x", "y", "width", "height"))
    if width <= 0 or height <= 0:
        raise ValueError(f"{name}: positive AX bounds required")
    return x, y, width, height


def unique(tree: object, title: str) -> dict:
    matches = [node for node in tree if isinstance(node, dict) and
               title in {node.get("title"), node.get("description")}] if isinstance(tree, list) else []
    if len(matches) != 1:
        raise ValueError(f"native AX {title!r} matched {len(matches)} nodes; one is required")
    return matches[0]


def close(a: tuple[float, ...], b: tuple[float, ...]) -> bool:
    return all(abs(x-y) <= 2 for x, y in zip(a, b))


def derive(survey_path: Path, width: int, percent: int) -> tuple[dict, dict]:
    if (width, percent) not in {(500, 100), (360, 200)}:
        raise ValueError("Run19 drawer admission covers exactly 500/100 and 360/200")
    survey = json.loads(survey_path.read_text())
    expected_id = f"drawer_{width}_{percent}_survey"
    if survey.get("case", {}).get("id") != expected_id or survey.get("passed_native_checks") is not True or \
            survey.get("capture_inputs_stable") is not True or \
            survey.get("binary_source_admission", {}).get("state") != "VerifiedBuildReceipt":
        raise ValueError("survey case/native/source admission is absent")
    window = survey.get("window", {})
    if window.get("capture_scope") != "app_display" or type(window.get("window_id")) is not int:
        raise ValueError("survey must bind an exact app-display SCWindow")
    win = rect(window.get("window_frame_pt"), "selected SCWindow")
    if abs(win[2] - width) > 3:
        raise ValueError("survey selected window width differs from requested case")
    actions = survey.get("actions", [])
    initial = next((a.get("ax") for a in actions if a.get("phase") == "initial"), None)
    probe = next((a.get("posted", {}).get("ax") for a in actions if a.get("label") == "measure native Library shelf"), None)
    if not isinstance(initial, dict) or not isinstance(probe, dict) or \
            initial.get("tree_truncated") or probe.get("tree_truncated"):
        raise ValueError("complete initial and open-drawer native AX trees required")
    if unique(initial.get("tree"), f"{percent}%").get("selected") is not True:
        raise ValueError(f"native {percent}% text choice was not selected before drawer capture")
    full = rect(unique(initial.get("tree"), "Full").get("bounds_pt"), "Motion Full radio")
    drawer = rect(unique(probe.get("tree"), "Library shelf").get("bounds_pt"), "Library shelf dialog")
    probe_window = rect(probe.get("window", {}).get("bounds_pt"), "probed AX window")
    if not close(win, probe_window):
        raise ValueError("AX window moved during survey")
    wx, wy, ww, wh = win
    dx, dy, dw, dh = drawer
    if dx < wx - 2 or dy < wy - 2 or dx + dw > wx + ww - 32 or dy + dh > wy + wh + 2:
        raise ValueError("measured drawer leaves no verified right strip")
    full_center = (full[0] + full[2]/2, full[1] + full[3]/2)
    if not (dy + 8 <= full_center[1] <= dy + dh - 8):
        raise ValueError("Motion Full radio is outside the drawer gesture height")
    candidate = full_center[0]
    down_x = candidate if dx + dw + 4 <= candidate <= wx + ww - 4 else wx + ww - 8
    down_y = full_center[1]
    up_x = dx + 8
    if not (dx + dw + 4 <= down_x <= wx + ww - 4 and wx + 4 <= up_x < dx + dw - 4):
        raise ValueError("observed bounds do not admit a right-strip to drawer drag")
    display = rect(window.get("display_frame_pt"), "captured display")
    width_px, height_px = window.get("requested_width_px"), window.get("requested_height_px")
    if type(width_px) is not int or type(height_px) is not int or width_px < 1 or height_px < 1:
        raise ValueError("survey display pixel dimensions absent")
    sx, sy = width_px/display[2], height_px/display[3]
    def pixels(box: tuple[float, float, float, float]) -> list[int]:
        x, y, w, h = box
        left, top = math.floor((x-display[0])*sx), math.floor((y-display[1])*sy)
        right, bottom = math.ceil((x+w-display[0])*sx), math.ceil((y+h-display[1])*sy)
        if left < 0 or top < 0 or right > width_px or bottom > height_px or left >= right or top >= bottom:
            raise ValueError("AX bound does not map inside captured display pixels")
        return [left, top, right-left, bottom-top]
    case = {'id':f'drawer_{width}_{percent}_full','flow':'drawer','owner_phase':'serving',
            'motion':'full','transition':'open_close','viewport':'narrow','text_scale':str(percent),'live_index':False}
    plan = {'schema':1,'name':f'drawer-{width}-{percent}-native-press-drag-refusal','case':case,
            'window_id':window['window_id'],'expected_window_frame_pt':list(win),
            'capture_scope':'app_display','capture_fps':60,'duration_ms':1450,'max_frame_gap_ms':90,
            'expected_reduce_motion':False,'actions':[
                {'at_ms':80,'kind':'key','keycode':42,'modifiers':['command'],'label':'open drawer','expect_visual_ms':330},
                {'at_ms':430,'kind':'probe','label':'drawer owns native AX','expect_ax_title':'Library shelf'},
                {'at_ms':500,'kind':'mouse_down','x':down_x,'y':down_y,'label':'press measured right strip over Settings'},
                {'at_ms':550,'kind':'move','x':up_x,'y':down_y,'label':'drag held press into measured drawer'},
                {'at_ms':600,'kind':'mouse_up','x':up_x,'y':down_y,'label':'release inside measured drawer'},
                {'at_ms':830,'kind':'probe','label':'drawer remains after drag-off','expect_ax_title':'Library shelf'},
                {'at_ms':1010,'kind':'key','keycode':53,'label':'Escape drawer','expect_visual_ms':300},
                {'at_ms':1330,'kind':'probe','label':'post-drawer Settings tree','expect_ax_title':'Appearance'}],
            'crops':[]}
    window_px = pixels(win)
    drawer_px = pixels(drawer)
    strip_px = pixels((dx+dw+4, dy+8, wx+ww-dx-dw-8, dh-16))
    full_px = pixels(full)
    for at in [80, 300, 500, 600, 830, 1100, 1330]:
        plan['crops'].append({'label':f'window-{at}','at_ms':at,'rect_px':window_px})
    for at in [300, 500, 600, 830]:
        plan['crops'].append({'label':f'drawer-{at}','at_ms':at,'rect_px':drawer_px})
        plan['crops'].append({'label':f'strip-{at}','at_ms':at,'rect_px':strip_px})
    for at in [80, 600, 830, 1330]:
        plan['crops'].append({'label':f'motion-full-{at}','at_ms':at,'rect_px':full_px})
    derivation = {'schema':1,'survey_capture_path':str(survey_path.resolve()),
                  'survey_capture_sha256':sha256(survey_path),'survey_source':survey.get('source'),
                  'survey_binary':survey.get('binary'),'window_id':window['window_id'],
                  'window_bounds_pt':list(win),'drawer_bounds_pt':list(drawer),
                  'motion_full_bounds_pt':list(full),'right_strip_down_pt':[down_x,down_y],
                  'drawer_up_pt':[up_x,down_y],
                  'down_strategy':'native Full radio center' if down_x == candidate else 'measured right strip at radio height',
                  'display_frame_pt':list(display),'display_pixels':[width_px,height_px]}
    return plan, derivation


def derive_retirement(survey_path: Path, width: int, percent: int) -> tuple[dict, dict]:
    """Press a live Settings radio, cover/uncover it by keyboard, then release."""
    plan, derivation = derive(survey_path, width, percent)
    survey = json.loads(survey_path.read_text())
    initial = next(a["ax"] for a in survey["actions"] if a.get("phase") == "initial")
    full = unique(initial.get("tree"), "Full")
    if full.get("selected") is not False:
        raise ValueError("native Motion Full radio must begin unselected")
    fx, fy, fw, fh = rect(full.get("bounds_pt"), "Motion Full radio")
    wx, wy, ww, wh = derivation["window_bounds_pt"]
    point = [fx + fw / 2, fy + fh / 2]
    if not (wx + 4 < point[0] < wx + ww - 4 and wy + 4 < point[1] < wy + wh - 4):
        raise ValueError("native Motion Full radio center is outside selected window")
    plan["name"] = f"drawer-{width}-{percent}-held-underlay-retirement"
    plan["case"] = {**plan["case"], "id": f"drawer_{width}_{percent}_retirement",
                    "transition": "underlay_retirement"}
    plan["duration_ms"] = 1600
    plan["actions"] = [
        {"at_ms": 20, "kind": "probe", "label": "uncovered Settings baseline",
         "expect_ax_title": "Appearance", "expect_ax_unselected_title": "Full"},
        {"at_ms": 120, "kind": "mouse_down", "x": point[0], "y": point[1],
         "label": "hold native Motion Full underlay"},
        {"at_ms": 270, "kind": "key", "keycode": 42, "modifiers": ["command"],
         "label": "keyboard opens drawer over held radio", "expect_visual_ms": 250},
        {"at_ms": 540, "kind": "probe", "label": "native drawer covers held radio",
         "expect_ax_title": "Library shelf"},
        {"at_ms": 660, "kind": "key", "keycode": 53,
         "label": "Escape closes keyboard cover while held", "expect_visual_ms": 300},
        {"at_ms": 1000, "kind": "probe", "label": "same radio remounts before release",
         "expect_ax_title": "Appearance", "expect_ax_unselected_title": "Full"},
        {"at_ms": 1150, "kind": "mouse_up", "x": point[0], "y": point[1],
         "label": "release retired underlay press"},
        {"at_ms": 1420, "kind": "probe", "label": "stale release did not select Motion Full",
         "expect_ax_title": "Appearance", "expect_ax_unselected_title": "Full"},
    ]
    crops = []
    for prefix, times in [("window", [20, 120, 400, 540, 900, 1150, 1420]),
                          ("drawer", [400, 540, 900]),
                          ("motion-full", [20, 540, 1000, 1150, 1420])]:
        rect_px = next(crop["rect_px"] for crop in plan["crops"] if crop["label"].startswith(prefix + "-"))
        crops.extend({"label": f"{prefix}-{at}", "at_ms": at, "rect_px": rect_px} for at in times)
    plan["crops"] = crops
    derivation["scenario"] = "underlay_retirement"
    derivation["held_radio_point_pt"] = point
    return plan, derivation


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--survey-capture',type=Path,required=True)
    parser.add_argument('--width',type=int,choices=[360,500],required=True)
    parser.add_argument('--percent',type=int,choices=[100,200],required=True)
    parser.add_argument('--scenario',choices=['drag_off','underlay_retirement'],default='drag_off')
    parser.add_argument('--out-plan',type=Path,required=True)
    args = parser.parse_args()
    builder = derive_retirement if args.scenario == 'underlay_retirement' else derive
    plan, derivation = builder(args.survey_capture, args.width, args.percent)
    args.out_plan.parent.mkdir(parents=True,exist_ok=True)
    args.out_plan.write_text(json.dumps(plan,indent=2)+'\n')
    require_plan(args.out_plan)
    derivation['plan_sha256'] = sha256(args.out_plan)
    args.out_plan.with_suffix('.derivation.json').write_text(json.dumps(derivation,indent=2)+'\n')
    print(args.out_plan)
    return 0

if __name__ == '__main__':
    raise SystemExit(main())
