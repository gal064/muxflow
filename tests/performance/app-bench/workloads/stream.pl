#!/usr/bin/perl
# Agent-like output: about <rate> lines per second of ANSI-colored text for
# <seconds>, with long lines that wrap, on an absolute schedule so a slow
# terminal does not lower the offered rate. Deterministic (fixed seed).
# Usage: stream.pl [seconds=60] [rate=60]
use strict;
use warnings;
use Time::HiRes qw(time sleep);

my ($seconds, $rate) = @ARGV;
$seconds ||= 60;
$rate ||= 60;
srand(42);
$| = 1;
my @words = qw(agent tool read write edit file patch diff test build cargo
  pnpm render frame pane tmux socket bridge daemon host snapshot cursor glyph
  atlas buffer stream token reply context plan step verify commit branch merge
  result error warning info debug trace span event queue worker thread async);
my @fg = (31, 32, 33, 34, 35, 36, 37, 90, 91, 92, 93, 94, 95, 96);
my $start = time;
my $count = int($seconds * $rate);
for my $i (0 .. $count - 1) {
    my $due = $start + $i / $rate;
    my $wait = $due - time;
    sleep($wait) if $wait > 0;
    my $len = 6 + int(rand(40));    # some lines exceed the pane width and wrap
    my $line = sprintf("\e[2m%05d\e[0m ", $i);
    for (1 .. $len) {
        my $w = $words[int(rand(@words))];
        my $r = rand();
        if ($r < 0.15) {
            $line .= sprintf("\e[1;%dm%s\e[0m ", $fg[int(rand(@fg))], $w);
        } elsif ($r < 0.25) {
            $line .= sprintf("\e[38;5;%dm%s\e[0m ", 16 + int(rand(216)), $w);
        } elsif ($r < 0.30) {
            $line .= sprintf("\e[38;2;%d;%d;%dm%s\e[0m ", int(rand(256)), int(rand(256)), int(rand(256)), $w);
        } else {
            $line .= "$w ";
        }
    }
    print "$line\n";
}
