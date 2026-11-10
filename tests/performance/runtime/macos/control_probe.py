"""No GUI: compare flood-backlogged control stdin with a fresh tmux client.
Run with uv, inside the disposable container. Owns only socket muxflow-qa-s11-control.
"""
import subprocess as sp, threading, time, queue, json
base=['tmux','-L','muxflow-qa-s11-control','-f','/dev/null']
def tm(*a):return sp.check_output(base+list(a),text=True).strip()
children=[]
try:
 tm('new-session','-d','-s','s11','-x','160','-y','44')
 for _ in range(7):tm('split-window','-d','-t','s11:0');tm('select-layout','-t','s11:0','tiled')
 tm('new-window','-d','-t','s11:1')
 panes=tm('list-panes','-t','s11:0','-F','#{pane_id}').splitlines()
 def client(flags):
  p=sp.Popen(base+['-C','attach-session','-f',flags,'-t','s11'],stdin=sp.PIPE,stdout=sp.PIPE,stderr=sp.DEVNULL);children.append(p);return p
 out=client('pause-after=5');inp=client('no-output,ignore-size,no-detach-on-destroy');markers=queue.Queue()
 def drain(p):
  for line in iter(p.stdout.readline,b''):
   if line.startswith(b'S11_MARK_'):markers.put((line.strip().decode(),time.perf_counter()))
 threading.Thread(target=drain,args=(out,),daemon=True).start();threading.Thread(target=drain,args=(inp,),daemon=True).start()
 for p in panes:tm('send-keys','-t',p,'exec /usr/local/bin/s11-flood','Enter')
 time.sleep(2)
 result={'control_stdin_ms':[],'fresh_client_ms':[]}
 for i in range(6):
  # Remain on the flooded window: switching away suppresses its output.
  target='s11:0';token=f'S11_MARK_{i}'
  t=time.perf_counter();out.stdin.write(f'select-window -t {target}\ndisplay-message -p {token}\n'.encode());out.stdin.flush()
  try:
   while True:
    name,seen=markers.get(timeout=15)
    if name==token:break
   result['control_stdin_ms'].append(round((seen-t)*1000,3))
  except queue.Empty:result['control_stdin_ms'].append(None)
  t=time.perf_counter();tm('select-window','-t',target);result['fresh_client_ms'].append(round((time.perf_counter()-t)*1000,3))
 print(json.dumps(result))
finally:
 for p in children:
  p.terminate()
  try:p.wait(timeout=2)
  except sp.TimeoutExpired:p.kill();p.wait()
 sp.run(base+['kill-server'],capture_output=True)
