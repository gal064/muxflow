# Muxflow — agent instructions

## Compatibility

Assume the desktop app and the host helper are always updated together. There is
no older desktop or older host in the field: do not add or keep compatibility
arms, legacy protocol fields, or version-skew fallbacks. When a wire change is
made, change both sides in the same commit and delete the old path.

The mobile app updates independently, so an older or newer phone can meet the
current host. It is admitted only on an equal `PROTOCOL_MAJOR`. Bump the major
only for a real incompatibility: the previous released mobile app would break
or misbehave against the new host, or the new mobile app against the previous
released host. Additive changes the other side ignores harmlessly do not bump.
For every wire change, answer both directions in review. Do not add
compatibility arms for mobile either; a mismatch is refused, not supported.
