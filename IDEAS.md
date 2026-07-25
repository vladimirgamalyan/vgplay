# vgplay — ideas

A parking lot for features worth considering. Nothing here is planned or promised.
Once an idea is acted on, the reasoning that survives belongs in an ADR under
`adr/`, not here.

## Select a region of the waveform and loop it

Drag on the waveform to mark a region (a shaded band with draggable edges).
Playback then loops over that region instead of the whole file, so the existing
repeat toggle applies to the selection when one exists.

Open questions:

- How the selection interacts with repeat and with plain play/pause: does playing
  outside the selection clear it, or seek back into it?
- rodio has no loop-region primitive, so looping likely means watching the position
  and seeking back to the start when it passes the end.
- Edge precision: keyboard nudging of the edges, snapping to zero crossings.

## Save the selected region as a file

Export the selected piece as a new sound file — the cut, without opening an editor.

Open questions:

- Output format: always WAV (lossless, no encoder to link) or match the source
  format (an encoder per codec, and re-encoding a lossy source degrades it again)?
- Destination: a Save dialog, or a sibling file next to the source with an
  auto-generated name?
- Cutting must be sample-accurate, so it needs a decode of the region — the drawn
  envelope is far too coarse to cut from.

## Zoom and pan the waveform

Zoom into the waveform (mouse wheel, anchored at the cursor, the way vgiew zooms
images) and pan along it, so a long file can be inspected and a selection placed
precisely.

Open questions:

- The stored envelope is bounded (`WF_RES` buckets, halved as the file streams), so
  a deep zoom just enlarges the same blur. Real detail needs re-analysis of the
  visible range — a second, ranged decode pass off-thread.
- Three gestures now compete on the waveform — scrub, pan, select. They need
  distinct bindings or a mode.
- Some hint of where the visible window sits within the whole file (a scrollbar or
  an overview strip).
