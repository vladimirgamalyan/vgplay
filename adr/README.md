# Architecture Decision Records

This folder records notable architecture and design decisions for
`vgplay`, together with the reasoning behind them. It exists so that a
decision — especially a debatable or previously-revisited one — gets made
once, on record, instead of being silently re-litigated in every session.

## When to write one

Add a new ADR for a decision that is:

- **Architecturally significant** — affects the rendering/graphics path,
  window and interaction behavior, audio playback, file-type associations,
  or how a whole feature area works.
- **Debatable** — there was a real alternative, and someone (human or agent)
  could reasonably propose reopening it later.
- **Costly to reverse** — changing course later means touching multiple
  files, breaking the CLI contract, or redoing prior work.

Skip an ADR for routine bug fixes or refactors with no behavioral choice to
record.

## Before proposing a debatable change

Before proposing or re-proposing a decision that feels debatable, check
`adr/` first:

1. Search existing ADRs for the topic (filenames and titles).
2. If a relevant ADR exists and is `Accepted`, treat it as settled. Follow
   it. Only propose reopening it if you have new information the ADR did
   not consider — and say explicitly what that new information is.
3. If you do reopen it, do not edit the old ADR's decision in place. Write a
   new ADR that supersedes it (see below), so the history of *why* stays
   intact.

## File format

- Filename: `NNNN-short-kebab-title.md`, numbered sequentially
  (`0001-...`, `0002-...`).
- Use `template.md` as the starting point for a new record.
- Status is one of: `Proposed`, `Accepted`, `Rejected`, `Superseded by
  NNNN`. When a decision changes, add a new ADR and update the old one's
  status to `Superseded by NNNN` rather than rewriting it.

## History

vgplay was split out of the `vgiew` image viewer. The decision to make it a
separate standalone project — and the earlier one to originally bundle sound
into the image binary that it supersedes — live in vgiew's ADRs 0014 and 0009.
The hand-drawn play/stop and repeat controls are documented in vgiew's ADR 0013.

## Index

- [0001](0001-vgplay-standalone-sound-player.md) — vgplay is a standalone sound
  player split out of vgiew
