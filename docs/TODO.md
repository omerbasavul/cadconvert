# xt-parser TODO

## Current Status

- **1000 ABC models tested**: 1000/1000 parse OK (100%), 947/1000 STEP match (94.7%)
- **6 reference models**: 6/6 perfect STEP match
- **53 STEP mismatches**: almost all XT > STEP (extra sheet/wire bodies in multi-file XT)

## Medium Priority

### STEP cross-validation at scale

We have 10,000 STEP files and 1,000 XT models. Running validation on all 1,000 would give better coverage and find edge cases. Automate with a script that produces a summary table.

### Wire/sheet body support in build.rs

`build_one_body` handles solid bodies well but wire (type=12) and sheet (type=7) bodies may have different topology chains. The 4 "clean mismatches" (XT > STEP face counts) may be from sheet bodies that STEP doesn't export.

### Partition support

The Z1 record contains `partition_count` (separate from the preamble's `entity_count`). Files with `partition_count > 0` may have per-entity partition indices that we're not reading. Need to verify with Ghidra RE whether partition indices are written after each entity.

## Low Priority

### Geometry extraction accuracy

Validate NURBS control points, knot vectors, and analytic geometry parameters against STEP equivalents. Current build.rs extracts geometry but values haven't been cross-checked beyond face/edge counts.

### Integration with solverang-cad

The xt-kernel-bridge from the monorepo dependency graph connects xt-parser to the geometric kernel. Once parsing is solid, build this bridge.

### Assembly/instance support

Types 10 (ASSEMBLY) and 11 (INSTANCE) with TRANSFORM (type 100) are parsed but not built into the typed IR. Multi-body assemblies with transforms would need this.

## Resolved

- [x] Path B variable-length entity detection (INTERSECTION_DATA type 204)
- [x] BODY PS30+ field indices (34-field annotated layout)
- [x] NURBS_SURF field index mapping (sch_13006 positions)
- [x] Vector/Box/Interval `?` sentinel (single `?` fills all components, confirmed via Ghidra RE)
- [x] Float `?` sentinel (consumes only `?` byte, leaves digits for next field)
- [x] ATTRIBUTE variable-length schema (7 fixed + variable ptr array)
- [x] ATTRIB_DEF variable-length schema (26 fixed + variable uint array, callbacks transmit=0)
- [x] INT_VALUES VarType fix (d→int32, was i16 causing overflow)
- [x] 1000/1000 ABC models parse, 947/1000 STEP match
