# Muxflow — agent instructions

## Compatibility

Assume the desktop app and the host helper are always updated together. There is
no older desktop or older host in the field: do not add or keep compatibility
arms, legacy protocol fields, or version-skew fallbacks. When a wire change is
made, change both sides in the same commit and delete the old path.
