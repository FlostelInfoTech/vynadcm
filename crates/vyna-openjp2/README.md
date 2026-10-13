# `vyna-openjp2` — VynaDCM fork of `openjp2` 0.6.1

`openjp2` is a mechanical C-to-Rust port of OpenJPEG
(BSD-2-Clause, see `LICENSE`).
Upstream: <https://crates.io/crates/openjp2/0.6.1>,
repository <https://github.com/Neopallium/openjp2> (`openjp2-rs`).

This fork exists for one soundness fix (below) that has not landed
upstream. If upstream fixes it, this fork should be retired in favor
of the upstream release. The library target is renamed to
`vyna_openjp2`; the API is otherwise identical to `openjp2` 0.6.1.

## The patch (marked `VYNA-PATCH` in `src/tcd.rs`)

Two `free()` calls in the C original became unconditional Rust
`dealloc()` calls. C `free(NULL)` is a no-op; Rust `dealloc(null, _)`
is instant undefined behaviour and aborts debug builds
(`NonNull::new_unchecked` precondition).

Any codestream with tag-tree-skipped code blocks — ordinary valid
input, e.g. flat image regions — aborts the whole process when the
decoder drops. Observed via `vyna-dcm-codec` round-trip tests,
decoding both our own and `pure_jpeg2k` encoder output.

- `opj_tcd_code_block_dec_allocate` (~line 1432): null-guard the
  `decoded_data` deallocation on the re-allocation path.
- `opj_tcd_code_block_dec_deallocate` (~line 2510): null-guard the
  `decoded_data` deallocation on the teardown path.

Both restore exact C `free()` semantics. No behaviour change when the
pointer is non-null.

Incidental (also `VYNA-PATCH`-marked): `types.rs` names one elided
lifetime; zero behaviour change.

## Audit notes (2026-10-11, reviewer: VynaDCM P0.2)

- All 15 raw `dealloc()` call sites inspected: `t1.rs` sites were
  already null-guarded; `dwt.rs` sites are same-function
  alloc/dealloc pairs.
- `opj_free` (172 calls) delegates to C `free()` and is null-safe.
- Caller-side workarounds in `vyna-dcm-codec` (not patched here):
  `tile_size_on` is always set (the port's non-tiling path leaves the
  tile grid empty) and `image.x1/y1` are set after
  `opj_image::create`.

## Features: `std` is required

Upstream ships `std` / `file-io` features and `no_std` scaffolding
(`extern crate alloc`, cfg'd imports), but the port was never
finished: `stream.rs` is built on `std::io` buffering, `t1.rs` keeps
its scratch state in a `thread_local!`, and `jp2.rs` uses byteorder
over `std::io`. Upstream 0.6.1 fails `--no-default-features` with
~28 errors, and this fork fails identically by design: completing
the port would be a large, risky divergence in foreign code for a
configuration VynaDCM never builds (the codec uses default
features).

Instead of failing with only the confusing errors, building
without `std` leads with one explicit `compile_error!` in
`src/lib.rs` (marked `VYNA-PATCH`). `file-io` without `std` stops
there too. If upstream ever completes `no_std`, drop that block
(and this section) during the rebase and add
`--no-default-features` to CI.

## Re-basing onto a new upstream

1. Copy the new `openjp2-<version>` crate sources over `src/`.
2. Re-apply the `VYNA-PATCH` guards (check whether upstream fixed them).
3. Run `cargo test -p vyna-dcm-codec` from the workspace root — the
   `jpeg_2000` suite covers flat, gradient and foreign-encoder streams.
