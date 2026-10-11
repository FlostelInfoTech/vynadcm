# Vendored `openjp2` 0.6.1 — VynaDCM patch record

Source: <https://crates.io/crates/openjp2/0.6.1> (BSD-2-Clause, see `LICENSE`).
Upstream repository: <https://github.com/Neopallium/openjp2> (`openjp2-rs`).

## Why vendored

`openjp2` is a mechanical C-to-Rust port of OpenJPEG. Two `free()` calls in
the C original became unconditional Rust `dealloc()` calls. C `free(NULL)`
is a no-op; Rust `dealloc(null, _)` is instant undefined behaviour and
aborts debug builds (`NonNull::new_unchecked` precondition).

Any codestream with tag-tree-skipped code blocks — ordinary valid input,
e.g. flat image regions — aborts the whole process when the decoder drops.
Observed via `vyna-dcm-codec` round-trip tests, decoding both our own and
`pure_jpeg2k` encoder output.

## The patch (marked `VYNA-PATCH` in `src/tcd.rs`)

- `opj_tcd_code_block_dec_allocate` (~line 1432): null-guard the
  `decoded_data` deallocation on the re-allocation path.
- `opj_tcd_code_block_dec_deallocate` (~line 2510): null-guard the
  `decoded_data` deallocation on the teardown path.

Both restore exact C `free()` semantics. No behaviour change when the
pointer is non-null.

Incidental (also `VYNA-PATCH`-marked): `types.rs` names one elided lifetime
so the workspace clippy gate stays warning-free; zero behaviour change.

## Audit notes (2026-10-11, reviewer: VynaDCM P0.2)

- All 15 raw `dealloc()` call sites inspected: `t1.rs` sites were already
  null-guarded; `dwt.rs` sites are same-function alloc/dealloc pairs.
- `opj_free` (172 calls) delegates to C `free()` and is null-safe.
- Caller-side workarounds in `vyna-dcm-codec` (not patched here):
  `tile_size_on` is always set (the port's non-tiling path leaves the tile
  grid empty) and `image.x1/y1` are set after `opj_image::create`.

## Re-vendoring

1. Copy the new `openjp2-<version>` crate directory over this one.
2. Re-apply the `VYNA-PATCH` guards (check whether upstream fixed them).
3. Run `cargo test -p vyna-dcm-codec` — the `jpeg_2000` suite covers flat,
   gradient and foreign-encoder streams.
