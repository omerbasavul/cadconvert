# xt-winnow — Parasolid PS30+ XT Text Format Parser

Winnow-based parser for the Parasolid `.x_t` compact transmit format (PS 30+). Produces B-Rep topology (bodies, shells, faces, loops, fins, edges, vertices) and geometry (planes, cylinders, cones, spheres, tori, NURBS). Cross-validated against STEP ground truth from the ABC dataset.

## Quick Reference

```sh
cargo test                                      # unit tests
cargo run --release -p xt-parser --example parse_xt -- file.x_t        # parse + print topology
cargo run --release -p xt-parser --example validate -- dir/            # batch stats
cargo run -p xt-parser --example dump_types -- file.x_t      # entity type breakdown
```

## Conventions

- **Rust Edition 2024 / 1.85+**.
- **Fail-fast on unknowns.** Unknown entity types or field codes are parse errors, never silently skipped.
- **No placeholders or stubs.** Implement fully or don't implement at all.
- **Error context mandatory.** Every fallible op chains `.context()` (anyhow).

---

The Parasolid **XT format specification, schema system, Ghidra RE notes,
validation workflow, debug loop, reference material and known issues** live in
`crates/xt-parser/CLAUDE.md` — loaded automatically when working under that
crate. The other crates (`cad-step`, `cad-ir`, `cad-tess`, `cad-export`) are
documented by their own rustdoc.
