"""Solid sentinel; poll size too because uv's child may not receive SIGWINCH."""
import argparse, os, termios, time, tty
p=argparse.ArgumentParser();p.add_argument('--rgb',required=True);args=p.parse_args()
r,g,b=map(int,args.rgb.split(','));tty.setcbreak(0)
a=termios.tcgetattr(0);a[3]&=~termios.ECHO;termios.tcsetattr(0,termios.TCSANOW,a)
last=None
while True:
 size=os.get_terminal_size(0)
 if size!=last:
  cols,rows=size;last=size
  os.write(1,(f'\x1b[?25h\x1b[2 q\x1b[48;2;{r};{g};{b}m\x1b[H'+'\r\n'.join([' '*cols]*rows)+'\x1b[H').encode())
 time.sleep(.02)
