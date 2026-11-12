#!/usr/bin/perl
# Writes a deterministic colored-text file for the heavy-output scenario:
# <lines> lines of 16-, 256- and truecolor SGR text, some wider than the pane.
# Usage: colored.pl <lines> > file
use strict;
use warnings;

my $lines = shift || 200000;
srand(7);
my @words = qw(lorem ipsum dolor sit amet consectetur adipiscing elit sed do
  eiusmod tempor incididunt ut labore et dolore magna aliqua enim minim veniam);
for my $i (1 .. $lines) {
    my $line = sprintf("\e[33m%07d\e[0m ", $i);
    my $n = 4 + int(rand(30));
    for (1 .. $n) {
        my $w = $words[int(rand(@words))];
        my $r = rand();
        if ($r < 0.3) {
            $line .= sprintf("\e[%dm%s\e[0m ", 31 + int(rand(7)), $w);
        } elsif ($r < 0.5) {
            $line .= sprintf("\e[38;5;%dm%s\e[0m ", int(rand(256)), $w);
        } elsif ($r < 0.6) {
            $line .= sprintf("\e[48;2;%d;%d;%dm%s\e[0m ", int(rand(128)), int(rand(128)), int(rand(128)), $w);
        } else {
            $line .= "$w ";
        }
    }
    print "$line\n";
}
