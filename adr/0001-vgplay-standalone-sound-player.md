# 0001. vgplay is a standalone sound player split out of vgiew

Status: Accepted

## Context

Sound playback was first built inside the `vgiew` image viewer, sharing one
binary that dispatched by file type (vgiew ADR 0009). The project owner then
decided to make the sound player its own product rather than a mode of the
image viewer, and vgiew ADR 0014 records that split and supersedes 0009.

This ADR records the same decision from vgplay's side, so vgplay's own history
is self-contained. The forces (product identity, a small shared surface, and
independent evolution of the player's UI) are set out in vgiew ADR 0014 and not
repeated here.

## Decision

We will develop vgplay as a separate, standalone project with its own
repository, binary, icon, `--register`/`--unregister` (ProgID `vgplay.sound`
for `wav`/`mp3`/`flac`/`ogg`), and install/uninstall scripts. It carries no
image-viewer code. The few platform helpers common to both apps — the DWM-cloak
no-flash reveal, the class-background backstop, console attach, icon loading,
`absolutize`, and the assoc-changed notify — are copied into vgplay rather than
factored into a crate shared with vgiew.

## Consequences

- vgplay owns its own associations, install flow, and release cadence,
  independent of vgiew.
- The hand-drawn play/stop and repeat controls (vgiew ADR 0013) are carried
  forward here unchanged; that ADR remains the record of *why* they are drawn
  as geometry rather than with a widget/font toolkit.
- The shared platform helpers are duplicated, not shared. Acceptable while they
  are small and stable; if they start to diverge or drift, revisit extracting a
  shared crate. This is the trade-off vgiew ADR 0014 chose deliberately.
- Room to grow the player (progress bar, time readout, volume) without touching
  any image path.
