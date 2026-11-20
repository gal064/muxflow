#!/usr/bin/env perl
# A stand-in agent for the switch bench: run as `exec -a claude perl
# marker-agent.pl RRGGBB` (or codex) so the helper lists it as that agent.
#
# Paints one solid block of its own colour, MARK_COLS x MARK_ROWS cells, with
# its top-left cell at row 2, column 3, and parks a steady block cursor
# on the first column of the block, two rows below it. The bench finds the
# pane by the colour and tells a focused pane (filled cursor) from an
# unfocused one (hollow cursor) at that cell. Then it waits, repainting on
# resize.
use strict;
use warnings;

my ($hex, $cols, $rows) = (@ARGV, 24, 5);
die "usage: marker-agent.pl RRGGBB [cols rows]\n" unless $hex && $hex =~ /^[0-9a-fA-F]{6}$/;
my ($r, $g, $b) = map { hex } unpack '(A2)3', $hex;
$| = 1;

sub paint {
    # Clear, steady block cursor (a blinking one is off half the time).
    my $out = "\e[0m\e[2J\e[H\e[2 q";
    for my $row (0 .. $rows - 1) {
        $out .= sprintf "\e[%d;3H\e[48;2;%d;%d;%dm%s\e[0m", 2 + $row, $r, $g, $b, ' ' x $cols;
    }
    $out .= sprintf "\e[%d;3H", 2 + $rows + 2;
    print $out;
}

$SIG{WINCH} = \&paint;
$SIG{TERM} = $SIG{HUP} = sub { print "\e[0m\e[0 q"; exit 0 };
paint();
sleep 3600 while 1;
