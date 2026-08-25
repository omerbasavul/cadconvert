//! The allocator the converter's binaries run on.
//!
//! Two crates produce something a user runs — the command line and the C ABI —
//! and both want the same allocator and the same two options. A library must
//! not choose either for its caller, so neither choice lives in one; this crate
//! is where they both live instead.
//!
//! # Why not the system allocator
//!
//! The Parasolid reader is millions of small short-lived allocations and the
//! tessellator is a handful of megabyte buffers. The converter frees a body's
//! boundary representation — thousands of small blocks — and then asks for a
//! mesh buffer; on macOS the system allocator cannot put the second inside the
//! first, and 42 MB of released memory moved the process peak by nothing at
//! all. Under mimalloc the same release is worth 34 MB.
//!
//! Measured on the pilot assembly, peak memory footprint, threads pinned, the
//! binaries run alternately, byte-identical output throughout:
//!
//! ```text
//!                     system      mimalloc
//!   Parasolid   291 MB, 31.9 s   258 MB, 22.4 s
//!   STEP        280 MB, 12.1 s   280 MB, 10.0 s
//! ```

pub use mimalloc::MiMalloc;

// ── The dial this does not turn, and why ────────────────────────────────────
//
// mimalloc holds a freed page before returning it to the kernel — a full
// second in the version built here, and arena purges on a multiple of that.
// A held page is still the process's, so it counts in its footprint. On the
// pilot, reading alone peaked at 148 MB while the data alive at that moment
// came to 114, and the whole 34 MB difference was pages already freed.
//
// Two options turn that off, and in the environment they work:
//
//     MIMALLOC_PURGE_DELAY=0 MIMALLOC_ARENA_EAGER_COMMIT=0
//
//     Parasolid   258 MB, 22.6 s  ->  204 MB, 26.0 s
//     STEP        270 MB, 10.3 s  ->  219 MB, 10.6 s
//
// Three ways of setting them from inside the program were tried and none is
// here, each for a measured reason:
//
//   * `mi_option_set` at the top of `main` is **too late**. mimalloc reads its
//     options when it initialises, and the Rust runtime has allocated before
//     `main` runs. The same binary: 258 MB setting them itself, 205 MB with
//     the identical values in the environment.
//
//   * `mi_collect(true)` between the reader and the tessellator, and again
//     before the writer — the two moments where tens of megabytes stop being
//     needed. Six interleaved pairs: 209.5 MB without, 211 MB with. Nothing.
//
//   * Both of the above cost an `unsafe` FFI dependency on the option ids,
//     which are an unnamed C enum whose numbering has moved between releases
//     — and whose documented defaults moved too: `purge_delay` is 10 in v2's
//     header and 1000 in the v3 this builds.
//
// One caution for anyone measuring this: the purge delay is a wall clock, so
// **a slower run purges more and peaks lower**. The same binary on the same
// file read 257 MB at 22 s and 209 MB at 31 s, the difference being nothing
// but what else the machine was doing. Interleave the two binaries and take
// medians, or the load will answer the question instead of the code.

// A third attempt belongs with the two above, because it was the most
// promising and it failed hardest. `mi_collect(true)` *between conversions* —
// where, unlike between stages, nothing at all is live — was exported through
// the C ABI for a host to call when a request finished. Measured in a .NET
// process converting the STEP five times over, held memory between
// conversions went from a median of 471 MB to 583, and stopped moving: the
// collect walks every heap and brings the pages it visits back into the
// working set faster than it gives any up. Peak went 537 -> 583 with it.
//
// So: the allocator returns memory on its own schedule and nothing this
// program does from the inside changes that. What a host can rely on is that it
// *does* come back — the same five conversions settle from 400 MB to 203
// twenty seconds after the last, against 41 before the first — and that it
// plateaus rather than climbing: eight conversions in one process gave 328,
// 230, 260, 282, 272, 344, 253, 422 MB, a series with no trend in it.
