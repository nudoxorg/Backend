#!/usr/bin/env python3
"""Export genuine GPUI motion frames, sidecars, MP4s and contact sheets.

No interpolation. Capture cadence is 32ms; the native harness draws at 16ms.
Requires a separately built facet-gallery and ffmpeg. Never builds the project.
"""
import argparse, hashlib, importlib.util, json, pathlib, shutil, subprocess, time

ROOT=pathlib.Path(__file__).resolve().parents[2]
DEFAULT_SCENES=('graph-journey','graph-flight-a','graph-flight-b','graph-check-interrupt','graph-check-hover-phases')

def sha(path): return hashlib.sha256(path.read_bytes()).hexdigest()

def contact_sheet(frames, path, capture_ms, duration):
    from PIL import Image, ImageDraw, ImageFont
    marks=list(range(0,duration+1,1000))
    scene=frames[0].get('scene')
    phase=scene=='graph-check-hover-phases'
    if scene in ('graph-flight-a','graph-flight-b'): marks += [200,264,328,392,456,520,584,648,712,800]
    if scene=='graph-check-interrupt': marks += [2000,2080,2160,2200,2280,2400,2520,2600,2720,2816,2880,3200]
    if scene=='graph-journey': marks += [200,400,650,1400,1500,1600,1700,3300,3360,3500,3800,4500,4600,4800]
    if phase: marks += [880,900,920,944,976,1008,1040,1080,1120,1400,1600,1952,2000,2032,2064,2096,2130,2160,3000,3040,3080,3120,3180,3260,3360,3600,3680,4300,4320]
    selected=sorted({min(range(len(frames)),key=lambda i:abs(frames[i]['time_ms']-mark)) for mark in marks}|{len(frames)-1})
    columns=min(4,len(selected));rows=(len(selected)+columns-1)//columns
    width=360
    with Image.open(frames[0]['image']) as first:image_height=round(first.height*width/first.width)
    label_height=48 if phase else 32;canvas=Image.new('RGB',(columns*width,rows*(image_height+label_height)),(20,24,33))
    font=ImageFont.truetype(str(ROOT/'tools/gui-harness/assets/fonts/GeistMono[wght].ttf'),13)
    draw=ImageDraw.Draw(canvas);records=[]
    for ordinal,index in enumerate(selected):
        frame=frames[index];x=(ordinal%columns)*width;y=(ordinal//columns)*(image_height+label_height)
        with Image.open(frame['image']) as source:canvas.paste(source.convert('RGB').resize((width,image_height),Image.Resampling.LANCZOS),(x,y))
        state=frame['state'];focus=(state.get('focused') or {}).get('name','—')
        label=f"{frame['time_ms']} ms · {state['exploration']} · {focus}"
        draw.text((x+8,y+image_height+8),label,fill=(216,222,234),font=font)
        if phase:
            hover=(state.get('hovered') or {}).get('name','—');fade=state.get('fading_hover') or {}
            fading=(fade.get('node') or {}).get('name','—')
            draw.text((x+8,y+image_height+26),f"hover {hover} {state.get('hover_strength',0):.2f} · fade {fading} {fade.get('strength',0):.2f}",fill=(174,186,207),font=font)
        records.append({'time_ms':frame['time_ms'],'image':frame['image'],'state':state,'label':label})
    canvas.save(path,compress_level=1)
    metadata={'columns':columns,'rows':rows,'selection':'nearest actual captures to full seconds and scene transition marks, plus exact final capture','requested_marks_ms':sorted(set(marks)), 'frames':records}
    path.with_suffix('.json').write_text(json.dumps(metadata,indent=2)+'\n')
    return metadata

def film_state_findings(report, scene):
    """Do not accept a movie whose native inputs silently missed their state."""
    spec=importlib.util.spec_from_file_location('graph_verify',pathlib.Path(__file__).with_name('graph_verify.py'))
    verifier=importlib.util.module_from_spec(spec);spec.loader.exec_module(verifier)
    failures=verifier.state_findings(report,scene)
    frames=report.get('frames',[])
    if not frames:return failures
    states=[frame['state'] for frame in frames];last=states[-1]
    if scene in ('graph-flight-a','graph-flight-b'):
        name='RelationLabel' if scene=='graph-flight-a' else 'from_str'
        if (last.get('focused') or {}).get('name')!=name or (last.get('prism') or {}).get('gathered',0)<.999:
            failures.append('flight did not arrive at its actual intended symbol/prism')
        if last.get('moving') or last.get('pending_motion') or frames[-1]['requested']:
            failures.append('flight final state still requests motion')
    if scene=='graph-journey':
        initial=states[0]['camera']
        for lower,upper in ((650,1400),(3400,4500)):
            if not any(lower<=frame['at_ms']<upper and not frame['state'].get('focused') and frame['state']['camera']['w']<initial['w']*.9 for frame in frames):
                failures.append(f'journey package view absent in {lower}..{upper}ms')
        if not any((state.get('focused') or {}).get('name')=='RelationLabel' and (state.get('prism') or {}).get('gathered',0)>=.999 for state in states):
            failures.append('journey never visibly gathered its intended RelationLabel prism')
        if last.get('focused') or any(abs(last['camera'][key]-initial[key])>1e-4 for key in ('x','y','w')):
            failures.append('journey never returned to its actual initial world camera')
        if last.get('moving') or last.get('pending_motion') or frames[-1]['requested']:
            failures.append('journey world tail still requests motion')
    return sorted(set(failures))

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=pathlib.Path,required=True)
    parser.add_argument('--out',type=pathlib.Path,required=True)
    parser.add_argument('--scenes',default=','.join(DEFAULT_SCENES))
    parser.add_argument('--duration',type=int,default=8000)
    parser.add_argument('--capture-ms',type=int,default=32)
    parser.add_argument('--frame-ms',type=int,default=16)
    parser.add_argument('--size',default='1440x824')
    parser.add_argument('--ffmpeg',default=shutil.which('ffmpeg') or '/opt/homebrew/bin/ffmpeg')
    args=parser.parse_args()
    if min(args.duration,args.capture_ms,args.frame_ms)<=0 or args.capture_ms%args.frame_ms:
        parser.error('positive duration and capture cadence divisible by draw cadence required')
    args.out=args.out.resolve();args.out.mkdir(parents=True,exist_ok=True)
    source=args.binary.resolve();binary=args.out/'gallery-executable';shutil.copy2(source,binary)
    manifest={'binary_source':str(source),'binary_sha256':sha(binary),'tool_sha256':sha(pathlib.Path(__file__)),
        'repository':str(ROOT),'started_utc':time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime()),
        'size':args.size,'capture_ms':args.capture_ms,'draw_ms':args.frame_ms,'commands':[],'films':[],'failures':[],
        'notes':['Every input image is an actual GPUI draw with exact state/time sidecar. No synthetic tweening or interpolated frames.',
                 'MP4 playback rate equals capture cadence. The motion report separately observes all draws at the finer draw cadence.',
                 'Captured/probed CPU is diagnostic, not official release performance. PNG encoding and ffmpeg are outside draw timing.']}
    def save(): (args.out/'FILMS.json').write_text(json.dumps(manifest,indent=2)+'\n')
    def run(label,command,timeout=180):
        started=time.monotonic()
        manifest['active_command']={'label':label,'argv':command,'timeout_s':timeout};save()
        try: result=subprocess.run(command,cwd=ROOT,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=timeout)
        except subprocess.TimeoutExpired as exc:
            output=exc.stdout or b'';output=output.decode(errors='replace') if isinstance(output,bytes) else output
            result=subprocess.CompletedProcess(command,124,output+'\nWATCHDOG timeout\n')
        log=args.out/(label+'.log');log.write_text(result.stdout)
        manifest.pop('active_command',None)
        manifest['commands'].append({'argv':command,'exit':result.returncode,'timeout_s':timeout,'elapsed_s':time.monotonic()-started,'log':str(log)})
        if result.returncode: manifest['failures'].append(f'{label}: exit {result.returncode}')
        save();return result.returncode==0
    times=list(range(0,args.duration+1,args.capture_ms))
    for scene in args.scenes.split(','):
        directory=args.out/scene;directory.mkdir(exist_ok=True)
        if not run(scene+'-frames',[str(binary),'sequence','--scene',scene,'--size',args.size,'--scale','1',
                '--frame-ms',str(args.frame_ms),'--times',','.join(map(str,times)),'--until',str(args.duration),'--out',str(directory)]): continue
        frames=[]
        for sidecar in directory.glob('*.json'):
            if sidecar.name=='SEQUENCE.json':continue
            data=json.loads(sidecar.read_text())
            if 'time_ms' in data:frames.append(data)
        frames.sort(key=lambda f:f['time_ms'])
        if [f['time_ms'] for f in frames]!=times or any(not isinstance(f.get('state'),dict) for f in frames):
            manifest['failures'].append(scene+': incomplete native frame/state coverage');save();continue
        links=directory/'ordered';links.mkdir(exist_ok=True)
        for index,frame in enumerate(frames):
            link=links/f'frame-{index:06d}.png'
            if link.is_symlink():link.unlink()
            link.symlink_to(pathlib.Path(frame['image']).resolve())
        movie=directory/(scene+'.mp4');sheet=directory/(scene+'-contact.png')
        movie_ok=run(scene+'-mp4',[args.ffmpeg,'-hide_banner','-nostdin','-y','-framerate',str(1000/args.capture_ms),
                '-i',str(links/'frame-%06d.png'),'-c:v','libx264','-preset','medium','-crf','18','-pix_fmt','yuv420p','-movflags','+faststart',str(movie)])
        try:contact=contact_sheet(frames,sheet,args.capture_ms,args.duration)
        except Exception as exc:
            contact=None;manifest['failures'].append(scene+': contact sheet '+str(exc));save()
        report=directory/'motion.json'
        run(scene+'-motion',[str(binary),'motion-report','--scene',scene,'--size',args.size,'--scale','1','--frame-ms',str(args.frame_ms),
            '--times','0,200,400,800,1600,2400,4000,6000,7600','--until',str(args.duration),'--out',str(report)])
        state_failures=film_state_findings(json.loads(report.read_text()),scene) if report.is_file() else ['native motion state report absent']
        manifest['failures'].extend(scene+': '+failure for failure in state_failures)
        manifest['films'].append({'scene':scene,'state_findings':state_failures,'frames':len(frames),'times_ms':times,'sidecars':str(directory),
            'mp4':str(movie) if movie_ok else None,'contact_sheet':str(sheet) if sheet.exists() else None,'contact_metadata':contact,
            'motion_report':str(report),'first_state':frames[0]['state'],'last_state':frames[-1]['state'],
            'mp4_sha256':sha(movie) if movie_ok else None})
        save()
    manifest['passed']=not manifest['failures'] and len(manifest['films'])==len(args.scenes.split(','));save()
    print(args.out/'FILMS.json');return 0 if manifest['passed'] else 1

if __name__=='__main__':raise SystemExit(main())
