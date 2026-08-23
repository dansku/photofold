# photofold — Architecture

This document answers the seven pre-implementation questions in `plan.md`. It is the design of record. Update it when the design changes.

Priority order, from `plan.md`: **correctness > data safety > low false-positive rate > performance > maintainability > user experience.**

---

## 1. Proposed architecture

The program is a linear pipeline of stages. Each stage owns one module, takes the previous stage's output, and returns a new value. No stage reaches back into an earlier stage.

```text
cli.rs         parse args, validate paths             -> Config
scanner.rs     walk inputs, classify, filter          -> Library
cache.rs       replay earlier measurements            -> fills the Library
exact.rs       size buckets -> partial -> full BLAKE3 -> exact groups
analyze.rs     parallel driver for the two below      -> fills the Library
  metadata.rs    EXIF / container metadata            -> MediaMetadata
  fingerprint.rs decode small, normalise, hash        -> VisualFingerprint
live_photo.rs  pair stills with movies, force co-travel
bucketing.rs   build candidate buckets                -> candidate pairs
matching.rs    evidence -> MatchConfidence            -> edges
grouping.rs    union-find over accepted edges         -> DuplicateGroups
quality.rs     score every file, pick winner + reason
planner.rs     winners -> CopyPlan (collision-free destination names)
copier.rs      execute plan, or skip on --dry-run
report.rs      JSON / CSV report + terminal summary
```

Supporting modules: `models.rs` (data types and format sniffing), `error.rs` (`thiserror` error enum plus a non-fatal `FileError` collector), `isobmff.rs` (a minimal MOV/MP4 box walker), `apple.rs` (Live Photo content identifiers, built on `isobmff`), `video.rs` (codec and bitrate, also built on `isobmff`), `decode/` (pluggable image decoders), `progress.rs` (indicatif), `timing.rs` (per-stage timing and throughput).

Why a pipeline rather than a per-file state machine: every stage after `scanner` is a bulk operation over the whole set, and three of them (exact grouping, bucketing, grouping) are inherently global. A pipeline makes the global steps obvious and keeps the parallel steps embarrassingly parallel.

Deviation from the module list in `plan.md`: `analyzer.rs` is split into `metadata.rs` and `fingerprint.rs`, `perceptual_hash.rs` becomes `fingerprint.rs`, and `matching.rs` / `bucketing.rs` / `planner.rs` / `cache.rs` are added. This keeps each module under one responsibility.

### Decoder abstraction

`decode/mod.rs` exposes one trait:

```rust
pub fn probe(path: &Path, format: FileFormat) -> Option<ProbeInfo>;   // no pixel decode
pub fn thumbnail(path: &Path, format: FileFormat, target: u32) -> Result<DynamicImage, String>;
```

Backends: `jpeg.rs` (DCT-scaled decode), `standard.rs` (`image` crate: PNG, WebP, TIFF), `heif.rs` (libheif, behind the `heic` feature, prefers the embedded thumbnail). Adding a format is adding one file plus one match arm, not editing the pipeline.

### Formats come from the bytes, not the extension

`FileFormat::sniff` reads the first 16 bytes and `FileFormat::reconcile` decides which answer to trust. This is not a nicety. In the real Google Photos export used during development, **344 of 347 files named `.HEIC` were plain JPEG inside**. Trusting the extension would have sent every one of them to the HEIC decoder, failed, and then handed each a "HEIC is the camera original" bonus in the quality ranking.

The extension wins in only two cases, where it is more specific than a magic number can be: camera raw files are TIFF containers, and QuickTime and MP4 share one container. Files are always copied under their original names; sniffing changes how a file is analysed, never what it is called.

---

## 2. Dependency choices and reasoning

Versions verified against crates.io on 2026-08-23.

| Crate | Version | Why this one |
|---|---|---|
| `clap` (derive) | 4.6 | Standard. Derive API keeps `--help` in one place. |
| `walkdir` | 2.5 | Chosen over `ignore`: `ignore` respects `.gitignore` and skips hidden files **by default**. Silently skipping a photo is a data-safety violation here. The walk is not the bottleneck. |
| `rayon` | 1.12 | Work-stealing pool, thread count set once from `--workers`. No hand-rolled threads. |
| `blake3` | 1.8 | Faster than SHA-256, no compatibility reason to avoid it. |
| `memmap2` | 0.9 | Hash large files without reading them into RAM. |
| `image` | 0.25 | PNG / WebP / TIFF decode and resizing. |
| `jpeg-decoder` | 0.3 | Used **directly**, not through `image`. It exposes `Decoder::scale()` for 1/8, 1/4, 1/2 DCT-scaled decode. `image`'s JPEG backend (zune-jpeg) has no scaled-decode option, so a 48 MP JPEG would otherwise be fully decoded just to make a 32x32 hash. |
| `libheif-rs` | 3.0 | HEIC backend, behind the default-on `heic` feature. Matches the system libheif 1.23.1, which ships a libde265 HEVC decoder. |
| `rusqlite` (bundled) | 0.40 | The analysis cache. `bundled` compiles SQLite into the binary, so there is no runtime dependency. |
| `re_mp4` | — | **Not used.** `nom-exif` already supplies video duration and dimensions, and no crate exposes the Apple content-identifier atom or the codec, so `isobmff.rs` walks the boxes itself — about 100 lines — rather than adding a dependency that would not have covered the gap anyway. |
| `nom-exif` | 3.6 | Pure Rust, and the only maintained option that reads EXIF from JPEG, HEIF **and** QuickTime/MP4. `kamadak-exif` (last release 2024) cannot read MOV. `rexiv2` needs C libraries. |
| `image_hasher` | 3.1 | Maintained successor to `img_hash`. Provides Gradient (dHash), Median (pHash-like), and Blockhash in one crate. |
| `re_mp4` | 0.5 | Most current pure-Rust ISOBMFF box parser. `mp4parse` and `mp4` were last released in 2023. |
| `rusqlite` (bundled) | 0.40 | Analysis cache. `bundled` keeps SQLite inside the binary. |
| `indicatif` | 0.18 | Progress bars and multi-bar support. |
| `serde`, `serde_json`, `csv` | 1.0 / 1.0 / 1.4 | Report serialization. |
| `thiserror`, `anyhow` | 2.0 / 1.0 | `thiserror` for library-style typed errors in modules, `anyhow` at the `main` boundary. |
| `tracing`, `tracing-subscriber` | 0.1 / 0.3 | Structured diagnostics gated behind `--verbose`. |

Thread count comes from `std::thread::available_parallelism`, so `num_cpus` is not needed.

### Cargo features

```toml
default = ["heic"]
heic    = ["dep:libheif-rs"]   # needs libheif 1.23+ and pkg-config
```

`libheif-rs` 3.0 requires libheif 1.23 or newer. Homebrew ships 1.23.1; Ubuntu 24.04 ships 1.17.6, so the feature cannot be built there from distribution packages. CI therefore tests the HEIC build on macOS and the portable build on both platforms, and the released binaries are portable builds.

Building without `heic` still scans, hashes, reads EXIF from, and copies HEIC files. They simply get no visual fingerprint, so they are never visually merged. That is the conservative failure mode, and the whole test suite passes in both configurations.

Camera raw formats (`.dng`, `.cr2`, `.cr3`, `.nef`, `.arw`) are scanned, hashed, read for EXIF, and copied, but not decoded, so they are never merged visually. Decoding them through `rawler` is the natural next backend in `decode/`.

---

## 3. Data model

```rust
enum MediaKind { Image, Video, Sidecar }          // videos are never treated as images

struct ScannedFile { id: FileId, path, source_root: SourceId, kind, size, modified_at }

struct MediaFile {
    id: FileId,
    path: PathBuf,
    source_root: SourceId,
    kind: MediaKind,
    size: u64,
    modified_at: SystemTime,
    exact_hash: Option<Blake3Hash>,
    metadata: MediaMetadata,
    fingerprint: Option<VisualFingerprint>,
    error: Option<FileError>,                     // analysis failed; file still travels
}

struct MediaMetadata {
    width: Option<u32>, height: Option<u32>,
    capture_time: Option<OffsetDateTime>,
    camera_make: Option<String>, camera_model: Option<String>,
    orientation: Option<u16>,
    format: FileFormat,
    bit_depth: Option<u8>,
    gps: Option<GpsCoord>,
    software: Option<String>,
    live_photo_id: Option<String>,
    exif_tag_count: u32,                          // metadata richness signal
    video: Option<VideoMetadata>,                 // duration, codec, bitrate
}

struct VisualFingerprint { dhash: u64, blockhash: [u8; 32], aspect_ratio: f32 }

enum MatchConfidence { Exact, VeryHigh, Probable, Uncertain }

struct DuplicateGroup {
    id: GroupId,
    files: Vec<FileId>,
    winner: FileId,
    confidence: MatchConfidence,
    winner_reason: Vec<String>,
    evidence: Vec<Evidence>,
}
```

Files are addressed by `FileId` (an index into one arena `Vec<MediaFile>`). Nothing downstream clones a `MediaFile`. Paths are stored once.

`width`/`height`/`bit_depth` come from **metadata or the container** (the HEIF `ispe` box is authoritative), never from decoder output — the pure-Rust HEIC backend silently truncates 10-bit to 8-bit and would corrupt the quality score.

---

## 4. Duplicate detection strategy

### 4.1 Exact duplicates

1. Group by file size. A unique size cannot have a byte-identical twin, so it is never hashed. This removes most of the library from hashing work.
2. Within a size group, hash the first and last 64 KiB (partial hash). Distinct partial hashes rule out a match.
3. Only survivors get a full BLAKE3 over a memory map.

Identical bytes give `MatchConfidence::Exact` with no further checks.

### 4.2 Visual duplicates: bucket, then compare

An all-pairs comparison is not implemented. Every image is inserted into several candidate buckets, and comparisons happen **only inside a bucket**:

- Nine bands cut from the 256-bit block hash. Two hashes differing in at most eight bits must agree exactly on at least one band, because eight differences cannot cover nine bands, so band matching finds every pair the matcher would merge at the default setting.
- `capture_time` rounded to a 10-second window, plus the neighbouring window.
- The file name with export suffixes such as `(1)` removed — this catches `IMG_1234.HEIC` against `IMG_1234.JPG` when timestamps are missing.

Bucket size is capped at 400. An oversized bucket (thousands of files sharing a placeholder timestamp, say) is logged, counted, and reported to the user rather than allowed to become quadratic. Skipping can only miss a duplicate, never invent one.

### 4.3 Two hashes, two jobs

Every image gets a 64-bit gradient hash and a 256-bit block-mean hash.

**The block hash decides.** Measured on a real iPhone HEIC and the JPEG exported from it, the block hashes differed by 2 bits while the gradient hashes differed by 7 — more than any safe threshold would allow. The gradient hash is computed from an 8x8 grid and is very sensitive to which decoder produced the thumbnail; the block hash averages over 16x16 cells and is not. The gradient hash is still computed and reported, because it is a readable "how different do these look" number, but nothing is merged on its say-so.

### 4.4 Evidence-based confidence

Comparison produces `Vec<Evidence>`, not a boolean. Evidence variants: `IdenticalBytes`, `VisualDistance`, `DetailDistance`, `SameDimensions`, `SameAspectRatio`, `CaptureTimeDelta`, `SameCamera`, `SameBaseFilename`, `SameLivePhotoId`, `SameLocation`, `DifferentFormat`.

A small declarative rule table maps evidence to a confidence, so the logic is readable and testable in isolation rather than being an `if` chain. **Every rule that can remove a file demands structural agreement on top of visual agreement:**

| Evidence | Confidence |
|---|---|
| identical bytes | Exact |
| same Live Photo identifier + same picture + same shape | VeryHigh |
| same picture + same shape + same base file name | VeryHigh |
| same picture + same shape + identical capture time + **different format** | VeryHigh |
| similar picture + same size + captured within 2 s | Probable |
| similar picture + same shape + captured within 10 s | Probable |
| similar picture + same size | Probable |

Anything else that was compared at all is `Uncertain`. A veto applies on top: capture times more than a month apart cap the result at `Uncertain`, whatever the pixels say.

**Why structural evidence is required.** An earlier version accepted "same size, same camera, captured within five seconds". Run against a real 1,096-file library it merged five pairs of consecutive frames — `IMG_6691.JPG` and `IMG_6692.JPG`, one second apart, different file sizes, block hashes 8 bits apart. They were five separate shutter presses, and merging them would have destroyed five photographs. Visual likeness alone is the signature of a burst, not of a duplicate. A format difference is safe evidence precisely because a camera writes every frame of a burst in the same format.

**Only `Exact` and `VeryHigh` merge.** `Probable` and `Uncertain` appear in the report with `merged: false` and both files are copied. An `--aggressive` flag that also merges `Probable` is left as a one-line change, not implemented in v1.

### 4.5 Orientation normalization
Google Photos sometimes bakes rotation into the pixels while the camera original carries an EXIF orientation flag. Thumbnails are rotated to their EXIF-upright form **before** hashing, so those pairs match instead of becoming false negatives. libheif already applies a HEIF file's own rotation while decoding, so `decode::orientation_applied_by_decoder` tells the fingerprinter not to rotate those a second time.

### 4.6 Grouping

Accepted edges feed a union-find. Groups are then sorted deterministically by their lowest source path, so two runs over the same library produce byte-identical reports.

---

## 5. Quality-ranking strategy

`quality.rs` exposes one function: `score(&MediaFile, &Config) -> QualityScore`, where `QualityScore` carries the total plus the itemized reasons. Ranking rules live only here.

Weights, highest first:

1. **Pixel count** — dominant. A 4032x3024 file always beats a 2048x1536 file.
2. **Camera-original evidence** — intact MakerNote, no editing `Software` tag, camera-style filename (`IMG_`, `DSC_`), format that a camera actually writes.
3. **Format preference** — HEIC and RAW over JPEG at equal resolution; lossless over lossy.
4. **Metadata richness** — EXIF tag count, presence of GPS, camera make/model.
5. **Bit depth**, when known.
6. **File size** — tie-break only, and deliberately weak. A recompressed Google JPEG is often *larger* than the HEIC original, so size must never dominate.
7. **`--prefer-source`** — a final tie-break bonus, small enough that it never overrides a genuine resolution or originality advantage.

Every contributing rule pushes a human-readable string, so `winner_reason` in the report is generated, not hand-written.

---

## 6. Live Photo strategy

A Live Photo is a still (HEIC or JPEG) plus a motion file (MOV or MP4) that share an Apple **content identifier**.

The rule is: **the still decides, the movie follows.** A paired movie takes no part in duplicate detection at all. It is copied when its still is copied, dropped when its still is dropped, and given the same output stem either way, so `IMG_1234.HEIC` and `IMG_1234.MOV` stay together even when the stem had to change for a name collision.

Pairing signals, strongest first:

1. **Content identifier match.** On the still this is Apple MakerNote tag `0x11`; on the movie it is `com.apple.quicktime.content.identifier` in the `moov/meta` keys and ilst atoms. No crate exposes either, so `apple.rs` parses both: the MakerNote as a TIFF-style directory with its own byte order and offset base, the movie atom with a hand-written box walker. The walker probes whether a `meta` box carries the four version-and-flags bytes that MP4 uses and QuickTime does not; guessing wrong misaligns everything below it.
2. **Same folder, same file name, corroborated** by a movie duration of 15 seconds or less, or capture times within 60 seconds. The corroboration is required. A name match on its own would pair a two-minute holiday clip with an unrelated photo, and since a paired movie shares its still's fate, that would throw the video away.

**Measured reality.** In the Google Photos export used during development, the stills still carried their content identifiers but the exported movies had no `meta` box at all — Google strips it. Meanwhile 309 `.HEIC` stems had a matching `.MP4`. So identifier pairing is a bonus and file-name pairing is the workhorse. The implementation reflects that: 316 pairs were found in that library, all by name.

A winner keeps its own movie and never adopts a loser's. When a removed copy had a movie and the winner does not, the report says so in `winner_reason`.

`--no-live-photos` turns pairing off, and movies then behave like any other file.

## 7. Performance strategy

Targets: 10k to 200k files.

- **Never read a whole file to hash it.** Size bucketing skips most files entirely; survivors are memory-mapped.
- **Never decode at full resolution.** Perceptual hashing needs roughly 32x32 pixels. JPEG uses DCT-scaled decode (1/8 scale where possible), HEIF asks libheif for a downscaled image or its embedded thumbnail, RAW uses the embedded preview. A 48 MP image never becomes a 48 MP buffer.
- **Never hold all pixels at once.** Thumbnails are consumed inside the rayon map and dropped; only the 40-byte fingerprint survives. Peak memory tracks worker count, not library size.
- **Never compare all pairs.** Bucketing (4.2) bounds comparisons to near-linear in practice, with a hard cap on bucket size as a backstop.
- **One arena, no clones.** `FileId` indices move between stages; `MediaFile` values do not.
- **Cache across runs.** SQLite keyed on `(path, size, modification time)` stores the format, exact hash, both fingerprints, and the whole metadata record as JSON. A cached file skips both the hashing stage and the analysis stage entirely. Measured on the 1,096-file, 21.8 GB development library: **2.42 s cold, 0.028 s warm**, with byte-identical decisions and a 532 KB database — about 485 bytes per file, so a 200,000-file library costs roughly 100 MB.

  The cache replays measurements only; it never stores a duplicate decision. The schema carries a version number, and a database written by a different version is dropped and rebuilt rather than half-trusted, because the fingerprint algorithm and the sniffing rules both feed it. A cache that cannot be opened or written logs a warning and the run proceeds at full cost — a broken cache must never stop a merge. Enabled by default; `--no-cache` and `--cache-path` control it. The default location is the platform cache folder, deliberately not inside any photo folder, where a later run would try to scan it.
- **Bounded parallelism.** One rayon pool sized by `--workers`. No thread-per-file.
- **Measure.** `timing.rs` records wall time and throughput per stage (MB/s for hashing, images/s for analysis, comparisons/s for matching) and prints it under `--verbose`.

---

## 8. Safety design

Guarantees, enforced in code rather than by convention:

- Source files are opened **read-only**. No code path in the crate opens a source path for writing, and no code path calls `remove_file` or `rename` on a source.
- The output directory is canonicalized and checked against every canonicalized input root. If output is inside an input, the output subtree is pruned from the walk so the tool cannot ingest its own output. If output *is* an input root, the run aborts.
- Destination writes use `create_new` (O_EXCL), so an existing destination file can never be overwritten. Filename collisions between genuinely different photos resolve to `IMG_1234_2.HEIC`, `IMG_1234_3.HEIC`, and so on.
- Copies are written to a temporary file and renamed into place, so an interrupted run leaves no truncated destination file.
- `--dry-run` performs the entire analysis and writes the report, but takes no filesystem write path other than the report itself.
- Per-file failures are collected into `Vec<FileError>` and reported at the end. They never abort the run. A file that fails analysis is still copied if it is not a confirmed duplicate.
- AppleDouble sidecars (`._IMG_1234.HEIC`, common on external and FAT-formatted volumes) are detected by name prefix and magic number, excluded from analysis, and counted separately so they do not pollute the error report.

---

## 9. Measured on real photographs

Design decisions here were checked against a real 1,096-file Google Takeout export rather than argued from first principles. Four findings changed the code:

| Finding | Consequence |
|---|---|
| 344 of 347 `.HEIC` files were JPEG inside | Formats are sniffed from the bytes, not the extension |
| A real HEIC and its JPEG export: block hash 2 bits apart, gradient hash 7 | The block hash decides; the gradient hash only reports |
| Five pairs of consecutive frames passed a "same size, same camera, seconds apart" rule | Merging now requires structural evidence, never visual likeness alone |
| Exported movies had no Apple metadata at all, but 309 `.HEIC` stems had a matching `.MP4` | Live Photo pairing leans on file names, with the content identifier as a bonus |

---

## 10. Deferred, by design

Hooks exist; implementations do not:

- Google Takeout JSON sidecars — `MediaKind::Sidecar` is already recognized and associated by name.
- `--organize year/month` — the planner already separates "choose destination name" from "copy".
- Perceptual video matching — `VideoMetadata` already records duration, dimensions, codec and bitrate, so there is something to compare on.
- RAW decoding — `decode/` takes one more backend file; `rawler` is the candidate.
- `--aggressive` — a single threshold in the confidence rule table.
