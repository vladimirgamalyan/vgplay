## Language Requirements
- All code comments MUST be in English only
- All logging messages MUST be in English only
- All error messages MUST be in English only
- All docstrings MUST be in English only
- All variable names, function names, and class names MUST be in English only
- All git commit messages MUST be in English only
- Project documentation (README.md, files in docs/) MUST be in English only

## Code Guidelines
Follow the behavioral rules in @CODE_GUIDELINES.md

## Test Sounds
`test-sounds/` holds local sample audio for manually testing the player.
The media files there are git-ignored (only its `README.md` is tracked); put
whatever samples you need for a test into that folder.

## Relationship to vgiew
vgplay was split out of the `vgiew` image viewer (vgiew ADR 0014). The two share
a small amount of platform glue (the DWM-cloak no-flash reveal, class-background
backstop, console attach, icon loading, `absolutize`, assoc-changed notify),
copied into each rather than shared through a crate. Keep changes to that glue in
mind if you touch it in one project; the copies are intentionally independent.

## Architecture Decision Records
Notable and debatable architecture/design decisions are logged in `adr/`
(see `adr/README.md` for the full process). In particular:
- Before proposing or re-proposing a debatable design change, check `adr/`
  for an existing decision on the topic. If one is `Accepted`, follow it
  instead of re-litigating it, unless you have genuinely new information.
- After making a debatable or architecturally significant decision, add a
  new ADR under `adr/` using `adr/template.md`.
- Never edit an old ADR's decision in place; supersede it with a new one
  instead, so the history of *why* is preserved.
