#!/usr/bin/env python3
"""Regenerates FACET's icon assets and their Rust tables from the board spec.

Source: Nudox-Design-System/spec/icons.json (verbatim board SVG) and the
Brand board's small chevron-diamond mark. Output:

  apps/facet/assets/icons/{ui,kind,mod,cap,lang,core}/<name>.svg
  apps/facet/assets/brand/mark.svg      (the chevron diamond, below 48 px)
  apps/facet/src/icons/table.rs         (embedded file table)
  apps/facet/src/icons/set.rs           (closed enums: Icon, Kind, Mod, Cap, Lang)

The SVGs are monochrome and `currentColor`-free (black on transparent) so
GPUI's alpha-mask tinting colours them: outline strokes at full alpha, the
"one lit facet" filled sub-paths (`class="f"`) at the group's opacity, solid
marks (`class="s"`) filled in ui/kind and stroked in mod/cap, exactly as the
board CSS resolves them. Stroke width is the group's canonical CSS value;
the asset source serves per-size variants (see `icons.rs`).

Run from the repo root: python3 apps/facet/assets/icons/gen_icons.py
"""
import json, os, re

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), '..', '..', '..', '..'))
SPEC = os.path.join(ROOT, 'Nudox-Design-System/spec/icons.json')
ASSETS = os.path.join(ROOT, 'apps/facet/assets')
SRC = os.path.join(ROOT, 'apps/facet/src/icons')

# group -> (dir, canonical stroke width, .f opacity, .s filled?)
GROUPS = {
    'ui': ('ui', '1.6', '.3', True),
    'kind': ('kind', '1.7', '.34', True),
    'modifier': ('mod', '1.6', '.3', False),
    'capability': ('cap', '1.5', '.3', False),
    'core': ('core', '1.6', '.3', True),
}

MOD_SHORT = ['const', 'async', 'unsafe', 'generic', 'static', 'crate', 'deprecated', 'abstract',
             'derived', 'blanket', 'auto', 'override', 'inherited', 'makes', 'reads', 'changes',
             'consumes']
CAP_SHORT = ['clone', 'copy', 'eq', 'ord', 'hash', 'debug', 'display', 'convert', 'thread',
             'default', 'serde', 'iter', 'deref', 'error']
FAMILY = {'ns': 'Namespace', 'ty': 'Type', 'co': 'Contract', 'ca': 'Callable', 'va': 'Value'}
LANG = {  # aria name -> (ecosystem, board colour)
    'rust': ('cargo', 0xf0a27a), 'typescript': ('npm', 0x7fb0ff), 'python': ('pypi', 0x8fc4ff),
    'go': ('go', 0x7fdcf0), 'java': ('maven', 0xf2a0a0), 'csharp': ('nuget', 0xc6a6ff),
    'cpp': ('conan', 0x9db4ff),
}
LANG_TITLE = {'rust': 'Rust', 'typescript': 'TypeScript', 'python': 'Python', 'go': 'Go',
              'java': 'Java', 'csharp': 'C#', 'cpp': 'C++'}


def camel(name):
    return ''.join(p[:1].upper() + p[1:] for p in re.split(r'[-_ ]', name) if p)


def paths_of(svg):
    return re.findall(r'<path([^>]*)></path>', svg)


def attr(a, name):
    m = re.search(r'\b' + name + r'="([^"]*)"', a)
    return m.group(1) if m else None


def mono(entry, width, f_opacity, s_filled):
    out = []
    for a in paths_of(entry['svg']):
        d = attr(a, 'd')
        cls = attr(a, 'class')
        dash = attr(a, 'stroke-dasharray')
        extra = f' stroke-dasharray="{dash}"' if dash else ''
        if cls == 'f':
            out.append(f'<path class="f" fill="#000" stroke="none" opacity="{f_opacity}" d="{d}"/>')
        elif cls == 's' and s_filled:
            out.append(f'<path class="s" fill="#000" stroke="none" d="{d}"/>')
        else:
            out.append(f'<path d="{d}"{extra}/>')
    head = ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24" '
            f'fill="none" stroke="#000" stroke-width="{width}" stroke-linecap="square" '
            'stroke-linejoin="miter" stroke-miterlimit="3">')
    return head + ''.join(out) + '</svg>\n'


def lang_svg(entry):
    body = ''.join(f'<path d="{attr(a, "d")}"/>' for a in paths_of(entry['svg']))
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24" '
            f'fill="#000">{body}</svg>\n')


# The Brand board's small mark ("the chevron diamond below [48 px]"), verbatim
# geometry and colours with unique gradient ids.
MARK = '''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 240 240" width="240" height="240">
<defs>
<linearGradient id="lbl" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#aab6ff"/><stop offset=".5" stop-color="#6f7ce0"/><stop offset="1" stop-color="#4a54b0"/></linearGradient>
<linearGradient id="lil" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#3b4374"/><stop offset="1" stop-color="#1b2040"/></linearGradient>
<linearGradient id="lcl" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#8fe8ae"/><stop offset="1" stop-color="#27ae5c"/></linearGradient>
</defs>
<path d="M120 3 237 120 120 237 3 120z" fill="url(#lbl)"/>
<path d="M120 17 223 120 120 223 17 120z" fill="url(#lil)"/>
<path d="M120 33 207 120 120 207 33 120z" fill="#141831"/>
<g stroke="#9fb0ff" stroke-width="5" opacity=".55"><path d="M62 118V150"/><path d="M74 118V160"/><path d="M86 118V170"/><path d="M98 118V180"/></g>
<path fill="url(#lcl)" d="M96 62h34l42 58-42 58H96l42-58z"/>
</svg>
'''


def write(rel, text):
    path = os.path.join(ASSETS, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, 'w') as fh:
        fh.write(text)
    return rel


def main():
    spec = json.load(open(SPEC))
    files = []
    ui, kinds, mods, caps, langs = [], [], [], [], []
    for e in spec:
        g = e['group']
        if g in ('ui', 'kind', 'core'):
            d, w, f, s = GROUPS[g]
            name = 'chevron' if g == 'core' else e['name']
            files.append(write(f'icons/{d}/{name}.svg', mono(e, w, f, s)))
            if g == 'ui':
                ui.append(name)
            elif g == 'kind':
                fam = FAMILY[e['class'].split()[1]]
                kinds.append((name, fam))
        elif g == 'modifier':
            d, w, f, s = GROUPS[g]
            short = re.search(r'aria-label="([^"]+)"', e['svg']).group(1)
            files.append(write(f'icons/{d}/{short}.svg', mono(e, w, f, s)))
            voice = 'Some(Voice::Amber)' if 'amber' in e['class'] else 'None'
            mods.append((short, e['name'], voice))
        elif g == 'capability':
            d, w, f, s = GROUPS[g]
            short = re.search(r'aria-label="([^"]+)"', e['svg']).group(1)
            files.append(write(f'icons/{d}/{short}.svg', mono(e, w, f, s)))
            caps.append((short, e['name']))
        elif g == 'language':
            files.append(write(f'icons/lang/{e["name"]}.svg', lang_svg(e)))
            langs.append(e['name'])
    assert [m[0] for m in mods] == MOD_SHORT, [m[0] for m in mods]
    assert [c[0] for c in caps] == CAP_SHORT, [c[0] for c in caps]
    assert len(ui) == 50 and len(kinds) == 20 and len(langs) == 7

    files.append(write('brand/mark.svg', MARK))
    logo_src = os.path.expanduser('~/Downloads/logo.svg')
    if os.path.exists(logo_src):
        files.append(write('brand/logo.svg', open(logo_src).read()))
    else:
        files.append('brand/logo.svg')

    files.sort()
    table = ['// @generated by apps/facet/assets/icons/gen_icons.py — do not edit.',
             '',
             '/// Every embedded asset: `(path, bytes)`, sorted by path.',
             'pub(super) static FILES: &[(&str, &[u8])] = &[']
    for rel in files:
        table.append(f'    ("{rel}", include_bytes!("../../assets/{rel}")),')
    table.append('];')
    open(os.path.join(SRC, 'table.rs'), 'w').write('\n'.join(table) + '\n')

    rs = ['// @generated by apps/facet/assets/icons/gen_icons.py — do not edit.',
          '',
          '// Lookup tables: several rows legitimately share a value.',
          '#![allow(clippy::match_same_arms)]',
          '',
          'use crate::tokens::{Family, Voice};',
          '']

    def enum(name, doc, variants):
        rs.append(f'/// {doc}')
        rs.append('#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]')
        rs.append(f'pub enum {name} {{')
        for v, vdoc in variants:
            rs.append(f'    /// {vdoc}')
            rs.append(f'    {v},')
        rs.append('}')
        rs.append('')

    def table_fn(enum_name, fn, ret, doc, rows):
        rs.append(f'    /// {doc}')
        rs.append('    #[must_use]')
        rs.append(f'    pub const fn {fn}(self) -> {ret} {{')
        rs.append('        match self {')
        for v, val in rows:
            rs.append(f'            Self::{v} => {val},')
        rs.append('        }')
        rs.append('    }')
        rs.append('')

    enum('Icon', 'The 50 UI icons (`.ico`).', [(camel(n), f'`{n}`') for n in ui])
    rs.append('impl Icon {')
    rs.append('    /// Every icon, in board order.')
    rs.append('    pub const ALL: [Self; ' + str(len(ui)) + '] = [' + ', '.join('Self::' + camel(n) for n in ui) + '];')
    rs.append('')
    table_fn('Icon', 'name', "&'static str", 'The board name.', [(camel(n), f'"{n}"') for n in ui])
    table_fn('Icon', 'path', "&'static str", 'The asset path.', [(camel(n), f'"icons/ui/{n}.svg"') for n in ui])
    rs[-1:] = []
    rs.append('}')
    rs.append('')

    enum('Kind', 'The 20 kind marks: hue = family, shape = kind.', [(camel(n), f'`{n}` ({fam.lower()})') for n, fam in kinds])
    rs.append('impl Kind {')
    rs.append('    /// Every kind, in board order (grouped by family).')
    rs.append('    pub const ALL: [Self; ' + str(len(kinds)) + '] = [' + ', '.join('Self::' + camel(n) for n, _ in kinds) + '];')
    rs.append('')
    table_fn('Kind', 'name', "&'static str", 'The tooltip name (`struct`, `trait`, …).', [(camel(n), f'"{n}"') for n, _ in kinds])
    table_fn('Kind', 'family', 'Family', 'The family, which picks the hue.', [(camel(n), f'Family::{fam}') for n, fam in kinds])
    table_fn('Kind', 'path', "&'static str", 'The asset path.', [(camel(n), f'"icons/kind/{n}.svg"') for n, _ in kinds])
    rs[-1:] = []
    rs.append('}')
    rs.append('')

    enum('Mod', 'The 17 modifier marks: a mark with a tooltip, never an uppercase label.', [(camel(s), f'`{s}`: {t}') for s, t, _ in mods])
    rs.append('impl Mod {')
    rs.append('    /// Every modifier, in board order.')
    rs.append('    pub const ALL: [Self; ' + str(len(mods)) + '] = [' + ', '.join('Self::' + camel(s) for s, _, _ in mods) + '];')
    rs.append('')
    table_fn('Mod', 'name', "&'static str", 'The short name.', [(camel(s), f'"{s}"') for s, _, _ in mods])
    table_fn('Mod', 'tooltip', "&'static str", 'The tooltip sentence, verbatim from the Language board.', [(camel(s), json.dumps(t)) for s, t, _ in mods])
    table_fn('Mod', 'voice', 'Option<Voice>', 'The voice it speaks in when it warns (amber for `unsafe`, `deprecated`).', [(camel(s), v) for s, _, v in mods])
    table_fn('Mod', 'path', "&'static str", 'The asset path.', [(camel(s), f'"icons/mod/{s}.svg"') for s, _, _ in mods])
    rs[-1:] = []
    rs.append('}')
    rs.append('')

    enum('Cap', 'The 14 capability marks (the derive-trait strip).', [(camel(s), f'`{s}`: ' + ', '.join('`' + x.strip() + '`' for x in t.split(','))) for s, t in caps])
    rs.append('impl Cap {')
    rs.append('    /// Every capability, in board order.')
    rs.append('    pub const ALL: [Self; ' + str(len(caps)) + '] = [' + ', '.join('Self::' + camel(s) for s, _ in caps) + '];')
    rs.append('')
    table_fn('Cap', 'name', "&'static str", 'The short caption (`clone`, `serde`, …).', [(camel(s), f'"{s}"') for s, _ in caps])
    table_fn('Cap', 'traits', "&'static str", 'The trait(s) it stands for (the tooltip).', [(camel(s), json.dumps(t)) for s, t in caps])
    table_fn('Cap', 'path', "&'static str", 'The asset path.', [(camel(s), f'"icons/cap/{s}.svg"') for s, _ in caps])
    rs[-1:] = []
    rs.append('}')
    rs.append('')

    enum('Lang', 'The 7 ecosystems, as their language marks.', [(camel(n), f'{LANG_TITLE[n]} (`{LANG[n][0]}`)') for n in langs])
    rs.append('impl Lang {')
    rs.append('    /// Every ecosystem, in board order.')
    rs.append('    pub const ALL: [Self; ' + str(len(langs)) + '] = [' + ', '.join('Self::' + camel(n) for n in langs) + '];')
    rs.append('')
    table_fn('Lang', 'name', "&'static str", 'The language name.', [(camel(n), f'"{LANG_TITLE[n]}"') for n in langs])
    table_fn('Lang', 'ecosystem', "&'static str", 'The registry caption (`cargo`, `npm`, …).', [(camel(n), f'"{LANG[n][0]}"') for n in langs])
    table_fn('Lang', 'rgb', 'u32', 'The mark colour the boards use (`0xRRGGBB`).', [(camel(n), '0x{:04x}_{:04x}'.format(LANG[n][1] >> 16, LANG[n][1] & 0xffff)) for n in langs])
    table_fn('Lang', 'path', "&'static str", 'The asset path.', [(camel(n), f'"icons/lang/{n}.svg"') for n in langs])
    rs[-1:] = []
    rs.append('}')
    open(os.path.join(SRC, 'set.rs'), 'w').write('\n'.join(rs) + '\n')
    # Keep the generated Rust in rustfmt's shape (a no-op without rustfmt).
    import shutil, subprocess
    if shutil.which('rustfmt'):
        subprocess.run(['rustfmt', '--edition', '2024', os.path.join(SRC, 'set.rs'), os.path.join(SRC, 'table.rs')], check=False)
    print(f'{len(files)} assets')


if __name__ == '__main__':
    main()
