# photofold

[![CI](https://github.com/dansku/photofold/actions/workflows/ci.yml/badge.svg)](https://github.com/dansku/photofold/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/photofold.svg)](https://crates.io/crates/photofold)
[![Licence](https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-blue.svg)](#licence)

Merge photos and videos from several folders into one, keeping the best copy of each picture.

Point it at your iPhone folder, your Google Photos export, and that old backup drive. It finds the files that are the same photo — even when the format, the file name, or the compression differs — and copies only the best version into one output folder.

```bash
photofold \
  "/Users/me/Photos/iPhone" \
  "/Users/me/Downloads/Google Photos" \
  "/Volumes/Backup/Old Photos" \
  --output "/Users/me/Photos/Merged"
```

**Your source folders are never touched.** They are opened read-only. Nothing in them is moved, changed, renamed, or deleted.

**When it is not sure, it keeps both.** A photo you lose cannot be recovered; a photo kept twice costs disk space. The program is built around that trade.

---

## Contents

- [What it does](#what-it-does)
- [Installing](#installing)
- [Building](#building)
- [Usage](#usage)
- [Examples](#examples)
- [How duplicates are found](#how-duplicates-are-found)
- [How the best copy is chosen](#how-the-best-copy-is-chosen)
- [HEIC and iPhone photos](#heic-and-iphone-photos)
- [Live Photos](#live-photos)
- [Google Photos exports](#google-photos-exports)
- [Dry runs](#dry-runs)
- [Reports](#reports)
- [The cache](#the-cache)
- [Performance](#performance)
- [Safety guarantees](#safety-guarantees)
- [Limitations](#limitations)
- [Development](#development)
- [Licence](#licence)

---

## What it does

1. Scans every source folder, all the way down.
2. Identifies each file by its contents, not by its extension.
3. Finds byte-for-byte duplicates.
4. Finds photos that are visually the same but stored differently: HEIC against JPEG, an original against a re-compressed export, the same picture renamed.
5. Groups those together and picks the best version of each.
6. Keeps Live Photo stills and their movies together.
7. Copies only the winners into the output folder.
8. Writes a report explaining every decision.

Supported photo formats: `.jpg` `.jpeg` `.png` `.webp` `.heic` `.heif` `.tif` `.tiff`, plus `.dng` `.cr2` `.cr3` `.nef` `.arw`.
Supported video formats: `.mov` `.mp4` `.m4v`.

---

## Installing

### From crates.io, with HEIC support

This is the recommended way on macOS:

```bash
brew install libheif pkg-config
cargo install photofold
```

**HEIC support needs libheif 1.23 or newer.** Homebrew has it. No current Debian or Ubuntu release does — Ubuntu 24.04 ships 1.17.6, which is too old. On Linux, either build libheif from source first, or use the portable build below. HEIC files still work in the portable build; they are only left out of visual matching.

### Prebuilt binary

Download a release from [the releases page](https://github.com/dansku/photofold/releases), then:

```bash
tar xzf photofold-aarch64-apple-darwin.tar.gz
mkdir -p ~/.local/bin && mv photofold ~/.local/bin/
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
```

On Apple Silicon, `/usr/local/bin` is not on the default `PATH`, which is why `~/.local/bin` is used above. Homebrew's `/opt/homebrew/bin` works too.

**The prebuilt binaries are built without HEIC support**, on purpose. They link no system libraries, so they run anywhere with nothing to install. HEIC files are still scanned, hashed, read for metadata, and copied — they are simply left out of visual matching. If you want a HEIC merged with its JPEG version, use `cargo install` above.

### From source

```bash
git clone https://github.com/dansku/photofold
cd photofold
cargo build --release                      # needs libheif 1.23+
cargo build --release --no-default-features # no system libraries at all
cp target/release/photofold ~/.local/bin/
```

Rust 1.88 or newer is required.

### Windows

Windows is untested, but nothing in the program is macOS-only. Build with `--no-default-features` if libheif is awkward to install.

---

## Building

```bash
cargo build --release
```

The binary lands at `target/release/photofold`.

### Building without HEIC support

If you cannot install libheif, drop the feature:

```bash
cargo build --release --no-default-features
```

HEIC files are still scanned, hashed, read for metadata, and copied. They simply get no visual fingerprint, so they are never merged with a JPEG version of the same photo. Nothing is lost; some duplicates are just missed. The whole test suite passes in this configuration too.

---

## Usage

```text
photofold [OPTIONS] --output <PATH> <SOURCE>...
```

| Option | What it does |
|---|---|
| `-o, --output <PATH>` | Where the merged library goes. Created if missing. **Required.** |
| `--dry-run` | Analyse everything and write the report, but copy nothing. |
| `-v, --verbose` | Say more. `-vv` says a lot more, including per-stage timings. |
| `--workers <N>` | Worker threads. Defaults to the number of CPU cores. |
| `--hash-distance <N>` | How different two pictures may look and still count as the same photo, in bits of a 256-bit fingerprint. Default 8. Lower is safer. |
| `--report <PATH>` | Where to write the report. Default `./photofold-report.json`. |
| `--report-format <json\|csv>` | Report format. Default `json`. |
| `--copy-mode <copy\|hardlink>` | Copy the bytes, or hard link to the source. Default `copy`. |
| `--include-videos` | Include videos. This is already the default. |
| `--no-videos` | Skip video files entirely. |
| `--no-live-photos` | Do not keep Live Photo stills and movies together. |
| `--prefer-source <PATH>` | Prefer this source folder when two copies are otherwise equal. Repeat to set an order. |
| `--no-cache` | Analyse every file again instead of reusing earlier results. |
| `--cache-path <PATH>` | Where to keep the analysis cache. |

Run `photofold --help` for the full text.

---

## Examples

**Look before you leap.** Always worth doing first on a large library:

```bash
photofold ~/Photos/iPhone ~/Downloads/GooglePhotos -o ~/Merged --dry-run
```

**Trust the phone originals** when quality is otherwise equal:

```bash
photofold ~/Phone ~/Google -o ~/Merged --prefer-source ~/Phone
```

**Photos only, with a spreadsheet-friendly report:**

```bash
photofold ~/Phone ~/Google -o ~/Merged --no-videos \
  --report merge.csv --report-format csv
```

**Preview the result without using disk space.** Hard links share storage with the sources, so the output folder costs almost nothing until you delete the originals:

```bash
photofold ~/Phone ~/Google -o ~/Merged --copy-mode hardlink
```

Hard links only work within one filesystem. Across volumes, photofold copies instead and tells you it did.

**Be extra careful** on a library you care about:

```bash
photofold ~/Phone ~/Google -o ~/Merged --hash-distance 4
```

---

## How duplicates are found

Two separate mechanisms, in order of cost.

### Byte-for-byte duplicates

Files are grouped by size first. A file whose size no other file shares cannot have an identical twin, so it is never even opened. Files that do share a size get a quick hash of their first and last 64 KB, and only the survivors of that are hashed in full with BLAKE3, straight from a memory map.

On a 200,000-file library this reads a tiny fraction of the bytes a naive approach would.

### Photos that look the same

Every image gets two fingerprints, computed from a small thumbnail rather than the full picture:

- A **64-bit gradient hash**, used to find candidates cheaply and reported as a readable "how different do these look" number.
- A **256-bit block hash**, which is what actually decides.

The block hash decides because it is far more stable. On a real iPhone HEIC and the JPEG exported from it, the block hashes differed by 2 bits while the gradient hashes differed by 7 — the gradient hash is computed from an 8x8 grid and is very sensitive to which decoder produced the thumbnail.

Photos are never compared all against all. Each one is filed into buckets by fingerprint band, capture time, and file name, and only photos sharing a bucket are ever compared.

### The important part: looking alike is not enough

**A run of burst shots looks alike. It is not a set of duplicates.**

So no photo is ever removed on visual likeness alone. Every rule that can remove a file also demands structural evidence:

| Evidence | Result |
|---|---|
| Identical bytes | Merged |
| Same Live Photo identifier, and the same picture | Merged |
| The same picture under the same file name | Merged |
| One capture stored in two formats, with the same timestamp | Merged |
| A similar picture, but nothing structural agrees | **Both kept**, listed in the report |

Two capture times more than a month apart cap the result at "uncertain" no matter what the pixels say.

This matters. An earlier version of these rules accepted "same size, same camera, taken within five seconds". Run against a real 1,096-file library it merged five pairs of consecutive frames — `IMG_6691.JPG` and `IMG_6692.JPG`, one second apart, different file sizes. Those were five separate shutter presses, and merging them would have destroyed five photographs.

The report lists everything that looked similar but was kept anyway, so you can review those yourself.

---

## How the best copy is chosen

Not by file size. A re-compressed Google Photos export is often *larger* than the camera original it came from.

Instead each candidate is scored, highest weight first:

1. **Pixel count.** Dominant, by design. More detail always wins, because lost detail cannot be recovered.
2. **Camera-original evidence.** Camera-style file name, camera make and model in the metadata, no editing software tag.
3. **Format.** Raw beats HEIC beats lossless beats JPEG, at equal resolution.
4. **Metadata richness.** Tag count, GPS, camera identification.
5. **Bit depth**, where the container reports it.
6. **File size.** A weak, capped tie-break only.
7. **`--prefer-source`.** A final nudge, never enough to override a better picture.

Every rule that fires writes a sentence, and those sentences become the `winner_reason` in the report. The explanation is generated by the same code that made the decision, so it cannot drift out of step:

```json
"winner_reason": [
  "3024x4032 (12.2 MP)",
  "HEIC keeps more of the original data than JPEG",
  "looks like the camera original",
  "42 metadata tags",
  "preferred over IMG_9460.jpg by 4004 points"
]
```

---

## HEIC and iPhone photos

HEIC files are copied exactly as they are. They are never converted, re-encoded, or renamed. Decoding happens only in memory, only to compute a fingerprint, and only at thumbnail size — the embedded thumbnail inside the HEIC is used when there is one, so a 24-megapixel photo never becomes a 24-megapixel buffer.

Where a HEIC and a JPEG are the same photo, the HEIC normally wins: same resolution, better format, richer metadata.

Dimensions come from the container's own `ispe` box rather than from EXIF or from the decoder's output, because those two can disagree with the truth.

### A file named `.HEIC` is not always a HEIC

photofold identifies every file by its first bytes, not by its extension. This is not a nicety. In the real Google Photos export used to develop this tool, **344 of 347 files named `.HEIC` were plain JPEG inside**.

Trusting the extension there would have failed to decode all 344, and then handed each one a "HEIC is the camera original" bonus it had not earned. Files are always copied under their original names; sniffing changes how a file is *analysed*, never what it is *called*.

---

## Live Photos

A Live Photo is a still plus a short movie. photofold treats the pair as one thing:

**The still decides. The movie follows.**

A paired movie takes no part in duplicate detection at all. It is copied when its still is copied, dropped when its still is dropped, and always given the same output name — so `IMG_1234.HEIC` and `IMG_1234.MOV` stay together even when the name had to change to avoid a collision.

Pairing uses, strongest first:

1. **Apple's content identifier**, read from the MakerNote on the still and from the `moov/meta` atoms on the movie.
2. **Same folder, same file name**, but only when corroborated — the movie must be 15 seconds or shorter, or the two must have been captured within a minute of each other. Without that corroboration nothing is paired, because a wrongly paired movie would share the still's fate and could be thrown away.

In practice the file name does most of the work. Google Photos keeps the identifier on the still but strips it from the exported movie. In the test library, 316 pairs were found, all by name.

A winner keeps its own movie and never adopts one from a copy that lost. If a removed copy had a movie and the winner does not, the report says so.

`--no-live-photos` turns all of this off, and movies then behave like any other file.

---

## Google Photos exports

This tool was built against a real Google Takeout export, and it handles what those exports actually contain:

- **Files whose extension lies about their contents.** Identified by their bytes.
- **Re-compressed copies** of photos you also have as originals. Matched by fingerprint, and the original wins.
- **Live Photo movies renamed from `.MOV` to `.MP4`**, with the still's name kept. Paired by name.
- **Stripped video metadata.** Handled, because pairing does not depend on it.
- **Export suffixes** such as `IMG_1234(1).JPG`. Trimmed before names are compared.
- **`-edited` copies.** Treated as different pictures and kept, since they are.

Google Takeout `.json` sidecar files are recognised and counted, but not yet used. The report shows how many were seen.

---

## Dry runs

```bash
photofold phone google --output merged --dry-run
```

A dry run does the entire analysis and writes the full report. It does not copy a single byte, and it does not even create the output folder.

```text
DRY RUN — nothing was copied

Sources
  /Users/me/Phone
  /Users/me/Google Photos

Scanned
         1,096 files
           643 photos
           453 videos

Duplicates
             0 exact
             0 visual
            34 unsure, kept both (see the report)

Result
           643 photos
           453 videos
           316 Live Photos

Storage
  Input:     21.8 GB
  Output:    21.8 GB
  Saved:         0 B

Report
  ./photofold-report.json

No files were changed.
```

---

## Reports

Every decision goes in the report, including the ones that were *not* acted on. A group with `"merged": false` is a pair that looked similar but was left alone — both files were copied.

```json
{
  "tool": "photofold",
  "dry_run": true,
  "summary": {
    "files_scanned": 1096,
    "images_scanned": 643,
    "videos_scanned": 453,
    "exact_duplicate_files": 0,
    "visual_duplicate_files": 5,
    "redundant_live_photo_components": 2,
    "unique_photos": 638,
    "live_photo_pairs": 314,
    "groups_kept_apart": 29,
    "unreadable_files": 0,
    "bytes_input": 23400000000,
    "bytes_output": 23379000000,
    "bytes_saved": 21000000
  },
  "groups": [
    {
      "id": "g000012-very_high",
      "confidence": "very_high",
      "merged": true,
      "winner": {
        "path": "/Phone/IMG_1234.HEIC",
        "format": "heif",
        "width": 4032,
        "height": 3024,
        "size": 2837211
      },
      "winner_reason": [
        "4032x3024 (12.2 MP)",
        "HEIC keeps more of the original data than JPEG",
        "looks like the camera original",
        "42 metadata tags"
      ],
      "duplicates": [
        {
          "path": "/Google/IMG_1234.JPG",
          "format": "jpeg",
          "width": 4032,
          "height": 3024,
          "size": 5248212,
          "perceptual_distance": 2
        }
      ]
    }
  ],
  "errors": [],
  "timings": []
}
```

The CSV report is one row per file, with `role` set to `winner`, `duplicate`, or `kept_too`:

```bash
photofold ~/Phone ~/Google -o ~/Merged --report merge.csv --report-format csv
```

Files that could not be read appear under `errors`, with the stage that failed and why. They are still copied — a photo that cannot be analysed is never assumed to be a duplicate.

---

## The cache

Hashing and decoding dominate the cost of a large merge, and neither depends on anything but the file itself. Results are stored in a small SQLite database and reused as long as the file has not changed.

The cache key is `(path, size, modification time)`. Any edit changes at least one of those, so a stale result is never used. The cache only replays measurements; it never decides that two files are duplicates.

Measured on a 1,096-file, 21.8 GB library:

| Run | Time |
|---|---|
| First | 2.42 s |
| Second, from cache | 0.03 s |

Identical decisions both times.

By default the database lives in the system cache folder, so it never ends up inside a photo folder where a later run would try to scan it:

- macOS: `~/Library/Caches/photofold/analysis.sqlite`
- Linux: `~/.cache/photofold/analysis.sqlite`

Use `--cache-path` to put it elsewhere, or `--no-cache` to switch it off. A cache that cannot be opened or written never stops a run; photofold warns and does the full work.

---

## Performance

Built for libraries of 10,000 to 200,000 files and beyond. On a 16-core Apple Silicon machine, the 1,096-file, 21.8 GB test library above analyses in **2.4 seconds**.

What makes it scale:

- **Photos are never compared all against all.** Bucketing keeps the number of comparisons near-linear, with a hard cap as a backstop.
- **Files are not read to be hashed** unless another file shares their size, and then only their edges at first.
- **Pictures are never decoded at full resolution.** JPEG uses its decoder's own 1/8-scale mode, HEIC uses its embedded thumbnail, and everything else is shrunk immediately.
- **Full-size pixels are never held.** Only a 40-byte fingerprint survives each image. Peak memory tracks worker count, not library size.
- **Large files are never read into RAM**, only memory-mapped.
- **One bounded thread pool**, sized by `--workers`.

`-v` prints where the time went:

```text
Time spent
  scan            0.01s  82690 files/s
  exact_hash      0.00s  386.8 MB/s
  analyze         1.94s  331 images/s
  match           0.00s  1958264 comparisons/s
  copy            4.12s  1204.5 MB/s
```

If a bucket of look-alike files grows too large to compare in full, photofold says so rather than silently skipping the work.

---

## Safety guarantees

These are enforced in code, not by convention, and each has a test:

- **Source files are opened read-only.** No code path in the program opens a source file for writing, renames one, or deletes one.
- **Nothing in the output folder is ever overwritten.** Destinations are created with an exclusive create, which fails if anything is already there.
- **Two different photos with the same name both survive**, as `IMG_1234.HEIC` and `IMG_1234_2.HEIC`. Name collisions are compared case-insensitively, because macOS and Windows filesystems are.
- **An interrupted copy leaves no half-written photo.** Bytes go to a temporary file and are renamed into place.
- **The output folder can live inside a source folder.** It is pruned from the scan, so a run never ingests its own output.
- **The output folder may not *be* a source folder.** That is refused before anything is written.
- **Trash and system folders are skipped**, so a merge cannot quietly undelete a photo.
- **Symbolic links are never followed.**
- **AppleDouble sidecars** (`._IMG_1234.HEIC`, common on external and FAT-formatted drives) are skipped and counted, not reported as corrupt photos.
- **One unreadable file never stops the run.** Failures are collected, reported at the end, and the file is still copied.

---

## Limitations

Known, and deliberate:

- **Camera raw files are not decoded.** `.dng`, `.cr2`, `.cr3`, `.nef` and `.arw` are scanned, hashed, read for metadata, and copied, but they get no visual fingerprint, so they are only ever matched byte-for-byte.
- **Videos are matched byte-for-byte only.** A re-compressed copy of a video is not detected. Duration, dimensions, codec and bitrate are recorded so a later version can.
- **Google Takeout JSON sidecars are recognised but not used.**
- **The output folder is flat.** There is no `--organize year/month` yet, though the code is arranged to make adding it small.
- **There is no `--aggressive` mode.** Matches below the merge threshold are reported, never acted on.
- **Windows is untested.**
- **HEIC support is effectively macOS-only for now.** It needs libheif 1.23 or newer, which Homebrew provides and current Linux distributions do not yet package. Everything else works identically on Linux.
- **Bursts are kept.** Consecutive shots of the same scene are reported as similar and both kept. That is the intended behaviour, not an oversight.
- **A Live Photo movie is never deduplicated against a standalone video.** Paired movies are excluded from duplicate detection so they cannot be separated from their still. If the same clip also exists on its own elsewhere, both copies are kept. This errs towards keeping too much, never towards losing anything.

---

## Development

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --no-default-features     # the build without HEIC support
```

Test fixtures are generated in code — real JPEG, PNG, QuickTime and HEIC files built at test time — so the repository carries no large binaries.

To see what photofold makes of particular files, and why two of them did or did not match:

```bash
cargo run --example inspect -- a.heic b.jpg
```

```text
a.heic
  declared=heif actual=heif
  probe: 3024x4032 depth=Some(8)
  meta: Some(3024)xSome(4032) orient=Some(6) time=Some(1787407667) camera=Some("iPhone 17 Pro") tags=42
  live photo id: Some("8A3BAAAD-94D8-42AE-8CE0-A6D04BBA7B78")
  thumbnail: 192x256
  dhash=0x104bc6ec2e72e240

distances:
  a.heic vs b.jpg: visual=7 detail=2
```

The design, and the reasoning behind each decision, is written up in [`docs/architecture.md`](docs/architecture.md). [`CONTRIBUTING.md`](CONTRIBUTING.md) explains the one rule that governs changes.

---

## Licence

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).

`photofold` links libheif when built with the `heic` feature. HEVC and HEIF may be covered by third-party patents in some countries. The prebuilt binaries contain no HEVC decoder.
