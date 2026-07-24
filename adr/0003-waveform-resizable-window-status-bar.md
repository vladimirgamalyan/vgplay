# 0003. Resizable window with a waveform, playback cursor, and status bar

Status: Accepted

## Context

The player was a fixed 420×120 window showing two round buttons (play/stop and
repeat). ADR 0001 anticipated growing it ("room to grow the player — progress
bar, time readout, volume"). The requested feature set:

- A resizable window (previously fixed; the maximize button was even disabled,
  see commit "Disable the maximize button on the player window").
- A **waveform** of the sound with a moving playback **cursor**.
- A **status line**: format, sample rate, channel layout, position / duration.
- **Seek** by clicking/dragging the cursor along the waveform.

The overriding constraint, and vgplay's whole identity, is **instant startup**:
visualization is secondary; the sound must start with no perceptible delay
(≤ ~50 ms is fine). Several forces had to be reconciled:

- Drawing a waveform needs the *entire* file decoded to compute an envelope, but
  decoding on the launch path would delay playback.
- The app has no widget/font toolkit — everything is hand-drawn geometry (ADR
  0001; vgiew ADR 0013). A status line needs text, which did not exist yet.
- Seeking and position readout depend on what rodio's decoders actually support.

We verified rodio 0.22's real behavior before designing: with the crate's
`wav`/`mp3`/`flac`/`vorbis` features (which expand to the **symphonia** backend,
not the native claxon/minimp3/hound/lewton decoders), `Player::get_pos()`,
`Player::try_seek()`, and `Source::total_duration()` all work for **all four
formats**. Had the native decoders been in use, mp3/flac/ogg seeking would have
returned `NotSupported`.

## Decision

We will grow `run_sound` into a resizable waveform player, keeping the launch
hot path byte-for-byte as fast as before:

- **Instant start preserved.** Playback still begins with a lazy `Decoder` +
  `player.append()` and the first frame is painted before anything waveform-
  related runs. *After* the window is revealed, a background thread decodes the
  file a **second time** purely to build a per-channel min/max envelope, delivered
  back through the event loop (`EventLoopProxy`). The event type becomes an enum
  `AppEvent { Open(PathBuf), Waveform(..) }`. A `gen` counter tags each opened
  file so a late analysis result for a replaced file (window reuse, ADR 0002) is
  dropped. Decoding twice is deliberately accepted as the price of zero added
  start latency and a clean split from playback; the envelope is streamed with
  adaptive downsampling so memory is bounded regardless of file length.
- **Stereo waveform.** The envelope is kept per channel; ≥2-channel files draw
  two stacked lanes (L over R), mono draws one. Re-bucketed to the window width at
  draw time, so resizing never re-decodes.
- **Hand bitmap font.** The status line is drawn with a tiny 5×7 bitmap font
  (digits, A–Z, `:` `.` `/`), consistent with the "draw geometry by hand, no
  widget/font toolkit" decision of ADR 0001. It reads `FORMAT · sample rate (full
  Hz) · bit depth · channel layout · position / duration`, and shrinks its scale
  to fit the window width. rodio's `Source` does not expose the source bit depth,
  so it is read with a cheap header-only **symphonia** probe (`bits_per_sample`)
  added as a direct dependency — the same symphonia crate rodio already pulls in
  transitively, so there is effectively no added build cost. Lossy codecs (mp3,
  vorbis) report no bit depth and the field is simply omitted.
- **Play/pause, not play/stop.** With a seek bar, the transport is play/pause and
  preserves position (`Player::pause()`/`play()`); seeking works while paused.
  Natural end resets to a stopped state; repeat still re-appends.
- **Seek.** Clicking or dragging the waveform scrubs the cursor and commits a
  `try_seek` on release; this relies on the symphonia backend (see Context).
- **Resizable window.** Default 640×260, min 360×180, maximize re-enabled — which
  supersedes the earlier "disable maximize" commit. The playhead animates at
  ~30 fps only while actually playing; the loop stays idle when paused/stopped.

## Consequences

- The player gains a waveform, cursor, seek, status line, and resize without
  touching the launch latency that defines the product.
- The file is decoded twice per open. Acceptable for a "click a sound, hear it"
  tool where files are short; for a very long file the waveform just appears a
  little later, never blocking playback. If this ever matters, a single-decode
  tee or a metadata-first early event could be revisited.
- We now carry a hand-written font. It is intentionally minimal (only the glyphs
  the status line uses); adding new text may require adding glyphs. This keeps us
  off a text-shaping dependency, matching ADR 0001.
- symphonia is now a direct dependency (previously only transitive via rodio),
  used solely to read the source bit depth. It is the same crate/version already
  compiled, so the cost is a Cargo line, not build weight. If rodio ever exposed
  bit depth on `Source`, this direct dependency could be dropped.
- Seeking/position correctness is tied to rodio using the **symphonia** backend.
  If the decoder features change to the native ones, mp3/flac/ogg seeking would
  regress to `NotSupported`; that constraint must be kept in mind.
- The transport changed from stop-and-restart to pause/resume; this is a visible
  behavior change from earlier builds, chosen because it fits a seek bar.
