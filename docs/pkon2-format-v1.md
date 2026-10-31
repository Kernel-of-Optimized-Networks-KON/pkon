> **Note:** ELLIE is the current name of the engine formerly called PKON2. This document describes the legacy `.pkon2` container (v1) and keeps its original name; the format is unchanged.

# PKON2 file format, version 1 (`.pkon2`)

> Legacy container version. New files use the shared `.pkon` container, version 2: `docs/pkon-format-v2.md`. This document stays normative for v1 files, which `pkon::decode` still reads.

Normative for the implementation in `src/pkonfile.rs`. Key words MUST / MUST NOT are used as in RFC 2119.
All multi-byte integers are **little-endian, unsigned**. A file is exactly: `header (24 bytes) || payload (payload_len bytes)`.

## 1. Header

| Offset | Size | Field | v1 value / valid range | Validation and effect |
|---:|---:|---|---|---|
| 0 | 4 | `magic` | `89 50 4B 32` (`\x89PK2`) | Mismatch -> `BadMagic`. High-bit first byte detects 7-bit stripping; not a ZIP (`PK\x03\x04`) or PNG signature. |
| 4 | 1 | `version` | `1` | Any other value -> `UnsupportedVersion`. Single integer: a new version means a new layout/semantics; there is no minor version. |
| 5 | 1 | `flags` | `0` | Every bit is reserved and mandatory-to-understand; non-zero -> `ReservedFlags`. Writers MUST write 0. |
| 6 | 1 | `codec` | `1`, `2` or `3` | Codec id (section 3). Writers use 3 (codecs 1 and 2 remain selectable via `encode_with_codec`); readers MUST accept all three. Unknown -> `UnsupportedCodec`; never substituted. |
| 7 | 1 | `pixel_format` | `0` gray8, `1` gray+alpha8, `2` RGB8, `3` RGBA8 | Unknown -> `UnsupportedPixelFormat`. Determines channel count and plane mapping. |
| 8 | 4 | `width` | 1 ..= limit | 0 -> `InvalidDimensions`. |
| 12 | 4 | `height` | 1 ..= limit | 0 -> `InvalidDimensions`. |
| 16 | 4 | `payload_len` | 1 ..= 2^32-1 | Exact byte length of the payload; see section 5. |
| 20 | 4 | `crc32` | any | CRC-32 of the reconstructed pixel bytes (section 4). |

The header length is fixed at 24 for version 1 and is not stored. The payload starts at byte 24 and ends at `24 + payload_len`.
There is no variable-length metadata, no padding, and no alignment requirement. Header fields are not individually
checksummed: magic/version/flags/codec/format are checked exactly; a corrupted `width`/`height`/`pixel_format` that is still
structurally valid decodes to different pixels and is rejected by the pixel CRC (or by the stream-end check) except with
probability about 2^-32.

## 2. Image representation (what v1 can and cannot hold)

Samples are **8-bit unsigned integers**; row-major, top row first, left to right, no row padding, channels interleaved. The
reconstructed buffer has `width * height * channels` bytes (`canon::Canonical` layout).

| `pixel_format` | channels | byte order within a pixel |
|---:|---:|---|
| 0 | 1 | `Y` |
| 1 | 2 | `Y, A` |
| 2 | 3 | `R, G, B` |
| 3 | 4 | `R, G, B, A` |

Alpha is straight (non-premultiplied) and is stored like any other sample: RGB values under `A = 0` are preserved. No
colour-space conversion, normalisation, resampling or channel dropping occurs. **Not supported in v1** (and not implied):
16-bit or float samples, palettes, ICC profiles, EXIF, orientation, animation, multiple frames, tiling, arbitrary metadata.
The encoder returns `UnsupportedImage` for 16-bit input rather than converting.

## 3. Codec ids 1 and 2 and their mapping to the internal stream

`codec = 1` means "PKON2 X4 as promoted by N3", frozen as:

* planes in coding order: RGB(A) -> `G, R, B, (A)`; gray(+alpha) -> natural order. (`pkonfile::plane_channels`)
* reversible colour transform `SubG` when there are >= 3 planes: `R' = R-G+128`, `B' = B-G+128` (mod 256) on G, R, B; alpha untouched.
* `pkon2x::run_x` with `XCfg { inputs: [Ab, B, C, D], sign_ctx: true, bias: Off, ens: Off }`, driven by `BinEnc`/`BinDec`
  (LZMA-style binary range coder, 16-bit probabilities).

This configuration is written out in `pkonfile::codec1_cfg()` rather than calling the research default `x4_cfg()`. A unit
test asserts they are equal today; if the research default ever changes, that test fails and the change must either keep
codec 1 intact or allocate codec id 2. `tests/pkonfile.rs::codec1_payload_is_pinned` pins the payload bytes of a fixed image.

**Relation to the pre-existing stream.** The existing X4 stream has *no serialized header*: `pkon2::HEADER_BYTES = 11` is a
notional accounting constant, never written. Width, height, layout and depth were always supplied externally. The payload of a
`.pkon2` file is exactly that headerless range-coder output (`BinEnc::finish`), byte for byte; so
`payload_len + 11` equals the historical "production B" byte counts. The container header replaces the notional 11 bytes.

Independent-implementation note: the payload is *defined by* the reference algorithm in `src/pkon2.rs` + `src/pkon2x.rs`
(predictor, contexts, mixer, integer tables). There is no prose specification of that model beyond those sources; a
second implementation must reproduce them bit-exactly, including the `f64`-derived stretch/squash/rate tables built in
`pkon2x::tables()`.

### Codec id 2 (PKON2P): X4 plus predictor disagreement

`codec = 2` is codec 1 with exactly one additional mixer input and nothing else changed (same planes, `SubG`, MED prediction, contexts
`Ab,B,C,D`, sign contexts, binarization, range coder, tables). `pkonfile::codec2_cfg()` = `XCfg { inputs: [Ab, B, C, D, P], sign_ctx: true,
bias: Off, ens: Off }`. Version stays 1 and the header layout is unchanged; per section 5 a new model is a new codec id.

* Feature `P` (49 classes) = `qd(GAP - MED) * 7 + qd(GRAD - MED)`, where MED, GAP and GRAD are the predictions `pkon2::predict(Med|Grad)` and
  `pkon2::gap` of the current plane from already-decoded pixels (W, N, NW, NE, WW, NN, NNE; outside-image pixels substituted exactly as for MED),
  `GRAD = clamp(W + N - NW, 0, 255)` (`pkon2::predict(Pred::Grad)`), and
  `qd(d)` = 0 for d <= -9, 1 for -8..-3, 2 for -2..-1, 3 for 0, 4 for 1..2, 5 for 3..8, 6 for d >= 9.
* P is the 5th (last) input of every magnitude decision (context = P, probability table = per plane x P x node) and the 4th input of the sign decision.
  Its mixer weight starts at 0 (the other inputs keep their codec-1 initial weights 1.25/4 and 1.25/3), then adapts with the same rule as the others.
* Adaptive-state update order is unchanged: after each coded bit every input's probability and the mixer weights update.
* Decoders MUST implement both codecs. A codec-1 file is never reinterpreted as codec 2 (the stream differs, so a patched codec byte fails the pixel CRC).
* Compatibility: all files written before codec 2 (codec byte 1) decode unchanged; `tests/golden_codec1/` holds 44 such files from the pre-change encoder.

### Codec id 3 (PKON2PJ, REL6): codec 2 plus a joint sign context

`codec = 3` is codec 2 with exactly one additional mixer input, at the sign decision only (magnitude decisions are unchanged). It is the default writer codec (`pkonfile::encode`); promoted after the production A/B in `results/20261027-rel6-production-ab/`.
`pkonfile::codec3_cfg()` = `XCfg { inputs: [Ab, B, C, D, P, J], sign_ctx: true, bias: Off, ens: Off }`.

* The sign decision mixes five inputs: the four codec-2 sign inputs plus a 5th with one bit model per plane x context, context =
  `((sgn(prev-plane z) * 27 + sgn(W) * 9 + sgn(N) * 3 + sgn(NE)) * 49 + P) * 4 + min(class - 1, 3)`, i.e. 3 x 27 x 49 x 4 = 15,876 contexts per plane.
  (`sgn` is 0 for z = 0, 1 for odd z, 2 for even nonzero z, as in the codec-2 sign contexts.)
* Its mixer weight starts at 0, its bit models start at p = 1/2, n = 0, and it updates in the same pass as the other inputs.
* Decoders MUST implement all three codecs. Codec 1 and 2 streams are unchanged.

## 4. Integrity

`crc32` = CRC-32/ISO-HDLC (reflected polynomial `0xEDB88320`, init and final XOR `0xFFFFFFFF`; the zlib/PNG CRC; check value
of ASCII `123456789` is `0xCBF43926`) over the **reconstructed pixel buffer** exactly as defined in section 2. It is stored in
the header (the encoder holds the whole image, so no trailer is needed, and the payload then ends the file). It detects
accidental corruption; it is not authentication and offers no protection against deliberate modification.

A second, independent check covers the payload: the range decoder must finish having read **exactly** `payload_len` bytes
(`BinDec::consumed() == payload_len`), otherwise `CorruptPayload`. This catches most damaged or mis-sized streams before the
pixel CRC and does not depend on pixel content.

Known property: the final 4 payload bytes are the range coder's flush; several of their bit patterns decode to identical
pixels. A flip there may therefore be accepted, always with pixel-identical output (tested). The CRC guarantees correct
pixels, not canonical file bytes; do not use file hashes of untrusted files as identity. Encoder output is deterministic.

## 5. Boundaries, trailing data, extension policy

* `file_len < 24` -> `Truncated` (or `BadMagic` if the prefix present already differs from the magic).
* `file_len < 24 + payload_len` -> `Truncated`. `file_len > 24 + payload_len` -> `TrailingBytes`. **Trailing, appended and
  concatenated data are rejected**; one file holds one image.
* `payload_len == 0`, or `width*height*channels / 32768 > payload_len`, -> `InvalidPayloadLength`. The bound follows from the
  probability clamp (every sample costs at least ~3.5e-4 bits, i.e. at most ~22,700 samples per payload byte; the densest
  measured stream, a flat 1024x1024 RGBA image, is ~15,800 samples/byte) so a tiny file cannot request unbounded decode work.
* Evolution: any change to header layout, field meaning, or reserved-bit use requires a new `version`. A new compression
  model requires a new `codec` id under the same `version` (decoders reject ids they do not implement). Reserved `flags`
  bits are all "must understand"; there is no optional-feature mechanism in v1 and none should be added without a concrete need.

## 6. Resource policy

`DEFAULT_MAX_DECODED_BYTES = 1 << 28` (256 MiB of reconstructed pixels, `pkonfile.rs`); `decode_with_limit` / `parse_header_with_limit`
override it. The decoder computes `width * height * channels` with checked `u64` arithmetic, requires it to fit the limit
and `usize`, and performs all header checks **before** allocating anything; payload length is checked against the actual
input slice, never trusted for allocation. Peak decode memory is roughly 3-4x the decoded size. Malformed input yields a
`PkonError` and never a panic (exercised by a deterministic mutation test in debug and release builds).

## 7. API (`nanoscope::pkonfile`)

`encode(&Canonical) -> Result<Vec<u8>, PkonError>` (codec 3, `CODEC_CURRENT`); `encode_with_codec(&Canonical, codec)` (1, 2 or 3; other ids -> `UnsupportedCodec`); `decode(&[u8]) -> Result<Canonical, PkonError>` (metadata is in
`.spec`); `decode_with_limit`; `parse_header` / `parse_header_with_limit` -> `Header { spec, codec, payload_len, crc32 }`
(header-only validation, no payload access); `encode_payload` / `decode_payload` (bare codec-1 stream); `encode_payload_codec` (panics on an unassigned id) / `try_encode_payload_codec` / `decode_payload_codec` (any assigned codec; unassigned id -> `UnsupportedCodec`); `crc32`.

## 8. Version vs. codec, dispatch, and evolution rules (normative)

**Container version** (byte 4) governs only the outer structure: header layout, field semantics, integrity rules, how
reserved bits are interpreted. **Codec id** (byte 6) governs only how the payload bytes reconstruct pixels. They are
independent: a new codec does not change `version`; `version` changes only when the header/integrity/extension rules
change incompatibly.

Reader algorithm: (1) check magic; (2) read `version`; if not 1 stop with `UnsupportedVersion` *without interpreting any
other byte* (a v2 header may be laid out differently; a v1 reader must never reuse v1 offsets for it); (3) under v1,
`flags != 0` -> `ReservedFlags`; (4) dispatch on `codec` through the single function `pkonfile::cfg_for`, which lists every
assigned id explicitly and has no fallback arm: unknown -> `UnsupportedCodec`, never another codec.

Assigned codec ids (permanent; an id is never reassigned or repurposed): 1 = X4, 2 = X4P, 3 = X4PJ/REL6 (current writer).
`0` and `4..=255` are unassigned; they are rejected by v1 readers today. New codecs take the next unused id, with a frozen
`codecN_cfg()` (never a call to a research default), one arm in `cfg_for`, a pinned-payload test, and a round-trip test over
all four layouts. Ids are not reserved for private use.

Future container versions: use a new `version` byte value (2..), keeping magic and byte 4 in place so that every reader can
discover the version first. Old v1 files stay readable by new readers that retain the v1 parser; old readers reject v2
files with `UnsupportedVersion`. Optional metadata/extensions are **not** possible in v1 (no length field, no
skippable region, flags are all must-understand); they are reserved for a future container version.

Integrity limits, stated plainly: there is no header checksum. The only checksum is the pixel CRC-32, which covers the
reconstructed pixels, not the header bytes and not the payload bytes directly. Header corruption is caught by exact checks
(magic, version, flags, codec, format), by size/length validation, and, for width/height/format/codec changes that remain
structurally valid, by the stream-end check or the pixel CRC (not by a header CRC). `payload_len` is not covered by any
checksum but must equal the actual remaining file length exactly. Adding a header or payload checksum would be a new
container version.

Validation order (v1): length >= 4 magic prefix -> header length (24) -> magic -> version -> flags -> codec -> pixel format ->
dimensions non-zero -> checked `w*h*channels` (u64) vs. decode limit and `usize` -> `payload_len` bounds -> (in `decode`)
file length vs. `24 + payload_len` (truncated / trailing) -> allocate and decode -> stream-end check -> pixel CRC.

## 9. Compatibility matrix (verified by tests in `tests/pkonfile.rs`, `tests/pkonfile_codec3.rs`)

| Container version | Codec id | Read | Write | Default writer | Pinned / golden coverage | Exact reconstruction tested |
|---:|---:|:--:|:--:|:--:|---|---|
| 1 | 1 (X4) | yes | yes (`encode_with_codec`) | no | 44 golden files in `tests/golden_codec1/` (decode + byte-exact re-encode); pinned payload | gray, gray+alpha, RGB, RGBA |
| 1 | 2 (X4P) | yes | yes (`encode_with_codec`) | no | pinned payload; no golden files | all four layouts (round trip) |
| 1 | 3 (X4PJ, REL6) | yes | yes (`encode`) | **yes** | payload equals independent research arm | all four layouts (round trip) |
| 1 | 0, 4..=255 | rejected (`UnsupportedCodec`) | rejected | - | tested | - |
| != 1 | any | rejected (`UnsupportedVersion`) | - | - | tested | - |

Scope: compatibility is verified against the fixtures and pinned payloads in the repository only; no claim is made about
files from other tools or history not represented by those fixtures. Codec 2 and 3 have no files archived from earlier releases.
