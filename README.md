# PKON

**I'm building the best lossless compressor in the world.**

PKON is a lossless compression platform that I'm building independently in Rust. It has two complementary engines in one container format: **ELLIE** for fast, practical everyday image compression, and **LISA** for high-efficiency lossless RAW/DNG compression. Every file decodes back to exactly the samples that went in.

### ELLIE Weissman score: 1.78

> **Richard Hendricks had Pied Piper. We built PKON.** On this setup ELLIE scores a Weissman **1.78** against JPEG XL effort 7. Weissman score didn't get Richard Hendricks funded.

<sub><i>Not peer-reviewed by Stanford. Richard Hendricks was unavailable for comment. See actual micro-benchmarks below.</i></sub>

Using the formula W = (R<sub>c</sub> / R<sub>r</sub>) × √(T<sub>c</sub> / T<sub>r</sub>), with R as compression ratio and T as compression throughput:

| Metric | ELLIE (X4) | JPEG XL e7 (reference) | Ratio |
| :--- | :--- | :--- | :--- |
| Compression ratio | 4.545 (16,610,254 B) | 4.582 (16,476,961 B) | 0.992 |
| Encode throughput (net, single-thread) | 3.58 MP/s (6,735 ms) | 1.11 MP/s (21,793 ms) | 3.24 → √ = 1.80 |
| **Weissman score** | | | **0.992 × 1.799 = 1.78** |

- **Reference codec:** JPEG XL effort 7, one thread. It is the practical JPEG XL setting and the like-for-like single-thread comparison for ELLIE.
- **Corpus:** the 23-image [external benchmark](benchmarks/pkon2/20261027-143537Z-ext-benchmark/report.md) (all combined, 75,497,472 raw bytes), exact lossless on every image.
- **Other references, same data:** JPEG XL e9 scores 3.47 and WebP lossless 5.96, because they encode much more slowly. PNG scores 2.20. They are not the headline because e7 is the closest competitor.
- **Caveats:** this run measured the experimental X4 configuration, not the production codec. JPEG XL timings include ffmpeg process spawn, minus a calibrated floor. Against multi-threaded JPEG XL e7 (3.16 MP/s) the score would be about 1.06.

> **ELLIE is built for speed. LISA is built to push compression further.**

## The two engines

| | **ELLIE: Consumer** | **LISA: Pro** |
| :--- | :--- | :--- |
| Built for | Everyday image workflows | Demanding RAW/DNG imaging workflows |
| Formats | PNG and JPEG (a JPEG is encoded from its decoded pixels) | Lossless RAW/DNG |
| Priorities | Fast, practical compression and decompression; lightweight operation (about 63 MB peak working set, single thread, in the production benchmark); efficient storage | Maximum compression efficiency; exact preservation of every RAW sample; specialized prediction and adaptive entropy coding |
| Trade-off | Not the fastest decoder (PNG and WebP decode much faster) | Much slower than the codecs I compare it to; the extra computation buys smaller files |

**Naming:** ELLIE was previously called PKON2. Historical benchmark reports, the `benchmarks/pkon2/` directory, and the legacy `.pkon2` file format (container v1, still readable) keep the old name because those are frozen records and a compatibility surface. Code identifiers are `ellie` / `elliex`.

They share the PKON container, local tooling and the same exact-reconstruction guarantee. They have different priorities and are not competing algorithms.

## Strongest measured results

ELLIE and LISA are benchmarked separately, on different data and against different baselines. Don't compare numbers across the two.

### ELLIE: JPEG XL-class compression, with substantially faster encoding

On the 25-image benchmark, ELLIE stores 18,804,922 bytes: **2.60% smaller than JPEG XL effort 9**, with encoding **about 8.5× faster** than effort 9 and **about 2× faster** than effort 7, and **exact lossless reconstruction** of every image. These are the original JPEG/PNG-to-`.pkon2` converter benchmarks. The input is each photo's **decoded 8-bit pixels**, centre-cropped to at
most 1024×1024. It is not the original JPEG file bytes. Sizes are complete files (24-byte header plus payload), and every output was decoded and compared
byte-for-byte with the source pixels. All outputs were exact in every run below.

**25 diverse images** ([report](benchmarks/pkon2/20261028-015857Z-bench25-diversity/report.md); production default, codec 3):

| Codec | Total bytes | Size vs ELLIE | Encode (MB/s of raw pixels) | Decode (MB/s) |
|---|---:|---:|---:|---:|
| **ELLIE** | **18,804,922** | | **5.58** | 5.38 |
| JPEG XL (effort 9) | 19,293,200 | +2.60% | 0.66 | 7.21 |
| JPEG XL (effort 7) | 19,464,716 | +3.51% | 2.86 | 6.63 |
| WebP lossless | 21,705,408 | +15.42% | 0.32 | 27.22 |
| PNG | 31,661,761 | +68.37% | 4.67 | 206.19 |

**50 images** ([report](benchmarks/pkon2/20261027-155433Z-bench50/report.md)): 38,914,941 bytes for ELLIE against 39,118,943 for
JPEG XL e9 (0.52% larger), 39,519,208 for e7 (1.55% larger), 44,695,086 for WebP (14.85% larger) and 65,492,478 for PNG
(68.30% larger). ELLIE's biggest per-image wins over JPEG XL e9 were `skyline_n0` (−26.6%) and `beach_n0` (−22.3%).
In a later production run of the improved ELLIEP coding mode (codec 2), 50 of 50 images came out smaller than the previous production
codec, 1.63% smaller in total ([report](benchmarks/pkon2/20261027-pkon2p-production/report_benchmark.md)).

How I read these honestly:

- The margin over JPEG XL is **thin**. In the 50-image run JPEG XL e9 produced the smallest file on 28 of 50 images and ELLIE on 20. ELLIE's real advantage over JPEG XL is **speed at the same ballpark of size**: in the 25-image run it encoded about 8.5× faster than JPEG XL e9 and about 2× faster than e7, and in the 50-image run about 12× faster than e9.
- ELLIE is not the fastest at decoding. PNG and WebP decode much faster, and JPEG XL decoded somewhat faster than ELLIE in the 25-image run.
- Timings include ffmpeg process spawn for JPEG XL and WebP (calibrated and subtracted in the reports where noted). They come from one laptop (i7-1185G7); the reports describe the exact procedure.
- These benchmarks do not compare against the size of the original lossy JPEG file, because ELLIE stores the decoded pixels losslessly. That is a different job.

### LISA: 11.03% fewer bytes than JPEG XL on real RAW files

25 real DNG images, 434.7 megapixels, all represented as 16-bit samples. LISA used 11.03% fewer bytes than JPEG XL across the 25 images and 15.97% fewer on the eight held-out images. It compressed better on 22 of 25 images, and all 25 were reconstructed exactly. **Qualification: the LISA figures below are Stage 4 compressed payload sizes, not complete PKON container sizes.**

| Codec | Total encoded bytes (all 25) | Held-out 8 only |
|---|---:|---:|
| **LISA** | **397,011,489** | **90,543,174** |
| JPEG XL | 440,815,878 | 105,007,221 |
| PNG | 591,948,741 | 147,040,755 |

- LISA used **11.03% fewer bytes than JPEG XL** and **49.10% fewer bytes than PNG**.
- LISA compressed better than JPEG XL on **22 of 25 images**. It did not on the other three (see [the challenge below](#current-challenge-drone-imagery)).
- On the 8 held-out images, LISA used **15.97% fewer bytes than JPEG XL**.
- **All 25 images passed exact lossless round-trip verification**, including all 8 held-out images.

> **Measurement qualification:** LISA figures are **Stage 4 compressed payload sizes**, not complete PKON container sizes. JPEG XL and PNG were
> measured using the same 16-bit RAW sample representation. LISA is also much slower: about 2.1 MiB/s encode, against 8.9 for JPEG XL and
> 15.3 for PNG. Details: [benchmarks/lisa-vs-jxl-png/report.md](benchmarks/lisa-vs-jxl-png/report.md).

**Bayer-plane prediction** predicts each of the sensor's four colour planes separately.
- It gave a **13.57% smaller payload** across the full 25-image corpus and a **17.24% smaller payload** across the eight held-out images.
- Payloads were smaller on **all 24 Bayer images**. The X-Trans image used the existing fallback.
- Source: [historical/20261029-lisa-bayer-plane-small-tree/summary.md](historical/20261029-lisa-bayer-plane-small-tree/summary.md).

**OR-128** is a 128-context residual classifier.
- It was trained on 13 images and selected using a separate validation set of 4 images.
- It was evaluated on the 8 held-out images from the frozen 25-image corpus.
- Records: [lisa-pcq1](historical/20261029-lisa-pcq1/report.md), [lisa-or128-tree](historical/20261029-lisa-or128-tree/report.md).

## Current challenge: drone imagery

**I'm still struggling with drone imagery.** I'm not getting the compression performance I want on drone DNG files, and I haven't solved it.

- In the RAW benchmark, LISA was **larger than JPEG XL on the two drone dusk shots**: `parrot_anafi_dusk` by **67.7%** (the worst case in the corpus) and `dji_fc220_dusk` by **11.9%**. The third loss was an X-Trans chart (8.0% larger), which uses the fallback predictor.
- `dji_fc220_dusk` is one of the 8 held-out images, so the held-out result is not a clean sweep: LISA beat JPEG XL on 7 of those 8.
- I've written diagnostics on the Parrot file ([drone-diagnostics1](historical/20261028-drone-diagnostics1/REPORT.md), [drone-residual-diagnostic](historical/20261028-drone-residual-diagnostic/REPORT.md)). They record measurements, but they don't establish why it compresses badly, and I'm not going to guess. Finding out is open work.

## Why PKON

- **Smaller lossless storage.** ELLIE is ahead of JPEG XL, WebP and PNG in total size on the 25-image and 50-image photo benchmarks; LISA is ahead of JPEG XL and PNG in total size on the RAW benchmark.
- **Faster encoding for everyday images.** ELLIE encodes several times faster than JPEG XL at comparable size.
- **Specialized RAW compression.** LISA works on the sensor's own samples, per colour plane for Bayer sensors, with no demosaic or other processing.
- **Exact sample reconstruction.** Every benchmark output was decoded and compared with its source, and the tooling reports integrity hashes on every conversion.
- **One container, local tooling.** A single `.pkon` format carries both engines, with a local converter UI and a Windows decoder. Nothing leaves your machine.

PKON is under active development. This repository doesn't set licensing terms, pricing or availability, so I'm not stating any here. If you have demanding image-storage needs, [open an issue](https://github.com/varman96/pkon/issues) to start a conversation.

## Quick start: the local converter

**Prerequisites:** a [Rust toolchain](https://rustup.rs/). For DNG files you also need Python 3 with `numpy` and `tifffile`. Run the commands from the repository root.

```
cargo run --release --bin serve
```

Then open **http://127.0.0.1:8080/** in a browser. The server prints `PKON converter: http://127.0.0.1:8080/   (Ctrl+C to stop)`. Press **Ctrl+C** in the terminal to stop it. To use another port, pass it as an argument: `cargo run --release --bin serve -- 9000`. The server listens on localhost only, and nothing is stored on disk.

What the page does:

| You provide | The page produces |
|---|---|
| PNG, JPEG (8-bit) | `.pkon` using ELLIE |
| DNG | `.pkon` using LISA |
| `.pkon` / `.pkon2` (standard image) | PNG |
| `.pkon` (LISA RAW) | lossless `.u16` or `.u8` sample export, plus a `.json` metadata file |

There is also a viewer for opening a standard-image `.pkon` directly in a new tab without converting it. Each conversion shows an integrity report (sizes, ratio and SHA-256 hashes).

## Supported formats and capabilities

| Input | Engine | Notes |
|---|---|---|
| PNG | ELLIE | 8-bit gray, gray+alpha, RGB, RGBA; lossless |
| JPEG | ELLIE | Encoded from decoded pixels; original JPEG bytes are not preserved |
| DNG (RAW) | LISA | 1–16 bit single-channel raster with CFA metadata; exact sample preservation; Bayer-plane prediction, with a fallback for other layouts such as X-Trans |

Output: `.pkon` (v2 container) and legacy `.pkon2`, decodable back to PNG or raw u16/u8 samples.

## Hardware Requirements

**Minimum:** A CPU that can execute instructions without crying.

**Recommended:** Enough RAM to hold the frame you just told it to read.

**Supported Platforms:** Systems with silicon.

## Exit Codes

> **Humorous example, not a documented interface.** PKON has no compress-and-exit CLI, and the code defines no exit codes. The only user-facing binary is `serve`, a localhost HTTP server that runs until you press Ctrl+C. A file that isn't a valid image gets an HTTP 400 response, not a process exit, and a codec panic becomes an HTTP 500. If `serve` can't bind its port it panics, which Rust reports as exit code 101. `137` is not something PKON returns: it is what a Unix shell reports when the OS or container OOM killer sends SIGKILL. The table is what the exit codes would say if they existed.

| Code | Meaning |
| :--- | :--- |
| `0` | Success (bytes crushed). |
| `1` | Your file wasn't a valid sensor frame. |
| `137` | OOM (you tried to pack an uncompressed 8K IMAX master on a Raspberry Pi Zero). |

## Architecture

```text
                      input
        +---------------+----------------+
        |                                |
  8-bit gray / gray+alpha          RAW / DNG stored raster
  RGB / RGBA (JPEG = decoded       (1-16 bit, single channel,
  pixels)                           CFA metadata)
        |                                |
        v                                v
+----------------------+      +-----------------------------+
| ELLIE                |      | LISA RAW                    |
| codec ids 1, 2, 3    |      | codec id 4                  |
|                      |      |                             |
| plane split / colour |      | 1. OR predictor residuals   |
|   (G, R-G, B-G)      |      |            |                |
| adaptive context-    |      | 2. Bayer-plane split /      |
|   mixing coder       |      |    traversal                |
|                      |      |            |                |
|                      |      | 3. OR-128 context           |
|                      |      |    classification           |
|                      |      |            |                |
|                      |      | 4. adaptive probability     |
|                      |      |    models + PKON coder      |
+----------+-----------+      +--------------+--------------+
           |                                 |
           +----------------+----------------+
                            v
        +--------------------------------------------+
        | PKON v2 container                          |
        | 32-byte header | codec metadata | payload  |
        +--------------------------------------------+
                            |
                            v
          pkon::decode -> Decoded::Standard | Decoded::Raw
```

## Technical architecture

### LISA: the four-stage pipeline

| Stage | What it does | Code |
|---|---|---|
| 1. Prediction / decorrelation | Predicts each sample from already-reconstructed neighbours, choosing among candidate patterns by replaying them on nearby reconstructed samples, with a bounded bias term. It emits exact zigzag-mapped residuals and needs no side information. For Bayer data this runs per colour plane (the production default); other layouts use the mosaic version. | `src/lisa.rs`, `src/lisa_bayer_plane.rs` |
| 2. Organization | A reversible permutation of the residuals. Values are never altered, and a Bayer mosaic is separated into its four CFA-position planes. | `src/lisa.rs` |
| 3. Context classification | Assigns each residual one of 128 contexts (OR-128) from its causal neighbourhood (W, N and NW residuals, each quantised to a half-octave level), using a frozen table. Nothing about the mapping is stored in the stream. | `src/lisa_or128*.rs` |
| 4. Entropy coding | Binarized, count-adaptive range coding with logistic mixing and per-context adaptive probability models (PKON coding). | `src/lisa.rs`, `src/pkon2x.rs` |

The Stage 1 mode, Stage 3 rule and Stage 4 coding version are recorded in the stream, so the decoder needs no out-of-band information and rejects versions it doesn't implement.

### ELLIE and the PKON container

ELLIE (codec "X4" and its production variants) codes 8-bit gray, gray+alpha, RGB and RGBA images with an adaptive, context-mixing coder. The PKON v2 container
([docs/pkon-format-v2.md](docs/pkon-format-v2.md)) has a 32-byte header, codec metadata and a payload:

| Codec id | Payload |
|---|---|
| 1, 2, 3 | ELLIE for standard images (payloads identical to `.pkon2` v1; codec 3 is the default writer) |
| 4 | LISA RAW (1–16 bit, single channel, with CFA metadata) |

`pkon::decode` returns `Decoded::Standard` or `Decoded::Raw` and still reads legacy `.pkon2` v1 files ([docs/pkon2-format-v1.md](docs/pkon2-format-v1.md)).
A JPEG is encoded from its *decoded* pixels; the original JPEG bytes, EXIF and ICC profile are not recoverable.

### DNG ingestion and exact sample preservation

A DNG is read through `tools/lisa_dng_extract.py` (run as a subprocess by `src/dng.rs`) and encoded from its **stored raster, untouched**:
no crop, black-level subtraction, white balance, scaling or demosaic.

### Integrity checks and round-trip verification

Integrity hashes are computed on the canonical sample bytes (`src/integrity.rs`). The container header carries a CRC and length checks. Every benchmark above decodes every output and compares every sample with the source.

### Frontend and Windows decoder

```
cargo run --release --bin serve        # then open http://127.0.0.1:8080/
```

A localhost-only converter UI ([frontend/](frontend/)): PNG/JPEG to `.pkon`, DNG to `.pkon` (LISA RAW), and `.pkon`/`.pkon2` back to PNG or lossless u16/u8
sample export, with an integrity report on every conversion. Nothing is stored on disk. [wic/](wic/) is a separate crate that registers a
Windows Imaging Component decoder for Photos and Explorer thumbnails (`wic/install.ps1`, per-user, no admin; `wic/test.ps1` checks every pixel). Build it from inside `wic/`.

## Benchmark reports and documentation

| Resource | What it is |
|---|---|
| [benchmarks/lisa-vs-jxl-png/report.md](benchmarks/lisa-vs-jxl-png/report.md) | **LISA RAW**: 25 DNGs, payload bytes vs JPEG XL and PNG, 16-bit samples |
| [benchmarks/pkon2/20261028-015857Z-bench25-diversity/report.md](benchmarks/pkon2/20261028-015857Z-bench25-diversity/report.md) | **ELLIE**: 25 diverse images, production codec 3, size and speed vs JPEG XL, WebP, PNG |
| [benchmarks/pkon2/20261027-155433Z-bench50/report.md](benchmarks/pkon2/20261027-155433Z-bench50/report.md) | **ELLIE**: 50 images, per-image wins and losses |
| [benchmarks/pkon2/20261027-143537Z-ext-benchmark/report.md](benchmarks/pkon2/20261027-143537Z-ext-benchmark/report.md) | **ELLIE**: earlier 23-image external comparison (JPEG XL was smaller here) |
| [benchmarks/pkon2/20261027-pkon2p-production/report_benchmark.md](benchmarks/pkon2/20261027-pkon2p-production/report_benchmark.md) | **ELLIEP** (codec 2) vs previous production codec |
| [docs/pkon-format-v2.md](docs/pkon-format-v2.md), [docs/pkon2-format-v1.md](docs/pkon2-format-v1.md) | Container format specifications |
| [historical/](historical/) | The development history, including failed and superseded experiments |

## Running Benchmarks

Run everything from the repo root, in release mode. The crate is named `nanoscope`, but the binary names below are what you pass to `--bin`.

**Prerequisites**
- Rust toolchain. `Cargo.toml` uses a portable release profile (no `target-cpu=native`), so results are comparable across machines.
- **FFmpeg** with libjxl and libwebp on `PATH` (override with `NANOSCOPE_FFMPEG`; the LISA Python script calls `ffmpeg` directly). It drives the JPEG XL, WebP and PNG baselines. See [Build and development](#build-and-development).
- Python 3 for the report scripts. `build_manifest50.py` also needs Pillow, and the LISA script needs `numpy`.
- `data/` is gitignored and holds the inputs. It is populated locally (`python tools/fetch_dataset.py`, `python tools/fetch_dataset50.py`; they download CC0 images from Wikimedia Commons).

**ELLIE (JPEG/PNG to `.pkon2`)**: sources in [benchmarks/pkon2/](benchmarks/pkon2/). Each run writes a new `benchmarks/pkon2/<stamp>-<name>/` directory, so a rerun does not overwrite the published reports.

| Binary | Input | Output directory |
|---|---|---|
| `bench25` | `data/dataset25.json` (build with `python benchmarks/pkon2/build_manifest25.py`) | `<stamp>-bench25-diversity/` |
| `bench50` | `data/dataset50.json` (build with `python benchmarks/pkon2/build_manifest50.py`) | `<stamp>-bench50/` |
| `bench_external` | `data/manifest.json` (23 images) | `<stamp>-ext-benchmark/` |
| `bench_p` | `data/dataset50.json`, plus crop hashes from the frozen `20261027-155433Z-bench50/dataset_manifest.json` | the CSV given by `--out` |

```
# Quick check: 3 images, 1 rep, nothing written (bench25, bench50 and bench_external accept --smoke)
cargo run --release --bin bench25 -- --smoke

# Full runs (new timestamped directory each time)
cargo run --release --bin bench25
cargo run --release --bin bench50
cargo run --release --bin bench_external

# Render the report from a run's results.json
python -I benchmarks/pkon2/report25.py benchmarks/pkon2/<stamp>-bench25-diversity
python benchmarks/pkon2/report50.py benchmarks/pkon2/<stamp>-bench50

# Production .pkon2 file path, one codec per process (the ELLIEP report used codec 2)
cargo run --release --bin bench_p -- --codec 2 --out <scratch>/run_c2.csv
```

`bench_external` has no report script; it writes `report.md`, `per_image.csv` and `results.json` itself. `bench_p` is Windows-only in practice (it reads peak memory through PowerShell). Point its `--out` at a scratch path, not at the frozen `20261027-pkon2p-production/` files.

**LISA (RAW) vs JPEG XL vs PNG**: sources in [benchmarks/lisa-vs-jxl-png/](benchmarks/lisa-vs-jxl-png/). Inputs are the frozen 16-bit rasters listed in `source_manifest.json` (`data/praw1/canon/*.u16`, sha256-checked). The runner takes `<manifest.json> <out.json> [reps=3]`, and the Python script takes `<exp_dir> [reps=3]`.

```
cargo run --release --bin exp_lisa_vs_jxl_png -- <scratch>/source_manifest.json <scratch>/lisa_raw.json 3
python -I benchmarks/lisa-vs-jxl-png/exp_lisa_vs_jxl_png.py <scratch>
```

The Python script reads `source_manifest.json`, `split.json` and `lisa_raw.json` from `<scratch>`, and writes `ext_raw.json`, `results.json`, `per_image.csv` and `report_tables.md` there. Copy `source_manifest.json` and `split.json` into `<scratch>` first. Running against `benchmarks/lisa-vs-jxl-png/` itself overwrites the published results.

**Published results versus fresh runs.** The reports in the table above are historical, fixed outputs. The commands above regenerate the measurement on your machine; sizes should match when the inputs match, but timings will differ with hardware. The `--smoke` runs and the `<scratch>` variants are the cheap ways to check your setup. The full runs encode and decode every image through several codecs, so expect them to take a while.

## Known Issues / FAQ

**Q: Will you support an enterprise cloud SaaS tier?**

A: No. Run the binary.

**Q: Can we transfer this project to our organization?**

A: No.

**Q: Does this implement actual middle-out compression?**

A: The math is in the code. You have the source; check the diff.

## Privacy

PKON runs locally on your machine. Your images are processed locally and are never uploaded, tracked, or collected. I have no access to your photos or their contents. Your files stay under your control.

## Report a bug

Open a GitHub Issue at **https://github.com/varman96/pkon/issues**. Where applicable, please include:

- PKON version or Git commit.
- Operating system.
- Input image format and relevant properties (dimensions, bit depth, channels, camera and CFA layout for DNG).
- Steps to reproduce.
- Expected versus actual behaviour.
- Error messages or logs.
- Whether the problem affects encoding, decoding, or both.
- A minimal sample file, only if it's safe and legally permissible to share.

For compression or lossless-integrity bugs, say explicitly whether the decoded output differs from the source. Please don't upload private or sensitive images to a public issue; describe the file's properties instead.

## Contributing

1. Fork the repository.
2. Clone your fork.
3. Create a focused branch.
4. Make your change.
5. Build and run the relevant tests (below).
6. Open a pull request describing the change and how you verified it.

```
cargo fmt --check                          # formatting check
cargo check --all-targets --release
cargo test --release --test <target>       # focused: e.g. pkon_container, pkonfile_codec3, lisa_stage4, roundtrip
cargo test --release                       # full gate before opening the PR
```

`cargo fmt --check` is the recommended formatting check. The repository currently has pre-existing formatting issues, so it may report differences in files you didn't touch. Please don't reformat unrelated files just to make the check pass. Format the Rust files you actually changed, and keep formatting changes focused on your own change.

What I expect from a pull request:

- Keep it focused.
- Preserve lossless correctness. Decoding must reproduce the source exactly.
- Include reproducible benchmark evidence for any performance or compression claim.
- Don't silently change frozen benchmark baselines or historical results.
- Don't change established format behaviour without documenting the compatibility implications in [docs/](docs/).

## Build and development

```
cargo build --release
cargo check --all-targets --release
cargo test --release                       # full correctness gate (ffmpeg-dependent tests skip loudly if ffmpeg is missing)
cargo test --release --test lisa_or128     # OR-128 (reads historical/20261029-lisa-pcq1/train/map_K128.bin)
cargo test --release --test pkon_container
```

Other test targets: `lisa_bayer_plane`, `lisa_or128_tree`, `lisa_pcq`, `lisa_stage4`, `ellie`, `pkonfile`, `pkonfile_codec3`, `roundtrip`.
Verification binaries: `verify_pkon2_file`, `verify_x4_promotion`.

Requirements:
- **FFmpeg** with libjxl (and libwebp) for the JPEG XL / WebP / PNG baselines; override the binary with `NANOSCOPE_FFMPEG`. Install libjxl (e.g. `winget install libjxl`) before quoting JPEG XL numbers as "the" baseline.
- **Python 3** with `numpy` and `tifffile` for DNG ingestion. `tools/lisa_dng_extract.py` must stay in `tools/`, because `src/dng.rs` finds it relative to the crate root.
- `data/` (gitignored) holds local datasets, e.g. `python tools/fetch_dataset.py`.

The Rust crate is still named `nanoscope` (`nanoscope::pkon`, `nanoscope::pkonfile`), and the FFmpeg override variable is `NANOSCOPE_FFMPEG`. These are code identifiers I haven't renamed; the project is PKON.

## Repository structure

```
README.md  Cargo.toml  Cargo.lock  build.rs  .gitignore
src/                 production library (lisa*, pkon*, dng, canon, integrity, ...)
  bin/               production binaries: serve, verify_pkon2_file, verify_x4_promotion
frontend/            serve.html, viewer.html (embedded by src/bin/serve.rs)
benchmarks/
  pkon2/             JPEG/PNG -> ELLIE benchmarks: sources, manifest/report scripts, reports and results
  lisa-vs-jxl-png/   LISA vs JPEG XL vs PNG: runner, script, report and raw results
experiments/bin/     experiment, probe and diagnostic binaries
historical/          development history, including failed and superseded experiments
tests/  tools/  docs/  wic/  data/
```

Reports in `historical/` are byte-for-byte as written and may still say `results/<name>`; read that as `historical/<name>`, or `benchmarks/pkon2/<name>` for the four ELLIE benchmark directories.

> **Notice to Hooli Engineers:** If compiling on Nucleus hardware, ELLIE may cause an unhandled panic due to excessive efficiency. Do not file an issue.
