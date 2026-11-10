"""Disposable S11 Mac driver. Use uv run run.py APP ENDPOINT RUN.
APP: tauri/gpui. ENDPOINT: ssh/local. Requires the fixture and Muxflow S11 Meter grants.
The same ScreenCaptureKit/HID executable observes both release apps.
"""
import json, os, subprocess as sp, time, pathlib, sys, math, random, plistlib, fcntl
ROOT=pathlib.Path(__file__).resolve().parents[4]
WORK=ROOT/'tmp/s11'
OUT=ROOT/'tests/performance/runtime/renderer-baseline/s11-macos'
OUT.mkdir(parents=True,exist_ok=True)
APP,ENDPOINT,RUN=sys.argv[1:4]
lock=(WORK/'benchmark.lock').open('w');fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
RNG=random.Random(110)
SCENARIOS=os.environ.get('S11_SCENARIOS','key-1,key-8-filled,key-flood7,tab-flood7,pane-flood6,resize-flood7,self-flood,all-flood60').split(',')
SAMPLES=int(os.environ.get('S11_SAMPLES','120'))
def run(*args,**kw):return sp.run(list(map(str,args)),capture_output=True,text=True,timeout=30,**kw)
def cua(name,**args):
 r=run('cua-driver','call',name,json.dumps(args));return json.loads(r.stdout)
def processes():
 out={}
 for line in run('ps','-axo','pid=,ppid=,time=,command=').stdout.splitlines():
  parts=line.split(None,3)
  if len(parts)==4:
   pid,ppid,cpu,cmd=parts;m,s=cpu.split(':');out[int(pid)]=(int(ppid),float(m)*60+float(s),cmd)
 return out
class Fixture:
 def __init__(self):
  self.env=os.environ.copy();self.env.update(ADE_HOST_RUNTIME_DIR='/private/tmp/muxflow-s11-localhost',TMUX_TMPDIR='/private/tmp/muxflow-s11-tmux',ADE_TMUX_SOCKET_NAME='muxflow-qa-s11')
  self.helper=ROOT/'tmp/work/cache/release-target/macos/release/muxflow-host'
 def command(self,args):
  import shlex
  if ENDPOINT=='ssh':r=run('ssh','-F',WORK/'ssh_config','muxflow-s11-container',shlex.join(list(map(str,args))))
  else:r=run(*args,env=self.env)
  if r.returncode:raise RuntimeError(r.stderr.strip())
  return r.stdout.strip()
 def tmux(self,*a):return self.command((['tmux'] if ENDPOINT=='ssh' else ['tmux','-L','muxflow-qa-s11','-f','/dev/null'])+list(a))
 def send(self,pane,text):self.tmux('send-keys','-t',pane,text,'Enter')
 def reset(self,n):
  try:self.command([str(self.helper) if ENDPOINT=='local' else '.local/bin/muxflow-host','daemon-stop'])
  except Exception:pass
  try:self.tmux('kill-server')
  except Exception:pass
  self.tmux('new-session','-d','-s','s11','-x','160','-y','44','bash --noprofile --norc')
  self.tmux('set','-g','status','off')
  for _ in range(n-1):self.tmux('split-window','-d','-t','s11:0','bash --noprofile --norc');self.tmux('select-layout','-t','s11:0','tiled')
  self.tmux('new-window','-d','-t','s11:1','bash --noprofile --norc')
  self.panes=self.tmux('list-panes','-t','s11:0','-F','#{pane_id}').splitlines()
  self.tmux('select-pane','-t',self.panes[-1])
 @property
 def flood(self):return '/usr/local/bin/s11-flood' if ENDPOINT=='ssh' else str(WORK/'flood')
 def start_flood(self,n):
  for p in self.panes[:n]:self.send(p,'exec '+self.flood)
class App:
 def __init__(self,fixture):self.f=fixture;self.p=None;self.pixel=None;self.pidset=set();self.colors={}
 def start(self):
  before=processes();home=WORK/f'home-{APP}-{ENDPOINT}';cfg=home/'Library/Application Support/dev.muxflow.desktop';cfg.mkdir(parents=True,exist_ok=True)
  connection={'mode':'local'} if ENDPOINT=='local' else {'mode':'ssh','profileId':'s11','target':'muxflow-s11-container','configPath':str(WORK/'ssh_config')}
  profile_id='local' if ENDPOINT=='local' else 's11'
  (cfg/'profiles.json').write_text(json.dumps(dict(schemaVersion=1,profiles=[dict(id=profile_id,label='S11 fixture',shown=True,connection=connection)],lastProfileId=profile_id)))
  # Keep the app's chrome from making the terminal area incomparable.
  state=cfg/'app-state.json'
  template=ROOT/'apps/desktop/src/features/shell/persistedAppState.contract.json'
  if not state.exists():
   initial=json.loads(template.read_text());initial.update(appTabs=[],workspaceUi=[],hostSetup={},workspaceDefaults={},commands={'shortcutOverrides':{}})
   initial['shell'].update(panelOpen=False,windowGeometry=None,terminalFontSize=13,uiScale=1.0,focusFollowsMouse=False)
   state.write_text(json.dumps(initial))
  if state.exists():
   s=json.loads(state.read_text());s['shell'].update(theme='dark',sidebarCollapsed=True);s.setdefault('hostSetup',{})[profile_id]='declined';s['commands']['shortcutOverrides'].update({'tab.select1':'Ctrl+Alt+F5','tab.select2':'Ctrl+Alt+F6'});state.write_text(json.dumps(s))
  e=self.f.env.copy();e.update(HOME=str(home),CFFIXED_USER_HOME=str(home),ADE_HOST_HELPER_LINUX_X86_64_PATH=str(WORK/'muxflow-host-linux-x86_64'))
  if APP=='tauri':cmd=[str(ROOT/'tmp/work/cache/release-target/macos/release/bundle/macos/Muxflow.app/Contents/MacOS/muxflow')]
  else:
   cmd=[str(ROOT/'spikes/gpui/target/release/spike-terminal'),'host']
   if ENDPOINT=='ssh':e['SPIKE_HOST_SSH_CONFIG']=str(WORK/'ssh_config')
   else:
    e.pop('SPIKE_HOST_SSH_CONFIG',None);e['SPIKE_HOST_LOCAL_HELPER']=str(self.f.helper)
  self.log=open(WORK/f'{APP}-{ENDPOINT}-{RUN}.log','a');self.p=sp.Popen(cmd,env=e,stdout=self.log,stderr=sp.STDOUT)
  self.pidset.add(self.p.pid)
  for _ in range(60):
   if self.p.poll() is not None:raise RuntimeError('app exited; inspect '+self.log.name)
   ws=cua('list_windows')['windows'];w=next((w for w in ws if w['pid']==self.p.pid and w['is_on_screen'] and w['bounds']['height']>100),None)
   if w:break
   time.sleep(.2)
  else:raise RuntimeError('no window')
  sized=cua('set_window_frame',pid=self.p.pid,window_id=w['window_id'],x=100,y=60,width=1280,height=800)
  time.sleep(.5)
  w=next(v for v in cua('list_windows')['windows'] if v['window_id']==w['window_id'])
  if abs(w['bounds']['width']-1280)>1 or abs(w['bounds']['height']-800)>1:raise RuntimeError('window size normalization failed: '+str(sized))
  self.w=w;self.width=int(w['bounds']['width']*2);self.height=int(w['bounds']['height']*2)
  cua('bring_to_front',pid=self.p.pid,window_id=w['window_id']);time.sleep(2)
  after=processes();self.pidset.update(pid for pid,(ppid,cpu,cmd) in after.items() if ppid==self.p.pid or (pid not in before and 'com.apple.WebKit.' in cmd) or ('muxflow-host daemon --socket /private/tmp/muxflow-s11-localhost/' in cmd))
  self.pixel_job='dev.muxflow.s11-meter-bench'
  run('launchctl','bootout',f'gui/{os.getuid()}/{self.pixel_job}')
  self.meter_in=WORK/'meter-input';self.meter_out=WORK/'meter-output'
  for path in [self.meter_in,self.meter_out]:
   if path.exists():path.unlink()
  os.mkfifo(self.meter_in)
  meter=WORK/'Muxflow S11 Meter.app/Contents/MacOS/meter'
  job=WORK/'meter-bench.plist'
  with job.open('wb') as fp:plistlib.dump(dict(Label=self.pixel_job,ProgramArguments=list(map(str,[meter,self.p.pid,w['window_id'],self.meter_in,self.meter_out])),RunAtLoad=True,KeepAlive=False,StandardErrorPath=str(WORK/'meter-errors.log')),fp)
  r=run('launchctl','bootstrap',f'gui/{os.getuid()}',job)
  if r.returncode:raise RuntimeError(r.stderr)
  self.meter_writer=self.meter_in.open('w')
  for _ in range(100):
   if self.meter_out.exists():break
   time.sleep(.05)
  self.meter_reader=self.meter_out.open()
  ready=self.read_meter(15)
  if not ready.get('ready'):raise RuntimeError(str(ready))
  self.width=ready['width'];self.height=ready['height']
  focused=cua('bring_to_front',pid=self.p.pid,window_id=w['window_id'])
  if not focused.get('activated') and not (focused.get('observed',{}).get('workspace_frontmost_pid')==self.p.pid and focused.get('exact_window_effect',{}).get('focused')):raise RuntimeError('cannot focus exact app window: '+str(focused))
  time.sleep(1)
  # Both frontends must focus the SAME last quiet pane, independently of their
  # initial focus policy. Use the tmux layout only to aim the real mouse click.
  geom=list(map(int,self.f.tmux('display-message','-p','-t',self.f.panes[-1],'#{pane_left} #{pane_top} #{pane_width} #{pane_height} #{window_width} #{window_height}').split()))
  left,top,cols,rows,totalcols,totalrows=geom
  x=w['bounds']['x']+(left+cols/2)/totalcols*w['bounds']['width']
  y=w['bounds']['y']+32+(top+rows/2)/totalrows*(w['bounds']['height']-32)
  self.key(56,[0,0,1,1],flags=0,timeout=.01) # Neutral modifier transition before a mouse click.
  self.call(op='click',x=x,y=y,rect=[0,0,1,1],timeout=.05);time.sleep(.5)
  active=self.f.tmux('display-message','-p','-t','s11:0','#{pane_id}')
  if active!=self.f.panes[-1]:raise RuntimeError('quiet pane did not gain authoritative focus: '+active)
  return self
 def read_meter(self,timeout):
  end=time.monotonic()+timeout;buffer=""
  while time.monotonic()<end:
   line=self.meter_reader.readline()
   buffer+=line
   if buffer.endswith('\n'):return json.loads(buffer)
   time.sleep(.002)
  raise RuntimeError('meter did not answer')
 def call(self,**a):
  self.meter_writer.write(json.dumps(a)+'\n');self.meter_writer.flush()
  if a.get('op')=='quit':return {}
  r=self.read_meter(a.get('timeout',5)+10)
  if 'error' in r:raise RuntimeError(r['error'])
  return r
 def key(self,code,rect,flags=0,**kw):return self.call(op='key',code=code,flags=flags,rect=rect,**kw)
 def band(self):
  # Calibrate with no flood, watching only pixels changed by a real letter.
  full=[0,0,self.width,self.height]
  self.key(37,full,flags=1<<18,echo=False);time.sleep(.4) # Ctrl+L
  before=self.call(op='region',rect=full)['pixels']
  self.key(0,full,echo=True);time.sleep(.2)
  after=self.call(op='frame')['pixels']
  coords=[(i%self.width,i//self.width) for i,(a,b) in enumerate(zip(before,after)) if a!=b]
  if not coords:raise RuntimeError('typed key changed no pixels')
  x=min(p[0] for p in coords);y=min(p[1] for p in coords);right=max(p[0] for p in coords);bottom=max(p[1] for p in coords)
  if (right-x)*(bottom-y)>self.width*self.height/4:raise RuntimeError('calibration changed >25% of window')
  rect=[max(0,x-2),max(0,y-2),min(self.width-x,320),bottom-y+5]
  self.key(32,rect,flags=1<<18);time.sleep(.4) # Ctrl+U
  return rect
 def full(self):return self.call(op='region',rect=[0,0,self.width,self.height])['pixels']
 def iscolor(self,p,color):
  if color in self.colors:return p==self.colors[color]
  return all(abs(((p>>s)&255)-((color>>s)&255))<125 for s in [16,8,0])
 def bbox(self,color):
  ps=self.full()
  if len(ps)!=self.width*self.height:raise RuntimeError('capture region has stale dimensions')
  counts=__import__('collections').Counter(ps)
  candidates=[(n,p) for p,n in counts.items() if self.iscolor(p,color)]
  if not candidates:raise RuntimeError('sentinel color absent; dominant colors='+str(counts.most_common(8)))
  self.colors[color]=max(candidates)[1]
  coords=[(i%self.width,i//self.width) for i,p in enumerate(ps) if p==self.colors[color]]
  if len(coords)<100:raise RuntimeError('sentinel tile too small')
  return [min(x for x,y in coords),min(y for x,y in coords),max(x for x,y in coords)+1,max(y for x,y in coords)+1]
 def until(self,trigger,rect,predicate,timeout=15):
  # Python starts this clock before asking the SAME meter to inject the event.
  # Includes baseline-frame/IPC overhead; key samples use the meter's HID timestamp.
  t=time.perf_counter();self.call(rect=rect,timeout=.05,**trigger)
  while time.perf_counter()-t<timeout:
   if predicate(self.call(op='frame')['pixels']):return {'ms':(time.perf_counter()-t)*1000,'timeout':False}
   time.sleep(.002)
  return {'ms':None,'timeout':True}
 def footprint(self,label):
  live=processes();pids=sorted(self.pidset & live.keys());path=OUT/f'{APP}-{ENDPOINT}-{RUN}-{label}-footprint.json'
  r=run('footprint',*pids,'-f','bytes','-j',path)
  if r.returncode:return {'error':r.stderr,'pids':pids}
  data=json.loads(path.read_text())
  if data.get('errors'):return {'error':data['errors'],'pids':pids,'raw_file':path.name}
  return {'pids':pids,'raw_file':path.name,'footprint_bytes':data['total footprint'],'graphics_categories_bytes':sum(v['dirty'] for k,v in data['summary'].items() if 'graphics' in k or k in ['IOSurface','IOAccelerator'])}
 def stop(self):
  if hasattr(self,'meter_writer'):
   try:
    self.key(56,[0,0,1,1],flags=0,timeout=.01)
    self.call(op='quit');self.meter_writer.close()
   except Exception:pass
  if hasattr(self,'pixel_job'):run('launchctl','bootout',f'gui/{os.getuid()}/{self.pixel_job}')
  if self.p:
   owned=processes();children=set()
   for _ in range(5):children.update(p for p,v in owned.items() if v[0]==self.p.pid or v[0] in children)
   self.p.terminate()
   try:self.p.wait(timeout=5)
   except sp.TimeoutExpired:self.p.kill();self.p.wait()
   for pid in children:
    if pid in processes():
     try:os.kill(pid,15)
     except ProcessLookupError:pass
  try:self.f.command([str(self.f.helper) if ENDPOINT=='local' else '.local/bin/muxflow-host','daemon-stop'])
  except Exception:pass
  try:self.f.tmux('kill-server')
  except Exception:pass

def dist(rows):
 vals=sorted(r['ms'] for r in rows if r.get('ms') is not None)
 return dict(n=len(vals),timeouts=sum(r.get('timeout',False) for r in rows),p50=vals[math.ceil(len(vals)*.50)-1] if vals else None,p95=vals[math.ceil(len(vals)*.95)-1] if vals else None,p99=vals[math.ceil(len(vals)*.99)-1] if vals else None,raw=rows)

f=Fixture();result=dict(app=APP,endpoint=ENDPOINT,run=RUN,scenarios={})
if ENDPOINT=='ssh':
 result['ssh_roundtrip_ms']=[]
 peer=sp.Popen(['ssh','-F',str(WORK/'ssh_config'),'muxflow-s11-container',"while IFS= read -r line; do printf '%s\\n' \"$line\"; done"],stdin=sp.PIPE,stdout=sp.PIPE,stderr=sp.DEVNULL)
 try:
  peer.stdin.write(b'warmup\n');peer.stdin.flush()
  if peer.stdout.readline()!=b'warmup\n':raise RuntimeError('SSH echo calibration failed')
  for i in range(30):
   token=f'roundtrip-{i}\n'.encode();t=time.perf_counter();peer.stdin.write(token);peer.stdin.flush()
   if peer.stdout.readline()!=token:raise RuntimeError('SSH echo mismatched')
   result['ssh_roundtrip_ms'].append((time.perf_counter()-t)*1000)
 finally:
  peer.stdin.close();peer.wait(timeout=5)
 result['ssh_process_roundtrip_ms']=[]
 for _ in range(10):
  t=time.perf_counter();f.command(['true']);result['ssh_process_roundtrip_ms'].append((time.perf_counter()-t)*1000)
path=OUT/f'{APP}-{ENDPOINT}-{RUN}.json'
for scenario in SCENARIOS:
 a=App(f)
 try:
  f.reset(1 if scenario=='key-1' else 8)
  if scenario=='key-8-filled':
   for p in f.panes:f.send(p,f'{f.flood} 400')
  a.start()
  if scenario=='self-flood':
   program='uv run --no-project --python 3.12 workload.py pane-park-flood' if ENDPOINT=='ssh' else f'uv run --no-project --python 3.12 {ROOT}/tests/performance/runtime/renderer_baseline.py pane-park-flood'
   f.send(f.panes[-1],'exec '+program)
   time.sleep(1)
  rect=a.band() if scenario not in ['pane-flood6','resize-flood7'] else [0,0,1,1]
  r=dict(rect=rect,window_bounds=a.w['bounds'],capture_size=[a.width,a.height])
  if scenario.startswith('key'):
   if scenario=='key-flood7':f.start_flood(7);time.sleep(3)
   rows=[]
   for i in range(SAMPLES):
    if i and i%18==0:a.key(32,rect,flags=1<<18);time.sleep(.3)
    time.sleep(RNG.uniform(.02,.08));rows.append(a.key(0,rect,echo=True,timeout=5))
    if rows[-1].get('timeout'):break # Late echoes must not be credited to another key.
   r.update(dist(rows))
  elif scenario=='tab-flood7':
   program='uv run --no-project --python 3.12 /home/ade/s11-paint.py --rgb 255,255,0' if ENDPOINT=='ssh' else f'uv run --no-project --python 3.12 {ROOT}/tests/performance/runtime/macos/paint.py --rgb 255,255,0'
   f.send(f.tmux('list-panes','-t','s11:1','-F','#{pane_id}'),'exec '+program);time.sleep(1)
   roi=[rect[0]+30,rect[1]+10,96,48]
   a.key(97,roi,flags=(1<<18)|(1<<19)|(1<<23),timeout=5);time.sleep(.5)
   ps=a.call(op='region',rect=roi)['pixels']
   if len(ps)!=roi[2]*roi[3] or sum(a.iscolor(p,0xffff00) for p in ps)<len(ps)*.9:
    raise RuntimeError('tab calibration did not show yellow window; active='+f.tmux('display-message','-p','#{window_id}')+'; target_geometry='+f.tmux('display-message','-p','-t','s11:1','#{window_width}x#{window_height} pane=#{pane_width}x#{pane_height}')+'; colors='+str(__import__('collections').Counter(ps).most_common(4)))
   a.colors[0xffff00]=__import__('collections').Counter(ps).most_common(1)[0][0]
   a.key(96,roi,flags=(1<<18)|(1<<19)|(1<<23),timeout=5);time.sleep(.5)
   f.start_flood(7);time.sleep(3);rows=[]
   for i in range(12):
    to_yellow=i%2==0
    pred=lambda ps: (sum(a.iscolor(p,0xffff00) for p in ps)>len(ps)*.9)==to_yellow
    rows.append(a.until(dict(op='key',code=97 if to_yellow else 96,flags=(1<<18)|(1<<19)|(1<<23)),roi,pred))
    if rows[-1]['timeout']:break
    time.sleep(.1)
   r.update(dist(rows));r['note']='Expected yellow content, 96x48 px quiet-pane region. Clock includes meter baseline/IPC.'
  elif scenario=='pane-flood6':
   for pane,color in zip(f.panes[-2:],['0,255,0','255,0,255']):
    program=f'uv run --no-project --python 3.12 /home/ade/s11-paint.py --rgb {color}' if ENDPOINT=='ssh' else f'uv run --no-project --python 3.12 {ROOT}/tests/performance/runtime/macos/paint.py --rgb {color}'
    f.send(pane,'exec '+program)
   time.sleep(1);green=a.bbox(0x00ff00);pink=a.bbox(0xff00ff)
   roi=[green[0],green[1],26,36]
   centers=[(a.w['bounds']['x']+(b[0]+b[2])/4,a.w['bounds']['y']+(b[1]+b[3])/4) for b in [green,pink]]
   a.call(op='click',x=centers[0][0],y=centers[0][1],rect=roi,timeout=.1);time.sleep(.3)
   a.call(op='click',x=centers[1][0],y=centers[1][1],rect=roi,timeout=.1);time.sleep(.5)
   outline=a.call(op='region',rect=roi)['pixels'];time.sleep(.5)
   if len(outline)!=roi[2]*roi[3] or outline!=a.call(op='frame')['pixels'] or len(set(outline))<2:raise RuntimeError('unfocused cursor not stable/visible; pixels='+str(len(outline))+' colors='+str(len(set(outline))))
   f.start_flood(6);time.sleep(3);rows=[]
   for i in range(12):
    to_a=i%2==0;x,y=centers[0 if to_a else 1]
    rows.append(a.until(dict(op='click',x=x,y=y),roi,lambda ps:(ps!=outline)==to_a))
    rows[-1]['authoritative_pane']=f.tmux('display-message','-p','-t','s11:0','#{pane_id}')
    if rows[-1]['timeout']:break
    time.sleep(.1)
   r.update(dist(rows));r.update(rect=roi,note='Expected focus cursor picture; two quiet painted panes, six flooders. Clock includes meter baseline/IPC.')
  elif scenario=='resize-flood7':
   program='uv run --no-project --python 3.12 /home/ade/s11-paint.py --rgb 255,0,255' if ENDPOINT=='ssh' else f'uv run --no-project --python 3.12 {ROOT}/tests/performance/runtime/macos/paint.py --rgb 255,0,255'
   f.send(f.panes[-1],'exec '+program);time.sleep(1)
   sizes=[(1280,800),(1000,640)];boxes=[]
   for width,height in sizes+[sizes[0]]:
    a.call(op='resize',width=width,height=height,rect=[0,0,1,1],timeout=.05);time.sleep(.8)
    box=a.bbox(0xff00ff)
    if len(boxes)<2:boxes.append(box)
   x=max(0,min(b[0] for b in boxes)-5);y=max(0,min(b[1] for b in boxes)-5)
   roi=[x,y,max(b[0] for b in boxes)-x+64,max(b[1] for b in boxes)-y+64]
   def edges(mask):
    if len(mask)!=roi[2]*roi[3]:return (-1,-1)
    row=mask[(roi[3]-8)*roi[2]:(roi[3]-7)*roi[2]]
    col=mask[roi[2]-8::roi[2]]
    return (next((i for i,v in enumerate(row) if v),-1),next((i for i,v in enumerate(col) if v),-1))
   refs=[]
   a.call(op='resize',width=sizes[1][0],height=sizes[1][1],rect=roi,timeout=.05);time.sleep(.6)
   for width,height in sizes+[sizes[0]]:
    a.call(op='resize',width=width,height=height,rect=roi,timeout=.05);time.sleep(.6)
    if len(refs)<2:
     ps=a.call(op='region',rect=roi)['pixels']
     if len(ps)!=roi[2]*roi[3]:raise RuntimeError('resize ROI has stale dimensions')
     refs.append(edges([a.iscolor(p,0xff00ff) for p in ps]))
   if any(min(v)<0 for v in refs) or refs[0]==refs[1]:raise RuntimeError('resize sentinel boundaries not calibrated; refs='+str(refs)+' boxes='+str(boxes))
   f.start_flood(7);time.sleep(3);rows=[]
   for i in range(10):
    j=(i+1)%2;width,height=sizes[j]
    pred=lambda ps: all(abs(got-want)<=2 for got,want in zip(edges([a.iscolor(p,0xff00ff) for p in ps]),refs[j]))
    rows.append(a.until(dict(op='resize',width=width,height=height),roi,pred))
    if rows[-1]['timeout']:break
    time.sleep(.4)
   r.update(dist(rows));r.update(rect=roi,sentinel_boxes=boxes,note='Both calibrated top/left content boundaries within 2 px; same sentinel criterion as Linux. Fixed capture canvas, native window resizes. Clock includes meter baseline/IPC.')
  elif scenario=='self-flood':
   a.key(6,rect,echo=True);time.sleep(.3)
   pid=f.tmux('display-message','-p','-t',f.panes[-1],'#{pane_pid}')
   for _ in range(4):
    try:child=f.command(['pgrep','-P',pid]).splitlines()
    except Exception:break
    if len(child)!=1:raise RuntimeError('ambiguous sentinel child PIDs')
    pid=child[0]
   f.command(['kill','-USR1',pid]);f.start_flood(7);time.sleep(3)
   codes=[5,4,34,38,40,37,45,31,35,12,15,1,17,32,9,13,7,16,5,4]
   rows=[];self_started=time.monotonic()
   for code in codes:
    time.sleep(RNG.uniform(.3,.4));rows.append(a.key(code,rect,echo=True,timeout=8))
    if rows[-1].get('timeout'):break
   r.update(dist(rows));r['keys']=20;r['attempted']=len(rows);r['completed']=r['n'];r['elapsed_s']=time.monotonic()-self_started
  elif scenario=='all-flood60':
   # Reduce CPU readback to one pixel; the same capture stream remains active for both apps.
   a.call(op='region',rect=[0,0,1,1]);f.start_flood(8);time.sleep(3)
   r['memory_start']=a.footprint('flood-start');start=processes();t=time.monotonic();series=[]
   for _ in range(60):
    time.sleep(1)
    if a.p.poll() is not None:raise RuntimeError('app exited during 60 s flood')
    series.append({str(p):v[1] for p,v in processes().items() if p in a.pidset})
   end=processes();elapsed=time.monotonic()-t;r['seconds']=elapsed;r['cpu_percent']=sum(end[p][1]-start[p][1] for p in a.pidset if p in end and p in start)*100/elapsed;r['memory_end']=a.footprint('flood-end');r['gpu_memory']='not obtained';r['cpu_series_seconds']=series
  result['scenarios'][scenario]=r
 except Exception as e:
  result['scenarios'][scenario]={'error':str(e)}
  if a.p and a.p.poll() is None and hasattr(a,'w'):
   try:
    cua('start_session',session='s11-failure')
    shot=OUT/f'{APP}-{ENDPOINT}-{RUN}-{scenario}-failure.png'
    observed=cua('get_window_state',session='s11-failure',pid=a.p.pid,window_id=a.w['window_id'],include_accessibility_tree=False,screenshot_out_file=str(shot))
    if observed.get('screenshot_frame_valid'):result['scenarios'][scenario]['screenshot']=shot.name
   except Exception:pass
 finally:
  a.stop();path.write_text(json.dumps(result,indent=2));print(APP,ENDPOINT,RUN,scenario,result['scenarios'][scenario].get('error','done'),flush=True)
