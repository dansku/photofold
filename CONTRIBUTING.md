# Contributing to photofold

Thank you for looking. This is a tool people point at their only copy of their family photographs, so the bar for changes is set by that.

## The one rule

**Losing a photo is the only unacceptable bug.** Keeping a duplicate costs disk space. Dropping a picture costs something that cannot be bought back. Every design decision in this project follows from that, and so should every change.

In practice: when a change makes duplicate detection more eager, it needs evidence. When it makes detection more cautious, it needs much less.

## Before you open a pull request

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --no-default-features
```

All four must pass. CI runs the same commands on Linux and macOS.

## Adding a matching rule

`src/matching.rs` holds an ordered table of rules. A rule that can merge files must demand **structural** evidence, not just visual likeness: a shared Live Photo identifier, a shared file name, or one capture stored in two formats.

The reason is written into the module, and it is not theoretical. An earlier rule accepted "same size, same camera, seconds apart". On a real library it merged five pairs of consecutive frames that were five separate photographs.

If you add a rule, add a test that shows what it must **not** merge.

## Adding a format

Add one file under `src/decode/` and one arm to the match in `src/decode/mod.rs`. Add its magic bytes to `FileFormat::sniff`. The pipeline does not change.

## Test fixtures

Fixtures are generated in code. `tests/common/mod.rs` builds real JPEG, PNG and QuickTime files, and `tests/heic.rs` encodes real HEIC through libheif. Please do not commit binary fixtures.

## Reporting a bug

The most useful bug report is the output of:

```bash
cargo run --example inspect -- the-two-files-that-should-have-matched.heic and-this-one.jpg
```

If you can, include the relevant part of `photofold-report.json`. Please do not attach photographs you would not want to be public.

## Licence

By contributing you agree that your work is licensed under MIT OR Apache-2.0, matching the project.
