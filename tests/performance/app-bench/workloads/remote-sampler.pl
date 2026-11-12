#!/usr/bin/perl
# Samples this user's processes on the SSH host once per interval until the
# stop file appears. Output, one block per tick:
#   T <epoch seconds>
#   <pid> <ppid> <comm> <self ticks> <child ticks> <start ticks> <rss kB> <pss kB or -1>
# comm has spaces replaced by '_'. Pss is read only for muxflow-host and tmux.
# Usage: remote-sampler.pl <out file> <stop file> <interval seconds>
use strict;
use warnings;
use Time::HiRes qw(time sleep);

my ($out, $stop, $interval) = @ARGV;
$interval ||= 1;
my $uid = $<;
my $page_kb = 4;
open(my $fh, '>', $out) or die "open $out: $!";
select((select($fh), $| = 1)[0]);
my $next = time;
while (!-e $stop) {
    printf $fh "T %.6f\n", time;
    opendir(my $dh, '/proc') or die;
    for my $pid (grep { /^\d+$/ } readdir($dh)) {
        my @st = stat("/proc/$pid");
        next unless @st && $st[4] == $uid;
        open(my $sf, '<', "/proc/$pid/stat") or next;
        my $raw = <$sf>;
        close($sf);
        next unless defined $raw;
        my $l = index($raw, '(');
        my $r = rindex($raw, ')');
        my $comm = substr($raw, $l + 1, $r - $l - 1);
        $comm =~ s/\s/_/g;
        my @f = split(' ', substr($raw, $r + 2));
        my $pss = -1;
        if ($comm =~ /^(muxflow-host|tmux)/) {
            if (open(my $sm, '<', "/proc/$pid/smaps_rollup")) {
                while (my $line = <$sm>) {
                    if ($line =~ /^Pss:\s+(\d+)/) { $pss = $1; last; }
                }
                close($sm);
            }
        }
        printf $fh "%d %d %s %d %d %d %d %d\n", $pid, $f[1], $comm,
            $f[11] + $f[12], $f[13] + $f[14], $f[19], $f[21] * $page_kb, $pss;
    }
    closedir($dh);
    $next += $interval;
    my $wait = $next - time;
    sleep($wait) if $wait > 0;
}
close($fh);
