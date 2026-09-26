#!/usr/bin/env python3
"""Two sequential graph audits. Never builds; keep compile work outside these runs.

python3 verify_graph.py --mode correctness --binary PATH [--out DIR]
python3 verify_graph.py --mode perf --binary RELEASE_PATH [--out DIR]
Every command, exit status, complete stdout/stderr, fixture and executable digest
is retained. No PASS text is trusted without command success and structured data.
"""
import argparse, hashlib, html, json, math, pathlib, re, shutil, subprocess, time
from urllib.parse import quote

ROOT = pathlib.Path(__file__).resolve().parents[2]
CASES = ['graph-check-idle', 'graph-check-interrupt', 'graph-check-wheel-drag',
         'graph-check-hover', 'graph-check-keyboard', 'graph-check-weather', 'graph-check-reach-tour', 'graph-check-hold', 'graph-check-chain', 'graph-check-peek', 'graph-check-parked']
LIVE = ['graph-world', 'graph-present', 'graph-engine', 'graph-glyph',
        'graph-members', 'graph-hover', 'graph-focus', 'graph-journey',
        'graph-flight-a', 'graph-flight-b', 'graph-check-live-hover', 'graph-live-dense-focus', 'graph-check-hover-phases']
CONTROLS = ['graph-world-naive','graph-world-paths','graph-check-live-hover-naive','graph-check-live-hover-paths']
TIMES = '0,400,800,1600,2000,2100,2400,4000,6000,7600'

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None

def compilers():
    result = subprocess.run(['ps', '-axo', 'pid=,comm='], text=True, capture_output=True, check=True)
    return [line.strip() for line in result.stdout.splitlines()
            if pathlib.Path(line.split()[-1]).name in {'cargo', 'rustc', 'clang', 'cc', 'ld', 'ld64.lld'}]

def pct(frames, fraction, field="cpu_ms"):
    values = sorted(f[field] for f in frames)
    # Match Rust's round((n-1)*p), rather than Python's ties-to-even.
    return values[int(math.floor((len(values)-1)*fraction + .5))] if values else None

def stats(frames, budget, field='cpu_ms'):
    return {'n': len(frames), 'field':field, 'p50_ms': pct(frames,.5,field), 'p95_ms': pct(frames,.95,field),
            'p99_ms': pct(frames,.99,field), 'max_ms': max((f[field] for f in frames), default=None),
            'over_budget': [f for f in frames if f[field] > budget]}

def inside(inner, outer, tolerance=.5):
    return inner['x']>=outer['x']-tolerance and inner['y']>=outer['y']-tolerance and inner['x']+inner['width']<=outer['x']+outer['width']+tolerance and inner['y']+inner['height']<=outer['y']+outer['height']+tolerance

def scroll_reaches(scroll, box):
    viewport=scroll['viewport'];content=scroll['content']
    vertical=content['height']>viewport['height']+.5
    horizontal=content['width']>viewport['width']+.5
    if not (vertical or horizontal) or not inside(box,content): return False
    return (horizontal or (box['x']>=viewport['x']-.5 and box['x']+box['width']<=viewport['x']+viewport['width']+.5)) and (vertical or (box['y']>=viewport['y']-.5 and box['y']+box['height']<=viewport['y']+viewport['height']+.5))

def overlaps(a,b):
    return a['x'] < b['x']+b['width']-.5 and a['x']+a['width'] > b['x']+.5 and a['y'] < b['y']+b['height']-.5 and a['y']+a['height'] > b['y']+.5


def stable_motion(report):
    # CPU times are deliberately nondeterministic; ledger values and alignment
    # are not. Keep coverage in the equality check so n/a cannot masquerade as OK.
    return {'captures': report.get('captures'), 'alignment': report.get('alignment'),
            'script': report.get('script'),
            'frames': [{key: value for key,value in frame.items() if key not in {'cpu_ms','input_cpu_ms','input_max_ms'}} for frame in report.get('frames',[])]}

def sweep(width, height):
    acts=[]
    rounded=lambda n: int(math.floor(n+.5))
    for step in range(61):
        t=step/60
        acts.append(f'move {rounded(t*(width-1))},{rounded(t*(height-1))} @{step*16}')
    for step in range(61):
        t=step/60
        acts.append(f'move {rounded((1-t)*(width-1))},{rounded(height/3)} @{(61+step)*16}')
    return '; '.join(acts)

def state_findings(report, scene):
    failures=[]
    frames=report.get('frames',[])
    if not frames or not all(isinstance(f.get('state'),dict) for f in frames):
        return ['actual per-frame graph state was not sampled']
    states=[f['state'] for f in frames]
    for f in frames:
        state=f['state']; cam=state.get('camera')
        if not cam or any(not isinstance(cam.get(k),(float,int)) or not math.isfinite(cam[k]) for k in ('x','y','w')) or cam['w']<=0:
            failures.append(f'invalid camera at {f["at_ms"]}ms')
        for key in ('focused','hovered','selected','tour_stop'):
            node=state.get(key)
            if node and (not isinstance(node.get('id'),int) or not 0<=node['id']<state['world_nodes']):
                failures.append(f'invalid {key} ID at {f["at_ms"]}ms')
        if state['find_open'] and state['exploration']!='free': failures.append('find coexists with exploration')
        viewport=state.get('viewport')
        for scroll in state.get('scrolls',[]):
            if not viewport or not inside(scroll['viewport'],viewport): failures.append(f'actual scroll viewport escapes graph at {f["at_ms"]}ms')
            if scroll['key'] in ('graph-focus-scroll','graph-chain-scroll','graph-tour-scroll'):
                card=state.get('measured_card_bounds')
                if not card or not inside(scroll['viewport'],card): failures.append(f'actual {scroll["key"]} viewport escapes measured card at {f["at_ms"]}ms')
        for key in ('measured_card_bounds','find_bounds'):
            box=state.get(key)
            if box and (not viewport or not inside(box,viewport)): failures.append(f'{key} escapes actual viewport at {f["at_ms"]}ms')
        if state.get('pointer') and state.get('prism') and state['prism']['gathered']>=.999:
            if state.get('hover_slot') != state.get('pointer_prism_pick'): failures.append(f'parked pointer uses stale prism pick at {f["at_ms"]}ms')
        if not state.get('moving') and state.get('focused'):
            gem=state.get('focus_bounds'); card=state.get('measured_card_bounds')
            if not gem: failures.append('actual settled focused glyph bounds absent')
            elif not viewport or not inside(gem,viewport): failures.append(f'settled focus escapes viewport at {f["at_ms"]}ms')
            elif card and overlaps(gem,card): failures.append(f'settled focus overlaps measured card at {f["at_ms"]}ms')
            room=state.get('prism_room')
            for label in state.get('prism_labels',[]):
                if (state.get('prism') or {}).get('gathered',0)<.999: continue
                box={'x':label['x0'],'y':label['y0'],'width':label['x1']-label['x0'],'height':label['y1']-label['y0']}
                if not viewport or not inside(box,viewport): failures.append(f'actual prism label escapes viewport at {f["at_ms"]}ms')
                if card and overlaps(box,card): failures.append(f'actual prism label overlaps measured card at {f["at_ms"]}ms')
                header=state.get('find_bounds')
                if header and overlaps(box,header): failures.append(f'actual prism label overlaps find header at {f["at_ms"]}ms')
    captures={f['time_ms']:f for f in report.get('captures',[])}
    for frame in report.get('captures',[]):
        card=frame['state'].get('measured_card_bounds')
        for text in frame.get('texts',[]):
            if not text['key'].startswith('graph-focus-'): continue
            box={key:text[key] for key in ('x','y','width','height')}
            reachable=card and any(scroll['key']=='graph-focus-scroll' and inside(scroll['viewport'],card) and scroll_reaches(scroll,box) for scroll in frame.get('scrolls',[]))
            if not card or not (inside(box,card) or reachable):
                failures.append(f'actual {text["key"]} text escapes card without measured native scroll reachability at {frame["time_ms"]}ms')
    if scene=='graph-check-idle':
        at=captures.get(2100,{}).get('state',{})
        if not at.get('find_open') or at.get('query')!='glyph::RelationLabel' or not at.get('rows'):
            failures.append('input after idle did not produce actual visible results')
    if scene=='graph-check-interrupt':
        for lower,upper,want in ((2080,2200,0),(2280,2400,5)):
            segment=[f['state'] for f in frames if lower<=f['at_ms']<upper]
            if not segment or not any(st.get('focused',{}).get('id')==want for st in segment if st.get('focused')):
                failures.append(f'focus checkpoint {want} never occurred')
    if scene in ('graph-check-hover','graph-check-live-hover','graph-check-live-hover-naive','graph-check-live-hover-paths','graph-check-peek'):
        hovered=[st['hovered']['id'] for st in states if st.get('hovered')]
        if not hovered: failures.append('pointer script never picked a real symbol')
        if not any(st.get('hovered') and st['drawn']['edges']>0 and not st.get('focused') for st in states):
            failures.append('real hovered symbol never drew edges without focus')
        if scene.startswith('graph-check-live-hover') or scene=='graph-check-hover-phases':
            intended=[int(m.group(1)) for event in report.get('input_events',[]) if (m:=re.fullmatch(r'route graph-hover (\d+)',event['act']))]
            if not intended or intended[0] not in hovered: failures.append('largest-fanout subject was never actually picked')
        if scene=='graph-check-hover' and len(set(hovered))<2: failures.append('rapid sweep covered fewer than two real picked IDs')
        if scene=='graph-check-peek':
            for at in (400,800):
                entries=[entry for stack in captures.get(at,{}).get('stacks',[]) for entry in stack['entries']]
                if at==800 and not any(e['kind']=='peek' and e['phase']=='open' for e in entries):
                    failures.append('same-symbol jitter lost real open peek')
    if scene=='graph-check-hover-phases':
        targets=next((state['expected_hover_targets'] for state in states if state.get('expected_hover_targets')),None)
        if not targets: failures.append('actual visible dense/sparse fixture targets absent; fallback is not coverage')
        else:
            dense=targets['dense']['id'];sparse=targets['sparse']['id']
            for lower,upper,want in ((200,900,dense),(1400,2000,dense),(3000,3080,dense),(3080,3120,sparse),(3120,3180,dense),(3180,3260,sparse)):
                span=[f['state'] for f in frames if lower<=f['at_ms']<upper]
                if not span or not any(st.get('hovered',{}).get('id')==want for st in span if st.get('hovered')):
                    failures.append(f'actual hover handoff target {want} absent in {lower}..{upper}ms')
            if any(st.get('focused') for st in states): failures.append('hover/drag film accidentally focused a subject')
            if not any(2032<=f['at_ms']<2300 and f['state'].get('moving') for f in frames): failures.append('native drag onset was never observed')
            if not any(900<=f['at_ms']<1200 and f['state'].get('fading_hover') for f in frames): failures.append('actual leave fading packet absent')
            if any(st.get('fading_hover') or st.get('hovered') for f,st in zip(frames,states) if f['at_ms']>=7600): failures.append('hover phase tail did not close')
    if scene=='graph-check-parked':
        hovered=[st for st in states if st.get('hovered') and st['hovered']['id']==4]
        if not hovered: failures.append('parked pointer never picked actual unrelated render proxy node4')
        held=[st for f,st in zip(frames,states) if 1050<=f['at_ms']<2200 and st.get('pointer')]
        if not held or len({(st['pointer']['x'],st['pointer']['y']) for st in held})!=1:
            failures.append('weather did not preserve parked pointer coordinates')
        if not any(st.get('viewport',{}).get('width')==480 and st['reduced_motion'] for st in held if st.get('viewport')):
            failures.append('parked-pointer narrow reduced geometry was not sampled')
    if scene=='graph-check-keyboard':
        selected=[f['state']['selected']['id'] for f in frames if 2200<=f['at_ms']<2320 and f['state'].get('selected')]
        focused=[f['state']['focused']['id'] for f in frames if 2320<=f['at_ms']<4300 and f['state'].get('focused')]
        if not selected: failures.append('arrows never selected a real prism proxy')
        elif not focused or selected[-1] not in focused: failures.append('Enter did not follow the actual selected proxy identity')
    if scene=='graph-live-dense-focus':
        intended=[st['expected_focus']['id'] for st in states if st.get('expected_focus')]
        if not intended: failures.append('live qualified dense target absent; fallback is not coverage')
        elif not any(st.get('focused',{}).get('id')==intended[0] and st.get('prism',{}).get('gathered',0)>=.999 for st in states if st.get('focused') and st.get('prism')):
            failures.append('actual live dense target never focused and gathered')
    if scene=='graph-check-chain' and not any(st.get('held_chain')==[10,12,13] for st in states): failures.append('two-call chain was never actually held')
    if scene=='graph-check-weather':
        widths={st.get('viewport',{}).get('width') for st in states if st.get('viewport')}
        if not {480,640}<=widths: failures.append('weather never resized actual graph through480 and640 widths')
        if not any(f['at_ms']>=448 and f['state']['reduced_motion'] for f in frames): failures.append('midflight reduced motion was not applied')
        if not any(st.get('focused',{}).get('id')==0 for st in states if st.get('focused')): failures.append('weather never focused pinned RelationLabel')
    if scene=='graph-check-hold':
        if not any(st.get('focused',{}).get('id')==0 for st in states if st.get('focused')): failures.append('hold never interrupted a real focus flight')
        if not any(280<=f['at_ms']<400 and f['state'].get('moving') for f in frames): failures.append('hold scenario had no moving camera beforepress')
        if any(f['state'].get('moving') for f in frames if f['at_ms']>=1200): failures.append('hold failed to stop actual camera')
    if scene=='graph-check-reach-tour':
        if not {'reach','tour'}<=set(st['exploration'] for st in states): failures.append('reach/tour mode was never entered')
        for lower,upper,want in ((1200,1600,5),(1600,1760,8),(1760,1920,5),(1920,2080,8)):
            span=[f['state'] for f in frames if lower<=f['at_ms']<upper]
            if not span or not any(st.get('tour_stop',{}).get('id')==want for st in span if st.get('tour_stop')):
                failures.append(f'actual tourstop {want} absent in{lower}..{upper}ms')
    if scene.startswith('graph-check-'):
        tail=[f for f in frames if f['at_ms']>=7600]
        if not tail: failures.append('settled tail unobserved')
        elif any(f['requested'] or f['state']['pending_motion'] or f['state']['find_open'] or f['state']['searching'] for f in tail):
            failures.append('tail did not settle to zero requests, pending tracks and search')
    return sorted(set(failures))

def live_hover_topology_findings(report, degree):
    failures=[]
    actual=[f['state'] for f in report.get('frames',[]) if f.get('state',{}).get('hovered') and not f['state'].get('focused') and f['state'].get('hover_strength',1)>.001]
    for state in actual:
        node=state['hovered']['id'];drawn=state['drawn']
        if node not in degree: failures.append('actual hovered node is not a top-level fixture item')
        elif drawn.get('hover_relations') != degree[node]: failures.append(f'hover relation count {drawn.get("hover_relations")} does not preserve fixture neighbourhood {degree[node]} for actual node {node}')
        if drawn.get('hover_routes',0)>drawn.get('hover_relations',0):
            failures.append('actual hover routes exceed retained relations')
    for node in {state['hovered']['id'] for state in actual}:
        if degree.get(node,0)>0 and not any(state['hovered']['id']==node and state['drawn'].get('hover_routes',0)>0 for state in actual):
            failures.append('actual hover semantic routes were never visibly submitted')
    return sorted(set(failures))

def complete_png(path):
    # A watchdog can leave an unfinished file. Never caption it as evidence.
    if path.stat().st_size < 20: return False
    with path.open('rb') as handle:
        if handle.read(8) != b'\x89PNG\r\n\x1a\n': return False
        handle.seek(-12,2)
        return handle.read() == b'\x00\x00\x00\x00IEND\xaeB`\x82'

def write_atlas(out):
    records=[]; cards=[]
    for report_path in sorted(out.glob('run-*/correctness-*/motion.json')):
        report=json.loads(report_path.read_text()); directory=report_path.parent
        images={int(m.group(1)):p for p in directory.glob('*.png') if complete_png(p) and (m:=re.search(r'-t(\d+)(?:-rm)?@[12]x\.png$',p.name))}
        for frame in report.get('captures',[]):
            at=frame['time_ms']; image=images.get(at); state=frame.get('state')
            if image is None or state is None: continue
            relative=image.relative_to(out).as_posix()
            record={'scene':report['scene'],'time_ms':at,'image':str(image),'report':str(report_path),'state':state}
            records.append(record)
            names=lambda key: (state.get(key) or {}).get('name','—')
            caption=f'{report["scene"]} · {at}ms · {state["exploration"]} · focus {names("focused")} · hover {names("hovered")} · selected {names("selected")}'
            camera=state.get('camera') or {}; geometry=f'camera {camera} · card {state.get("card_bounds")} · pending {state["pending_motion"]} · requested {state["frames_requested"]} · edges {state["drawn"]["edges"]}'
            cards.append('<article><a href="'+quote(relative)+'"><img loading="lazy" src="'+quote(relative)+'" alt="'+html.escape(caption,quote=True)+'"></a><p>'+html.escape(caption)+'</p><small>'+html.escape(geometry)+'</small><details><summary>Exact state and evidence</summary><pre>'+html.escape(json.dumps(record,indent=2))+'</pre></details></article>')
    (out/'ATLAS.json').write_text(json.dumps(records,indent=2)+'\n')
    (out/'atlas.html').write_text('<!doctype html><meta charset="utf-8"><title>Native GPUI graph evidence</title><style>body{background:#141821;color:#d8deea;font:14px system-ui;margin:24px}h1{font-size:22px}main{display:grid;grid-template-columns:repeat(auto-fit,minmax(560px,1fr));gap:24px}article{min-width:0;padding:12px;background:#202632}img{width:100%;height:auto}p{line-height:1.6;margin:8px 0}small{display:block;color:#aebad1;word-break:break-word}pre{white-space:pre-wrap;overflow-wrap:anywhere;font-size:12px}</style><h1>Native GPUI graph evidence</h1><p>Each image is a real native-renderer capture. Captions come from the actual state sampled in that frame; full state, exact image path and motion report are attached.</p><main>'+''.join(cards)+'</main>')
    return len(records)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=pathlib.Path, required=True)
    parser.add_argument('--out', type=pathlib.Path, default=ROOT / '.local' / 'graph-audit')
    parser.add_argument('--mode', choices=['correctness','perf','all'], default='all')
    parser.add_argument('--runs', type=int, default=2)
    parser.add_argument('--scale', type=int, choices=[1,2], default=2, help='Correctness screenshot device scale; perf keeps its native defaults')
    parser.add_argument('--scenes', help='Optional comma-separated subset; REPORT records limited scope explicitly')
    args = parser.parse_args()
    if args.runs != 2:
        parser.error('this audit requires two consecutive runs')
    selected = set(args.scenes.split(',')) if args.scenes else None
    args.binary = args.binary.resolve()
    args.out = args.out.resolve()
    if not args.binary.is_file(): parser.error('gallery executable is absent; root must build first')
    args.out.mkdir(parents=True, exist_ok=True)
    original_binary = args.binary
    args.binary = args.out/'gallery-executable'
    shutil.copy2(original_binary,args.binary)
    manifest = {'repository':str(ROOT), 'verifier_sha256':digest(pathlib.Path(__file__)), 'binary_source': str(original_binary), 'binary': str(args.binary), 'binary_sha256': digest(args.binary),
                'fixture_sha256': digest(ROOT/'Nudox-Design-System/v4/graph/world.json'),
                'prototype_sha256': digest(ROOT/'Nudox-Design-System/v4/graph/world.js'),
                'started_utc': time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime()),
                'correctness_pixel_scale':args.scale, 'scope': sorted(selected) if selected else 'full', 'commands': [], 'runs': [], 'failures': [], 'skips': [], 'notes': [
                    'p95 is a distribution gate, not a worst-frame guarantee; every over-budget draw is retained.',
                    'input/requested/all subsets include cold frames; warm-up is reported separately.',
                    'Two deterministic motion/capture runs must agree; CPU timings are never compared for identity.',
                    'Live extraction is scale/invariant coverage; pinned scenes provide exact value assertions.',
                    'Window::draw CPU is render/layout/prepaint/paint time, not displayed cadence or GPU completion.',
                    'Detailed motion reports enable probes; official perf runs do not. Both measurements are retained.',
                    'Naive and AllPaths controls are comparative alternatives: their release budget failures are recorded explicitly, while chosen Batched gates remain unchanged.']}
    def save(): (args.out/'REPORT.json').write_text(json.dumps(manifest,indent=2)+'\n')
    def command(run_dir, label, argv, quiet=False, comparative_budget=False):
        if quiet:
            busy = compilers()
            if busy: raise RuntimeError('concurrent compiler invalidates performance audit: '+repr(busy))
        start = time.monotonic()
        timeout = 180 if argv[0] == 'perf' else 90
        timed_out = False
        try:
            proc = subprocess.run([str(args.binary),*argv], cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout)
        except subprocess.TimeoutExpired as exc:
            timed_out = True
            output = exc.stdout or ''
            if isinstance(output, bytes): output = output.decode('utf-8', errors='replace')
            proc = subprocess.CompletedProcess([str(args.binary),*argv], 124, output+'\nWATCHDOG: command terminated after '+str(timeout)+' seconds.\n')
        (run_dir/(label+'.log')).write_text(proc.stdout)
        record = {'argv':[str(args.binary),*argv], 'exit':proc.returncode,
                  'elapsed_s':time.monotonic()-start, 'timeout_s':timeout, 'timed_out':timed_out, 'log':str(run_dir/(label+'.log'))}
        manifest['commands'].append(record)
        if quiet and compilers(): manifest['failures'].append(label+': compiler started during measurement')
        comparative_failure = comparative_budget and proc.returncode==1 and not timed_out and 'FAIL (budget' in proc.stdout and 'NOT BUDGETED' not in proc.stdout
        record['comparative_budget_failure'] = comparative_failure
        if proc.returncode and not comparative_failure: manifest['failures'].append(label+': exit '+str(proc.returncode))
        save()
        return proc
    listing = command(args.out,'list',['list']).stdout
    absent = [s for s in CASES+LIVE+CONTROLS+['graph-pinned-focus','graph-pinned-offset'] if not re.search(r'\b'+re.escape(s)+r'\b', listing)]
    if absent: raise RuntimeError('missing registered scenarios: '+repr(absent))
    live = list(LIVE)
    fixture_hover_degree={}
    fixture_path=ROOT/'Nudox-Design-System/v4/graph/world.json'
    if not fixture_path.is_file():
        manifest['skips'].append({'scenes':live,'reason':'workspace scale fixture absent; live coverage not claimed'})
        live=[]
    else:
        world=json.loads(fixture_path.read_text())
        packages={p['name'] for p in world['packages']}
        names={n['n'] for n in world['nodes']}
        targets={(world['packages'][n['p']]['name'],world['modules'][n['m']]['path'],n['n']) for n in world['nodes']}
        manifest['fixture_counts']={k:len(world[k]) for k in ('packages','modules','nodes','edges')}
        # Independently project each edge to its parent item, retain distinct
        # directed item pairs, and discard self-pairs. This is a topology
        # invariant from the actual fixture, never an ID/value pinned to it.
        top=[n['u'] if n.get('u',-1)>=0 else i for i,n in enumerate(world['nodes'])]
        projected={(top[a],top[b]) for a,b,*_ in world['edges'] if top[a]!=top[b]}
        fixture_hover_degree={i:0 for i,node in enumerate(world['nodes']) if node.get('u',-1)<0}
        for a,b in projected:
            fixture_hover_degree[a]+=1;fixture_hover_degree[b]+=1
        required={'graph-present':('backend-present','RelationLabel'),
                  'graph-glyph':('backend-present','RelationLabel'),
                  'graph-members':('backend-present','RelationLabel'),
                  'graph-hover':('backend-present','RelationLabel'),
                  'graph-focus':('backend-present','RelationGroup'),
                  'graph-flight-a':('backend-present','RelationLabel'),
                  'graph-flight-b':('serde_json','from_str'),
                  'graph-journey':('backend-present','RelationLabel'),
                  'graph-engine':('backend-engine',None),
                  'graph-live-dense-focus':('std','Debug')}
        for scene,(package,name) in required.items():
            module='fmt' if scene=='graph-live-dense-focus' else 'de' if package=='serde_json' else 'page' if name=='RelationGroup' else 'glyph'
            if package not in packages or (name is not None and (package,module,name) not in targets):
                live.remove(scene)
                manifest['skips'].append({'scene':scene,'reason':f'fixture target absent: {package}/{name}; fallback world is not coverage'})
    save()
    previous = {}
    for run in range(1,3):
        run_dir = args.out/f'run-{run}'
        run_dir.mkdir(exist_ok=True)
        entries = []
        if args.mode in {'correctness','all'}:
            variants = [(s,[]) for s in CASES]
            variants += [('graph-pinned-offset',[]),('graph-pinned-offset',['--theme','glacier','--text-scale','200'])]
            variants += [('graph-check-interrupt',['--reduced-motion']),('graph-check-keyboard',['--reduced-motion'])]
            variants += [('graph-pinned-focus',['--size',f'{width}x824','--theme','glacier','--text-scale','200',*motion])
                         for width in (480,640,760) for motion in ([],['--reduced-motion'])]
            variants += [(s,['--size','480x900','--theme',theme,'--text-scale','200'])
                         for s in ('graph-focus','graph-check-live-hover') if s in live
                         for theme in ('abyss','glacier')]
            variants += [('graph-check-hover-phases',['--theme',theme]) for theme in ('abyss','glacier') if 'graph-check-hover-phases' in live]
            variants += [('graph-live-dense-focus',['--size',size,'--theme',theme,'--text-scale',text,'--reduced-motion'])
                         for size,text in (('480x824','200'),('640x824','150')) for theme in ('abyss','glacier')
                         if 'graph-live-dense-focus' in live]
            # Pairwise short-height coverage includes native scrolling reading
            # panels, held chain and reach/tour; each variant keeps exact state.
            variants += [(scene,['--size',size,'--theme',theme,'--text-scale',text,'--reduced-motion'])
                         for scene in ('graph-pinned-focus','graph-check-chain','graph-check-reach-tour','graph-live-dense-focus')
                         if scene!='graph-live-dense-focus' or scene in live
                         for size,theme,text in (('760x400','abyss','100'),('760x400','glacier','200'),('480x400','glacier','100'),('480x400','abyss','200'))]
            if selected: variants = [(scene,options) for scene,options in variants if scene in selected]
            if not variants: raise RuntimeError('requested correctness subset contains no cases')
            for i,(scene,options) in enumerate(variants):
                label=f'correctness-{i:02d}-{scene}'
                art=run_dir/label; art.mkdir(exist_ok=True)
                report_path=art/'motion.json'
                common=['--scene',scene,*options,'--scale',str(args.scale)]
                proc=command(run_dir,label+'-motion',['motion-report',*common,'--times',TIMES,'--until','8000','--out',str(report_path)])
                if report_path.is_file():
                    report=json.loads(report_path.read_text())
                    alignment=report['alignment']
                    for reason in state_findings(report,scene): manifest['failures'].append(label+': '+reason)
                    if scene.startswith('graph-check-live-hover') or scene=='graph-check-hover-phases':
                        for reason in live_hover_topology_findings(report,fixture_hover_degree): manifest['failures'].append(label+': '+reason)
                    if not alignment['pass']: manifest['failures'].append(label+': motion alignment')
                    if alignment['checks']['idle']['evaluated']==0:
                        manifest['failures'].append(label+': idle check had no coverage')
                    stable=stable_motion(report)
                    if run==2 and previous.get(label)!=stable: manifest['failures'].append(label+': deterministic motion ledger differs')
                    previous[label]=stable
                    texts={f['time_ms']:{t['key']:t['content'] for t in f.get('texts',[])} for f in report.get('captures',[])}
                    if scene=='graph-check-chain':
                        if 'Invocation' not in texts.get(800,{}).get('graph-chain-title',''):
                            manifest['failures'].append(label+': held chain title absent at800ms')
                        if texts.get(2000,{}).get('graph-chain-code')!='invocation.grammar()?.aliases()':
                            manifest['failures'].append(label+': actual Alt code differs from pinned optional chain')
                    if scene=='graph-check-reach-tour':
                        if '2 direct' not in texts.get(400,{}).get('graph-reach-summary',''):
                            manifest['failures'].append(label+': nonzero reach content absent')
                        if 'serde_json' not in texts.get(1600,{}).get('graph-tour-title',''):
                            manifest['failures'].append(label+': actual tour title absent')
                    if scene=='graph-check-interrupt':
                        if 'RelationLabel' not in texts.get(2100,{}).get('graph-focus-title',''):
                            manifest['failures'].append(label+': expected focused RelationLabel content absent')
                    entries.append({'label':label,'alignment':alignment,'checkpoint_texts':texts})
                if proc.returncode == 124:
                    entries.append({'label':label,'watchdog_timeout':True,'captures_unavailable':True})
                    continue
                proc=command(run_dir,label+'-film',['film',*common,'--times',TIMES,'--frames','--onion','--columns','4','--out',str(art)])
                hashes=re.findall(r'rgba-sha256 ([0-9a-f]{64})',proc.stdout)
                if not hashes: manifest['failures'].append(label+': film emitted no frame digests')
                if run==2 and previous.get(label+'-hashes')!=hashes: manifest['failures'].append(label+': film pixels differ')
                previous[label+'-hashes']=hashes
                command(run_dir,label+'-lint',['lint',*common,'--time','7600'])
        if args.mode in {'perf','all'}:
            perf_scenes = [scene for scene in live+CASES if selected is None or scene in selected]
            # Controls use identical scripts/viewports; comparison results do
            # not redefine the release budget for the chosen batched renderer.
            controlled=[s for s in CONTROLS if selected is None or s in selected]
            if not perf_scenes+controlled: raise RuntimeError('requested performance subset contains no cases')
            for scene in perf_scenes+controlled:
                control=scene in CONTROLS
                for width,height,budget in ((1440,900,8),(2560,1440,12)):
                    label=f'perf-{scene}-{width}x{height}'
                    size=f'{width}x{height}'
                    # Match perf::measure's sweep fallback for scenes with no declared script.
                    inputs=[] if scene in CASES+['graph-hover','graph-journey','graph-flight-a','graph-flight-b','graph-check-live-hover','graph-check-live-hover-naive','graph-check-live-hover-paths','graph-live-dense-focus','graph-check-hover-phases'] else ['--input',sweep(width,height)]
                    proc=command(run_dir,label,['perf','--scene',scene,'--sizes',size,*inputs],quiet=True,comparative_budget=control)
                    if not control and ('NOT BUDGETED' in proc.stdout or 'PASS (budget' not in proc.stdout):
                        manifest['failures'].append(label+': absent release budget PASS')
                    report_path=run_dir/(label+'.json')
                    command(run_dir,label+'-frames',['motion-report','--scene',scene,'--size',size,*inputs,
                            '--until','8000','--out',str(report_path)],quiet=True)
                    if not report_path.is_file(): continue
                    report=json.loads(report_path.read_text()); frames=report.get('frames',[])
                    if not frames: manifest['failures'].append(label+': all-frame export absent'); continue
                    subsets={'all':frames,'cold':[f for f in frames if f['at_ms']<100],
                             'warm':[f for f in frames if f['at_ms']>=100],
                             'requested':[f for f in frames if f['requested']],
                             'input':[f for f in frames if f['events']>0]}
                    measurements={k:stats(v,budget) for k,v in subsets.items()}
                    input_frames=[f for f in frames if f.get('input_events',0)>0]
                    timing_present=all(all(k in f for k in ('input_cpu_ms','input_events','input_max_ms')) for f in frames)
                    if not timing_present: manifest['failures'].append(label+': actual input dispatch timing absent')
                    input_measurement=stats(input_frames,budget,'input_cpu_ms') if timing_present else None
                    if input_measurement:
                        input_measurement.update({'events':sum(f['input_events'] for f in input_frames),
                            'max_event_ms':max((f['input_max_ms'] for f in input_frames),default=None),
                            'meaning':'per-draw input batches; includes real dispatch/adapter/immediate foreground work, excludes background waits and draw; over-budget events diagnostic'})
                    if scene.startswith('graph-check-live-hover') or scene=='graph-check-hover-phases':
                        for reason in state_findings(report,scene): manifest['failures'].append(label+': '+reason)
                        for reason in live_hover_topology_findings(report,fixture_hover_degree): manifest['failures'].append(label+': '+reason)
                    if not report['alignment']['pass']: manifest['failures'].append(label+': alignment failed')
                    for key in ('all','requested','input'):
                        subset=measurements[key]
                        if not control and subset['n'] and subset['p95_ms']>budget:
                            manifest['failures'].append(f'{label}: {key} p95 {subset["p95_ms"]:.3f} > {budget} ms')
                    entries.append({'label':label,'budget_ms':budget,'comparative_control':control,'official_perf_log':str(run_dir/(label+'.log')),'subsets':measurements,'input_dispatch':input_measurement,
                                    'alignment':report['alignment']})
        manifest['runs'].append({'run':run,'entries':entries}); manifest['atlas_captures']=write_atlas(args.out); save()
    if args.mode in {'correctness','all'} and not manifest.get('atlas_captures'): manifest['failures'].append('atlas has no actual-state screenshots')
    manifest['passed']=not manifest['failures']
    save()
    print(str(args.out/'REPORT.json'))
    print('PASS' if manifest['passed'] else '\n'.join(manifest['failures']))
    return 0 if manifest['passed'] else 1

if __name__=='__main__':
    try: raise SystemExit(main())
    except (RuntimeError,ValueError,KeyError) as exc: raise SystemExit(str(exc))
