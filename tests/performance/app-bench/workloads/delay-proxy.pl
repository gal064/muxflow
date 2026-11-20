#!/usr/bin/env perl
# A slow link for ssh, as a ProxyCommand: relays stdin/stdout to HOST:PORT and
# holds every chunk for DELAY ms in each direction, in order, so the round
# trip grows by 2 x DELAY. No bandwidth limit, no loss, no jitter.
#
#   ProxyCommand perl delay-proxy.pl 127.0.0.1 2222 /path/to/delay_ms
#
# The third argument is a number of milliseconds or a file holding one, read
# once per connection, so a bench can change the link between app launches
# without rewriting its ssh_config.
use strict;
use warnings;
use IO::Socket::INET;
use IO::Select;
use Time::HiRes qw(time);

my ($host, $port, $delay) = @ARGV;
die "usage: delay-proxy.pl HOST PORT DELAY_MS|FILE\n" unless defined $delay;
if ($delay !~ /^\d+(\.\d+)?$/) {
    open my $fh, '<', $delay or die "delay-proxy: $delay: $!\n";
    $delay = <$fh> // 0;
    $delay =~ s/\s+//g;
    $delay = 0 if $delay eq '';
}
$delay /= 1000;

my $sock = IO::Socket::INET->new(PeerAddr => $host, PeerPort => $port, Proto => 'tcp')
    or die "delay-proxy: connect $host:$port: $!\n";
$sock->setsockopt(6, 1, 1);    # TCP_NODELAY: the delay is ours, not Nagle's
binmode STDIN;
binmode STDOUT;

my $in = \*STDIN;
my $out = \*STDOUT;
# Queues of [due, bytes]: upstream (stdin -> socket), downstream (socket -> stdout).
my (@up, @down);
my ($in_open, $sock_open, $up_shut) = (1, 1, 0);

sub drain {
    my ($queue, $fh, $now) = @_;
    while (@$queue && $queue->[0][0] <= $now) {
        my $data = shift(@$queue)->[1];
        my $off = 0;
        while ($off < length $data) {
            my $n = syswrite($fh, $data, length($data) - $off, $off);
            exit 0 unless defined $n;
            $off += $n;
        }
    }
}

while (1) {
    my $now = time;
    drain(\@up, $sock, $now);
    drain(\@down, $out, $now);
    if (!$in_open && !@up && !$up_shut) { shutdown($sock, 1); $up_shut = 1 }
    exit 0 if !$sock_open && !@down;
    my $sel = IO::Select->new;
    $sel->add($in) if $in_open;
    $sel->add($sock) if $sock_open;
    my @due = map { $_->[0][0] } grep { @$_ } (\@up, \@down);
    my $timeout;
    if (@due) {
        my $first = (sort { $a <=> $b } @due)[0];
        $timeout = $first - time;
        $timeout = 0 if $timeout < 0;
    }
    last if !$sel->count && !@due;
    my @ready = $sel->count ? $sel->can_read($timeout) : do { select(undef, undef, undef, $timeout); () };
    $now = time;
    for my $fh (@ready) {
        my $buf;
        my $n = sysread($fh, $buf, 65536);
        if ($fh == $in) {
            if (!$n) { $in_open = 0; next }
            push @up, [$now + $delay, $buf];
        } else {
            if (!$n) { $sock_open = 0; next }
            push @down, [$now + $delay, $buf];
        }
    }
}
