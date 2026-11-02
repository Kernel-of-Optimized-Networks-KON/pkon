# PKON container, version 2 (`.pkon`)

Normative for `src/pkon.rs`. MUST / MUST NOT as in RFC 2119. All multi-byte integers are **little-endian, unsigned**.
A file is exactly: `header (32 bytes) || codec metadata (meta_len bytes) || payload (payload_len bytes)`.

## 0. Identity, versions, and how they relate

| Concept | Where | Meaning |
|---|---|---|
| Extension | filename | `.pkon` for new files. Legacy `.pkon2` files (container v1) stay readable; the extension is not authoritative, the bytes are. |
| Magic | bytes 0..4 | `89 50 4B 32`, unchanged since v1 (the historical "PK2"). Kept on purpose: every existing reader finds the version byte at offset 4 and rejects v2 with `UnsupportedVersion` instead of `BadMagic`, and v2 readers dispatch v1 files by the same byte. |
| **Container version** | byte 4 | Outer structure only: header layout, metadata block framing, integrity rules, flag semantics. `1` = `docs/pkon2-format-v1.md` (24-byte header, `pkonfile`). `2` = this document. |
| **Codec id** | byte 6 | The compression algorithm *and its bitstream variant*. Permanent; an id is never reassigned. A change that alters decoding or reconstruction takes a **new id**. |
| **Metadata version** | byte 7 | How the codec's metadata block (and the shared header fields it relies on) is serialized and interpreted. Per codec; `1` for every codec today. |
| Stage versions | inside LISA metadata | LISA records which Stage 1 mode, Stage 3 rule and Stage 4 coding version produced the payload. The decoder implements an explicit list and rejects everything else. |

Compatibility rules (normative):

1. A reader MUST check magic, then the container version, **before interpreting any other byte**. A version it does not
   implement -> `UnsupportedVersion`.
2. Under v2, `flags != 0` -> `ReservedFlags`. All flag bits are mandatory-to-understand; none are assigned. Optional
   features are not expressible in v2 (they would need a new container version).
3. Unknown codec id -> `UnsupportedCodec`. Unknown metadata version for a known codec -> `UnsupportedMetaVersion`. A known
   codec whose metadata names a variant the decoder lacks -> `UnsupportedCodecVersion { field, value }`. A stream is never
   decoded "as the nearest" codec.
4. A change that affects decoding or reconstruction MUST NOT reuse an assigned codec id, metadata version, or an accepted
   metadata value with different meaning. Adding a *new* accepted value (e.g. a second Stage 4 coding version) is a
   compatible extension only if old files keep their meaning; readers that predate it reject it explicitly.
5. A bug fix that leaves every produced and accepted bitstream byte-identical needs no version change.
6. Changes to header layout, header field meaning, integrity coverage, or flag semantics require container version 3+.
   v1 and v2 parsers stay available.

## 1. Header (v2, 32 bytes)

| Offset | Size | Field | Valid values / validation |
|---:|---:|---|---|
| 0 | 4 | `magic` | `89 50 4B 32`; else `BadMagic` |
| 4 | 1 | `version` | `2` |
| 5 | 1 | `flags` | `0` |
| 6 | 1 | `codec` | `1`, `2`, `3` (retired ELLIE-X4), `4` (LISA), `6` (ELLIE) or `7` (ULISA) |
| 7 | 1 | `meta_version` | `1` (LISA/ULISA: `1` without lattice, `2` with lattice) |
| 8 | 1 | `bit_depth` | X4: `8`. LISA, ULISA: `1..=16` |
| 9 | 1 | `channels` | X4: `1..=4` (layout). LISA, ULISA: `1` |
| 10 | 2 | `meta_len` | length of the codec metadata block |
| 12 | 4 | `width` | `>= 1` |
| 16 | 4 | `height` | `>= 1` |
| 20 | 4 | `payload_len` | `>= 1`; exact payload length |
| 24 | 4 | `pixel_crc32` | CRC-32 of the reconstructed samples (section 4) |
| 28 | 4 | `header_crc32` | CRC-32 of bytes `[0, 28)` followed by the `meta_len` metadata bytes |

The metadata block starts at offset 32. The payload starts at `32 + meta_len` and ends at `32 + meta_len + payload_len`.
No padding, no alignment, no trailing data.

Validation order: magic -> version -> flags -> length >= 32 -> `meta_len` bounds (`Truncated`) -> **header checksum**
(`HeaderChecksumMismatch`) -> codec -> meta version -> bit depth / channels -> dimensions -> checked sample and decoded-byte
counts vs. the limit (`TooLarge`) -> payload bounds (`InvalidPayloadLength`) -> codec metadata (`InvalidMetaLength`,
`UnsupportedCodecVersion`, `InvalidMetadata`, CFA errors) -> (in `decode`) file length vs. `header + meta + payload`
(`Truncated` / `TrailingBytes`) -> decode -> exact payload consumption (`CorruptPayload`) -> pixel CRC
(`ChecksumMismatch`). All of this except the last three steps happens before any large allocation or decoding work.

## 2. Codecs

| Codec id | Algorithm | Produces | Notes |
|---:|---|---|---|
| 1 | ELLIE-X4 | decoded 8-bit standard image | frozen config of v1 codec 1 |
| 2 | ELLIE-X4P | same | v1 codec 2 |
| 3 | ELLIE-X4PJ (REL6) | same | v1 codec 3, default writer for standard images |
| 4 | LISA RAW, production | `u16` samples + `ImageMetadata` | OR-128 rule 2, Stage 4 coding 1 |
| 5 | unassigned | | rejected (`UnsupportedCodec`) |
| 6 | ELLIE (Stages 1-4, `src/ellie.rs`) | 8/16-bit gray, gray+alpha, RGB, RGBA standard image | per-plane streams; explicit colour representation (section 2.3) |
| 7 | ULISA Bayer RAW (`src/lisa_frame.rs` frame + `ellie::FlankCodec`) | `u16` samples + `ImageMetadata` | 2x2 Bayer only; one payload for four parity planes (section 2.4) |
| 0, 8..=255 | unassigned | | rejected (`UnsupportedCodec`) |

The codec ids 1..=3 are the same ids, with the same meaning and **byte-identical payloads**, as container v1. The payload
is the headerless range-coder stream defined in `docs/pkon2-format-v1.md` section 3 and produced by `pkonfile`; it does not
depend on the container version. Entropy coding is internal to each codec: both families use the shared binary range coder
(`elliex::BinEnc` / `BinDec`) with their own models, and the container neither names nor configures it.

### 2.1 Standard images (codec 1..=3)

Metadata block: empty (`meta_len` MUST be 0). `meta_version` 1. `bit_depth` MUST be 8. `channels` gives the layout:
1 gray `Y`, 2 gray+alpha `Y,A`, 3 `R,G,B`, 4 `R,G,B,A` (straight alpha). Samples are row-major, top row first, channels
interleaved, no padding, exactly as `canon::Canonical`.

The codecs compress the **decoded pixel samples**. For a JPEG or PNG source, the original file bytes, EXIF, ICC profile and other
source metadata are **not** stored and cannot be recovered; a decoded JPEG round-trips to the decoded pixels, not to the
JPEG. 16-bit input is not representable in codecs 1..=3.

Plausibility bound: `width*height*channels / 32768 <= payload_len` (the v1 bound).

### 2.2 LISA RAW (codec 4)

The payload is the Stage 4 range-coder output (`Stage4Output::payload`), unchanged. The decoder pipeline is exactly the
in-memory one: `decode_stage4` -> `restore_stage1_with_mode` -> `decode_stage1`. `width`, `height`, `bit_depth` come from the
header (`channels` = 1: a RAW file is one plane of mosaic or monochrome samples).

Metadata version 1 (`meta_version` = 1), at offset 32:

| Offset | Size | Field | Values |
|---:|---:|---|---|
| 0 | 1 | `stage1_mode` | `0` mosaic Stage 1 (`Stage1Decorrelation`); `1` Bayer-planar Stage 1 (`Stage1BayerPlanar`, requires layout 3) |
| 1 | 1 | `sample_layout` | `0` Unspecified, `1` Monochrome, `2` Linear, `3` BayerCfa, `4` OtherCfa |
| 2 | 1 | `stage3_rule_version` | `2` (OR-128). Others rejected |
| 3 | 1 | `stage4_coding_version` | `1` (counted estimator). Others rejected |
| 4 | 2 | `cfa_width` | only if layout is 3 or 4 |
| 6 | 2 | `cfa_height` | only if layout is 3 or 4 |
| 8 | 2 | `cfa_origin_x` | only if layout is 3 or 4 |
| 10 | 2 | `cfa_origin_y` | only if layout is 3 or 4 |
| 12 | `cfa_width*cfa_height` | `cfa_entries` | row-major `ColorFilter` codes: 0 R, 1 G, 2 B, 3 C, 4 M, 5 Y, 6 W |

`meta_len` MUST equal exactly 4 (layouts 0..=2) or `12 + cfa_width*cfa_height` (layouts 3, 4). Layout 3 additionally must
satisfy the Bayer rule (2x2, one R, one B, two diagonal G); the CFA pattern is stored exactly as supplied (never
normalized), including origin. Derived, not stored: Stage 2 organization (Bayer planes for layout 3, identity otherwise),
residual count, model count, plane geometry. Stage 3 and Stage 4 records are rebuilt from the two version bytes by
`Stage4Output::from_parts`, and `decode_stage4` validates them like an in-memory stream.

Not serialized (rejected, not guessed): the legacy and learned-tree Stage 3 rules (versions 1, 3, 4, 5) and the
experimental reward-penalty Stage 4 estimator (coding version 2).

An encoder that is asked for Bayer-planar Stage 1 on non-Bayer metadata writes `stage1_mode` 0, which is what was applied.

Plausibility bound: `width*height / 16384 <= payload_len`. Derivation: every sample costs at least one binary decision, and
the probability clamp (`[32, 65503]/65536`) bounds a decision's cost below by 7.3e-4 bits, so at most ~11,000 samples fit
in one payload byte; a flat 2048x2048 image measures 11,245 samples/byte (`tests/pkon_container.rs`). 16,384 keeps a
power-of-two margin and prevents a tiny file from requesting unbounded decode work.

### 2.3 ELLIE (codec 6)

Header: `bit_depth` 8 or 16, `channels` 1..=4 (the interleaved `Canonical` layout), `pixel_crc32` over the decoded bytes in
`Canonical` order (16-bit little-endian). `payload_len` is the sum of the plane stream lengths. Codecs 1..=3 (retired) are
unrelated and stay rejected; id 5 stays unassigned.

Metadata version 1, at offset 32, `2 + 5 * channels` bytes:

| Offset | Size | Field | Values |
|---:|---:|---|---|
| 0 | 1 | `sample_layout` | `0` Gray, `1` GrayAlpha, `2` Rgb, `3` Rgba (must agree with `channels`) |
| 1 | 1 | `representation` | `0` untransformed (planes = channels); `1` YCoCg-R (8-bit Rgb/Rgba only) |
| 2 + 5k | 1 | `plane_bits[k]` | coding bit depth of plane k; must equal the value the representation implies |
| 3 + 5k | 4 | `plane_len[k]` | encoded length of plane k's stream, > 0; all lengths sum to `payload_len` |

Payload: the plane streams back to back. Each plane is an independent Stage 1-4 stream (Stage 2 and Stage 3 restart per plane;
the range coder is finished per plane).

Representation `1` (YCoCg-R): `Co = R - B; t = B + (Co >> 1); Cg = G - t; Y = t + (Cg >> 1)`, inverse
`t = Y - (Cg >> 1); G = Cg + t; B = t - (Co >> 1); R = Co + B` (signed integers, arithmetic shifts). Plane 0 = Y (8-bit),
planes 1, 2 = Co + 255, Cg + 255 (9-bit, `0..=510`); an alpha plane 3 is coded unchanged at 8 bits. A decoded value outside
its domain, or an inverse result outside 0..=255, is `CorruptPayload`. Representation `0` codes every channel at `bit_depth`.

The reference encoder (`pkon::encode_standard`) tries both representations for 8-bit RGB/RGBA and writes the smaller complete
file (ties: untransformed); `encode_standard_with` forces one.

### 2.4 ULISA Bayer RAW (codec 7)

LISA supplies the sensor frame, ELLIE codes it. Header: `bit_depth` is the sample depth (`1..=16`), `channels` 1, `pixel_crc32`
as for LISA. Metadata version 1, at offset 32:

| Offset | Size | Field | Values |
|---:|---:|---|---|
| 0 | 1 | `model_profile` | `1` = ELLIE `Bc255S11Tree` per parity plane plus the cross-plane flank input |
| 1 | 1 | `cross_plane_context_version` | `1` (`ellie::FLANK_CTX_VERSION`) |
| 2 | 1 | `sample_layout` | must be `3` BayerCfa |
| 3 | 8 + w*h | CFA block | identical to LISA's (section 2.2): `width u16, height u16, origin_x u16, origin_y u16`, then colour codes; must be a valid 2x2 RGB Bayer arrangement |

Metadata version 2 appends LISA's 5-byte L1 lattice block (`origin u16, g u16, coding_bit_depth u8`, same validation, `g >= 2`). Width
and height must both be `>= 2`. Unknown profile/context version/layout, a non-2x2 CFA, or a lattice block that disagrees with the
metadata version is rejected before any payload work (`UnsupportedCodecVersion`, `InvalidMetaLength`, `InvalidMetadata`).

Coded samples are the mosaic after L1 (when a lattice is present). They are split into the four image-parity planes (plane `k`
holds `(x, y)` with `x & 1 = k & 1`, `y & 1 = k >> 1`; sizes `((w + 1 - ox) / 2, (h + 1 - oy) / 2)`) and coded in order 0, 1, 2, 3, raster
order within a plane, as one range-coder stream. Each plane restarts ELLIE's Stage 1/Stage 3 state; the flank state is shared by
the four planes. The flank context of plane `k` reads only residuals of planes `< k`, at the mosaic-flank sites: plane `k ^ 1`
(horizontal: columns `x, x+1` if `k` is odd, else `x-1, x`, same row) and plane `k ^ 2` (vertical, likewise); plane `k ^ 3` is not used. Their
signed sum (stored residuals clamped to `i16`, coordinates clamped to the source plane) gives `(sign * 10 + min(bitlen(|sum|), 9)) * 7 + qa`.
The model is integer-only; see the `ellie.rs` comment "Cross-plane flank stage" for the exact arithmetic.

Samples-per-payload-byte bound: every sample codes at least its zero-flag decision with `P(bit = 1)` clamped to `[32, 65503] / 65536`
(`ellie::COD_P_MIN/MAX`), so a decision costs at least `-log2(65503 / 65536)` = 7.27e-4 bits: at most 11,010 samples per byte.
The header check rejects `samples / 16384 > payload_len + 5` (the 5 bytes absorb the coder's termination, which can drop bytes below
the information content); `tests/ulisa_codec7.rs` checks flat images from 2x2 to 1024x1024 against it.

Engine selection (`nanoscope::ulisa`): `Engine::Ulisa` encodes with codec 7, or, for a mosaic outside the 2x2 Bayer frame, falls back
to LISA's RAW path (codec 4) and reports it (`Encoded::fallback`); `Fallback::None` makes that an error.

## 3. Resource policy

`DEFAULT_MAX_DECODED_BYTES = 1 << 28` bounds the reconstructed sample buffer: `width*height*channels*bytes_per_sample`, with
`bytes_per_sample = 1` for `bit_depth <= 8`, else `2`, using checked `u64` arithmetic, also required to fit `usize`.
`decode_with_limit` / `parse_header_with_limit` override it. LISA's transient decode working set is roughly 16 bytes per
sample on top of the output (residuals, contexts, rank tables), so memory-constrained callers should lower the limit
accordingly. Header, codec metadata and payload-length checks run before any allocation proportional to the image;
`payload_len` is checked against the actual input length and never trusted for allocation. LISA additionally enforces its
own geometry limits (`lisa::MAX_DIM`, sample count). Malformed input returns `ContainerError`, never panics
(`damaged_payloads_never_panic...`, truncation at every prefix length, and header-byte flips are tested).

## 4. Integrity

* `pixel_crc32`: CRC-32/ISO-HDLC (reflected polynomial `0xEDB88320`, init and final XOR `0xFFFFFFFF`; zlib/PNG CRC; check
  value of ASCII `123456789` is `0xCBF43926`) over the **reconstructed sample byte stream**: row-major, channels
  interleaved, each sample `bytes_per_sample(bit_depth)` bytes, **little-endian**. For 8-bit standard images this is
  identical to the v1 CRC (same bytes). For RAW at `bit_depth > 8`, each `u16` is two bytes, low byte first; at
  `bit_depth <= 8` one byte.
* `header_crc32`: same CRC over header bytes `[0, 28)` (including `pixel_crc32`) followed by the metadata block. Only the
  checksum field itself is excluded, so every other header and metadata byte is covered. Magic, version and flags are
  additionally checked exactly, before the checksum.
* Exact payload consumption: the range decoder must end exactly at `payload_len` (`CorruptPayload`).
* Trailing and truncated data are rejected; one file holds one image.
* Neither checksum is authentication. As in v1, the last range-coder flush bytes can have several pixel-identical
  encodings, so the CRC guarantees pixels, not canonical bytes; encoder output is deterministic.

## 5. API (`nanoscope::pkon`)

`encode_standard(&Canonical)` (codec 3) / `encode_standard_with_codec`; `encode_raw(&[u16], w, h, bit_depth, ImageMetadata)` /
`encode_raw_with_mode(.., Stage1Mode)`; `encode_ulisa` / `encode_ulisa_report` (codec 7); `decode(&[u8]) -> Result<Decoded, ContainerError>` where
`Decoded::Standard(Canonical)` or `Decoded::Raw(RawImage { samples, metadata, bit_depth })`; `decode_with_limit`;
`parse_header` / `parse_header_with_limit` (header + metadata validation, no payload access); `container_version`.
`decode` reads container v1 (`.pkon2`) through the unchanged `pkonfile::decode_with_limit` and returns it as `Standard`.
`pkonfile` remains the v1 reader and writer.

## 6. Examples (generated by `tests/pkon_container.rs`, pinned in `tests/golden_pkon/`)

### Standard image: `std_rgb_5x3.pkon`, 5x3 RGB, codec 3 (73 bytes)

```
89 50 4b 32  magic
02           container version 2
00           flags
03           codec 3 (X4PJ)
01           metadata version 1
08           bit depth 8
03           channels 3 (RGB)
00 00        meta_len 0
05 00 00 00  width 5
03 00 00 00  height 3
29 00 00 00  payload_len 41
a5 3b f5 70  pixel_crc32 0x70f53ba5
87 47 17 70  header_crc32 0x70174787
-- payload (41 bytes): 2b 02 28 59 95 61 fa e8 ...
```

### LISA RAW: `lisa_grbg_16x10_12bit.pkon`, 16x10, 12-bit GRBG Bayer (264 bytes)

```
89 50 4b 32  magic
02 00        container version 2, flags 0
04           codec 4 (LISA)
01           metadata version 1
0c           bit depth 12
01           channels 1
10 00        meta_len 16
10 00 00 00  width 16
0a 00 00 00  height 10
d8 00 00 00  payload_len 216
84 ab 6c ba  pixel_crc32 (CRC of 160 samples x 2 bytes, little-endian)
44 1c 98 dd  header_crc32
-- metadata (16 bytes):
01           stage1_mode 1 (Bayer planar)
03           sample_layout 3 (BayerCfa)
02           stage3_rule_version 2 (OR-128)
01           stage4_coding_version 1
02 00 02 00  cfa 2x2
00 00 00 00  origin (0,0)
01 00 02 01  entries G R B G
-- payload (216 bytes): 00 10 de 01 17 3a 05 28 ...
```

`lisa_othercfa_14x9_14bit.pkon` (293 bytes) shows a 6x6 `OtherCfa` pattern with origin (2,5) and Stage 1 mosaic.

## 7. Migration from `.pkon2` (container v1)

* v1 files are unchanged and decode through `pkon::decode`. `tests/golden_codec1/` (44 files) is checked through both
  `pkonfile::decode` and `pkon::decode`.
* New files are written as v2 by `pkon::encode_standard` / `pkon::encode_raw`; `pkonfile::encode` still writes v1 for
  tooling that needs it (golden fixtures, historical benchmarks).
* The v1 -> v2 differences for standard images: `bit_depth` and `channels` replace `pixel_format`; the header grows from 24
  to 32 bytes; a header checksum and a metadata block frame are added. Payload bytes and the pixel CRC are identical for the
  same image and codec (`standard_payload_is_byte_identical_to_the_v1_payload`).
