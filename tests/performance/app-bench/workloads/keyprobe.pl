#!/usr/bin/perl
# Typing-latency target. Reads keystrokes raw and answers each one by drawing
# one pure-blue (#0000ff) cell, so a screen capture can tell exactly when the
# keystroke's echo reached the screen. Same path as a shell echo: key -> app ->
# host -> tmux -> pty -> this process -> tmux -> host -> app -> paint.
# 'q' exits; Ctrl-L clears.
use strict;
use warnings;

system('stty', 'raw', '-echo');
$| = 1;
print "\e[2J\e[H";
while (sysread(STDIN, my $c, 1)) {
    last if $c eq 'q';
    if ($c eq "\x0c") {
        print "\e[2J\e[H";
        next;
    }
    print "\e[48;2;0;0;255m \e[0m";
}
system('stty', 'sane');
print "\r\n";
