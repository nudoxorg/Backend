#!/usr/bin/env python3
"""Export genuine GPUI motion frames, sidecars, MP4s and contact sheets.

No interpolation. Capture cadence is 32ms; the native harness draws at 16ms.
Requires a separately built facet-gallery and ffmpeg. Never builds the project.
"""
import argparse, hashlib, json, pathlib, shutil, subprocess, time

ROOT=pathlib.Path(__file__).resolve().parents[2]
DEFAULT_SCENES=('graph-journey','graph-flight-a','graph-flight-b','graph-check-interrupt','graph-check-hover-phases')

def sha(path): return hashlib.sha256(path.read_bytes()).hexdigest()

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
        try: result=subprocess.run(command,cwd=ROOT,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=timeout)
        except subprocess.TimeoutExpired as exc:
            output=exc.stdout or b'';output=output.decode(errors='replace') if isinstance(output,bytes) else output
            result=subprocess.CompletedProcess(command,124,output+'\nWATCHDOG timeout\n')
        log=args.out/(label+'.log');log.write_text(result.stdout)
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
        run(scene+'-contact',[args.ffmpeg,'-hide_banner','-nostdin','-y','-i',str(movie),'-vf',
            'fps=1,scale=360:-1,tile=4x3','-frames:v','1',str(sheet)])
        report=directory/'motion.json'
        run(scene+'-motion',[str(binary),'motion-report','--scene',scene,'--size',args.size,'--scale','1','--frame-ms',str(args.frame_ms),
            '--times','0,200,400,800,1600,2400,4000,6000,7600','--until',str(args.duration),'--out',str(report)])
        manifest['films'].append({'scene':scene,'frames':len(frames),'times_ms':times,'sidecars':str(directory),
            'mp4':str(movie) if movie_ok else None,'contact_sheet':str(sheet) if sheet.exists() else None,
            'motion_report':str(report),'first_state':frames[0]['state'],'last_state':frames[-1]['state'],
            'mp4_sha256':sha(movie) if movie_ok else None})
        save()
    manifest['passed']=not manifest['failures'] and len(manifest['films'])==len(args.scenes.split(','));save()
    print(args.out/'FILMS.json');return 0 if manifest['passed'] else 1

if __name__=='__main__':raise SystemExit(main())
