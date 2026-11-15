# Shared Zakuro findings for nativeMons

This branch holds only the shared lists. The nativeMons launcher downloads them from here, so a new list reaches
players without a Zakuro update.

- `hints/<title id>-<code hash>.hints`: places where Zakuro had to interpret a game's code, one address a line (odd
  for Thumb). `nativemons-recomp` adds a game's list to the player's own before building that exact code, so the
  build covers them. Addresses only, no game code.
- `index.txt`: `<file> <sha256>` for each list. The launcher reads it, downloads only the lists that changed, checks
  each against its checksum and format, and quietly rebuilds a built game whose list changed.

The lists start from upstream 3dsrecomp's `hints.txt` (the games nativeMons builds) and grow from players who chose
to share theirs from the launcher (an issue on Hailey-Ross/nativeMons). Each is checked before it's merged here.
