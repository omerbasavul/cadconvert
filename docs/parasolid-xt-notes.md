# Parasolid PS30+ XT text — a reader's record

> **The format reference.** Siemens publish the *Parasolid XT Format Reference*
> themselves; it is the authority on record layouts and field order. It is not
> in this repository — it is their document and not ours to redistribute — so
> fetch a copy and, if you want it beside these notes, drop it at
> `docs/xt-reference.md`, which is ignored. Everything below is our own
> reading: what the files actually contained, what that cost when we got it
> wrong, and what the measurements said afterwards.

Format knowledge for the `.x_t` compact transmit parser: everything below was
reverse-engineered or measured, none of it is derivable from the code.

## XT File Format Specification

### File Layout

```
Line 1:  **ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz**...   (charset validation)
Line 2:  **PARASOLID !"#$%&'()*+,-./:;<=>?@[\]^_`{|}~0123456789**...   (charset validation)
Line 3:  **PART1; MC=...; FRU=Parasolid 30.1.168; APPL=SolidWorks; FORMAT=text; ...
Line 4:  **PART2; SCH=SCH_3001168_30100; USFLD_SIZE=0;
Line 5:  **PART3;
Line 6:  **END_OF_HEADER***...     (padded to fixed width)
         <body — all tokens whitespace-delimited, newlines are just whitespace>
```

### PART1 Key-Value Pairs

| Key | Example | Meaning |
|-----|---------|---------|
| `FRU` | `Parasolid 30.1.168` | Frustrum (PS version string) |
| `APPL` | `SolidWorks` / `Onshape` | Writing application |
| `FORMAT` | `text` | Always "text" for X_T |
| `GUISE` | `transmit` | Always "transmit" |
| `DATE` | `2018-04-27T08:24:19 (UTC)` | ISO 8601 timestamp |

### Schema Key (PART2)

`SCH_{modeller_version}_{schema_version}` where modeller_version = major*100000 + minor*1000 + patch.

### T-line (first token after header)

```
T51 : TRANSMIT FILE created by modeller version 300116823 SCH_3001168_30100_13006
```
- `T` = text transmit guise
- `51` = internal format version (0x33)
- `300116823` = packed version: major*10000000 + minor*100000 + patch*1000 + build
- Schema key: `SCH_<modeller>_<schema>` with an **optional** third `_<base>` group

**The number of groups in the schema key selects the stream dialect** — see below.
The key's first group is itself a packed modeller version (`major*100000 + …`), and
its major (`20` for `SCH_2000000_200000`) is what selects the field layouts to read
with. It is unrelated to the `FRU=`/`SCH=` versions in the header: `500.081UB.x_t`
is written by PS 33.1 with header `SCH=SCH_3301260_33103`, but its T-line key is
`SCH_2000000_200000` — the V20 standard schema.

### Stream Dialects

Two dialects exist. `parse_tline()` reports which via `TLine::has_base_schema`.

**A — base-diff** (three-group key, e.g. `SCH_2401231_20000_13006`)

The third group names a base schema the file writes *diffs* against (V13's
sch_13006 in every file seen). Such files carry a two-int preamble followed by
per-type inline schema annotations in the entity stream:

```
<n_types> <n_secondary>          ← the preamble
<type_id> <n_fields> <annotation_chars…> Z <entity_idx> <fields…>
```

This is the ABC dataset's dialect and the one the parser was originally built for.

**B — standard-schema** (two-group key, e.g. `SCH_2000000_200000`)

The file's schema *is* the named schema. There is **no preamble and no inline
schema section at all** — the entity stream starts immediately after the T-line
and every entity's fields are read straight from the standard layout for that
schema version (`schema::standard_schema()`). Seen in newer SolidWorks exports
(`500.079UB.x_t`, `500.081UB.x_t`).

Feeding a dialect-B file through the dialect-A path fails instantly: the first two
entity-stream tokens get eaten as a preamble, and the next `type_id` is read as an
inline-schema field count.

### Schema Preamble (dialect A only)

Compact inline schema defining entity types present in this file. Format:
```
<n_types> <n_secondary> <annotation_chars><name_len> <type_name> <type_id> <flags> ...
```
See `~/cadatomic/solidworks/notes/xt/schema_preamble_text_parser.md` for definitive format.

`parse_schema_preamble()` reads only the two ints; annotations are parsed lazily by
the entity loop on each type's first occurrence.

### Z1 Record (entity list header)

```
Z1 <n_entities> <body_count> <partition_count> 0 0 0 0 0 0 <res_size> <res_linear> 0 <root_indices...>
```
- `n_entities` = total entities in file
- `res_size` = size resolution (typically 1e3)
- `res_linear` = linear tolerance (typically 1e-8)
- Root indices = BODY, SHELL, REGION references

> **This section looks wrong and the code has never implemented it.** In
> `500.076.x_t` the stream reads `… schema_embedding_map 82 0 Z1 517 2 3 0 0 0 0
> 1e3 1e-8 …`, and the parser handles it as: `Z` = the annotation-list terminator,
> `1` = the first BODY's entity index, `517 2 3 0 0 0 0 1e3 1e-8 …` = its 27 fields
> — which yields correct topology. The dialect-B file `500.079UB.x_t`, which has no
> annotations and hence no `Z` anywhere, opens with `12 1 287 2 3 0 0 0 0 1e3 1e-8
> …` — the same BODY field pattern, no record marker in sight. So "Z1" is `Z` + an
> entity index, not a record header; `res_size`/`res_linear` are BODY fields.
> Verified on the SolidWorks samples only — not re-checked against ABC.

### Entity Stream

After the preamble (dialect A) or straight after the T-line (dialect B), entities
are written as a flat token stream (newlines = whitespace):

```
<type_id>                         uint16 decimal
<inline_schema>                   ONLY on first occurrence of this type_id
  Path A (has base): <n_new_fields> <annotation_chars> Z
  Path B (no base):  <n_fields> <type_name> <alias> <field_descriptors...>
<version_int>                     ONLY if entity is variable-length
<entity_index>                    int32 (1-based entity handle)
<field_values...>                 per schema field descriptors
```

Terminator: `type_id == 1` followed by partition_idx.

### Field Types in Stream

| Type | Encoding | Notes |
|------|----------|-------|
| Integer (d/u/n) | Decimal: `12`, `-1`, `0` | |
| Float (f) | Decimal: `3.14159265358979324`, `1e-10`, `.004572` | 17 sig figs for round-trip |
| Pointer (p) | Integer index: `0` (null), `42` (entity 42) | NOT `#N` in compact format |
| Logical (l) | `T`/`F` characters, may be packed: `FFF1` | |
| Character (c) | Single char: `F`/`R` (face sense), `+`/`-` (edge sense), `S`/`V` (region) | |
| Vector (v) | 3 consecutive floats (no parens) | |
| Optional pointer | `?N` = optional pointer to entity N | `?` prefix |
| Optional float | `?` alone = NaN sentinel | field type determines interpretation |
| Array | Count determined by version_int or fixed schema | variable-length |

### Entity Types (Topology)

| ID | Name | Key Fields |
|----|------|------------|
| 3 | PARTITION | top-level container |
| 10 | ASSEMBLY | collection of instances |
| 11 | INSTANCE | body/assembly placement with transform |
| 12 | BODY | shell, region, surface/curve/point lists, res_size, res_linear |
| 13 | SHELL | body ref, first face |
| 14 | FACE | shell, loop, surface ref, sense (F/R), tolerance |
| 15 | LOOP | first fin (halfedge) |
| 16 | EDGE | start/end vertex, curve ref, tolerance |
| 17 | FIN (halfedge) | edge, next/prev in loop, sense (+/-) |
| 18 | VERTEX | point ref, tolerance |
| 19 | REGION | shell ref, type (S=solid, V=void) |
| 29 | POINT | 3D coordinates (v) |

### Entity Types (Analytic Geometry)

| ID | Name | Parameters | Math |
|----|------|------------|------|
| 30 | LINE | pvec(v), direction(v) | C(t) = pvec + t*direction |
| 31 | CIRCLE | centre(v), normal(v), x_axis(v), radius(f) | C(t) = centre + r*(cos(t)*x + sin(t)*(n x x)) |
| 32 | ELLIPSE | centre(v), normal(v), major_axis(v), semi_minor(f), semi_major(f) | |
| 40 | PARABOLA | | |
| 41 | HYPERBOLA | | |
| 50 | PLANE | point(v), normal(v) | (P - point) . normal = 0 |
| 51 | CYLINDER | pvec(v), axis(v), ref_dir(v), radius(f) | |
| 52 | CONE | apex(v), axis(v), ref_dir(v), half_angle(f) | |
| 53 | SPHERE | centre(v), radius(f) | |
| 54 | TORUS | centre(v), axis(v), major_r(f), minor_r(f) | |

### Entity Types (NURBS/B-Spline)

| ID | Name | Notes |
|----|------|-------|
| 43 | BSPLINE_CURVE | legacy B-spline |
| 45 | BSPLINE_VERTICES | control point array (n_vertices * vertex_dim doubles) |
| 124 | B_SURFACE | → NURBS_SURF(126) |
| 126 | NURBS_SURF | u_degree, v_degree, control grid, knots |
| 127 | KNOT_MULT | knot multiplicity array |
| 128 | KNOT_SET | distinct knot values |
| 134 | B_CURVE | → NURBS_CURVE(136) |
| 136 | NURBS_CURVE | degree, n_vertices, vertex_dim, knot data |

Control point layout for surfaces: U varies fastest. Rational NURBS (dim=4): homogeneous [x*w, y*w, z*w, w].

### Entity Types (Special Geometry)

| ID | Name | Notes |
|----|------|-------|
| 46 | OFFSET_CURVE | offset of curve by distance |
| 55 | OFFSET_SURF | offset of surface |
| 67 | SWEPT_SURF | surface swept along curve |
| 68 | SPUN_SURF | surface of revolution |
| 132 | PCURVE | curve in surface parameter space |
| 137 | SP_CURVE | curve on surface with chart |

### Entity Types (Attributes)

| ID | Name | Notes |
|----|------|-------|
| 70 | ATTRIBUTE | entity → attribute chain, 9 fields in PS30 (8 in sch_13006) |
| 71 | ATTRIB_DEF | attribute definition, multi-element fields (actions×8, legal_owners×14) |
| 82-89 | ATTRIB_*_VALUE | variable-length attribute value entities |

### Common Field Pattern (all curves/surfaces)

```
node_id              d    entity tag
attributes_features  p    → ATTRIBUTE chain
owner                p    → body/shell geometry list
next                 p    → next in owner's list
previous             p    → prev in owner's list
geometric_owner      p    → shared geometry ref (PS 7002+)
sense                c    '+' or '-'
<type-specific fields...>
```

### Non-Transmitted Fields

Fields with `transmit_flag=0` in sch_13006 are NOT in the stream — recomputed on load:
- `face_box`, `body_box` (bounding boxes)
- `body_box_tightness`
- `type` on LOOP/FACE (inferred from topology)
- `u_int`, `v_int` on FACE (UV parameter domain)
- CURVE_DATA, SURFACE_DATA caches

### Cross-Reference Resolution

Two-pass deserialize:
1. Read entities sequentially, store pointer fields as raw int32 indices
2. Patch all pointer fields: int32 index → entity reference

Forward references (entity N → entity M where M > N) are legal and common — topology is cyclic (face→loop→fin→edge→face).

---

## Schema System

Base schemas from `~/cadatomic/solidworks/SOLIDWORKS/data/pschema/sch_13006.s_t`. This is the base for the ABC dataset's `SCH_3001168_30100_13006`.

### Reading sch_13006.s_t

Each entity block:
```
<type_id> <type_name> <n_fields> <parent_type_id>
  <field_name>; <field_type>; <transmit_flag> <extra1> <extra2>
  ...
```

Key columns:
- `transmit_flag=0` → NOT in stream, skip
- `transmit_flag=1` → in stream, parse
- `extra2=0` → scalar field
- `extra2=N>1` → N elements per field (multi-element: P2, F9, etc.)
- `extra2=1` → variable-length array (entity is variable-length)

### Multi-element fields

| Entity | Field | Schema | FieldType |
|--------|-------|--------|-----------|
| INTERSECTION | surface | `p; 1 1006 2` | `P2` (2 pointers) |
| BLENDED_EDGE | surface | `p; 1 1006 2` | `P2` |
| TRANSFORM | rotation_matrix | `f; 1 0 9` | `F64x9` |
| ATTRIB_DEF | actions | `u; 1 0 8` | 8 × uint8 reads |
| ATTRIB_DEF | legal_owners | `l; 1 0 14` | 14 × T/F reads |

### Variable-length entities

Last field has `extra2=1` → entity is variable-length. The VERSION int (read before entity_idx) IS the array element count.

For entities with a fixed V (h-type) field already in base (CHART, LIMIT): `var_count = version - 1`.

Types 82-89 (attribute value entities) are the main variable-length types.

### Standard schemas (dialect B) — `standard_schema()`

Dialect-B files carry no inline schema, so the layouts have to come from
somewhere. `standard_schema(type_id, key_major)` supplies them:

- `key_major < 20` → sch_13006 with the two V10 changes below. Confirmed on
  `500.079UB.x_t` (`SCH_1000000_100040`), which parses end-to-end.
- `key_major >= 20` → sch_13006 with the V20 changes below. Confirmed on
  `500.081UB.x_t` (`SCH_2000000_200000`), which parses end-to-end.

Everything not listed annotates as `255` (base as-is) in the dialect-A files, so it
falls through to `base_schema()`.

**V20+ (`key_major >= 20`)**

| Type | Layout | Change vs sch_13006 |
|------|--------|---------------------|
| 12 BODY | base + `d p p p` (27 fields) | appends index_map_offset, index_map, node_id_index_map, schema_embedding_map |
| 70 LIST | `d u l p p p d d d p p` (11 fields) | inserts list_type(u), notransmit(l) after node_id; drops size_of_entry and one length; finger_index/finger_block move ahead of list_block |
| 74 POINTER_LIS_BLOCK | `d d p` + variable ptr array | one extra leading `d` |

**Pre-V20 (`key_major < 20`)**

| Type | Layout | Change vs sch_13006 |
|------|--------|---------------------|
| 17 FIN | `p p p p p p p p c` (9 fields) | **no leading attributes_features pointer** |
| 80 ATTRIB_DEF | `p p d` + `u`×8 + `l`×13 + var uint array | no field_names pointer; 13 legal_owners, not 14 |

The FIN change shifts every FIN field index down by one, which `build.rs` handles by
keying off the field count — see Known Issues.

**These were derived, not guessed.** For V20 the dialect-A SolidWorks files spell the
same diffs out as inline annotations, so dumping their post-annotation layouts gives
the V20 schema directly:

```sh
XT_DUMP_SCHEMA=1 cargo run --release --example parse_xt -- 500.076.x_t 2>&1 | grep '^\[schema\]'
# [schema] type=12 n=27 : d p p p p p p f f p p p u p u u p p p p p p p d p p p
# [schema] type=70 n=11 : d u l p p p d d d p p
# [schema] type=74 n=3  : d d p          ← plus a variable ptr array, see below
```

Use this whenever a new dialect-B schema version shows up: find a dialect-A file
from a comparable modeller version, dump its layouts, add a `key_major` arm.

When no comparable dialect-A file exists — as for V10 — diff the raw token runs of
**the same entity index** across files instead. Entity numbering is stable across
these exports, so the records line up field for field:

```
FIN 18   V24 (500.076.x_t)    17 255 18 | 0 20 18 18 0 21 9 0 0 +
         V10 (500.079UB.x_t)  17     18 |   20 18 18 0 21 9 0 0 +
                                         ↑ only the attribs pointer is missing
```

For entities that appear only once, cross-check against a record with the same
semantic key. All three ATTRIB_DEFs in `500.079UB.x_t` carry 13 logicals, and the
one for attribute type_id 8004 has the identical actions octet to the V20 file's —
which pins the missing field to `field_names`, not to an actions element.

### `C` onto a variable base's tail (Path A)

`base.fields` holds only the **fixed** fields — a variable base declares one more,
the trailing array, which `read_entity_fields()` reads separately from `var_count`.
So a `C` annotation landing exactly one past the end of `base.fields` is copying
that array, and must push **nothing**. Pushing a placeholder there (the old
behaviour) consumes the array's first element twice and desynchronises the whole
rest of the stream.

POINTER_LIS_BLOCK (74) in `500.076.x_t` is the worked example — base `[D, P]` plus a
pointer array, annotated:

```
74 4 C I16 index_map_offset 0 0 1 d C C Z 20 12 3 0 0 155 2 156 0 0 0 …
   │ │ └ insert index_map_offset(d)   │  │  │  └ 3 fixed fields   └ 20-element array
   │ └ C copies base[0] = n_entries(d) │  │  └ entity index 12
   │                                   │  └ var_count = 20
   │        C copies base[1] = next_block(p)
   └ n_new_fields = 4        final C copies base[2] = the array → push nothing
```

`3 0 0` + 20 pointers lands exactly on the next `type_id`. With the placeholder it
overran by one token, and the next entity's `type_id` was read as a field — which
is what made every dialect-A SolidWorks sample die in the attribute tail with
`unexpected annotation char '1'`.

### PS30-specific types (Path B)

Types NOT in sch_13006 get full inline schema (Path B). Currently known:
- Type 204 (INTERSECTION_DATA) — removed from ps13_schema, uses Path B

---

## Ghidra Reverse Engineering

Ghidra project `solidworks` has `pskernel.dll` imported and analyzed (42K functions).

### Key Addresses

| Address | Name | Purpose |
|---------|------|---------|
| `0x180a24ab0` | pk_receive_entity_typed | Main entity read: schema → version(conditional) → entity_idx → fields |
| `0x180a27d80` | pk_read_inline_schema | Parse Path A/B inline schema annotations |
| `0x180a1dbe0` | pk_read_field_data | Type dispatch for reading field values |
| `0x180a1ff90` | field_element_count | array_count=0→scalar, N>1→N elements, 1→variable |
| `0x182054680` | text_read_tag_ptr | Read integer (entity pointer / tag) |
| `0x182054480` | text_read_float | Read float, `?` = NaN sentinel |
| `0x1820540c0` | text_read_raw_byte | Read single raw byte |
| `0x182054fa0` | text_read_logical | Read T/F logical |
| `0x1845ca580` | global_schema_table | Runtime schema (not dumpable from static binary) |

### Ghidra Commands

```sh
# Open pskernel.dll
ghidra program open --program pskernel.dll --project solidworks

# Decompile key functions
ghidra decompile 0x180a24ab0   # entity read main loop
ghidra decompile 0x180a27d80   # inline schema parser
ghidra decompile 0x180a1dbe0   # field type dispatch
ghidra decompile 0x180a1ff90   # element count logic
ghidra decompile 0x182054680   # text tag/ptr reader
ghidra decompile 0x182054480   # text float reader

# Rename a function you've identified
ghidra rename 0x<addr> my_function_name --project solidworks --program pskernel.dll

# Search for related functions
ghidra xrefs 0x<addr> --project solidworks --program pskernel.dll
ghidra search "receive" --project solidworks --program pskernel.dll
```

### Key Ghidra Findings

- **has_version**: set when schema descriptor last field has `array_count == 1`
- **T-flag and extra flags**: ONLY read when `has_handle_map != 0` — skipped for text format transmit
- **Text format**: `?` prefix on pointer fields = optional. `?` alone in float fields = NaN.
- **Entity terminator**: type_id == 1, followed by partition index

---

## Validation Workflow: STEP Cross-Check

This is the primary development loop. Parse XT, compare topology counts against STEP ground truth, fix mismatches.

### Data Sources

- **XT files**: `~/cadatomic/xt-parser/test-data/abc/xt_files/` (extracted .x_t from ABC dataset)
- **XT archives**: `~/cadatomic/xt-parser/test-data/abc/extracted/<model_id>/*.zip`
- **STEP files**: `~/cadatomic/xt-parser/test-data/abc/step/` (download from NYU archive)
- **Schema files**: `~/cadatomic/solidworks/SOLIDWORKS/data/pschema/sch_13006.s_t`

### Setup (one-time)

```sh
# Download ABC STEP chunk 0000
cd ~/cadatomic/xt-parser/test-data/abc/
wget --no-check-certificate "https://archive.nyu.edu/rest/bitstreams/88598/retrieve" -O abc_0000_step_v00.7z
7z x abc_0000_step_v00.7z -ostep/ -y

# Extract XT files from parasolid zips
for id in 00000000 00000001 00000005 00000007 00000008 00000009; do
    unzip -o -q ~/cadatomic/xt-parser/test-data/abc/extracted/$id/*.zip -d /tmp/abc_validate/$id/
done
```

### Cross-Validation Commands

```sh
# STEP ground truth (topology counts)
grep -c "ADVANCED_FACE" file.step     # → face count
grep -c "EDGE_CURVE" file.step        # → edge count
grep -c "VERTEX_POINT" file.step      # → vertex count

# cadconvert output
cargo run --release --example parse_xt -- file.x_t
# Output: body[0]: type=Solid, shells=N, surfaces=N, curves=N, edges=N, vertices=N

# Batch validation
cargo run --release --example validate -- ~/cadatomic/xt-parser/test-data/abc/xt_files/

# Euler characteristic check: V - E + F ≈ 2 × shells (for closed solids)
```

### Current Status (6 ABC models, multi-file)

| Model | STEP Faces | XT Faces | STEP Edges | XT Edges | Status |
|-------|:--:|:--:|:--:|:--:|:--|
| 00000000 | 25 | 25 | 33 | 33 | match (6 files) |
| 00000001 | 103 | 103 | 246 | 246 | match (9 files) |
| 00000005 | 60 | 60 | 120 | 120 | match (5 files) |
| 00000007 | 3 | 3 | 2 | 2 | match |
| 00000008 | 21 | 21 | 34 | 34 | match |
| 00000009 | 6 | 6 | 5 | 5 | match |

6/6 perfect match. **1000 ABC models: 1000/1000 parse OK (100%), 947/1000 STEP match (94.7%)**. 53 mismatches are almost all XT > STEP (extra sheet/wire bodies in multi-file XT).

### SolidWorks samples (`~/Downloads/3d model/`)

Real SolidWorks exports, no STEP ground truth — only "does it reach the `1 <n>`
terminator without an error line". Useful because they cover both dialects and
four modeller generations.

| File | T-line key | Dialect | Status |
|------|-----------|:-------:|--------|
| 500.076.x_t | `SCH_2401231_20000_13006` | A | full parse — 1 solid, 19 faces |
| 500.076UB.x_t | `SCH_2401264_20000_13006` | A | full parse — 1 solid, 9 faces |
| 500.078.x_t | `SCH_2401231_20000_13006` | A | full parse — 1 solid, 15 faces |
| 500.078UB.x_t | `SCH_2401231_20000_13006` | A | full parse — 1 solid, 8 faces |
| 500.081.x_t | `SCH_2401231_20000_13006` | A | full parse — 1 solid, 10 faces |
| 500.079UB.x_t | `SCH_1000000_100040` | B | full parse — 1 solid, 10 faces |
| 500.081UB.x_t | `SCH_2000000_200000` | B | full parse — 173 entities, 2 bodies (Solid, 9 faces; body_type=3, 1 planar face) |

7/7 reach the terminator. Every solid reports `surfaces == curves == edges` with
`vertices=0` — these are turned parts whose edges are all circles, so there are no
vertices to serialise.

Quick regression check after touching schema/entity code — entity counts must not
drop:

```sh
for f in ~/Downloads/3d\ model/*.x_t; do
    printf "%-18s " "$(basename $f)"
    ./target/release/examples/parse_xt "$f" 2>&1 | grep -E "stopped|^Bodies" | tr '\n' ' '; echo
done
```

---

## Debug/Fix Loop

When a file fails to parse or produces wrong topology counts:

### Step 0 — Turn on tracing

Two env-var-gated hooks, both off by default:

```sh
XT_TRACE=1        cargo run --release --example parse_xt -- file.x_t
# [trace] #4 type=12 idx=7 fields=23 next="0 0 0 0 100 8 4 4 0 0 1 0 0 0 1 0 0 0 1 "
#   one line per entity: sequence number, type, entity index, field count, and a
#   40-char window of what comes next. Read the last few lines before the failure —
#   a misparse almost always shows up as the `next` window starting mid-entity.

XT_DUMP_SCHEMA=1  cargo run --release --example parse_xt -- file.x_t
# [schema] type=70 n=11 : d u l p p p d d d p p
#   one line per Path A inline schema, printing the post-annotation field layout.
```

Cross-check a suspect entity by pulling its raw token run out of the file and
counting: `entity_index` + N fields must land exactly on the next `type_id`.

### Step 1 — Identify the failing entity type

```sh
cargo run --example parse_xt -- file.x_t 2>&1 | grep "stopped"
# → [xt-winnow] stopped at type_id=N after M entities: <error>
```

### Step 2 — Find the raw data in the stream

The XT body is a flat token stream (newlines are whitespace). To find a specific entity:
```python
with open('file.x_t') as f: text = f.read()
body = text.split('**END_OF_HEADER')[1].split('\n', 1)[1].replace('\n', ' ')
idx = body.find(' <type_id> ')
print(body[idx:idx+200])
```

### Step 3 — Check the schema definition in sch_13006.s_t

```sh
grep -A20 "^<type_id> " ~/cadatomic/solidworks/SOLIDWORKS/data/pschema/sch_13006.s_t
```

Count only `transmit_flag=1` fields. Expand multi-element fields (`extra2>1`). Check if last field has `extra2=1` (variable-length).

### Step 4 — Verify with Ghidra decompilation

```sh
ghidra decompile 0x180a24ab0  # pk_receive_entity_typed — main read loop
ghidra decompile 0x180a1dbe0  # pk_read_field_data — field type dispatch
```

Look at how pskernel.dll actually reads this entity type. Compare field count and types against your schema.rs entry.

### Step 5 — Fix schema.rs

Common fixes:
- **Type not in sch_13006**: Remove from `ps13_schema()` so it falls through to Path B (inline schema from file)
- **Multi-element field**: Use `P2`, `P3`, `F2`, `F3`, `C2` FieldType variants
- **Wrong field count**: Recount transmitted fields, expand multi-element fields
- **Variable tail**: Set `is_variable=true`, choose correct `VarType`
- **PS30 adds a field**: Hardcode the extra field (like ATTRIBUTE's 9th field)

### Step 6 — Re-run cross-validation

```sh
cargo test && cargo run --release --example validate -- ~/cadatomic/xt-parser/test-data/abc/xt_files/
```

Compare face/edge/vertex counts against STEP. Check Euler characteristic.

### Step 7 — If build counts are wrong but parse succeeds

The issue is in `build.rs`, not parsing. Check:
- Does build.rs follow the correct pointer chain for this entity type?
- Are field indices correct? (PS30 annotation diffs can shift indices for BODY)
- Multi-body files: face chain may start from a different shell than the first BODY

---

## Reference Material

### Parasolid Reference Manual

Located at `~/cadatomic/solidworks/notes/xt/reference/` (120+ chapters of the Parasolid Functional Description). Key chapters:

| Chapter | Topic | Use When |
|---------|-------|----------|
| ch014 | Model structure | Understanding entity relationships (body→shell→face→loop→fin→edge) |
| ch015 | Body types | Solid vs sheet vs wire body differences |
| ch016 | Session/local precision | Resolution values (res_size, res_linear) |
| ch017 | Geometry | Parameterizations for all curve/surface types |
| ch019 | Nominal geometry | Approximate geometry handling |
| ch020 | Transformations | TRANSFORM entity format |
| ch021 | Assemblies/instances | ASSEMBLY and INSTANCE entities |
| ch092 | Attribute definitions | ATTRIB_DEF structure |
| ch093 | Attributes | ATTRIBUTE entity and chains |
| ch098 | Archives | Transmit format details (read/write pipeline) |
| ch121 | Math form of B-geometry | NURBS curve/surface mathematical definitions |
| ch124 | Glossary | Parasolid terminology |

### Reverse Engineering Notes

Located at `~/cadatomic/solidworks/notes/xt/`:

| File | Content |
|------|---------|
| `schema_preamble_text_parser.md` | Definitive T-line + annotation char format (from Ghidra) |
| `entity_loop_dispatch.md` | Entity read sequence from Ghidra decompilation |
| `entity_field_reader.md` | Per-field-type read functions (addresses + behavior) |
| `annotated_receive_path.md` | Full PK_PART_receive pipeline annotated |
| `annotated_write_path.md` | Write ordering (useful for understanding field order) |

### Schema Files

Located at `~/cadatomic/solidworks/SOLIDWORKS/data/pschema/`:

| File | Version | Notes |
|------|---------|-------|
| `sch_13006.s_t` | PS 13.0 base | Base schema for ABC dataset files |
| `sch_30100.s_t` | PS 30.1 | Mostly identical to 13006 for core types |
| `sch_37102.s_t` | PS 37.1 | Latest schema available |

### ABC Dataset

Located at `~/cadatomic/xt-parser/test-data/abc/`:

| Path | Content |
|------|---------|
| `xt_files/` | Extracted .x_t files (simple CAD models) |
| `extracted/<id>/*.zip` | Parasolid zip archives by model ID |
| `step/` | STEP files for cross-validation (download from NYU) |
| `ofs/` | FeatureScript YAML (parametric construction history) |

---

## Known Issues

1. **ATTRIBUTE is variable-length** — sch_13006: 7 fixed fields (D,P,P,P,P,P,P) + variable pointer array. VERSION int = array count. PS30 files typically have version=1.
2. **ATTRIB_DEF is variable-length** — sch_13006: 26 fixed fields (P,P,D, 8×U, P, 14×L) + variable uint array. `callbacks` field has transmit=0 (NOT in stream). VERSION int = array count.
7. **Newline stripping** — X_T column-80 wrapping intentionally splits long floats (17 sig figs) across lines. Filter-based newline stripping (remove `\n`/`\r`) correctly concatenates these. The Parasolid buffer refill also strips trailing spaces before newlines, but the X_T writer ensures token-terminating spaces are placed so that only floats span line breaks, not integer tokens.
3. **build.rs BODY field indices** — PS30 annotated BODY has 34 fields; geometry chain pointers at [19,20,21] (surf/curve/point), shell at [18], body_type at [14], region at [24]. PS13 base (23 fields) uses [3,4,5] for geometry, [16] for shell.
4. **build.rs BODY field indices, dialect B** — the V20 BODY is 27 fields, which
   falls *under* build.rs's `f.len() >= 30` threshold and so uses the base-schema
   index set ([3,4,5] geometry, [16] shell, [14] body_type, [20] region). That is
   correct — the four V20 additions are appended at the end and shift nothing —
   but the threshold works by luck, not intent. Add a 27-field arm carefully.
9. **FIN field indices shift pre-V20** — `build.rs::build_fins()` indexes FIN
   fields positionally ([6]=edge, [4]=vertex, [7]=pcurve, [9]=sense, [2]=forward).
   Pre-V20 FIN has no leading attribs pointer, so it subtracts 1 from each when
   `f.len() < 10`. Symptom when this is missed: faces and loops build fine, but
   `curves`/`edges` come out 0 — the fin cycle closes on the wrong pointer.
8. **body_type 3 is unmapped** — build.rs maps 1=Solid, 7=Sheet, 12=Wire and
   reports anything else as `Unknown(n)`. `500.081UB.x_t` has a body with
   `body_type=3` whose topology (one planar face, 4 edges, 4 vertices) reads as a
   sheet body, but the code is left failing loudly rather than guessing the code.
5. **Vertex under-counting** — some XT files omit VERTEX/POINT entities entirely. STEP infers vertices from edge endpoints; XT doesn't always serialize them.
6. **`?` notation** — Behavior depends on field type:
   - **Pointer (`p`)**: `?N` = optional pointer to entity N. `?` consumed, integer N read normally.
   - **Float (`f`)**: `?` = NaN sentinel. Only `?` consumed (1 byte), following digits belong to NEXT field.
   - **Vector (`v`)**: `?` = entire vector is NaN (all 3 components). Only `?` consumed; NOT 3 separate `?` reads.
   - **Box (`b`)**: `?` = all 6 components NaN. Single `?` consumed.
   - **Interval (`i`)**: `?` = both bounds NaN. Single `?` consumed.
   - Confirmed from Ghidra RE: text_read_vector (0x182055440) checks `?` once at start, fills all components.

## Fixed: splines evaluated outside their knot range

Parasolid anchors construction geometry a long way off — a swept surface's
profile in this assembly sits at `z = ±500 m`, and the sweep brings it back to
where the part is. Nothing wrong with that, but it made a latent bug in the
evaluator fatal: a B-spline says nothing outside its knots, and both the curve
and the surface evaluators ran de Boor there anyway. An inversion iterating on
the sweep parameter would then walk `u` outside `[0, 1]`, where the
extrapolated profile flies off fast enough to meet whatever point was being
searched for — and report a sweep distance of zero. Every boundary point of
those faces landed on the profile, the parameter region collapsed, and the
patch drew itself along a 500 m axis.

Measured on `910 2001 007.x_t`:

| | before | after |
|---|---|---|
| SP_CURVE polylines longer than 0.2 m | 14 | **0** |
| worst boundary point off its swept surface | 233.8 mm | **0.053 mm** |
| body `200 201 003-51` span | 0.380 m | **0.2564 m** — identical to STEP |

The fix is `nurbs_parameter` in `cad-ir`'s curve and surface evaluators: a
direction the spline closes in wraps by whole periods, which is exact; one it
does not clamps to the knot range, which is the nearest answer the spline has.


## The remaining mesh defects, and what they are not

Measured on `910 2001 007` at the default quality: STEP leaves 1,326 open
half-edges and 172 non-manifold ones over 1.1 million edges; the Parasolid path
leaves 4,141 and 2,975. Better than half of what remains comes from a few dozen
faces whose parameter boundary crosses itself, and two shapes of face account
for it — both present in the source model, neither a reading error.

A **sliver**: two rails a hundredth of a millimetre apart running most of a
millimetre. Dumped from a planar face, the ring's fifteen points are two arcs
that shadow each other:

    0 ( 0.00000,-0.00007)   8 ( 0.22176, 0.80438)
    1 (-0.01143, 0.10734)   9 ( 0.14858, 0.70566)
    2 (-0.01110, 0.21488)  10 ( 0.08805, 0.59870)
    ...                    ...

The mesh samples both rails at the model's own sag, which is ten times the gap
between them, so the two polylines interleave instead of bounding a strip.

A **slit**: the loop runs out along an edge and straight back down it,
enclosing nothing — `(6.401,-0.190) → (12.707,-0.377) → (6.401,-0.190)`.

Approaches measured and rejected, so they are not tried again:

| tried | result |
|---|---|
| split every crossing constraint | STEP open 2,625 → 8,218: each split invents a vertex the neighbour lacks |
| fall back to the containment test where parity is unsound | 2,613 → 3,553 |
| assign the region per union-find component | 1,323 → 2,428: a missing constraint merges two regions and a whole area takes one answer |
| cap the sag at 5% of an edge's own length | 5.7 M triangles |
| re-lay the failing faces on four-times-finer edges | 1,326 → 2,373; finer everywhere on those faces, 2,308 |
| force an interior grid line on every face | +96 k triangles, open +250; ruled directions need none |
| make the two seam walks bit-identical | no change — they already were |
| re-lay narrow faces at a budget taken from their own width | open 1,326 → 1,845 |
| give duplicate edge records one shared discretisation | no change |
| route folded faces *with holes* through the transfinite path | STEP open 199 → 217 |
| restrict the transfinite fallback to non-wrapping faces | STEP 199 → 240, Parasolid 2,532 → 2,630 |
| give a rebuilt patch's coincident ring points one vertex | Parasolid 2,532 → 2,783 |
| rebuild a band from its two original rings instead of the synthesised strip | STEP 199 → 358 |
| keep zero-area triangles in a rebuilt patch | Parasolid 2,532 → 2,498, worth 34 |

Every attempt to close these by sampling more finely made the count worse, and
by roughly the amount the extra segments predict. That is the shape of a rate,
not of a few pathological faces: about one boundary segment in three hundred
fails to find its partner, and adding segments adds failures. Whatever is left
is in that per-segment behaviour, not in a list of faces to fix.


## Meshing a face whose parameter image is not usable

Two things can go wrong with the region a face's boundary encloses in parameter
space, and both are now detected rather than tolerated.

The boundary **folds**, so a segment of it crosses another and cannot be
enforced: the region fill then has a gap to walk through and the face comes out
slit.

The boundary **pinches**, touching itself at a vertex: every segment is
enforced, but going round one way and the other way disagree about which side
is in, and the fill draws a seam along an ordinary edge that nothing asked for.
Measured on this assembly, eight STEP faces do this — tori and spheres, where
the parameterisation is degenerate at a pole — and they accounted for half the
holes that remained.

Neither is a fault in the boundary itself: measured over 195,308 boundary
segments, 195,270 are asked for by exactly two faces. So where the parameter
region cannot be read, the face is meshed from the boundary directly, by the
same transfinite construction the blend family is rebuilt with: the ring is cut
at its four corners, every boundary point's parameter is its arc-length
fraction along its side, and nothing is asked of the parameterisation at all.

| | before | after |
|---|---|---|
| STEP open half-edges | 974 | **199** |
| STEP non-manifold | 224 | **52** |
| Parasolid open half-edges | 3,627 | **2,532** |
| Parasolid non-manifold | 3,043 | **2,186** |


## Where the Parasolid path still differs from STEP

At the default quality STEP leaves 199 open half-edges and 52 non-manifold ones;
the Parasolid path leaves 2,532 and 2,186. Attributed by surface kind, the
difference is almost entirely one class:

| surface | open | non-manifold |
|---|---|---|
| grid (a face rebuilt from its boundary) | 973 | 4,348 |
| nurbs | 390 | 487 |
| plane | 354 | 1,123 |
| cylinder | 351 | 1,714 |
| cone | 268 | 263 |

STEP has no `grid` faces at all — it writes blends as ordinary splines, while
Parasolid writes them as a family with no closed form, so this reader rebuilds
each from its own boundary. Two things were checked and are not the cause:

- The rebuilt patches carry their own boundary: of 93,460 ring segments across
  1,525 rebuilt faces, 209 are missing from the face that owns them.
- No rebuilt face loses a boundary point to a merged triangulation vertex.

So the 973 are half-edges the rebuilt faces drew and their neighbours did not
match. Two measurements taken in the same run locate them exactly:

- 1,445 faces reach the transfinite path because their surface *is* a rebuilt
  grid. Their rings are made of shared edge chains: 82,814 segments, 56 of
  which are not a step of any chain.
- 1,525 faces reach it in total. The extra 80 arrive by the fallback, because
  their parameter region could not be read — and those are wrapping faces,
  whose ring `close_strip` has filled with seam points it synthesised. Nothing
  outside the face has those points.

Measured directly, the fallback rings carry 308 segments that are not a step of
any chain, out of 10,334 — the joins across the seam and the strip's closure.
With the 56 on the top-level route that is 364, against the 903 the attribution
reports as chords, so something beyond the rings accounts for the rest.

One assumption behind the seam is worth writing down because it turned out to
be false: the strip is closed by walking the seam up one side and back down the
other, and both walks were expected to lay down the same points, so that the
seam would come out welded to itself and used twice. It does not — of 58,662
ring points across 553 closed strips, only 1,343 appear twice. The reason is
that `v_steps` returns one segment for a ruled surface, so `seam_segment` emits
no interior points at all and the "seam" is a single join between the two
rings' start points. Restricting the fallback to non-wrapping faces was measured and
costs more than it saves — STEP 199 → 240, Parasolid 2,532 → 2,630 — because
the fallback repairs more than the seams cost.


## Do the two readers agree?

The strongest check available without a reference mesh is whether the STEP and
Parasolid paths land on the same geometry. Sampling 400 points per body from
the STEP mesh and measuring to the nearest vertex of the Parasolid mesh, across
the 39 bodies both files carry:

| | |
|---|---|
| worst distance, any sampled point | 1.13 mm |
| median of the per-body worst | 0.31 mm |
| mean on the largest body | 0.015 mm |

Read with the caveat that this is point-to-nearest-*vertex*, not
point-to-surface: at the default quality an edge runs 0.2 to 0.7 mm, so a point
sitting on a triangle's middle is already a third of that from any vertex. A
worst of 1.13 mm is about two edges, which is what two meshes of the same
surface with different vertex placement look like.

Alongside it: all 39 shared bodies agree on their bounding-box span to within
1 mm, and the two assemblies have the same overall extent, 0.285 x 0.350 x
0.173 m.


## Choosing how to mesh a face, by measurement

Two ways of meshing a face are available and neither is always right.
Triangulating the parameter region is exact wherever the region can be read,
and there are two ways it cannot: the boundary's image **folds**, so a segment
crosses another and cannot be enforced; or it **pinches**, touching itself at a
vertex, so that going round one way and the other disagree about which side is
in. Rebuilding from the boundary asks the parameterisation nothing at all, but
blends the interior rather than evaluating it.

Picking between them from a symptom — did a constraint fail, did the fill draw
a seam — works, but not as well as measuring. So where the first leaves any of
its boundary undrawn, the face is meshed every way available and the one that
draws the most boundary is kept: a boundary segment a face omits is a hole in
the finished mesh, and every other difference between the candidates is
interior. The candidates are the triangulated region, and the rebuild with and
without its holes cut, each with corners taken from the sharpest turns and
again spaced evenly.

| | STEP open | Parasolid open |
|---|---|---|
| chosen from a symptom | 199 | 2,532 |
| chosen by measurement | 107 | 2,381 |
| holes left to the measurement too | 107 | 1,922 |
| rebuild with holes offered as a candidate | 81 | 1,489 |
| evenly spaced corners offered as well | **81** | **1,460** |

A fifth candidate — the same triangulation read by the centroid test rather
than by walking the constraints — was offered and made it worse, 81 → 127: it
can lower the boundary-gap count while keeping triangles that lie outside the
region, which this measure does not see.

Two more things follow from the same principle once the comparison is in
place:

- **An empty region is not a reason to give up on a face.** Five STEP faces
  were failing with "no triangle fell inside the boundary" — the fill had put
  everything outside — while their boundary sat there, perfectly good. Treating
  an empty patch as one that drew none of its boundary lets the comparison find
  the rebuild. Faces meshed went from 11,207 to 11,212 of 11,214, and open
  half-edges from 81 to 43.
- **A patch that strays past its own boundary is a candidate to beat.** A
  hemisphere stands a radius proud of the circle bounding it, about half that
  circle's diagonal; past that a patch is not bulging but somewhere else. Where
  one does, the rebuild — which cannot stray, being built from the boundary —
  is offered against it, and among candidates with equal boundary coverage the
  one nearest its own boundary wins. 43 → 39.

That leaves, at the default quality, **39 open half-edges and 26 non-manifold
ones in 1.1 million STEP edges**, with 11,212 of 11,214 faces meshed. Exactly
one face anywhere in the model still fails to draw part of its own boundary,
and it leaves two segments undrawn.

## Closing the Parasolid side: eight reading gaps, found by tracing each crack

The STEP side had settled at 39 open half-edges while Parasolid sat at 1,442.
Attributing the difference by surface kind said "rebuilt blends and offsets"
and was misleading every time. What worked was taking each open edge back to
the thing that produced it, and the classification that did it is now
`CAD_TESS_TRACE_OPEN`: for every edge the mesh uses once, is it a segment of
some edge's discretisation, and if so, how many faces name that edge against
how many actually drew it. Three answers, three different bugs:

- *not on any edge chain* — the face drew a boundary the file never gave it;
- *named by two faces, one meshed* — the neighbour failed;
- *named by two faces, both meshed* — the two disagree about the segment.

`CAD_TESS_GAPS` then reports, per face, how much of the file's own boundary
the chosen patch omits, and `CAD_TESS_DROPPED` why a loop was discarded. The
eight fixes below came out of those three numbers, in that order.

**`OFFSET_SURF` (type 60) was not read at all.** Nine surfaces, and the faces
on them were skipped, leaving their boundaries open on every neighbour. Field
[9] is the base, [10] the distance, and **[6] the sense**: without applying it
the offset goes the wrong way on 70% of them. Measured, not assumed — with the
sense applied, `(p − base(uv))·n / d` is `+1.000` at all 1,830 boundary points;
without it, 1,276 of them read `−1.000`.

**`Surface::Offset` inverted onto its base.** Given `p = base(uv) + n(uv)·d`,
inverting `p` against the base answers a different question and lands off by
roughly the offset wherever the base curves. Undoing the offset first — a
fixed point, since the normal varies far more slowly than the surface — took
the worst boundary residual on those faces from **2.001 mm to 0.018 mm** on a
1 mm offset. The 2 mm was exactly `2 × d`, which is the signature to look for.

**An edge is described once per fin, and only one of them may be readable.**
A tolerant edge carries no 3D curve; its geometry is each fin's `SP_CURVE`, in
that face's parameter space. When the fin asking sits on a blend, that space
cannot be evaluated and the edge was lost — with it the loops that used it, and
with those the faces. Offering every fin on the edge and taking the first that
evaluates took lowering skips from 71 to 38. `SP_CURVE` field [9] (`original`,
the 3D curve it approximates) is zero in all 4,869 of them here, so there is no
shortcut past this.

**A rolling-ball blend's cross-section is an arc, not a chord.** `BLENDED_EDGE`
gives two mating surfaces, a spine, and a radius; the boundary curves are never
transmitted (zero in all 2,211). A Coons patch rules straight between the
rails, which on this assembly's 17 mm fillets is 5 mm out at the middle. The
construction that works starts from *one* rail: the ball touches it there, so
its centre is `r` along that surface's normal, and the far end of the section
is wherever that ball touches the other surface. Solving for the far contact is
what makes it exact — the two rails are sampled by arc length independently, so
station *i* on one is not the cross-section partner of station *i* on the
other, and pairing them directly bends every section that is not symmetric.
Offering all four sides as the rail and accepting only when the ball stays on
the far surface along the whole of it: **413 of 1,447 rebuilt blend faces**.

**A degree-one grid can be inverted exactly.** For degree one the control
points *are* the surface at the knots, so the patch is a grid of bilinear quads
and inversion is a sequence of small exact problems. The general spline search
samples instead, and on a grid built from a face's own boundary that lands
neighbouring boundary points on one parameter — the triangulation reads them as
one point and slits the face against every neighbour.

**A face that lost its boundary must not be given the surface's whole domain.**
`loops.is_empty()` meant "a whole sphere or torus, nothing to trim", but it
also caught faces that *declared* bounds and failed to build them. One offset
face was handed a fabricated 512-segment boundary — no neighbour anywhere near
it, so every segment was a crack, and that single face was 512 of the model's
then-1,602 open edges. Refusing is right: 1,602 → 1,066.

**A collapsed loop is a degenerate point bound.** Parasolid has no vertex-loop
concept, so a cone's apex arrives as a bound carrying a zero-length edge. Read
as an ordinary loop it collapses and is discarded, leaving the face with one
wrapping ring and nothing to close onto; six cone faces failed outright. A
bound whose every edge collapses onto one point states the same thing the
single-vertex form does. 1,066 → 711 open, six faces recovered.

**`geometric_bounds` must look through a trim.** The reference extent a body is
judged against takes vertices, plus its circles by median-radius consensus —
but only bare `Curve::Circle`. A writer that wraps each circle in a
`Curve::Trimmed` hid every one of them, the reference fell back to the four
clustered seam vertices of a turned ring, and `repair_runaway` then read the
ring's own 8 mm circles as runaways and collapsed them. **Two whole parts of
the 50-part assembly were missing from every render because of it.**

| | faces meshed | open | non-manifold | failures | lowering skips |
|---|---|---|---|---|---|
| before | 11,197/11,212 | 1,442 | 2,075 | 15 | 75 |
| after | **11,207/11,212** | **711** | **2,002** | **5** | **40** |

STEP is unchanged throughout at 11,212 of 11,214 faces, 39 open, 26
non-manifold. Two-reader agreement on the largest shared body improved from
0.017 mm to 0.010 mm mean, and the two bodies above rejoined the comparison,
taking it from 39 shared bodies to 40.

### Measured and rejected here

- **Loosening the rolling-ball acceptance.** The miss distribution over 6,313
  attempts is 336 within 1% of `r`, 450 at 1–10%, 676 at 10–50%, 4,851 beyond
  half of `r`. Accepting the 1–10% band would take 100 µm of error on a 1 mm
  fillet against a model tolerance of 10 µm. That is forcing the geometry, not
  reading it.
- **A finer rebuilt grid** (`N_MAX` 96 → 192 → 384) and **a finer boundary to
  build it from** (`PER_EDGE` 16 → 48 → 96): 250 → 250 and 250 → 242 gaps on
  the worst face. Neither is the binding constraint.
- **The classification `[nm] self-fold vs distinct-faces`** as first written
  called 2,002 edges self-folds. It was counting a face that legitimately used
  an interior edge twice as the culprit. Counting *per face* instead: 13 folds,
  1,989 overlaps across 499 faces. A classifier that cannot be wrong about the
  answer it gives is worth more than the number it prints.

### Two more, once the tracing said where to look

**A boundary encloses a region in its own plane whatever the surface does.**
Both readings available until now assume something about the face's shape: the
surface's own parameterisation can fold or pinch, and the boundary rebuild
reads the ring as a quadrilateral with four corners in it. One face here is
neither — a blend band round a closed feature, its outer boundary written as
fourteen edges and its inner as one — and every candidate left a quarter of its
1,102 boundary segments undrawn, which with its neighbours' side of the same
segments was two thirds of every crack in the model. Projecting the boundary
onto its own best-fit plane needs no corners, no solve and no assumption about
how many loops there are; the layout is planar and every position is real, the
boundary points being the neighbours' own and the interior points the surface
evaluated where the plane says to look. Two details decide whether it helps or
harms:

- It must be given **the file's own loops, not the merged region**. The region
  step bridges the loops into one walk, and a bridged ring projected onto a
  plane crosses itself, so every triangle in it reads as outside — the patch
  came back empty and the candidate looked useless.
- It must be offered **last, and only against cracks it can remove**. On a cone
  closed at an apex it draws the whole boundary too, and would win on any
  measure but that one while flattening the cone into the disc its boundary
  spans. A test that a cone stated with a collapsed apex bound still meshes is
  what caught it.

Offered that way it took Parasolid from 646 open edges to 212 — and STEP, which
had been sitting at 39 since the last round, from 39 to 10.

**A curve read as a polyline is sampled where the reader looked, not where any
criterion asked.** The tessellator's own sampling is held to a sag and a turn
angle, and every point it places is load-bearing. A Parasolid parameter-space
curve arrives already sampled — evenly in its own parameter, which says nothing
about how much shape lies between the points — and where that parameter runs
unevenly they pile up. Measured: **20,368 of the Parasolid chains' segments are
shorter than a hundredth of the sag, against 64 on the STEP side**, and all but
five of them are on polyline curves. Points that fine cannot carry shape the
mesh is allowed to show, but each makes a sliver triangle, and where the face
across the edge cuts the run differently, a crack.

They are dropped by Douglas-Peucker against *both* criteria the sampling itself
was held to — distance from the chord, and the turn across the span measured
through it — with the deviation bounded at a hundredth of the sag, so the chain
is kept a hundred times more faithfully than the mesh around it. It applies
only to polylines: everything else is evaluated on demand at parameters the
criteria chose.

| | triangles | open | non-manifold | needles |
|---|---|---|---|---|
| Parasolid before | 899,239 | 212 | 1,970 | 53,324 |
| Parasolid after | **765,845** | **215** | **108** | **10,801** |

Non-manifold edges fell by 95% and needles by 80%, and the mesh got 15%
smaller for it. Three attempts that did not work are worth recording, because
each looked right:

- **Skipping subdivision on an edge shorter than the sag.** A single chord is
  within tolerance for such an edge, so the segment floor only manufactures
  slivers — except that the floor is not the only criterion. A full circle of
  radius 1 under a sag of 100 is "shorter than the sag" and still has to turn
  through 360°, so it came back as two points.
- **Thinning by distance alone.** A shallow arc sits within any distance you
  like while turning through a corner, and the angular limit exists precisely
  to place points a chord test would not ask for. It undid it.
- **Thinning by distance and turn, applied to every curve.** Replicating the
  subdivision's own angular measure from points alone is not quite possible —
  a segment direction is a chord, not a tangent, so a span of *k* segments
  reads as (k−1) segments' worth of turn — and a full circle came back with 19
  or 20 points where the criterion asks for 24. Restricting the thinning to the
  curves whose points are a reader's samples removes the need to replicate
  anything.

### Three more, and STEP closes

With the crack tracing in place the remaining faces could be read one at a
time. `CAD_TESS_LEFTOVER` lists every face the comparison could not close,
with what it left undrawn, what it tore open inside itself, and — the useful
part — *why* its boundary went unenforced: two points landing on one
triangulation vertex, a segment crossing another, or a split that had to invent
a point. `CAD_TESS_UVBOX`-style parameter boxes alongside it turned three
separate mysteries into three plain statements.

**A crossing is worth splitting, sometimes.** Where the boundary's parameter
image crosses itself the segment cannot be enforced, and the region fill then
walks out through the gap and tears the middle out of the face. Splitting at
the crossing invents a point the neighbour has never heard of — a seam — which
is why it was refused outright. Measured on one face, refusing costs two
undrawn segments and nine torn ones; splitting costs two seams. Neither is
right in general, so the region is now built both ways and the one that leaves
less open is kept. Applied unconditionally it makes things worse (open 215 →
239); offered as a candidate it helps (215 → 198).

**A closed curve is periodic over its own range.** `Curve::period` answered only
for the conics, so a surface swept along a closed spline or polyline reported no
period at all. A face running right round such a profile then read as one loop
with another sitting *beside* it — the two boundary loops came out with
disjoint `v` ranges, which is not a loop with a hole in it and cannot be
triangulated as one. Reporting the natural span for any closed curve fixed the
region; it also required fixing what a closed polyline does off the end of its
range, which was to clamp — so a parameter past the seam pinned on the last
point and the edge came back as a chain of one repeated position. Open edges
198 → 115, non-manifold 101 → 82, needles 10,806 → 7,095.

**Where a periodic strip is cut is a free choice, and not a harmless one.**
The cut was placed at the wrapping loop's own minimum `u`, which is arbitrary
relative to everything else on the face. A hole lying across it is split in two
by it, and the halves then sit at opposite ends of the strip where no
triangulation can join them. Moving the cut into the largest stretch no hole
covers — nothing moves in space, only the seam — closed **the last open edge in
the STEP model**: 10 → 0. It was one cylindrical face, carrying a hole across
the cut, and both readers had it.

A fourth, smaller: **the outer loop is the one that contains the rest.** Signed
area says that for simple closed curves and stops saying it as soon as one is
not — a boundary whose parameter image doubles back has its own area cancel,
and the enclosing loop then scores lower than the loop it encloses. Containment
is what is meant; area is now only the tie-break.

### Measured and rejected here

- **Reading the region by centroid as well as by walking the constraints.**
  The two disagree exactly where the boundary pinches, and the centroid test
  cannot tear a narrow strip the way the walk can, so it looked like the answer
  for the three thin annular faces that remain. Built and measured against the
  crack count: identical totals, 103 open either way. Reverted — a change that
  buys nothing should not be carried.
- **More settling rounds.** 0, 8 and 24 rounds give 105, 103 and 103 open
  edges. The eight already in place are all that is available there.

## What OpenCASCADE does that we did not

Mayo, which previews STEP faultlessly, does not read STEP: it is a Qt front end
over **OpenCASCADE**, which reads with `STEPCAFControl_Reader`, *heals* with
`ShapeFix_*`, and only then meshes with `BRepMesh`. We read and mesh. Three
gaps came out of comparing the two, and one of them was not OCCT's lesson at
all but a bug of ours that OCCT's discipline would never have allowed.

OCCT is **LGPL-2.1-with-exception**: the exception permits linking, not copying
source into an open-source project of another licence. Everything below was
written from a prose statement of the rule against our own types. Where the
temptation to transcribe was real it is noted.

**A gate that cannot measure must not reject.** `escapes_body` refused any
patch reaching further from the body's centre than the body's own diagonal.
An O-ring is one toroidal face whose only bound is a single vertex — no curves,
no second point — so the body's extent came out as that point, its diagonal as
zero, and every patch "escaped". The message said so outright: *patch reaches
10.2 mm from the body centre, but the body spans 0.0 mm*. Both patches were
right; the radii were right. Requiring the reference to span something before
it is allowed to judge took **STEP from 11,212 of 11,214 faces to all 11,214**,
and put two whole parts back in the assembly. The same trap sat dormant in
`repair_runaway`, whose limit is the same diagonal. OCCT has no such gate at
all — it judges per edge and per vertex against a tolerance.

**A trimmed curve carries the curve's domain, never the edge's range.** OCCT
projects an edge's two vertices onto its 3D curve and repairs the pair, because
STEP names no parameters at all. We had the same routine in `cad-step` and
bypassed it in `cad-xt` for every trimmed curve, taking the file's interval on
trust. It is wrong for **444 of the Parasolid file's 26,531 edges, the worst by
53 mm** — all of them charts written as polylines, where the file's interval is
in the chart's own parameter while ours is the segment index. A range of
`[0.000028, 0.002837]` then addresses the first three thousandths of the first
segment: a sliver standing in for an edge three millimetres long, and the two
faces sharing it disagree about where their boundary runs. The only claim a
range makes that can be checked against something else the file says is where
it starts and stops — so it is checked against the edge's own vertices, and a
range that does not reach them falls through to recovery. **444 → 84 mis-ranged
edges, worst 53 mm → 23 mm.** The 84 that remain are charts that do not reach
their edge's ends at all, and their fins do not either; there is nothing better
to be had for them. Open half-edges went *up*, 65 → 74, which is what happens
when a defect stops hiding inside wrong geometry. `range` in the report is the
permanent form of this check.

**A wire that loses an edge stays open, and nothing noticed.** Counting how
often each edge is used cannot see it: the survivors are still used twice each.
The junctions have to be checked directly — every half-edge must end at the
vertex the next one starts from — and doing so found **8 loop joins that do not
meet, the widest 1.44 mm on a body whose stated tolerance is 0.01**. They come
from 18 fins that name no edge at all, all in one body, and none of them
appeared anywhere in the report. What can honestly be done is to say so and
make both sides agree: the drop is now recorded, and the gap is closed with a
straight edge between the two loose vertices, interned like any other edge and
keyed by the pair it joins — so the faces on either side are handed the *same*
`EdgeId`, it is sampled once, and their boundaries are identical to the bit
rather than merely close. It is not the curve the file lost and the report says
so. **8 broken joins → 0**, open half-edges 74 → 65. Two attempts got there:
tracking the ends in a private ledger made 50,778 joins look broken and opened
35,000 edges, and keying the bridge by an unordered pair without saying which
way the shared edge runs made the walk loop forever. Read the ends off the
interned edges, and give the half-edge the direction the shared edge actually
has.

**One face must not be able to lose the model.** The bridging work turned up a
boundary the constrained-Delaunay library aborts on — and the whole conversion
died with it, printing nothing. The release profile carried `panic = "abort"`.
For a library meant to be embedded behind an FFI boundary and a NuGet package,
a malformed face taking the host process with it is not a trade worth making,
so the profile unwinds and a face that panics is reported as a face that
failed. Measured cost: none worth reporting — 5.1 s for the Parasolid assembly,
3.6 s for the STEP one.

### Measured and rejected from the same comparison

- **Escalating to the fin's parameter curve whenever the recovered range misses
  the vertices.** It takes mis-ranged edges from 84 to 1, and open half-edges
  from 74 to **293**: on a chart that merely runs coarse the fin's curve is the
  worse of the two. It is offered and kept only when it actually reaches the
  edge's ends, which on this model is never.
- **`FixNotchedEdges`.** Our spur test is a picometre, so a notch whose rails
  are microns apart passes; OCCT's own test finds 59 such junctions on our IR.
  But the repair splits an edge, inventing a point the neighbour has not heard
  of — which this file already records as costing more than it saved every time
  — and it closes no open edge. The notched junctions are two sheets meeting
  along a line, invisible in a render.
- **`FixSmall` / `MinSize`.** 268 zero-length and 1,055 sub-tolerance edges on
  the Parasolid side, 279 and 733 on the STEP side — and STEP has no open edges
  at all. Short edges are demonstrably not what tears this model, and the spur
  loop and the per-point nudge already prevent what `FixSmall` prevents: every
  face that still fails reports `merged=0`.
- **`FixSmallAreaWire`.** 172 wires qualify. OCCT keeps it off by default for
  anything but a bare face, and dropping them would open the faces that own
  them. Correctly absent.

### Where we were already level, or ahead

Containment-then-area for the outer loop matches `ShapeFix_Face::FixOrientation`.
`nearest_branch` and `rephase_holes` are `FixShifted`. `wrapped_region` with
`reseam` is `FixMissingSeam` with `FindBestInterval`, and `reseam` is what
closed the last STEP open edge. `full_domain_loop`, gated on the face having
declared no bounds, is `FixAddNaturalBound` including OCCT's own caution about
when not to. The two-way `split_crossings` is `FixSelfIntersection`. There is
nothing for `FixFaceOrientation` to do: of 26,535 edges, none is traversed the
same way by both its faces. Our deflection defaults are an order of magnitude
finer than the ones Mayo ships. And watertightness here is exact shared `f64`
points reached through one `EdgeId`, which is a stronger claim than OCCT's
tolerance-sphere argument — most of its tolerance machinery has no meaning
against it.

### The chart is not the curve

Chasing the last faces whose boundary could not be drawn led to the largest
reading gap left on the Parasolid side. Three of them were thin annuli — an
outer loop and an inner one 1.2 mm apart — and dumping their parameter loops
showed the inner one poking *outside* the outer, which no in/out rule can read.
The reason was two edges of 39.91 mm drawn as **two points**: a straight chord
where a 66° arc belongs, cutting clean across the other loop.

Those edges are `INTERSECTION` curves, and Parasolid writes such a curve twice.
Once as the two surfaces it lies on, which is its definition. Once as a
**chart** — a handful of sampled points with a stated chordal error, which on
this file is `0.00267`, in metres. The chart is what the reader could reach,
and it is not the curve: it says so itself, in millimetres, on a body whose
tolerance is ten microns.

The definition is computable, and two things had to be fixed to reach it.

**The parser was throwing the second surface away.** A field written as a fixed
array of two pointers occupied one slot, and `read_field_value` read the first
element and discarded the rest — for every `P2`, `F2` and `C2` in the schema.
The schema's own comment recorded it: *"P2 reads both surface pointers from
stream but only stores the first. surf2 is discarded."* Keeping the tail in a
`RawEntity::extra` alongside the fields — so no index anywhere moves — took
intersection curves carrying both their surfaces from **0 to all 5,177**.

**Then the curve is a walk, not a guess.** At any point on both surfaces the
intersection runs along the cross product of the two normals, so: step along
it, fall back onto both surfaces by alternating nearest-point solves, repeat.
The step halves when the walk turns more than a few degrees and grows when it
does not, so a straight run costs thirty-two steps and a tight arc gets what it
needs. Only charts coarser than twenty times the body tolerance are walked; a
chart already inside tolerance *is* the curve and is cheaper.

One guard makes the difference between this working and this being worse than
the chart: **two surfaces can meet along more than one curve, and a walk that
sets off along the wrong one still arrives somewhere.** Without a branch check
it cost 65 open half-edges → 92. The chart is coarse but it is the file's
statement of *which* curve this is, and it states how coarse — so a walk that
strays further from the chart than the chart's own declared error is on the
other branch and is refused. **With the check: 65 open half-edges → 13**, and
the three thin annuli stopped being special cases, because the chords that made
them unreadable were chart chords.

Cost: 5.1 s → 9.2 s on the pilot assembly. Two attempts to buy that back were
measured — carrying each step's parameters forward as the next solve's hint is
14 % faster and costs three open edges, because a hinted solve follows a branch
a fresh one would not; it is not taken. Twelve settling rounds instead of
twenty-four cost nothing and are.

### Two more from the same pass

**A wire that loses an edge stays open, and nothing noticed.** Counting edge
*uses* cannot see it — the survivors are still used twice each. Checking the
junctions directly found **8 loop joins that do not meet, the widest 1.44 mm**
on a body whose stated tolerance is 0.01, all from 18 fins that name no edge at
all and none of which appeared anywhere in the report. Both are now said out
loud, and the gap is closed with a straight edge between the two loose
vertices, interned like any other and keyed by the pair it joins — so both
faces are handed the *same* `EdgeId`, it is sampled once, and their boundaries
match to the bit rather than approximately. It is not the curve the file lost
and the report says so. Two attempts got there: tracking the ends in a private
ledger made 50,778 joins look broken, and keying the bridge without saying
which way the shared edge runs made the walk loop forever. Read the ends off
the interned edges; give the half-edge the direction the shared edge has.

**One face must not be able to lose the model.** That work turned up a boundary
the constrained-Delaunay library aborts on, and the whole conversion died with
it, printing nothing — the release profile carried `panic = "abort"`. For a
library meant to sit behind an FFI boundary and a NuGet package, a malformed
face taking the host process with it is not a trade worth making. The profile
unwinds, and a face that panics is reported as a face that failed.

### The material library was loaded and never consulted

The SolidWorks library is bundled — 115 materials, 115 optical entries, 113
`pwshader` and 114 `cgshader` names, compiled into the crate — and it was being
loaded on every run and then not used. It sat behind `build_named`, which needs
a material *name*, and neither Parasolid nor STEP carries one for this
assembly. So every surface fell through to the colour tier and was shaded by a
preset written into this crate. The report said so plainly and it went unread:
the material names in the output are `paint-808080`, `aluminium-D1D1D1`,
`steel-555759` — colour-derived, every one.

The family is inferred, and that part is fine: a neutral grey surface in a
mechanical assembly *is* machined metal. What does not need inventing is how
that family reflects light, because the library states it, for these same
families, in the designer's own numbers. Each family now names one library
entry — `AISI 1020` for steel, `6061 Alloy` for aluminium, `Gray Cast Iron`,
`Tin Bearing Bronze`, `Pure Gold`, `Rubber`, `Oak` — and an inferred material
takes its optics from there, keeping the part's own colour as before. Naming
one entry rather than averaging the family keeps the choice auditable: this
steel is `AISI 1020` and you can go and look at it.

Measured on the pilot assembly: aluminium's roughness 0.32 → **0.22**, steel's
0.38 → **0.05**, and steel's base colour picks up the library's own tint. The
families the library does not carry — paint, concrete, fabric, foam, zinc — are
finishes and fillers rather than engineering materials, SolidWorks does not
list them, and they keep the preset. Both readers take the same path, so both
shade from the library.

The shader names were already being read and used: `is_metal` and
`measured_f0` consult them, so a material whose swatch is ambiguous but which
asks for the `Polished Steel` or `Verchromung` shader is classified by what
SolidWorks would draw it with.

### A blend's parameter curves run two ways, and both can be computed

An edge across a blend is a cross-section, and a cross-section is a circular
arc between the ball's two contacts — which the edge's own vertices give, so it
is built and not walked. An edge running *along* a blend is something else: the
track the ball leaves on one of the surfaces it touches. Its ends say nothing
about the curve between them, and the two surfaces meet there tangentially or
not at all, so the intersection walk has nothing to follow.

It is still one equation. Standing at a point of the surface the ball touches,
put the centre a radius along the normal and ask how far that centre is from
the other surface; on the track the answer is the radius exactly. So the track
is a level set, and walking a level set is stepping along it — across the
gradient, in the surface's own parameters — and correcting back with Newton.
Two things had to be right: the file says which end of the blend the curve sits
at, which says which surface the ball touches, but only if the parameterisation
is read the same way round as it was written — so it is a preference and the
other surface is tried after it, which took the start from **0.9 mm off the
surface to exactly on it**. And a step that lands somewhere the surface cannot
be inverted is a step that was too long, not a walk that failed.

**A boundary of three points is a patch.** The rebuild's own documentation says
so — "three uses a degenerate fourth side, exactly as a triangular patch
should" — and its guard demanded four, costing one face outright.

Together: faces meshed 11,210 → **11,211 of 11,212**, open half-edges 13 → 10,
unreadable edges 8 → 4.

The two edges left are the honest limit of a constant-radius reading. On one,
correcting the start moves it 1.4 mm off its own vertex — the blend has run out
there and the track is not defined at the vertex. On the other, no ball of the
stated radius reaches both ends, missing by 1.3 % of it against a body accurate
to 1 %. Neither is forced.

### `BLEND_BOUND`, and knowing when not to walk

The commonest reason an intersection could not be computed was surface type
**59, `BLEND_BOUND`** — it appeared in more of the refusals than every other
type together. It carries no geometry: `boundary(n)` and `blend(p)`, naming a
blend and which of its two sides this is. The surface it stands for is the
mating surface the ball touches there, which is one dereference away, and
following it took the refusals from most of the walks to almost none.

And then the walks failed anyway — 925 of them — which is the more useful half
of the finding. **A blend does not cut its mating surface, it touches it.** The
normals are parallel along the whole boundary, their cross product is nothing,
and a walk that follows it has no direction to go in. So the pair does not mean
a transversal meeting at all.

Walking the ball's own contact track instead is the right shape of answer and
was built: it arrives nowhere on this file, and costs **eighty per cent of the
running time** finding that out. The chart stands for these, and the walk is now
not attempted where the surfaces are tangential — which is both more honest and
faster than before the reference was followed at all: **8.8 s against 8.9 s**.

### The last leftovers, named

`CAD_TESS_LEFTOVER` gives four faces worth ten open edges, and their causes are
no longer mysteries:

- Seven of the ten are one **torus** face. Both its loops are chart polylines.
  One wraps the seam and the other has a single long step, `u` from 7.854 back
  to 6.480, where the true curve goes forward through the seam instead. With
  only two points on that edge there is no evidence either way, and the
  nearest-branch rule takes the shorter reading. It is the coarse chart again,
  on an edge whose surfaces are a blend and a blend boundary, so there is no
  walk available to give it the intermediate points that would settle it.
- A **plane** and an **offset** miss one boundary segment each.
- A **cylinder** has one parameter-space crossing.

And one face fails outright: an offset whose only two boundary edges are the
two that cannot be read — one where correcting the start moves it 1.4 mm off
its own vertex because the blend has run out there, one where no ball of the
stated radius reaches both ends, missing by 1.3 % against a body accurate to
1 %. Neither is forced.

### The blend surface, built, and what it actually costs and buys

The last structural gap was to evaluate the rolling-ball blend as a surface
rather than rebuilding each of its faces from that face's own boundary. It is
built, and the construction has no approximation in it: the blend is the
envelope of a ball rolling in a crease, so it is determined by where the ball
goes and what it touches. Where it goes is the **spine**, which is the ball's
own contact track on one of the surfaces lifted a radius along that surface's
normal — the same level-set walk the rails use. At each station the contacts
are the nearest points on the two surfaces, and the surface between them is the
arc of that radius. The file's chart of the spine supplies the ends to walk
between and the bound the walk may not leave.

Getting it to run at all took four measured corrections, and the arithmetic of
where to apply it took a fifth:

- **The definition read literally is unaffordable.** The spine is where the two
  surfaces *offset by the radius* meet, and each inversion of an offset is
  itself a fixed point over its base: over two minutes on this assembly.
  Lifting the contact track is the same curve for one inversion of each surface
  per step.
- **Even that is 65 ms a blend**, and there are 2,211 of them. Step ceilings,
  fewer Newton rounds and a one-sided gradient bought 27 % — the cost is
  structural, not a constant.
- **So it must not be paid where a cheaper reading is already right.** A chart
  that is merely coarse still reaches its edge's ends and the edge is usable;
  only where the chart cannot even do that is there no other reading. Asking
  the question that way took the blend builds from 619 to **7**, and the
  running time from over ninety seconds back to **8.9 s**.
- **A closed feature has a closed spine** — its two ends are the same point,
  and a walk needs two. The chart's own middle sample gives a third, so the
  loop is walked in two halves.
- **Two acceptance tests were refusing correct answers.** The correction
  stopped at a hundredth of the tolerance it would be judged by, so a gradient
  that could not be computed threw away a ball already six microns from
  touching on a tolerance of fifteen. And the start was held to the standard of
  an edge's vertex — but a spine's end is not a vertex, it is a chart sample,
  and the chart states its error in millimetres.

What it buys, measured: **one of the seven** blends that need it. The other six
fail with the same reason each time — the file's own chart of the spine is too
coarse to seed the walk, its ends landing further from the ball's real track
than the track's own radius. There is nothing further to read: the chart is all
the file says about where that spine is.

So the exact blend surface is in, it is tested against a fillet whose answer is
known, and it changes none of the remaining numbers. That is the honest result,
and it is worth recording precisely because the reasoning that led to it was
sound and the outcome still was not.

### A seam crossing and a fold look the same, so read the face both ways

Seven of the ten remaining open half-edges were on one torus face, and the
cause was a single step: its boundary's `u` appeared to jump 1.37 backwards in
one place and creep forward everywhere else. Two readings fit that. The
boundary genuinely doubles back — the nearest branch, which is what the walk
took — or it crosses the seam and keeps going, which is the same jump plus a
period. The edge is two points and a jump; **there is no local evidence
between them**, and two local rules were built and measured before that was
accepted: preferring the loop's prevailing direction never fires, because the
step is the loop's *first*; adding a magnitude threshold never fires either,
because 1.37 is less than half a period.

What does discriminate is the face. One reading makes a band and closes; the
other leaves a quarter of the boundary undrawn. So the face is now read both
ways wherever the first way leaves it open and the surface has a seam to cross,
and the one that leaves less open is kept — the same measured choice the patch
candidates already go through, one level up. The patch carries what it left
undrawn so the two readings can be compared at all.

**Open half-edges 10 → 3**, for 0.15 s: the second reading is only built for a
face that both has a seam and failed with the first, which on this assembly is
four of 11,212.

### The ball a cross-section actually is, rather than the one it says

One blend edge was refused because no ball of the stated radius reached both
its ends: it missed by thirteen microns on a millimetre fillet, against a body
accurate to ten. Loosening the threshold would have read it, and would have
read wrong ones too.

The ball is not free. It touches the near surface at one end, so its centre is
on that normal, and it must also pass through the other end. Two facts, one
unknown: with `d` the vector between the ends and `n` the normal,
`|d + n·r| = r` gives **`r = −|d|² / (2 d·n)`** outright. So the radius is
solved rather than tested — but only after the file's own radius has been tried
and missed, and only if the two agree to within a twentieth, because the file's
number is what the blend *is* and the solve is there to absorb its rounding,
not to overrule it. Applying the solve first, before trying the stated radius,
costs two edges that the stated radius reads perfectly well.

### One tolerance for a half-metre assembly is no tolerance for a two-millimetre letter

Embossed lettering came out as blobs — the counters of the `0`s smeared shut,
the strokes melted together — while a viewer built on OpenCASCADE showed every
glyph. It was not a reading fault. `tessellate_scene` resolved the relative
deflection against **the whole scene**: 0.0004 of a 480 mm assembly is 0.19 mm,
and that one number was then handed to every face in it. Measured on the pilot
assembly: **1,217 faces are smaller than five times that tolerance, and 104 are
smaller than the tolerance itself.** A face allowed to depart from its own
shape by more than its own size cannot come out as anything but a blob. The
comment justifying it was right about parts — a small bracket should not get a
hundred times the density of the frame it bolts to — and wrong about features.

OpenCASCADE sizes each edge and each face against **itself**
(`BRepMesh_Deflection::ComputeAbsoluteDeflection`): take that shape's own
bounding box, scale the relative deflection by it, and bound the proportion
between the shape and the model to `[0.5, 2]`. That is the whole rule, and it
is why a viewer built on it shows lettering that ours smeared.

Ours now does the same, with the constant doubled so that a feature the size of
the model still gets exactly the sag that number was measured at, and with a
floor at an eighth of it so the refinement ends somewhere:

| feature | old sag | new sag |
|---|---|---|
| 480 mm (the model) | 0.192 mm | 0.192 mm |
| 100 mm | 0.192 mm | 0.160 mm |
| 20 mm | 0.192 mm | 0.032 mm |
| 2 mm (a letter) | 0.192 mm | **0.024 mm** |
| 0.2 mm (its fillet) | 0.192 mm | **0.024 mm** |

The floor is what makes it affordable. Without it a two-millimetre letter asks
for 0.0032 mm and the Parasolid mesh goes to 2.6 M triangles; at an eighth of
the model's sag it is 1.3 M, the lettering is just as sharp, and non-manifold
edges come out *better* — 89 → 16, because features are no longer being
tessellated coarser than they are.

| | triangles | open | non-manifold | time |
|---|---|---|---|---|
| STEP before | 757,437 | 0 | 19 | 3.7 s |
| STEP after | 904,979 | **0** | 19 | 3.8 s |
| Parasolid before | 765,767 | 3 | 89 | 10.5 s |
| Parasolid after | 1,309,203 | 3 | **16** | 10.3 s |

### Measuring against OpenCASCADE, and what it found

Mayo is a Qt front end over OpenCASCADE; its readers are stock
`STEPCAFControl_Reader` with no tuning, so the thing worth learning from it is
not how it reads but how it meshes. Built here (`brew install cmake ninja
opencascade qt`, then plain CMake) it gives `mayo-conv`, which converts without
a display, and that turns "their render looks right" into a file that can be
measured against ours.

Its defaults are less demanding than ours: per *part*, absolute,
`deflection = 0.004 × the part's largest bounding-box side`, `angle = 20°`
(`BRepMeshingUtils::chordalDeflectionEstimate`, quality `Normal`). On the pilot
assembly that is **549,743 triangles against our 933,217** — so wherever theirs
looked better and ours did not, the cause was never density.

Two tools make the comparison, and both read the written file rather than the
tessellator's own bookkeeping:

- `cad-export --example mesh_diff ours.glb theirs.obj [part]` recovers the unit
  scale and axis convention from the bounding boxes, then measures every vertex
  of each mesh against the *triangles* of the other, per part. Vertex-to-surface
  in the *their → ours* direction is the honest one: their vertices sit on the
  exact surface, so a distance there is our error. The other direction is
  polluted by their coarseness. Triangle **centroids** are sampled as well,
  because a chord straight through a cylinder has both its ends in exactly the
  right place and is invisible to any vertex test.
- `cad-export --example glb_audit file.glb|file.obj` counts open half-edges,
  non-manifold edges and over-long triangles per body, on GLB or OBJ, so the
  same test can be put to the reference.

### A spline is a different polynomial on every span, and we were stepping over them

The first thing the comparison found: on part `221 201 001` — a spring —
**82.6% of OpenCASCADE's vertices were more than 0.2 mm from our surface**,
worst 3.5 mm on wire 1.2 mm thick, and our bounding box was 2 mm short. Every
triangle we drew was right (0.07 mm from their surface); we simply drew far too
few, 2,769 against their 16,002.

The face is one NURBS, degree 3×3, **7 × 596 control points — 594 knot spans**
along the helix. Our interior grid was capped at 96 lines, so each line stepped
over six spans, and `adaptive_steps` could not find the shape it stepped over:
a helix returns to its own chord once per turn, so a subdivider that asks only
about the midpoint reads a full turn as flat and stops.

Sampling is now seeded from the geometry's own knot vector — `native/crates/cad-tess/
src/knots.rs`, used by both `edge::sample_params` and `face::interior_samples`.
Not every knot survives: a reader may write a flat sheet as a hundred spans, so
each break is kept only where dropping it would move the chord further than the
tolerance already in force, and the deviation is sampled at three points inside
the span rather than one, for the same reason the subdivider could not be
trusted. The even grid is still there for smooth-but-curved directions; the
breaks are merged into it.

| the spring | before | after |
|---|---|---|
| our triangles | 2,769 | 7,196 |
| their vertices over 0.2 mm from us | 82.6% | **3.2%** |
| worst | 3.510 mm | **0.341 mm** |
| bounding box | 2 mm short | matches to 0.0001 |

| whole assembly, their vertices → our surface | before | after |
|---|---|---|
| mean | 0.0278 mm | **0.0168 mm** |
| p99 | 0.700 mm | **0.316 mm** |
| p99.9 | 1.696 mm | **1.086 mm** |
| over 1 mm | 2,319 | **487** |

Triangles rose 3%, from 904,979 to 932,153; the time did not move.

### Three faults the internal bookkeeping cannot see

All three were found by measuring the written file against OpenCASCADE's, and
none of them is visible from inside a face: in every case the face drew its
whole boundary and reported itself complete.

**The seam was cut in two places.** A face closed in u is bounded by two rings,
and the strip between them is built by joining them at a cut. Where the file
began each ring is wherever its own vertex happens to sit, and the two can be a
quarter turn apart — so the strip was closed by a boundary segment running that
quarter turn straight across, which on a cylinder is a chord through the axis.
`align_cuts` now rotates every ring to whichever of its own points lies nearest
the shared cut. No point is invented: a boundary vertex is shared with the face
across the edge, and one inserted on this side alone is a crack, so the rings
still begin up to one sampling step apart — the same magnitude as the sampling.
**398 faces carried such a chord; 168 after.**

**A ruled direction was exempted too widely.** A chord along a cylinder's
ruling lies on it exactly, so the ruled direction was given no interior line —
and since interior points are the *crossings* of the two families of lines, one
direction divided into a single cell leaves the face no interior points at all.
Over a rectangular region that is right. Over a region trimmed to some other
shape the triangulation has to reach across the interior to fill it, and with
nothing in the ruled direction to meet, it reaches across the curved one
instead. The exemption now applies only where the region really is a rectangle,
and over any other region the grid is also held to an aspect ratio of four in
3D — a cylinder 72 mm long and 14 mm across otherwise gets cells thirty times
longer than they are wide, and a Delaunay triangulation of points laid out like
that answers with needles whose long edges run wherever they like. **168 → 110.**
Insisting on square rather than 4:1 buys one more face and doubles the
assembly's triangle count; 4:1 keeps 99% of the gain at 64% of the triangles.

**A minority of faces were wound inside out.** This was the big one, and it had
been mis-read as holes: of 1,820 open half-edges, **1,817 were edges whose two
triangles traverse them the *same* way** — both faces present, one of them
backwards — and only 3 bounded anything missing. A closed mesh, but a renderer
culls the wrong side, the normals point into the part, and any tool working
from winding is misled. The file's per-face sense flag is right for the great
majority and wrong for a few, and no single face can tell which it is; the
topology can. `orient_shell` two-colours each shell across its shared edges —
the constraint being that two triangles meeting at an edge must traverse it in
opposite directions — which fixes the shell up to one global flip, and the
volume it encloses decides that flip outright, since a shell wound outward
encloses a positive one. Where a group encloses nothing to measure (a sheet,
a lone face) the file's reading is kept for the greater part of it.

| open half-edges | before | after | OpenCASCADE |
|---|---|---|---|
| STEP | 1,820 | **3** | 113 |
| Parasolid | 1,919 | **3** | — |
| of which wound backwards | 1,817 / 1,916 | **0** | 0 |
| bodies enclosing a negative volume | — | **0** | — |

Six of the seven SolidWorks sample parts now close exactly; the seventh leaves
four half-edges open.

### Both readers now close

Two more faults, and both were only visible from outside a face.

**A loop that was not a spur.** A loop can run out along an edge and straight
back down it — that is how a slit is written — and the spur it makes encloses
nothing, so it is stripped. But stripping is allowed to take away what encloses
nothing, not to take away the loop: where it would, the reading that called it
a spur was wrong. Two rails of a blend that run together at their ends look
exactly like a spur locally. The boundary is now kept whole in that case. It
does not yet recover the one Parasolid offset face that fails — its two rails
still coincide in parameter space, so the triangulation comes out empty — but
the failure has moved upstream to where it belongs.

**A T-junction the topology could not see.** Two faces that share an edge in
the topology share its points and cannot crack. Two faces that share an edge
only in *geometry* — a model carrying a duplicated or collapsed edge, which
both readers see identically because both files describe it — discretise it
separately, and one may put a point where the other draws a chord. On the pilot
that left exactly one triangle missing: three points on a plane, the middle one
0.050 mm off the chord between the other two, at its exact midpoint. The
topology cannot say those two edges are one; the geometry can. `stitch_t_junctions`
splits an open edge at any mesh vertex lying on it within the tolerance that
edge was drawn to. Nothing is moved and nothing is invented — the split point
is already a vertex of the mesh — so the surface is unchanged and only the
crack closes.

| | before | after | OpenCASCADE |
|---|---|---|---|
| STEP open half-edges | 3 | **0** | 113 |
| Parasolid open half-edges | 3 | **0** | 113 |
| bodies enclosing a negative volume | 0 | **0** | 0 |

Six of the seven SolidWorks sample parts close exactly. The seventh leaves four
half-edges open around a square one micrometre on a side — and that is the
file's own: the body declares thirteen edges, four of which reach only one
face. The mesh reproduces the topology it was given.

### A spline never got the angular limit

The renders showed what no number had: the pilot's spring came out a chunky
polygonal helix beside OpenCASCADE's smooth one, at a third of its triangles.
The knot work had fixed the direction *along* the helix; the direction *around
the wire* was still coarse.

`direction_steps` sends an analytic surface to `segments_for_arc`, which applies
both limits — the chord's sag and the angle between facets. It sends everything
else to `adaptive_steps`, which applied **only the sag**. That is fine until a
small feature sits inside a large face: the spring's wire is 1.2 mm thick on a
helix 43 mm long, the face is held to a fraction of *its own* size (0.069 mm),
and seven segments satisfy the chord. Seven segments around a wire is a
heptagon. The angular limit — 8° by default, which is what every cylinder and
cone in the model already gets — asks for forty-five.

`adaptive_steps` now measures the turn between consecutive steps as well, and
stops only when both limits are met.

| the spring | before | after |
|---|---|---|
| our triangles | 5,518 | 46,102 |
| their vertices over 0.2 mm from us | 269 | **4** |
| mean | 0.055 mm | **0.018 mm** |
| worst | 0.341 mm | **0.254 mm** |

| whole assembly | before | after |
|---|---|---|
| their vertices → our surface, mean | 0.0120 mm | **0.0109 mm** |
| p99 | 0.2547 mm | **0.2488 mm** |
| our centroids, p99 | 0.5622 mm | **0.4347 mm** |
| p99.9 | 2.179 mm | **1.896 mm** |
| triangles | 1,171,564 | 1,798,962 |
| time | 4.0 s | 3.8 s |

**It costs 54% more triangles**, and the GLB goes from 30 MB to 45 MB (19 MB to
30 MB compact). That is not a quality-for-size trade being made here — it is
the same rule the analytic surfaces were always held to, finally applied to the
splines. `Options::draft()` remains for anyone who wants the smaller file.

### Looking at it

`cad-export --example render file.glb|file.obj out.png [--size WxH] [--yaw D]
[--pitch D] [--zoom F] [--at X,Y,Z] [--part NAME] [--up z]` rasterises a mesh
to a PNG with no viewer, no browser and no image dependency — the PNG is
written by hand as stored deflate blocks. `--up z` frames a CAD kernel's OBJ
the same way as our Y-up GLB, so the two can be put side by side; `--part`
narrows to one body by the digits in its name, which both writers spell
differently.

Shading is deliberately flat. Interpolated normals hide faceting, which is
exactly what these renders exist to show — and it was a render, not a
measurement, that found the missing angular limit above. The set in `renders/`
covers the assembly from each reader and from OpenCASCADE, and the spring and
the main housing from ours and theirs at matched cameras.

### The measuring tools, and one trap


- `cad-export --example mesh_diff ours.glb theirs.obj|theirs.glb [part]` — recovers
  scale and axis convention from the bounding boxes, then measures every vertex of
  each mesh against the *triangles* of the other, per part. **Their vertices → our
  surface** is the honest direction. Triangle **centroids** are sampled too, because
  a chord through a cylinder has both its ends in exactly the right place and no
  vertex test can see it. It reads a GLB as the reference as well as an OBJ, which
  is how the two readers are checked against each other.
- `cad-export --example glb_audit file.glb|file.obj` — open half-edges, non-manifold
  edges, over-long triangles and enclosed volume, per body, on GLB or OBJ. It
  separates **an edge whose two triangles are wound the same way** (a reversed face)
  from **an edge used once** (a hole); they want opposite fixes, and reading the
  first as the second cost a whole round of work here.
- `CAD_TESS_FACE_SAG` — the measured faceting of each face, on the patch that was
  *kept*, by inverting each triangle edge's midpoint back onto its own surface.
- `cad-tess --example surface_check file.stp [part]` — every other check compares
  our mesh with someone else's; this one compares it with **our own reading of the
  geometry**, which separates the two ways a mesh can be wrong. A vertex far from
  every surface in its body means the tessellation left the surface; a mesh that
  hugs our surfaces while disagreeing with a reference mesher means the surface
  itself was read wrong. It was the second that found the torus bands. Because a
  blind inversion of a spline with hundreds of spans lands wherever Newton falls,
  it takes the better of a blind solve and one seeded from a coarse sweep, and is
  therefore a **lower bound** — it accuses only when no reading puts the vertex
  near the surface. `SURFACE_CHECK_TORUS=1` adds, per torus face, how far around
  the tube the mesh was actually drawn and the radius it reached.
- `CAD_TESS_ONLY=<part>` — tessellate only the bodies whose name contains this, so
  a per-face probe prints one part's faces instead of an assembly's.
- `cad-step --example probe_point file.stp X,Y,Z [part]` — what a body has at one
  place, in world millimetres as `mesh_diff` prints them (mind the frames: the GLB
  is Y-up metres, the STEP is Z-up millimetres, so a point `[x, y, z]` from
  `mesh_diff` is `[x, -z, y] * 1000` here). It asks each body both questions at
  once — how near its *untrimmed* surfaces come, and how near its triangles do —
  which separates "the geometry is not in our reading" from "it is there and the
  face was drawn somewhere else". It put the 4.3 mm error on a named sphere in one
  call.
- `CAD_TESS_LOOPS` — every trim loop's wrap accounting: first and last `u`, the
  span, whether it closes in 3D, and the counted wrap. `CAD_TESS_CROSSING` — the
  two boundary segments a triangulation could not enforce, in both parameter and
  space; that is what named the closing edge above.
- `GLB_AUDIT_VOLUME=<name>` — what a body encloses. Volume answers a question no
  distance can: whether a disagreement is material we added or material we left
  out. Ours is stable under refinement (draft 61435.3, normal 61475.4, fine
  61474.7 mm³ on `202 201 016-51`); OpenCASCADE's climbs toward it (61268.8 →
  61338.0) and its finer mesh has 234 open edges, so its volume is the less
  trustworthy of the two.
- `MESH_DIFF_DUMP=<mm>` — the coordinates of every disputed point, not just the
  percentage. A percentage says how much of a part is in dispute; only the
  coordinates say **which feature**, and on the bolt they put the whole
  disagreement in one 0.9 mm slab. Note that `mesh_diff`'s part filter now narrows
  *after* aligning on the whole model: one part's bounding box does not pin down
  the axis mapping, and narrowing first reported a fault that was only the
  alignment's.

**The trap:** `cargo build --release` does **not** build examples in this workspace.
Every measurement must follow `cargo build --release --examples`, or it is taken
against a stale binary — which produced three contradictory readings here before it
was noticed.

### The margin that starved the slivers

Interior samples are kept clear of the boundary so one cannot land on a
constraint and split it — the neighbouring face, whose samples are its own,
would not split its copy, and the two meshes would disagree along a shared
edge. The margin was **half the boundary's median step**, which is generous,
and generosity had a price: a region thin compared with its boundary's spacing
gets no interior points at all and is triangulated by chords running its whole
length. That is the sliver beside a corner where a spline's parameterisation
collapses, and it was 86 of the 94 faces carrying an edge more than twenty
times their tolerance from their surface.

Sweeping the fraction down, on the pilot:

| fraction | faces with such an edge | open half-edges | non-manifold | triangles |
|---|---|---|---|---|
| 0.5 | 86 | 0 | 11 | 1,798,962 |
| 0.125 | 78 | 0 | — | 1,880,192 |
| 0.0625 | 65 | 0 | 10 | 1,929,606 |
| 0.03 | 19 | 0 | 9 | 1,975,156 |
| **0.015** | **6** | **0** | **9** | 2,017,614 |
| 0.005 | 6 | 0 | 9 | 2,078,678 |

**The mesh never opens.** The fear the margin was guarding against does not
materialise, because a boundary point's *position* is cached and shared
whatever the triangulation does with the parameters around it. Below 0.015
nothing further is bought. The margin is kept rather than removed, because a
sample landing exactly on a constraint is still worth avoiding.

This is the largest single gain since the knot work:

| | before | after |
|---|---|---|
| their vertices → our surface, mean | 0.0109 mm | **0.0084 mm** |
| p99 | 0.2488 mm | **0.1035 mm** |
| p99.9 | 0.8205 mm | **0.6423 mm** |
| over 0.2 mm | 5,252 | **2,989** |
| over 1 mm | 138 | **113** |
| our centroids, p99 | 0.4347 mm | **0.3801 mm** |
| faces over 20× their tolerance | 86 | **6** |
| non-manifold (STEP) | 11 | **9** |
| triangles | 1,798,962 | 2,017,804 |
| time | 4.1 s | 4.6 s |

Measured faceting, surface-meshed faces only:

| | faces | worst | mean |
|---|---|---|---|
| plane | 2,025 | 0.0018 mm | 0.000004 mm |
| cylinder | 2,956 | **0.258 mm** | 0.0124 mm |
| torus | 1,869 | **0.845 mm** | 0.0189 mm |
| cone | 1,020 | **0.854 mm** | 0.0243 mm |
| nurbs | 2,924 | 4.232 mm | 0.0370 mm |
| sphere | 385 | 5.862 mm | 0.0535 mm |

The six that remain: two spheres of 100 triangles each at 5.86 mm — a narrow
polar cap on a sphere of 400 mm radius, fanned from its pole in a single row —
one spline face at 4.23 mm, two tori and one more spline under 0.76 mm.

### The one edge that ran away

`CAD_TESS_SAG` measures every edge's chain against the curve it stands for.
Across the pilot's 26,535 edges exactly one exceeded 0.2 mm — and it exceeded
it by **4.9 metres**. It is `#102613`, an edge whose curve is
`ELLIPSE(#252081, 1946.437, 1.5)`: semi-axes of 1.9 m and 1.5 mm, centred 700 mm
outside the model. Its two vertices are 1.5 mm apart.

A conic that elongated inverts two vertices a millimetre apart to nearly the
same parameter, and the reader then cannot tell a hair-thin arc from the whole
turn. It took the whole turn. `repair_runaway` caught the result — a chain
reaching 4.9 m from a 494 mm body — and replaced it with the chord between the
vertices, which on that part of that ellipse is right to within microns, since
the curvature radius there is `a²/b` ≈ 2.5 × 10⁶ mm. So the *mesh* was already
correct.

What was not correct was the chain it handed on: the points were the chord, the
parameters still claimed a full turn. Anything reading the parameters — the
probe that found this, a pcurve, a later repair — was told a story the points
contradict, and that is how a 4.9 m reading survived in a model whose worst real
edge is 0.12 mm.

The repair now asks the curve where its own vertices sit before giving up on
it: sweep the natural range for the parameter nearest each vertex, take the
short arc between them, and keep it if it stays inside the body and reaches
both vertices. Only when that fails does it fall back to the chord — and then
the parameters say chord, not turn.

**Every edge of the model is now within 0.121 mm of its curve**, against a
tolerance of 0.198 mm. Nothing over 0.2 mm remains.

`sample_params` also now checks the closed-form segment count against the
criterion it was meant to satisfy, rather than trusting it: an analytic count
is faster and better spaced than bisection, but only when the closed form is
given a radius it can make sense of. And `analytic_segments` takes the absolute
value of an ellipse's semi-minor axis, which it previously did not — a file is
free to write either axis negative.

### A sphere's latitude is its own, not the radius's

`Surface::invert` for a sphere read the latitude as `asin(d·axis / R)` — the
point's height above the equator measured against the **nominal** radius. That
is only the right question when the point is on the sphere, and inversion is
asked about points that are not, every time a boundary point arrives from a
neighbouring face's curve or a chord's midpoint is tested. The error is
`(1 − |d|/R) / cos(latitude)`: nothing at the equator, unbounded at the pole.

It now reads `asin(d·axis / |d|)`, which is the direction's own latitude.

| spheres, 393 faces | before | after |
|---|---|---|
| worst faceting | 5.862 mm | **0.250 mm** |
| mean | 0.0535 mm | **0.0056 mm** |

Both of the two worst faces left in the model were this, and neither was a
meshing fault at all: a triangle 0.25 mm off a 400 mm sphere was being read as
5.86 mm off. The same shape of mistake as the runaway ellipse above — the mesh
was right and the instrument was wrong — which is why every figure in this file
is now taken from at least two independent measurements before it is believed.

The faceting probe also seeds the midpoint's inversion from one of the chord's
own endpoints. A surface that passes close to itself — which a helical sweep
does once per turn — otherwise inverts the midpoint onto the *neighbouring*
coil and reports the gap between coils as faceting.

### Drawing what the materials say

The renderer shades from the file's own materials: base colour, metalness and
roughness per primitive, a specular lobe sized by the roughness, and a cheap
sky sampled along the reflected direction so a metal has something to reflect.
Two punctual lights leave a metal black everywhere but its highlight — a metal
has no diffuse term — and the difference between the steel and the paint is
most of what the material work recovered, so it has to be drawn rather than
averaged away. `--grey` turns it off when the question is shape.

The pilot's fourteen materials come out as the library and the STEP colours
describe them: `steel_555759` metallic at roughness 0.05, `aluminium_D1D1D1`
metallic at 0.22, eleven paints as dielectrics at 0.55, two rubbers at 1.0. In
the render the gear and its flange read as bare metal against the dark painted
castings, and the sensor is green.

### The seam was the one line nobody seeded

The largest genuine faceting figure left — 4.23 mm on one spline face, held to
0.07 mm — turned out to be the spring again, and looking at it down its own axis
showed it plainly: **two cones spanning the inside of the coil**, fanned from a
corner of the seam.

Five attempts to fix it as a *grid* problem all failed, measured:

| attempt | result |
|---|---|
| drop parameter-degenerate triangles by area | chords 94 → 99–104 |
| drop them by needle height | chords 94 → 103, OpenCASCADE mean 0.0109 → 0.0115 |
| snap the boundary's parameter jitter | chords 94 → 92, mean 0.0109 → 0.0124 |
| size each direction on its worst line, not its middle | no change, +108,000 triangles |
| space the grid by arc length | no change, +32,000 triangles |
| grade the grid by the fastest line | no change, +35,000 triangles |

They failed because the grid was never the problem. Reading the offending
triangle once more: two of its corners are *adjacent interior points* and the
third is a distant point on the **seam**. The interior grid was fine. The seam
column beside it was not.

`v_steps` sizes the seam by `adaptive_steps` alone — an even count, doubling,
stopping at 256. That is the same blindness the knot work fixed for the edges
and for the interior grid, and the seam was simply never given it: the spring's
face runs 594 knot spans along its helix, so its seam column had fewer than half
the points its interior grid had, and the triangulation bridged the gaps with a
fan reaching a twentieth of the way along the spring.

`seam_segment` now merges the surface's own breaks into its ladder, exactly as
`interior_samples` does, and both walks read one ladder forwards or backwards so
the two columns stay bit-identical.

| | before | after |
|---|---|---|
| worst spline faceting | 4.233 mm | **1.543 mm** |
| spline mean | 0.0371 mm | **0.0361 mm** |
| the spring's centroids over 1 mm from OpenCASCADE | 836 | **388** |
| its centroid p99 | 1.442 mm | **0.845 mm** |
| assembly centroids over 1 mm | 5,797 | **5,454** |
| triangles | 2,019,472 | 2,020,032 |

Five hundred and sixty triangles. The cones are gone.

### Every line on a surface, seeded

The seam fix closed the last place a line was built from a surface without its
breaks. The four places that build one, and what each gets:

| line | function | seeded |
|---|---|---|
| a boundary edge | `edge::sample_params` | yes |
| the interior grid | `face::interior_samples` | yes |
| a periodic face's seam column | `face::seam_segment` | yes |
| an untrimmed face's whole domain | `face::full_domain_loop` | yes |

The last changed nothing measurable here — no spline face in this assembly has
no trim loops at all — and is in for consistency: a file that does write one
would have been stepped over exactly as the seam was.

### Refine the triangles that missed, not the cells that might

A parameter grid can only be told to be finer **everywhere**. Six ways of
telling it were built and measured — probe the worst line, probe the ends,
space by arc length, grade by speed, and subdivide failing cells on four
criteria — and the best left the assembly's worst face where it was. They share
one flaw: whatever the grid is asked for, it is asked for the whole face, and
the spring needs sixteen hundred lines in its last hundredth where ninety-six
serve all the rest.

A **finished triangulation** answers a better question: which of *its own*
edges left the surface. `refine_patch` splits those and only those, rebuilding
each triangle from however many of its three edges were split — red-green
refinement, three rounds, bounded. It needs no inversion: the parameters ride
along with the patch, so the midpoint is exact.

**A boundary edge is never split** — its points are shared with the face across
it, and a vertex in the middle of the neighbour's edge is a crack. **A
triangle's middle can be split freely**, which fixes a triangle whose three
sides hug the surface while its interior bows.

Alongside it, `interior_samples` subdivides the grid cells that fail — the
surface at a cell's middle against the quad its four corners span. Four
criteria were measured; the plain one wins:

| cell criterion, and what is added | faces over 20× | cone worst | sphere worst |
|---|---|---|---|
| **middle against the corner quad, centre point** | **3** | **0.854 mm** | 0.250 mm |
| four sides, centre point | 4 | 0.854 mm | 0.250 mm |
| sides and both diagonals | 20 | 4.987 mm | **0.059 mm** |
| the same, adding the cell's edge midpoints | 19 | 1.699 mm | 0.059 mm |
| middle against the quad, edge midpoints too | 3 | 0.854 mm | 0.250 mm |

The diagonals find a real thing — a saddle, which is what a sweep makes where
it runs out — but they scatter points where the triangulation then reaches
ninety degrees across a cone to find them a neighbour, and the agreement with
OpenCASCADE gets worse: p99 0.103 mm → 0.120 mm.

### The last face: a window measured in metres against a curve indexed by samples

The one Parasolid face of 11,212 that failed outright now meshes, and the cause
was in the reading, one layer below where six turns of searching had looked.

Its two boundary edges carry `TRIMMED_CURVE`s whose basis has no closed form,
so each is lowered by **sampling** into a polyline indexed `0..n`. The window
that trims it is in whatever the *source* was parameterised by, and for these it
is **arc length**:

```
edge 259   edge range [0, 55]          the polyline's own index range
           base       polyline, natural [0, 55]
           trim       [0, 0.015909]    15.9 mm of arc, i.e. the whole curve
```

Measured over the 245 such curves here, the window's span divided by the
polyline's own 3D length has a median of **0.9972**. It is a length, not an
index. Applied as an index window, `[0, 0.0159]` names three ten-thousandths of
the curve: both rails sampled to two points, both chains were overwritten by
the edge's vertices, and the loop came out a spur bounding nothing.

**A window narrower than a single segment of a many-segment polyline is not an
index window**, so the curve is used whole — which is what the edge's own range,
`[0, 55]`, says. Two SP_CURVE cases with the same shape (`[-1, 0]` against
`[0, 1]` on a closed edge, where `-1` is the writer saying *all the way round*)
are handled the same way.

**Measured and rejected:** walking the arc-length window exactly onto the
index. It is the more faithful reading and it is worse — trimming a rail to a
sub-range leaves a chain that no longer reaches the edge's own vertices,
`discretise` pins the ends to them, and the distortion opened **fifty**
half-edges where taking the curve whole opens five. Restricting the exact
mapping to the quarter of windows that genuinely name a part changes neither
count and moves the two readers apart, 0.0139 mm to 0.0152 mm.

### Two chain points a micron apart are one point

A chain point closer to its neighbour than the tolerance carries nothing
anything downstream can read, and the pair is exactly what a triangulation
turns into a sliver. On this assembly a handful at one corner of a 256 mm body
left several faces meeting in a way no orientation can satisfy — sixteen
unsatisfiable constraints in `orient_shell`, and a 4 µm tangle of reversed and
non-manifold edges.

Merging them is safe precisely because it happens in `edge::discretise`: the
chain is built once and both faces along the edge receive it, so neither can
split what the other merged. The ends are never dropped — they are the vertices
two faces have to agree about to the bit.

**How far apart is "the same point" takes two bounds, and the tighter wins.**
The file states a tolerance for each edge and each of the pair carries it, so
points within a small multiple of it cannot be told apart by anything the file
says. And the mesh may leave the curve by the sag anyway, so a pair closer than
a fraction of that carries no shape it could show. Neither alone is right:

| bound | STEP non-manifold | Parasolid non-manifold |
|---|---|---|
| ¼ × tolerance | 2 | 13 |
| ½ × tolerance | 1 | 12 |
| 1 × tolerance | 1 | 12 |
| 2 × tolerance | 0 | 13 |
| 5 × tolerance | 0 | 7 — but 0.05 mm, twice the finest sag |
| **min(4 × tolerance, ½ × sag)** | **0** | **11** |

Five times the tolerance clears more, and it is 0.05 mm on a mesh whose finest
sag is 0.025 mm — it would discard shape the mesh is meant to show. Taking the
smaller of the two bounds stays inside both statements: it never drops more
than the file admits is uncertain, and never more than the mesh would smooth
away regardless.

| | before | after |
|---|---|---|
| STEP open half-edges | 0 | **0** |
| STEP non-manifold | 9 | **0** |
| Parasolid open half-edges | 5 | **0** |
| Parasolid non-manifold | 8 | 11 |
| every edge within its sagitta budget | yes | **yes, both readers** |

**The STEP mesh is now closed and manifold throughout** — no open half-edge, no
edge shared by more than two triangles, no body inside out — at 2,067,742
triangles over 11,214 faces.

**Measured and rejected:** a per-patch orientation pass, colouring a face's own
triangles before colouring the shell across its faces. Keyed on position it is
unsound — a periodic face's two seam columns hold the same points under
different indices, so triangles either side of the seam read as adjacent and
half the patch is flipped, taking the assembly from zero open half-edges to
247. Keyed on indices it is sound and never fires: no face in either model
disagrees with itself.

### Why a rebuilt face cannot be measured against its surface

Sixty faces of 11,214 are rebuilt from their boundary rather than evaluated
from their surface, and eight carry an edge more than twenty times their
tolerance from it. Both facts follow from one thing. `blend_patch` is offered
exactly where the surface path leaves boundary undrawn — the boundary's
parameter image folds or pinches — and its whole value is that **it never asks
the surface anything**; it does not even receive it. Measuring such a patch
against the surface answers a question the patch was built to avoid, and
projecting the rebuilt interior back onto it by inversion (tried, reverted)
reintroduces the parameterisation that failed, on the faces where it failed.

### Where both readers stand

| | faces meshed | open | non-manifold | inside out | triangles | time |
|---|---|---|---|---|---|---|
| STEP | **11,214/11,214 (100%)** | **0** | **0** | **0** | 2,067,742 | 7.8 s |
| Parasolid | **11,212/11,212 (100%)** | **0** | 11 | **0** | 2,083,804 | 13.0 s |
| OpenCASCADE, same file | — | 113 | 130 | 0 | 549,171 | 19.7 s |

Measured faceting, surface-meshed faces only:

| | faces | worst | mean |
|---|---|---|---|
| plane | 2,025 | 0.0018 mm | 0.000004 mm |
| sphere | 392 | 0.250 mm | 0.0055 mm |
| cylinder | 2,956 | 0.251 mm | 0.0118 mm |
| torus | 1,869 | 0.845 mm | 0.0176 mm |
| cone | 1,020 | 0.854 mm | 0.0243 mm |
| nurbs | 2,924 | 1.543 mm | 0.0334 mm |

Every edge of the model is within 0.121 mm of its curve, and no body encloses a
negative volume. All seven SolidWorks sample parts mesh 100% of their faces;
six close exactly with no non-manifold edge at all, and the seventh's four open
half-edges are the file's own topology.

**The two readers agree with each other to 0.0103 mm** over 3,080,815 sampled
points — not one point of either mesh stands 0.05 mm from the other's surface —
and they share no code from the file down to the IR.

Against OpenCASCADE, their vertices to our surface, over the session:

| | start | knot | seam+winding | angular | clearance | sphere+seam | cells+refine | trim+merge |
|---|---|---|---|---|---|---|---|---|
| mean | 0.0278 | 0.0168 | 0.0120 | 0.0109 | 0.0084 | 0.0084 | 0.0081 | **0.0081 mm** |
| p99 | 0.700 | 0.316 | 0.255 | 0.249 | 0.104 | 0.104 | 0.101 | **0.102 mm** |
| over 1 mm | 2,319 | 487 | 138 | 138 | 113 | 113 | 113 | **113** |
| STEP open half-edges | 1,858 | 1,858 | 0 | 0 | 0 | 0 | 0 | **0** |
| STEP non-manifold | — | — | 12 | 11 | 9 | 9 | 9 | **1** |
| faces meshed, Parasolid | 99.99% | 99.99% | 99.99% | 99.99% | 99.99% | 99.99% | 99.99% | **100%** |
| worst face faceting | — | — | — | 41.7* | 5.86* | 1.54 | 1.54 | **1.54 mm** |

\* instrument faults, not mesh faults — a rebuilt patch measured against the
surface it does not use, and a sphere inversion dividing by the nominal radius.

### The eleven that are left, named

Parasolid's remaining eleven non-manifold edges are not slivers and not the
file's own structure. Naming them took one probe:

```
[nonmanifold] 204 201 013-51: used 4 times by 1 distinct faces [119];
                              the file gives them 0 repeated edges
              face 119 nurbs meshed from its surface
```

**Used four times by one face.** A surface gives every interior edge two
triangles, one either side; four means the patch is lying on itself — its
parameter region folded, and the fill covered the same piece of surface twice.
And the file gives that face **no repeated edge** in its bounds, so it is not
describing a slit there. The fold is ours.

Twelve faces are involved: seven splines meshed from their surface, four
degree-one grids and one offset rebuilt from their boundary. The longest edge
is 0.35 mm and most are under 0.1 mm. The STEP reader has none — its own
lowering of the same geometry does not fold.

**Two readings ruled out, both of which looked likely:**

- *Not the file's own slit.* The faces this catches have **no repeated edge**
  in their bounds, so the model is not describing a surface that touches
  itself there.
- *Not a periodic spline read as clamped.* Twenty-two of the 1,338 spline
  surfaces in this file are periodic in u, and the obvious suspicion is that a
  periodic form needs its control points wrapped before a standard evaluator
  can use it. Checked directly — every surface declared closed in u brings the
  two ends of its valid domain to the same point, to under a micron, all 22 of
  them. The reading is right; the wrap is already in the poles.

**Measured and rejected:** counting self-overlap as a defect when choosing
between a face's candidate triangulations. It is the right *idea* — a candidate
that lies on itself should lose to one that does not — and it changes the
choice on a handful of faces for about 1,800 triangles, but the non-manifold
count does not move: the alternatives on offer fold in the same place. The
diagnostic is kept (`face::self_overlaps`, reported under `CAD_TESS_WIND`); the
scoring change is not.

The diagnostic also says the fold is wider than the audit sees: individual
faces lie on themselves along 4, 6, 9, 15 and 48 edges, where the finished mesh
reports eleven in total. Most of the overlap is inside a face and coincident to
within the weld, so a mesh audit cannot count it, but it is there. That is the
next thread, and it starts in `cad-xt`: the STEP reader's lowering of the same
geometry does not fold at all.

### Two rings bound two bands, and the file says which one

A face on a torus is bounded by two circles that each run the whole way round
the tube's axis. Those same two circles bound **two** bands: the one that
crosses the tube's outermost ring, where a flange rim bulges out, and the one
that crosses its innermost, where a fillet tucks in. Nothing in the topology
distinguishes them — both bands meet both edges, both close, both triangulate
without complaint — so a mesh can be built on the wrong one and every
structural check will pass it.

The rule that picks the right band was already written in `wrapped_region`, and
it could never fire. It reads:

```rust
if lower.wrap < 0 && upper.wrap > 0 { swap }
if lower.wrap > 0 && upper.wrap < 0 && mean_v(lower) > mean_v(upper) { upper.v += pv }
```

but forty lines earlier every ring had been turned to run the same way:

```rust
if r.wrap < 0 { r.uv.reverse(); r.xyz.reverse(); r.wrap = -r.wrap; }
```

After that normalisation both `wrap`s are positive, so neither condition can
hold, and the band was whatever `mean_v` sorting happened to give. The
direction is now kept in a separate field, `travel`, before the rings are
turned.

The rule also has to be read against the *face's* normal, not the surface's.
"Walking a boundary with the normal up keeps the face on the left" states the
sense; where `same_sense` is false the two normals point opposite ways and the
file's loops run the other way round. Applying the rule without that term was
measured and is worse than not applying it at all: mean against OpenCASCADE
0.0081 → 0.0175 mm, points over 1 mm 113 → 1,145, one part with 59.9% of its
vertices in dispute and a worst case of 14.7 mm. With the term, every one of
those numbers improves instead.

**How it showed.** A flanged bolt, 28 faces, not a spline among them. Our mesh
sat on our own surfaces to 2 nm — so the tessellation was faithful — and yet
19.1% of its vertices were more than 0.2 mm outside OpenCASCADE's surface, and
its finer mesh did not change that. The disagreement was confined to a slab
0.9 mm thick spanning the full head diameter: one feature, the flange rim.
`part_probe` named it a torus of major 8.4 and minor 0.6, whose outermost ring
is therefore at radius 9.0. Our mesh reached 8.6052 — exactly where the file's
own vertices stop — and a probe walking the tube found the face drawn from
±70° *away* from the outer ring rather than through it, tucking in to 7.8.

**Measured on the pilot assembly**, against OpenCASCADE's own mesh of the same
STEP file, their vertices to our surface:

| | before | after |
|---|---|---|
| mean | 0.0081 | **0.0054 mm** |
| p99 | 0.1016 | **0.0613 mm** |
| p99.9 | 0.6423 | **0.2134 mm** |
| over 0.05 mm | 9,262 | **6,108** |
| over 0.2 mm | 2,979 | **494** |
| over 1 mm | 113 | 113 |

and our vertices to their surface: mean 0.0249 → **0.0198 mm**, p99 0.3155 →
**0.1546 mm**, points over 0.2 mm 46,123 → **17,261**. Every part that had a
fifth or more of its points in dispute — four bolts, four washers, two more
small parts — left the list entirely. Structure is unchanged: 11,214 of 11,214
faces, no open half-edge, nothing non-manifold, no body inside out.

The two readers, which had agreed to 0.0103 mm, now agree to **15 µm at worst**
— mean 0.0000, p99 0.0004 mm, not one of 3,065,569 points as much as 0.05 mm
from the other reading's surface.

### A ring is a ring however much ground it covers on the way round

The largest single error left in the model was one face: a spherical cap
whose mesh sat **4.315 mm** from its own sphere against a sag budget of
0.036 mm, 120 times over. It carried 89 of the 113 points where OpenCASCADE's
mesh was more than a millimetre from ours.

It was not meshed from its surface at all. `[cand]` showed the surface reading
leaving **eight cracks** against the boundary rebuild's none, so the chooser
took the rebuild — and a rebuild spans a cap with a lid. The eight cracks came
from one crossing, and `CAD_TESS_CROSSING` named it:

```
[crossing] uv (8.035494,-0.749384)-(1.752309,-0.749384)
           xyz (-36.1729, -5.6, -17.2685) (-36.1729, -5.6, -17.2685)
```

Both ends of that segment are **the same point in space**, and its two
parameters are a whole period apart. It is the loop's own closing edge, and it
runs as a chord straight across the domain, crossing everything else in the
face.

The loop reached that state because `wrapping` had been emptied for it. One
wrapping loop that is not a *bare ring* is taken to close itself — the case
where the file put both seam edges in the loop, so the polygon is already
complete — and bareness was judged by area: a loop sweeping more than a
twentieth of its own parameter box is not a ring. This loop wanders in `v` as
it goes round, so it sweeps plenty of area, and was read as self-closing.

Area is the wrong question. A loop whose first and last points are the same
point in space, a period apart in parameter, has a closing edge that spans the
domain no matter what it did in between. Where the file supplies both seam
edges, the loop returns to its own `u` and `net_wrap` is zero, so it never
reaches this test at all. The condition is now: a single wrapping loop closes
itself unless **its ends meet in space**, and then only on a surface that has a
point to close onto — a sphere's pole, a cone's apex, a revolution's axis. A
spline's domain edge is an ordinary curve and a ring closed onto it would be a
lid the model does not have.

**Measured**, their vertices to our surface, on the same STEP file:

| | before | after |
|---|---|---|
| mean | 0.0054 | **0.0047 mm** |
| p99 | 0.0613 | **0.0601 mm** |
| p99.9 | 0.2134 | **0.1702 mm** |
| max | 4.3045 | **3.2125 mm** |
| over 0.2 mm | 494 | **345** |
| over 1 mm | 113 | **20** |

and on the Parasolid twin, points over 1 mm 148 → **53**, worst 4.21 →
**3.21 mm**. The part that carried it, `202 201 016-51`, went from 117 points
over 0.2 mm with a worst of 4.305 mm to **12 points with a worst of 0.389 mm**.

**What it cost.** The STEP mesh went from no non-manifold edge to five, and the
Parasolid mesh from eleven to fourteen. Turning the exception off returns both
counts and the triangle count to what they were, so the change is what made
them. The next section is what they turned out to be, and it closes most of
them.

A note on reading that report, because it cost a wrong conclusion here first:
`CAD_TESS_WIND` lists folds per solid at tessellation time and named spline
faces of `204 201 013-51`, while `glb_audit` on the written file named two
0.5 mm balls in another body entirely. `glb_audit` is the one that counts —
most tessellation-time folds are welded away before anything is written, and
the five that survive are not the ones the earlier report lists first.

### A facet laid twice is a facet the mesh cannot carry

Of the five non-manifold edges the last change left, four were one defect seen
twice — a 0.5 mm ball and its mirror image, faces 67 and 72 of
`205 221 011_oa_1`. `CAD_TESS_FOLD` reported the shape of it exactly:

```
[fold]   2 of 102 triangles are a second copy of one already laid
[fold]     triangle 41: 60@[-16.1687,-0.3724,9.8182] 61@[...] 55@[...]
[fold]     triangle 42: 62@[-16.1687,-0.3724,9.8182] 55@[...] 63@[...]
```

Triangles 41 and 42 stand on the same three points under different vertex
numbers, wound opposite ways. It happens where a strip closes onto a pole: the
two seam columns carry identical positions *by design*, so the two sides of the
seam agree to the bit, and as the strip narrows to nothing the last triangles
between them come out from both columns. The pair covers exactly what one of
them covers, and every edge they share reads as used four times.

`lay_each_facet_once` now runs on whichever reading a face keeps. Two vertices
standing at one point have to be recognised as one before their triangles can
be compared, and rounding alone will not do it — the two are computed by
separate routes and agree far inside a micron, but a pair either side of a bin
edge rounds apart — so each position takes the identity of the first found in
its own bin or any bin touching it. **Non-manifold edges: STEP 5 → 2,
Parasolid 14 → 13.** Nothing else moved: 11,214 of 11,214 faces, no open edge,
the same triangle count, the same distances.

The rule holds for the patch a face keeps, not after every later stage.
Applying it again after `stitch_t_junctions` was measured and is wrong: it
drops two triangles that stand on the same welded points but serve different
neighbours, and **six edges open up**.

**Two more, measured and reverted.**

*Making a wrapping ring monotone in u.* The strip builder reads a ring as a
monotone walk, and an inversion produces small backward steps where a trim
curve runs nearly along a meridian — ten of sixty-one steps on the sphere that
started this, the largest by 0.0064 rad. Nudging each backward step forward by
a thousandth of the ring's own median step, without moving any 3D point,
changed the assembly's triangle count and fixed nothing: non-manifold stayed at
five.

*Cutting a repeated boundary point only when the parameters repeat too.* The
remaining two edges trace to a crossing whose segment runs from `(u₀+2π, π/2)`
to `(u₀, 1.4395)` — from the pole at one end of the domain to the other seam
column, a chord straight across. It is there because `close_strip` collapses a
run of 3D-coincident points to the first of them, and a pole is a whole row of
one point spread across a period of `u`. Requiring parameter coincidence as
well keeps the row, and the strip's top edge with it. It kept the mesh closed —
still no open edge — and cost 20,000 triangles and a great deal of shape:
points over 1 mm against OpenCASCADE **20 → 130**, our own worst excursion
3.31 → **15.67 mm**. Reverted. The pole row's collapse is load-bearing; the
crossing it causes needs a different answer.

### A ring that already reaches the pole does not need closing onto it

The last two non-manifold edges in the STEP mesh — a 0.5 mm ball and its
mirror, faces 67 and 72 of `205 221 011_oa_1` — came from the seam path taking
a cap it should not have. `CAD_TESS_LOOPS` printed the fact the six earlier
attempts had all been working around:

```
a wrapping ring on a sphere: v -0.004561281..1.570795776,
domain v -1.570796327..1.570796327, nearest approach to a pole 5.507e-7
```

**The ring's own boundary reaches the pole.** Closing it onto an apex therefore
adds a second copy of a point the boundary already carries; the strip pinches
to nothing there, and the polygon crosses itself. Three cracks, and the
boundary rebuild wins the face — which is where the doubled facet and the
non-manifold edge came from.

The condition on the previous section's exception now has a third term: a
single wrapping loop takes the seam path when its ends meet in space, *and* the
surface has a point to close onto, *and* **the ring does not already reach that
point**. Measured in space, not in parameter: the ring here stops 5.5e-7 of a
radian short of the pole, which on a 0.5 mm sphere is three ten-thousandths of
a micron, and no threshold in `v` reads that as touching without also catching
rings that plainly are not. The distance to the surface's own degenerate point,
against the sag, says it directly.

**STEP is now closed and manifold throughout: 11,214 of 11,214 faces, no open
half-edge, no edge shared by more than two triangles, no body inside out** —
with the geometry unchanged, still 345 points over 0.2 mm and 20 over 1 mm
against OpenCASCADE. Parasolid drops from thirteen non-manifold edges to
eleven.

**Two more measured and reverted.**

*Cutting the cap where its ring is farthest from the pole.* The chord that
closes a strip runs from the apex to the first point of the seam beside it, so
it passes over any part of the ring nearer the apex than one seam step; cutting
the ring at its farthest point should give that chord the whole face's depth to
clear. It opened three edges in another body, and on the ball it did nothing at
all — the probe showed the cut was already at point 0, the farthest there is.
That is what sent the search to the ring's own `v` range and found the answer
above.

*Widening "reaches the point" from the sag to the ring's own sampling step.*
Two Parasolid caps in `201 201 003-51` run within 0.065 mm of their pole
against a sag of 0.0247 — close enough that the closing chord crosses them, but
2.6 times too far for the sag test. Judging closeness by the ring's median step
instead catches them, and also catches caps that genuinely do need closing: the
big sphere of the previous section goes back to a rebuild and its 4.3 mm lid
with it. Points over 1 mm against OpenCASCADE **20 → 109**. Reverted.

### Two whole bodies were leaving without a word

The Parasolid reading had 44 bodies where the STEP reading had 46, and the two
missing ones — `115.D684` and `115.D783` — were not missing from the file.
`parse_xt` counts 46 bodies in it. They were being dropped in `cad_xt::to_scene`
by

```rust
if solid.faces.is_empty() { continue; }
```

which is the one thing this project's own rules forbid: an unknown skipped in
silence. A body that lowers to nothing now says so, and the report named them
at once.

**Why they lowered to nothing.** Each is an O-ring: a single toroidal face
whose only bound is a vertex. `lower_face` refused it —

```rust
if bounds.is_empty() { return Err("face has no usable loops".into()); }
```

— although a face that trims nothing is a real thing on a surface closed in
both directions, and the tessellator has meshed it from the whole parameter
rectangle since `full_domain_loop` was written. The refusal now applies only
where the surface is *not* closed in both `u` and `v`, which is where a face
with no bounds really would be unbounded.

**Measured.** The Parasolid reading now covers the file: **46 bodies and 11,214
faces, the same as the STEP reading**, all of them meshed. Against
OpenCASCADE's mesh of the same assembly, their vertices to our surface:

| | before | after |
|---|---|---|
| mean | 0.0101 | **0.0085 mm** |
| p99 | 0.1742 | **0.1133 mm** |
| p99.9 | 0.6789 | **0.5419 mm** |
| over 0.2 mm | 3,926 | **2,373** |
| over 1 mm | 53 | **34** |

Structure is unchanged — no open half-edge, eleven non-manifold edges, no body
inside out — and lowering still takes 7.5 seconds. The two readers still agree
to 15 µm in both directions, over 3,076,970 and 3,054,333 points.

**Measured and reverted: lowering a `BLENDED_EDGE` as a face's own surface.**
Four faces carry surface type 56 directly rather than through a `BLEND_BOUND`,
and `blend_surface` — the rolling-ball reading already used for curves on a
blend — would lower them. Adding that arm took lowering from **7.4 to 99.7
seconds**, left one of the four unmeshable, and opened two edges that were
closed. Those faces fall back to a Coons rebuild, which is within 15 µm of the
STEP reading of the same fillets, so the reading loses nothing by it.

### What the eleven actually are, and why the obvious fix is worse

Named at last, with the probe extended to print the face behind each `[bounds]`
line. Faces 157 and 158 of `205 211 013-51-oa2` are two consecutive tiles of a
blend strip:

```
face 157 grid | circ x8[0.27]#488  poly x2[0.12]#489  circ x9[0.29]#490  poly x2[0.13]#408
face 158 grid | circ x9[0.29]#490  poly x2[0.37]#491  circ x9[0.30]#492  poly x2[0.42]#411
```

They share the circular edge `#490`, and the edge the audit objects to is
0.077 mm long and lies **inside both of them** — each uses it twice. Neither
face lies on itself: `self_overlaps` is zero for both. Two adjacent patches
have simply drawn the same short diagonal across the arc they share, where the
arc's points are straight enough at 0.036 mm a segment that either side can
claim the sliver between them.

Both rebuilds sit close to the surfaces they stand for — 0.0154 mm for face 157
and 0.0322 mm for face 158, against a sag of 0.0247 — so nothing here displaces
the model. That is consistent with the two readers agreeing to 15 µm.

**Measured and reverted: making a gap-free rebuild prove itself.** The branch
every rebuilt blend face goes down accepts a rebuild that draws its whole
boundary *on trust*:

```rust
Some((0, patch)) => return Ok(patch),
```

and the measurement above shows that trust is not always warranted — face 158's
middle is a sag and a half from its grid. Requiring the departure to be within
the sag, and carrying the patch down to the ordinary comparison otherwise, made
the assembly much worse: **332 open half-edges, 178 non-manifold, five faces
unmeshable, the triangle count up by half (2.09 M → 3.21 M) and tessellation
from 5.3 to 24 seconds.** The reason is the measurement itself: inverting a
point onto a degree-one grid is the one question that grid cannot answer, so
the departure is usually a fact about the solver rather than about the patch,
and it pushes sound rebuilds into a surface path that then fails. The probe is
kept behind `CAD_TESS_REBUILD_OFF` because where the inversion does hold the
number is real.

### Where the folds come from: a boundary solved along the edge of its domain

The eleven are two different things, and the report says which:

```
used 4 times by 1 distinct faces [1625]      it lies on itself along 48 edges
used 4 times by 2 distinct faces [157, 158]
```

The second kind is a *coincidence*, not a fold. Faces 157 and 158 are
consecutive tiles of a blend strip sharing the circular edge `#490`; each is a
proper manifold patch on its own, and the edge the audit objects to is an
interior edge of each, landing at the same place because the arc they share is
straight there to 2.5 µm over 77 µm. Both rebuilds sit within 0.033 mm of the
surfaces they stand for. Nothing is folded; two interior edges have simply met
in the weld.

The first kind is a real fold, and its cause is now known. Face 1625 of
`200 201 003-51` has a parameter region **0.0009 wide in u** and 75 boundary
points. One side of it runs along `u = 0.000916` exactly; the other runs along
the domain edge `v = 1`. Along that second side the solve wandered:

```
69 uv (0.000534,1.000000)   72 uv (0.000628,0.998331)
70 uv (0.000579,0.999409)   73 uv (0.000700,0.998694)
71 uv (0.000606,0.999791)   74 uv (0.000798,0.999904)
```

A wobble of 0.0016 in v across a region 0.00038 wide in u. Where the surface
barely changes across the edge of its own domain, `v` is ill-conditioned and
the answer is noise at that scale — and the boundary crosses itself. Two of the
six points landed on 1.0 exactly, which is what all six mean.

**Snapping such a point to the bound is now done — but the question is asked of
a *run*, never of a point.** A boundary lies along the edge of the domain only
if several of its points in a row do; an isolated point that happens to pass
the tests belongs where the solve put it. Three conditions, all necessary:

* the point sits within a hundredth of the domain's own span of the bound, so
  nothing interior is ever dragged out;
* moving it there costs nothing in space, within the tolerance the geometry is
  guaranteed to — the bound simply names the same point;
* and at least **three** consecutive points qualify together.

Every one of those is load-bearing, and each was measured. Asking it of single
points puts a non-manifold edge into the STEP reading, which has none. A run of
**two** does the same — a pair either side of a bin can pass by accident, three
in a row cannot. Dropping the proximity gate and allowing ten times the
tolerance opens **215 half-edges**; tightening the cost to a tenth of the
tolerance leaves STEP at one and takes Parasolid to twelve.

**Measured, with the run of three.** Face 1625's six points all come back at
`v = 1.000000` with `u` monotone. The Parasolid reading's self-overlapping
edges fall from **87 to 37** — face 1625's forty-eight and face 662's nine gone
outright — and the mesh gets *smaller* at the same accuracy:

| | before | after |
|---|---|---|
| STEP triangles | 2,051,656 | **1,972,354** |
| Parasolid triangles | 2,090,130 | **2,072,610** |
| STEP over 0.2 mm vs OCCT | 345 | **343** |
| STEP over 1 mm | 20 | **20** |
| Parasolid over 0.2 mm | 2,373 | **2,371** |
| Parasolid over 1 mm | 34 | **34** |
| STEP non-manifold | 0 | **0** |
| Parasolid non-manifold | 11 | **11** |

Four per cent fewer triangles for the same shape, the same closed and manifold
STEP mesh, and the folds inside the Parasolid faces more than halved. The count
that ships did not move, because the surviving eleven are the *coincidence*
kind described above and not folds at all.

### The worst disagreement with OpenCASCADE is OpenCASCADE's

Its normal mesh puts a vertex **3.213 mm** from our surface on `204 201 013-51`,
and that was the largest single disagreement in the whole assembly. Rendering
both at that point settles what it is: **we draw a smooth rounded boss; their
normal mesh draws the same boss as a coarse cone of long thin triangles**, and
their *fine* mesh draws it as we do. The outlier is their faceting, not our
shape. Twenty-nine of the Parasolid reading's thirty-four points over a
millimetre sit in that same region, at three heights six millimetres apart —
the pitch of the feature.

**The one outlier that was open is now explained, and it is ours.** Against
their fine mesh, a vertex at GLB `[0.0348, 0.1794, 0.0322]` is **3.9987 mm**
from our surface — the same 3.9987 for the nearest vertex and the nearest
triangle when our own GLB is read with the `gltf` crate and asked directly.
Meshing the same file at **fine** quality closes it to **0.0133 mm**. So the
geometry is right and the hole is our faceting at the normal setting.

What is behind it: **three cone faces of `204 201 013-51` fall back to a
boundary rebuild at normal quality, and those rebuilds leave the cone by up to
7.3 mm** against a sag of 0.039 — the worst measured faceting anywhere in that
part. A Coons patch spun across a helical band on a cone does not follow the
cone. At fine quality only two do, and face 136 is not among them: its surface
reading succeeds there and the hole closes with it. The trim curves are nurb
edges, and at the normal setting they are sampled too coarsely for the surface
path to enforce them — that is the thing to fix.

**Measured and rejected: refusing a rebuild that leaves an analytic surface.**
On a plane, cylinder, cone, sphere or torus the inversion is closed form, so
"how far has this patch left its surface" is arithmetic rather than a search —
unlike the spline case, where the same test cost 332 open half-edges. Refusing
any rebuild more than eight sags off such a surface does close this hole
(3.9987 → 0.0172 mm) but opens **19 half-edges and 9 non-manifold edges**: the
rebuild was covering cracks the surface reading leaves. Allowing the refusal
only where the surface reading is nearly whole makes it never fire (two cracks
is already too few); allowing four or more opens **141 to 159** half-edges and
makes the hole *worse*, at 4.78 mm, by switching a different face. The rebuild
is not the thing to attack; the boundary sampling under it is.

**Three instrument lessons, all paid for here.**

*`probe_point` was reading a frame of my own guessing.* The writer's
`root_transform` is `(x, z, −y)·s`, so a GLB point maps back as
`(X, −Z, Y)·1000`; a permutation that merely *found a body whose box contains
the point* answered a different question and named the wrong part twice.

*Two renders of "the same place" were not.* With `--up z` the `--at` is read in
the post-swap frame, which equals ours × 1000; giving it the pre-swap
coordinate framed a different feature entirely and made a coarse boss look like
agreement.

*`mesh_diff`'s part filter kept only triangles wholly inside the part's box*,
so the part's own boundary vertices had nothing to measure against. It now
keeps triangles within a twentieth of the box's span of it. (This was not what
produced the 4 mm above — that survives the fix — but it did produce others.)

`INSPECT_AT="x,y,z"` on `inspect_glb` now answers "what does the file have at
this place" through a reader this project did not write, which is what settled
the disagreement between the other two.

### The two rings never started at the same place

`align_cuts` turns every ring of a strip to whichever of its *own* points lies
nearest the shared cut. It cannot do better: inventing a point at the exact cut
would put a vertex in this face that the neighbour's chain does not have, and
the two would part along it. So the rings begin within one sampling step of
each other — near the same parameter, not at it.

`close_strip` then runs the strip from the lower ring's cut to a period on, and
forces the upper ring's start to that far end:

```rust
let upper_start = Vec2::new(u_hi, upper.uv[0].v);
```

while the upper ring's own last point sits at `u_lo' + period`, past `u_hi`
whenever `u_lo' > u_lo`. The boundary therefore steps backwards over the seam
and then forwards again — and crosses itself. Measured before the fix: **548 of
the 6,280 faces on a periodic surface had an outer loop spanning more than a
whole period.** On cone face 136 of `204 201 013-51` the overrun was 0.056 rad,
half a millimetre at that radius, and the single crossing it caused left three
cracks, lost the face its surface reading, and handed it to a boundary rebuild
that missed the cone by millimetres.

The residue is now taken out by **sliding** each further ring in parameter onto
the first one's cut. Nothing moves in space — every boundary vertex keeps the
position its chain gave it — and the ring's own shape in `u` is untouched; only
the constant it is measured from changes.

**Measured.** Against OpenCASCADE's mesh of the same STEP file, their vertices
to our surface:

| | before | after |
|---|---|---|
| over 0.2 mm | 343 | **307** |
| **over 1 mm** | **20** | **1** |
| ours→theirs p99.9 | 0.8252 | **0.5823 mm** |
| ours→theirs over 1 mm | 2,386 | **1,706** |

and on the Parasolid twin: mean 0.0086 → **0.0083 mm**, p99 0.1132 →
**0.1107**, over 0.2 mm 2,371 → **2,253**, over 1 mm 34 → **14**, and its worst
excursion **3.2128 → 1.3864 mm**.

The two readers, which had agreed to 15 µm, now agree to **3.9 µm** over
3,050,138 points. Structure is untouched: 11,214 of 11,214 faces both ways, no
open half-edge in either, no non-manifold edge in STEP, no body inside out.

The 4 mm hole this chase began from — a vertex of their fine mesh with nothing
of ours within 3.9987 mm — is **0.0267 mm** now.

### Where the Parasolid reading still parts from the STEP one, counted

The two readings agree to **3.9 µm** in both directions over three million
points, and yet OpenCASCADE finds 2,253 of its vertices more than 0.2 mm from
the Parasolid mesh against 307 from the STEP one. Both facts are true, and the
first is why the second was invisible for so long: `mesh_diff` measures
*vertices against triangles*, so a hole in one mesh is unseen unless the other
mesh happens to put a vertex inside it. Ours do not; OpenCASCADE's denser mesh
does. `INSPECT_AT` settles it in one line each — at three of those places the
STEP mesh is 0.012–0.035 mm away and the Parasolid mesh is **1.03–1.39 mm**.

What is behind them: `cad-xt` cannot evaluate a Parasolid blend, so it rebuilds
the face and keeps the rebuild as a degree-one grid — an ordinary surface
downstream. **1,447 faces of this model are such stand-ins**, and
`XT_BLEND_PROBE` counts how each got its grid:

| | faces |
|---|---|
| rolled — the ball's own geometry | **401** |
| a Coons patch over the boundary | **1,046** |

Seventy-two per cent are the fallback, and the fallback is an interpolation: at
the hole probed above the STEP reading has a spline **0.0096 mm** from the
point while the Parasolid reading's nearest is **0.80 mm**. That is the whole
of the remaining gap between the readers.

`XT_ROLL_PROBE` says why the ball is refused, per attempt (four rails are tried
per face):

| | attempts |
|---|---|
| the ball would not stay on both surfaces along the rail | **2,610** |
| a mating surface is itself a blend, and blends are not lowered | 988 + 704 |
| the blend is type `'E'`, not a rolling ball | 368 |

**Why the ball is refused, and why loosening does not help.** `XT_ROLL_BEST`
asks the sharper question: for a face that fell back, how near did the *best*
of its sixteen attempts come — the worst miss along its rail, since every point
has to pass. Of 531 such faces the median is **1.26 times** the tolerance, 63%
are inside 1.5 times and three quarters inside twice. They are near misses. A
hundredth of the radius, on a small fillet, is finer than the file is written
to, and the rail is a polyline sampled from the boundary rather than the exact
contact track.

So it was loosened, against the body's own tolerance, and measured:

| threshold | faces rolled | open half-edges | non-manifold | against OpenCASCADE |
|---|---|---|---|---|
| radius × 1% *(kept)* | 401 | 0 | 11 | 2,253 over 0.2 mm, 14 over 1 mm |
| body tolerance × 3 | 575 | **164** | **31** | one face unmeshable |
| body tolerance × 10 | 760 | 0 | 11 | **identical, to the last digit** |
| body tolerance × 20 | 802 | 0 | 11 | — |

**The middle breaks while both ends hold.** The first guess at why — that one
blend's faces were being read two ways — is now ruled out rather than assumed:
`make_each_blend_of_one_mind` puts every rolled face of a blend back onto the
interpolation its siblings use if any sibling could not roll, and it demotes
**nothing**, at any threshold. No blend is ever of two minds. What parts is a
blend and the blend it *mates against*: at three times only some of a pair
rolls, and the two disagree along the edge they share; by ten times both do.

**The tight test is kept.** Ten times rolls nearly twice as many faces onto the
ball's own geometry and moves not one number against the reference — the same
2,253 points over 0.2 mm, the same 14 over 1 mm, the same worst 1.3864 mm — for
4.5 seconds more lowering, a third of the Parasolid pipeline. And a threshold
that tears the mesh at one setting while looking clean at another is not one to
relax on a hope: the looser test admits balls the data does not support, and
×10 being clean on this file is not a guarantee. What would earn the loosening
is a check on the rolled grid itself — that the face's own boundary sits on it
— rather than on the ball's two contacts alone.

The consistency pass stays. It costs nothing, it holds an invariant that ought
to hold, and it is what turned "probably this" into a measurement.

**Measured and reverted: following a mating surface that is itself a blend.**
`surface_for_curve` does roll a blend out into a surface, and using it for the
two mating surfaces is a two-line change. It buys **fourteen** more rolled
faces, moves not one number against OpenCASCADE — 2,253 over 0.2 mm and 14 over
1 mm either way, worst 1.3864 mm — and takes lowering from **7.4 to 15.7
seconds**. The mating surfaces are not what turns these faces away. The roll is.

### The ball's two contacts are not the whole test

`roll` accepted a blend when the ball sat a radius off one mating surface and
touched the other at exactly `r`, judged to a hundredth of the radius. That is
a test on the *ball*. What the face needs is a surface **its own boundary sits
on**: the rails are the file's edges, shared with the neighbours, and a grid
that does not carry them leaves the face trimmed against something it is not
on. A ball can satisfy both contacts along one rail and still sweep a sheet the
face's other edges are nowhere near.

`grid_holds` asks that directly — is every point of the face's boundary within
a tolerance of the grid the ball swept, measured against the grid's own flat
cells, which is exact for a degree-one surface. It is the missing half of the
test, and it changes the picture completely:

* At the tight acceptance that was in place, **148 of the 401 rolled faces do
  not carry their own boundary** — 37% of them were being trimmed against a
  surface they are not on.
* Every acceptance threshold is now closed and manifold. Three times the body
  tolerance used to tear the mesh — 164 open half-edges, 31 non-manifold, a
  face unmeshable — because the grids that tore it are exactly the ones this
  refuses.

With the gate doing the real work, the ball's own threshold can be generous:
the ball is the file's definition of the surface and the Coons patch is ours,
so letting more candidates reach a check that verifies them costs time and buys
fidelity. Measured, at the gate set to three times the body tolerance — 0.03 mm,
about one facet's sag:

| gate | ball | lowering | rolled | refused | topology |
|---|---|---|---|---|---|
| ×3 | radius × 1% | 7.5 s | 253 | 148 | closed, manifold |
| ×3 | ×20 *(kept)* | 12.3 s | **385** | 417 | closed, manifold |
| ×10 | ×20 | 12.3 s | 523 | 279 | closed, manifold |
| ×1 | ×20 | 12.2 s | 163 | 639 | closed, manifold |

**385 blend faces now stand on the ball's own geometry with their boundary
verified against it**, where 401 stood on an unverified one and 148 of those
were wrong. The mesh is unchanged where it can be measured — 2,253 points over
0.2 mm against OpenCASCADE and 14 over 1 mm either way, the two readers still
agreeing to 3.9 µm in both directions — and lowering costs 4.8 seconds more.

**A trap worth recording.** Three earlier readings of this table were wrong
because a `re.sub` with `count=1` was rewriting the *first* threshold in the
file, which is the gate, while the run was labelled as varying the ball. Two
knobs of the same shape in one file need two distinct patterns, and a scan that
does not rebuild successfully still prints numbers — from the previous binary.

### The refused grids, and what an exhaustive search would cost

Of the 417 grids the boundary check refuses, a quarter are inside twice what is
allowed — near misses — but the **median misses by fourteen times it**, 0.42 mm,
and the tail reaches 45 mm. Those are not near misses; they are sheets
somewhere else, and refusing them is the point.

**Half of a refused boundary is adrift, not all of it.** Counting how much of
each refused face's ring misses: the median is **46%**, and none exceeds 90%.
That is the signature of a sheet built to the right rail and the *wrong far
track* — the grid carries the rail it was rolled along and misses the boundary
on the other side.

Which surface the rail lies on and which way the ball sits off it are not
stated in the file, so `rolling_ball_grid` offers all four readings and took
whichever first satisfied the ball's two contacts. That picks the wrong one
often, and the wrong one is exactly what the boundary check then refuses.

**They are now put in order first**, by where the far contact lands at the
rail's own first point: the reading whose contact falls on the face's boundary
is the reading whose sheet will carry it. Four evaluations of one point,
against four full rolls.

| | faces rolled | lowering |
|---|---|---|
| ordered, checked after *(kept)* | **468** | **14.0 s** |
| unordered, checked after | 385 | 12.3 s |
| ordered, checked inside the search | 572 | 27.7 s |
| unordered, checked inside the search | 585 | 26.0 s |
| only the two rails that run along the blend | 164 | 19.6 s |

Ordering buys 83 faces for 1.7 seconds where searching buys 104 more for
another 13.7 — and the cost of searching is the extra *rolling*, not the test,
which is about a second of it. Seeding each solve from the last (the rail is a
continuous path, so `invert` need not search the whole surface) and bounding
the sheet with a box before walking its cells were both tried and shift
nothing. Stopping at the "along" rails is worse than any of them, because most
of what rolls does so on a cross rail.

**The corners were a second wrong judgement.** After the ordering the refusals'
median drops to **26% of the ring adrift** — a quarter, which on a four-sided
face is one side. Where the four corners of the patch fall is a guess:
`quad_corners` takes the sharpest turns of a boundary that may curve smoothly
all the way round, and the rails handed to the ball follow from it entirely. A
rail cut in the wrong place sweeps a sheet the face's own *ends* are not on.
Both readings — sharpest turns and evenly spaced — are now offered, exactly as
the Coons rebuild in `cad-tess` offers both, and the boundary check decides
between them. That is **50 more faces for 2 seconds**.

Neither the blend's type nor its radius explains what is left: all 334 are type
`'R'` with the same nineteen fields, and a negative radius — which Parasolid
uses for a round rather than a fillet — succeeds at 31% against 33% for a
positive one, so the sign is not the discriminator either.

| | faces rolled | lowering |
|---|---|---|
| first reading that rolls, unordered | 401 (148 unverified) | 12.3 s |
| readings ordered by where the far contact lands | 468 | 14.0 s |
| both corner readings offered *(kept)* | **518** | **16.0 s** |

**518 blend faces now stand on the ball's own geometry with their boundary
verified against it**, where before this work 401 stood on an unverified one
and 148 of those did not carry their face. Every remaining refusal falls back
to the interpolation, which carries the boundary exactly, being built from it —
so every face in the model sits on a surface its own boundary is on.

### Which side of the patch is adrift, and the corpus as it stands

`report_refusal` now says *where* a refused grid missed, by the patch's own four
sides, because a rail adrift and an end adrift want different fixes. Across 559
refused attempts:

| side | badly adrift | partly |
|---|---|---|
| 0 | 14.0% | 10.2% |
| 1 | 33.3% | 30.9% |
| 2 | 29.2% | 37.6% |
| 3 | 34.9% | 27.9% |

Side 0 is the one the grid was rolled from and is held far more often, as it
must be. The other three are adrift at much the same rate — **there is no one
guilty side**, so the remaining refusals are not a single fault with a single
fix, and the two easy explanations are already ruled out: all of them are type
`'R'` with the same nineteen fields, and the sign of the radius does not
discriminate (31% of negative-radius blends roll against 33% of positive ones).

**The rest of the corpus, both formats, after all of this.** Six of the seven
SolidWorks sample parts come out closed, manifold and right way out:

| | bodies | faces | open | non-manifold | inside out |
|---|---|---|---|---|---|
| 500.076 | 1 | 19/19 | 0 | 0 | 0 |
| 500.076UB | 1 | 9/9 | 0 | 0 | 0 |
| 500.078 | 1 | 15/15 | 0 | 0 | 0 |
| 500.078UB | 1 | 8/8 | 0 | 0 | 0 |
| 500.079UB | 1 | 10/10 | 0 | 0 | 0 |
| 500.081 | 1 | 10/10 | 0 | 0 | 0 |
| 500.081UB | 2 | 10/10 | **4** | 0 | 0 |

The seventh is the one already on record: the body declares thirteen edges and
four of them reach only one face, which the reader says in its own words —
`edge use 4 of 13 edges reach only one face`. That is the file's topology, not
the reading's.

### Four hypotheses about the refusals, three of them killed

**Every one of the 926 faces that falls back had a grid built and refused.**
None fails for want of a rolling ball — with the acceptance generous and the
readings ordered, some ball always sweeps *something*; the question is only
whether the sheet carries the face. `coons-never-rolled` never appears.

*Is the arc too short — does the face reach past the ball's two contacts?* The
same ball was re-swept at 1.5, 2 and 3 times its own arc and the boundary
re-tested: **29 of 549 refusals** are carried by a wider arc, 520 by none of
them. The tube itself does not contain the face's boundary, so the sweep span
is not the fault.

*Is it a variable-radius blend, or a different record shape?* Printing all
nineteen fields of a blend that rolls beside one that does not: identical shape,
both `Char('R')`, both with `Float(0.0015) Float(0.0015)` for the radius at
either end — constant — and the same pointers in the same slots. The record is
not the fault.

*Is it the mating surface being itself a blend?* That was measured before the
boundary check existed and bought fourteen faces; with the check and the
ordering in place it buys **29 for 5.2 seconds** — against 83 for 1.7 and 50
for 2 from the two fixes that were kept. Not kept, on value.

*Is the generous acceptance admitting balls that then fail the check anyway?*
No: tightening it back to a hundredth of the radius loses **206 faces** (518 →
312) and saves 2.8 seconds. The generous threshold earns its keep now that the
check verifies what it lets through.

What is left is 897 faces where a ball rolls, its tube does not contain the
face's boundary, and no wider arc of it does. That is not one fault with one
fix, and the side-by-side counts say the same: no single side of the patch
dominates.

### One refused face, read end to end

The dump that was wanted: a face that falls back, with each of the patch's four
sides measured against the two surfaces its blend names.

```
a refused face: radius 0.00150, ring 51 points, mates cone and cylinder
  side 0: 17 points, 0.0030 long, off the cone by 0.000067, off the cylinder by 0.000342
  side 1:  9 points, 0.0030 long, off the cone by 0.000043, off the cylinder by 0.000342
  side 2: 11 points, 0.0018 long, off the cone by 0.000249, off the cylinder by 0.000039
  side 3: 18 points, 0.0034 long, off the cone by 0.000249, off the cylinder by 0.000387
```

**Sides 0 and 1 both lie on the cone.** A rolling ball has two contact tracks,
so a four-sided patch should have one side on each mate and two on neither —
here the corner search has cut a single rail in two, and rolling along a
"side" means rolling along half a rail, which sweeps a sheet covering half the
face. That is what the side-by-side counts were describing.

**Reading the tracks off the boundary instead was built and does not pay.**
Each boundary point is asked which mate it lies on, within the same tolerance
the roll uses, and the longest unbroken run on each is that surface's track.
On its own it rolls **76** faces where the corner readings roll 518; added
after them it is worth 50 more for **ten seconds**, the worst rate of anything
tried here (against 83 for 1.7 and 50 for 2 from the two that were kept). The
code stays, unused, with this note: the reasoning is right and the runs it
finds are wrong, and finding out why is the next thing to do.

### Where we stand against OpenCASCADE, at matched density

The comparisons above are at each mesher's own "normal", where we produce
1.97 M triangles to Mayo's 549 k — so they say as much about density as about
accuracy. Both at **fine** they are comparable, and ours is the leaner of the
two: **7,503,822 triangles to OpenCASCADE's 8,723,795**, 14% fewer.

| | our vertices → their surface | their vertices → our surface |
|---|---|---|
| mean | **0.0021 mm** | **0.0033 mm** |
| median | 0.0005 mm | 0.0005 mm |
| p99 | 0.0119 mm | 0.0639 mm |
| p99.9 | 0.1267 mm | 0.2464 mm |
| within 0.2 mm | 9,506,778 of 9,514,087 — **99.92%** | 4,970,893 of 4,979,611 — **99.82%** |
| within 1 mm | **99.977%** | **99.99964%** (18 points) |
| worst | 3.3325 mm | 1.9933 mm |

**Structurally the gap is not a percentage, it is a kind.** On the same
assembly, at its own normal setting, OpenCASCADE's mesh has **113 open
half-edges across four bodies, 130 non-manifold edges and 572 degenerate
triangles dropped**; at fine, 234 and 540. Ours has **none of any of them** on
the STEP reading and none but eleven non-manifold on the Parasolid one, with
every body enclosing a positive volume.

And there is a check no single-kernel converter can make: the same assembly
read through two independent front ends — STEP and Parasolid — agrees with
itself to **3.9 µm**, with not one of three million points as much as 0.05 mm
from the other reading's surface.

**What is not measured**, and so is not claimed: any comparison with a
commercial converter. One reference, one kernel, one 46-body assembly and seven
small parts.

### Why the tracks will not roll: the named pair runs out partway

The track reading finds the rails — that much was measured: of 1,444 blend
faces it names both tracks for 856, one for 450 and none for 138, and the
median track is 45% of the ring, which is what a rail of a four-sided patch
should be. Rolling along one still fails: **2,036 of 2,162 tracks would not
roll at all**, even with the boundary test switched off entirely.

Asking which half of the roll fails, over 3,490 (track, sign) pairs — the near
half cannot fail, since the track was chosen by lying on that surface:

| the ball reaches the far surface | share |
|---|---|
| at every point of the track | 6.0% |
| at no point at all | 15.4% |
| at some and not others | 78.6% |

and where it fails, it misses by a median of **3.2 times** the tolerance.

The last question decides what that means. Of the 2,743 partial reaches,
**88% have every reaching point in one unbroken run**, and the run covers a
median of **70% of the track**.

**So the ball does reach the surface the blend names — along a contiguous
stretch of the track, and then it stops.** The pair of surfaces in the
`BLENDED_EDGE` record describes part of the face and not all of it: the blend
runs on past where that mate ends. `roll` is all-or-nothing by design — one
point that does not touch kills the whole rail — so a face whose named pair
covers 70% of it rolls not at all, and the interpolation stands for the whole.

That is a limit of the shape of the reading, not of the search: one face is
lowered to one grid built from one pair of mates. Describing such a face
properly means finding the surface the track moves onto and rolling the rest
against that — which is the next thing to build, and the first thing here that
is a change of structure rather than of judgement.

### The structural change, built and measured: a far surface that is a set

If the named mate holds for a contiguous 70% of the track and the blend then
runs on, the surface it runs onto is a face this one already shares an edge
with. So `roll` was given the far side as a **set** — the surface the record
names plus the surfaces of every neighbouring face, walked loop → fin → edge →
partner fin → loop → face — and at each rail point took whichever candidate the
ball actually reached. (The walk itself needed one correction on the way: a
fin's edge is field `6 − a`, not `3 − a`; with the wrong index it found nothing
at all, and found 1–5 neighbours per face with the right one.)

| far side | faces rolled | lowering |
|---|---|---|
| the named mate only *(kept)* | **518** | 15.6 s |
| nearest of named + neighbours | 137 | 28.5 s |
| named first, a neighbour only where it misses | 506 | 18.0 s |

By proximity it is far worse: a neighbour can sit nearer than the true mate and
the sheet follows the wrong surface from the start. In order it is still
slightly worse and slower: a neighbour satisfies the distance test spuriously
often enough to let a roll *finish* with a wrong far track, where failing would
have sent the search on to a rail that works. Reverted; the measurement stays
in `roll`.

What this rules out is the cheap version of the structural fix. The one that
would work has to know *where* along the track the mate changes and split the
face there — two grids, not one candidate set — and that is the next thing.

### The handover, built and measured — and what it actually showed

The question before building anything: where the named mate lets go, does
another surface of the body hold the ball? **Yes — 93% of the 46,996 track
points the mate misses are held by some other surface.** So the centre line is
sound past the mate, and a roll that hands over from the named mate to whatever
surface the ball reaches, once it has let go and then committed to, should
finish.

It does finish. **506 faces roll, at 41 seconds, and not one of the 62 rolls
that handed over carries the face's boundary.**

That number reverses the reading. The sheet the ball sweeps after the handover
is real — it is the blend on the *other* surface — and it is not this face's.
The face ends where its mate ends. What runs on past that point is not the
blend but the **rail**: the boundary polyline the corner search cut is longer
than the face's own contact track, and the ball follows the rail faithfully
into a neighbouring fillet. Reverted; the measurement stays in `roll`.

The prediction this makes is cheap to test and is tested next: trim the rail
to the contiguous run the named mate holds, and the roll along it should carry
the face.

### What the refused blends are, settled

Five more hypotheses, each built and measured against the 926 faces that fall
back to the interpolation. None moved the count that ships, and together they
close the question.

*The rail runs on past the face.* Trimmed to the contiguous run the named mate
holds, the roll finishes on 2,228 of 2,266 rails — and carries the face on 38.
Not the rail.

*The record's radius is wrong.* The width of the face against the arc the ball
sweeps implies a radius of 0.2–0.5 mm where the record says 1.0–1.5; rolled at
the implied radius, 1,634 rails will not roll and 632 roll without carrying the
face. (Field 12, the "second radius", is 1000 mm on every blend in the file — a
sentinel, not a variable radius.) Not the radius.

*The face is a seam-hugging sliver, not a fillet.* The one face dumped end to
end was: its boundary sits within 0.4 mm of *both* mates, on their
intersection. But across all 411 refused faces the widest standoff of a contact
track has a median of **0.97 r** — most of them are fillet-width — and asking
the whole track, **190 of 394 (48%) have both tracks at a constant standoff of
0.90–0.92 r**: true fixed-radius fillets by every measure. Half are slivers or
variable; half are genuine.

*The roll is all-or-nothing and the ends miss.* On one of those 190, the ball
reaches 23 of 27 track points; the misses are at **points 18, 19, 20 and 26**,
mid-track, by 1.9–3.1 tolerances — not the ends. Letting the two end points
borrow a neighbour's section let ten grids through and **none carried the
face**. Not the ends.

*The tolerance is simply too tight for these faces.* Rolled at the tolerance
that face needs (3.3× and 2.0×), the grid builds — and the face's own boundary
sits **64 µm and 105 µm** off it, against a gate of 30 µm.

**That last number settles it.** Even a fillet that is fixed-radius by every
measurement is not the ideal 1.5 mm ball to better than 60–100 µm: the
modeller's surface and the ideal roll part by two to four times the mesh's own
sag (25 µm), which would be a visible facet error. The boundary gate refuses it
for exactly the right reason. The Coons interpolation is built *from* that
boundary and carries it with zero error; for these faces it is the more
faithful surface, not the fallback. The 518 that roll are the ones where the
ideal ball and the modeller's surface agree to within the gate; the 926 that do
not are the ones where they disagree, and the interpolation is right to win.

### Three edges read wrong, and what each one cost

**A torus 12 mm off its surface.** Face 273 of `201 201 003-51` is bounded by
two closed edges. One of them, edge 664, has no curve of its own in the file
— 2,445 of the assembly's 26,533 edges are like that — and stands on its
fin's SP_CURVE sampled through the surface. That SP_CURVE's parameter spline
is a line (degree one, two control points) written over `[0.75, 1.0]` and
trimmed to `(0.75, 1.75)`: a whole turn of the torus. The sampler clipped the
window to the spline's knots and drew a quarter turn; the polyline ended
23 mm short of the vertex it shares with its own start, and the face was
**12.14 mm** off the torus. A parameter line is exact wherever it is
extended, so for degree one with two control points the window is honoured
past the knots. Face 273: **0.109 mm.** Of 135 closed stand-in edges, seven
did not close; now six. The two readers, which agreed to 3.9 µm, agree to
**1.9 µm** in the STEP→Parasolid direction.

**Two spikes a kilometre long.** Edges 859 and 3107 of `200 201 003-51` are
intersection charts whose first point — field 6 of the CHART — sits 1,316 m
and 690 m from the rest of the chart, on edges whose vertices are millimetres
apart. The schema calls field 6 `hvec`, and dropping it on that reading was
measured: `chart_count` counts it, all 5,357 charts come up one short, seven
faces fail to mesh and 551 half-edges open. **It is the first point.** The
four bad charts are told apart by the data alone — a first point further from
the second than the whole rest of the chart spans, a hundredfold — and only
those lose it. Far points 2 → 0, worst end-miss 1,316 m → 2.2 mm, nothing
else moved.

**A cone 41 mm off its surface**, in both readers — face 10 of
`200 201 003-51`. Its surface reading has seven cracks from one crossing, so
the boundary rebuild wins; all four rebuild candidates are crack-free and stay
1.4 mm inside a 123 mm box, and one sits **41 mm** off the cone. The box test
cannot see that, but the cone can: on a plane, cylinder, cone, sphere or torus
inverting a point is closed form, so a rebuild that leaves its surface by a
tenth of the boundary's diagonal is refused — and only where the surface
reading it would displace is whole, because refusing it against a cracked one
opened 30 half-edges across six faces of the STEP reading. Face 10: 41.4 →
**11.9 mm**; the second candidate is wrong too, by less.

**The crossing itself, found.** The strip's cut is put at the first ring's
first point and the seam column stands there. That ring's own first step runs
*backwards* in `u` — (3.9500, 3.5) → (3.9411, 2.0), 0.0089 back, exactly the
amount its strip overran the period by — so the column was drawn through the
ring's first segment. Beginning the ring one point on, where it is already
moving forward, moves the cut off the crossing: nothing moves in space. Face
10 reads from its surface at **0.134 mm** in both readers, 334 triangles, and
the departure refusal above never fires again — measured off, nothing changes
— so it is removed: a rule that once opened thirty half-edges and now protects
nothing is a liability. Two earlier attempts at the same crossing — taking the
ring's trailing run along the apex row off, and its leading step down it —
changed nothing, and the second took the STEP reading from 1 to 69 points over
1 mm.

Also measured and reverted this round: storing arc-sectioned blend grids at
degree two so the tessellator evaluates them (2.07 M → 5.44 M triangles, four
open half-edges, 14 → 72 points over 1 mm: a quadratic through the arc
samples smooths their control polygon rather than passing through them), and
sampling the SP_CURVE stand-in to a chord tolerance (no effect: its parameter
splines are lines and the chords are exact).

### What is left

**STEP — 11,214 of 11,214 faces, closed and manifold throughout.** No open
half-edge, no edge shared by more than two triangles, no body inside out.

**Parasolid — 46 of 46 bodies and 11,214 of 11,214 faces, the same as the STEP
reading; no open half-edge, eleven non-manifold edges.** They sit in three
bodies: five in `201 201 003-51` (faces 79 offset, 91 sphere, 185 a degree-one
grid), four in `204 201 013-51` (faces 119 and 233, splines), two in
`205 211 013-51-oa2` (faces 157, 158, 308, 309, degree-one grids). Two of them
are caps whose rings run within 0.065 mm of their pole; the rest are faces
lying on themselves — and one pair is not a self-fold at all but two rebuilt
blend faces overlapping each other. The file gives none of them a repeated
edge, so the fold is ours, not the model's.

**The Parasolid reader still skips 24 things, none of which loses a face**:
eighteen fins that name no edge, four faces whose surface is a `BLENDED_EDGE`
and are rebuilt from their boundary instead, and two loops bridged across gaps
of 1.4 and 0.5 µm. No body
inside out in either. Every one of those edges is a *single* face lying on
itself. The two in STEP are one defect and its mirror: faces 67 and 72 of
`205 221 011_oa_1`, a 0.5 mm ball whose cap loses the candidate comparison to a
boundary rebuild because its parameter boundary crosses itself at the pole —
the chord described above, which the obvious fix makes worse. The file gives
none of these faces a repeated edge, so the fold is ours, not the model's.

**Three faces of 11,214 carry a triangle edge more than twenty times their own
tolerance** — the spring at 1.54 mm, a degenerate torus (`major 6.5, minor
7.5`) at 0.84 mm, a cone at 0.85 mm. Eight more are rebuilt from their
boundary, where the surface is not what the patch stands for and the figure
does not apply.

**One of seven SolidWorks sample parts leaves four half-edges open**, around a
square one micrometre on a side. That is the file's own topology: the body
declares thirteen edges, four of which reach only one face.

Everything else measured: every edge within 0.121 mm of its curve; no body
enclosing a negative volume; the two readers agreeing to **15 µm at worst**
over 3,065,569 sampled points — mean 0.0000, p99 0.0004 mm — with not one point
of either mesh 0.05 mm from the other's surface; and agreement with
OpenCASCADE at mean **0.0047 mm**, p99 **0.0601 mm**, with **one** point of its
445,576 more than a millimetre from our surface — against its own 113 open
half-edges and 130 non-manifold edges on the same assembly, and 234 open edges
and 540 non-manifold on its finer one.

### A STEP edge whose curve is somewhere else (2026-08-21)

Two edges of the pilot's STEP (`#89562`, `#89564`, both in `204 201 013-51`
and its mate) carry a `B_SPLINE_CURVE_WITH_KNOTS` whose four control points
lie 3.3–4.0 mm from the edge's two vertices — same 0.65 mm length, same
direction, displaced. Both vertices project onto one end of it, `edge_range`
collapses to `[0.041359, 0.041359]`, every parameter evaluates to one point
3 mm off the cone, that spike crosses a neighbouring boundary segment, the
cone loses its surface reading and the Coons rebuild sits 3.8 mm off
(`[facesag] 3.82 … rebuilt face=625`). The Parasolid twin writes the same
edges as straight lines between their vertices.

Rule (`cad-step/src/lower/topo.rs`, `intern_edge`): after the range is
recovered, if the curve's two range ends miss *both* vertices by more than
`vertex_tol` (10× the body tolerance), the edge is the chord of its vertices
— `Curve::Line { origin: p0, direction: p1 − p0 }`, range `[0, 1]`.
`CAD_STEP_RANGE_TRACE=1` prints each `[chord]` taken. Measured: cone 625
3.82 → 0.024 mm (surface, not rebuilt); STEP points over 1 mm vs OCCT
normal 1 → 0, over 0.2 mm 307 → 301; still 0 open / 0 non-manifold. Two
chords in the whole file; nothing else moved. A wider "natural domain when
its ends fit the vertices" branch was tried first and never fired — the
natural ends miss by the same 3.3 mm — so it was removed.

### A membrane over a bolt hole, and the cut that caused it (2026-08-22)

A visual sweep of the ten parts that carry the disagreement against
OpenCASCADE found one defect obvious without magnification, in **both**
readers: the two bolt holes of `219 204 008` were capped by a domed sheet
that left only a crescent open. The numbers had shown it only as
`2.149 mm, ours -> theirs` on a small part, which looked like OpenCASCADE's
coarseness because both readers agreed — they agreed because the fault was
downstream of both, in `cad-tess`.

The chain, from the mesh back:

1. The bore wall is a cylinder of radius 3.25 mm. Its rebuild-from-boundary
   candidate spans the bore instead of lining it — a sheet through the axis,
   3.22 mm off a surface 3.25 mm in radius — and it has **no** cracks, while
   the surface reading has three. "Fewer cracks wins" took the sheet.
2. The surface reading's three cracks are one undrawn boundary segment each.
   The bore's top rim is castellated: three arcs at z = 2.5 and three at
   z = 2.0, six 0.5 mm vertical steps between them (`CAD_TESS_SEAM=1` prints
   every ring's constant-u segments against the cut).
3. The cut landed on the last of those steps — `[seam] … constant-u steps
   [… 53@u=5.62645]` with `u_hi=5.62645`. The strip draws its own seam column
   at exactly that u, so the ring's own segment there is never drawn.

Two rules, both measured:

- **`wrapped_region`: the cut must clear the seam.** Where the cut goes is a
  free choice; it is now tried at each of the first ring's own points and the
  first one that offends neither way is taken — no ring segment standing at
  the cut, and no backward first step (the older rule, kept, from cone face 10
  of `200 201 003-51`). Nothing is invented: every cut is a point the ring
  already has, so the boundary stays shared with the neighbour. A cut that was
  already good is found at k = 0, so a face that was right does not move.
- **`triangulate_compare`: a rebuild of an analytic face has to be on that
  face.** Where the surface is a plane, cylinder, cone, sphere or torus,
  inverting a point is closed form, so this is arithmetic. A rebuild standing
  more than a tenth of the boundary's own diagonal off its surface is refused.
  An earlier, broader form of this was removed in July as protecting nothing;
  this narrow form now has a case, and `CAD_TESS_CANDIDATES=1` prints each
  refusal with the distance.

Measured, whole pilot, against OpenCASCADE at normal density:

| | STEP before | STEP after | x_t before | x_t after |
|---|---|---|---|---|
| ours → theirs, max | 3.5949 mm | **1.2127 mm** | 3.8970 mm | 3.8970 mm |
| ours → theirs, over 1 mm | 423 | **45** | 7889 | **7511** |
| ours → theirs, over 0.2 mm | 11167 | **9281** | 74256 | **72409** |
| theirs → ours, over 0.2 mm | 301 | 301 | 2253 | 2253 |
| open / non-manifold | 0 / 0 | 0 / 0 | 0 / 11 | 0 / 11 |

224 tests, no warnings. New probes: `CAD_TESS_GAP_WHERE` (which boundary
segment a candidate left undrawn, with its length), `CAD_TESS_RINGS` (each
ring's size, end gap and box), `CAD_TESS_SEAM` (the cut against every ring's
constant-u segments).

### Two ways of making the tessellator evaluate a blend's arc, both worse

The Parasolid reader solves a blend's cross-sections from the rolling ball and
stores them as a grid; the tessellator reads a degree-one-in-both-directions
surface as "rebuild me from my boundary" and never evaluates it, so 625 arc
grids reach the mesh as the rebuild's own interpolation. Both ways out were
built and measured:

- **The exact rational arc** — three control points across the section, weight
  cos(θ/2), one knot span. Triangles 2.07 M → 3.30 M, open half-edges 0 → 60,
  non-manifold 11 → 37, points over 1 mm 14 → 52. A conic that meets a cross
  rail at three points does not carry the rest of it, and every boundary point
  in between inverts onto a surface it is not on.
- **The grid as the rebuild's interior**, boundary untouched, only where the
  two agreed about the patch's four corners. Points over 1 mm 14 → 65, over
  0.2 mm 2253 → 2683, p99 0.1106 → 0.1213 mm.

The second is the informative one: the rebuild's rule is not the chord. The
two sides it rules between are the patch's cross-sections, and those are the
file's own edges — arcs, carried to the bit. Ruling between two arcs of a
constant-radius fillet reproduces every section between them; what it misses
is only the way the section turns as the rail curves, and our ball
reconstruction misses more than that. **The boundary is better information
than the geometry we recover from it.**

Also fixed while measuring this: `quad_corners` takes the cross-sections for
rails on 340 blend faces — their half-chords come out 2.2–14.8 r, which no
rolling ball can produce, and the giveaway is the side lengths (the short pair
measures 1.50 r where a quarter-circle section of radius r measures 1.571).
The other pairing is now asked the same question, which takes the Coons
stand-ins from 455 to 302 of 1,444 blend faces. It changes no vertex today,
because every rebuilt blend face is still drawn from its boundary.

### Making the GLB smaller without moving a vertex (2026-08-22)

The pilot assembly wrote 51.25 MB from the Parasolid path and 49.51 MB from
STEP. Where the bytes were: indices 42%, positions 29%, normals 29%.

**1. Every index in 16 bits (lossless).** Only six bodies of the pilot carry
more than 65 535 vertices, and they held 4.75 M of the file's 6.17 M indices —
19.00 MB where 9.50 MB would do. `add_geometry` now cuts each material run into
chunks that index at most 65 536 vertices, and every index in the file is a
short. The triangles arrive face by face, so a chunk taken in order is compact:
across the whole assembly the seams between chunks cost **678 duplicated
vertices**, 0.06 %. Measured: 51.25 → 41.78 MB, and `mesh_diff` of the two
files is 0.0000 mm both ways.

**2. `byteStride`, which was missing (a bug).** glTF requires every vertex
attribute to step by a multiple of four. The quantised normals were written
four bytes apart — three bytes and a pad — and the buffer view said nothing, so
a conforming reader steps three and takes the padding for data. Reading them
back measured **88° of error**; with the stride declared it is 0.153° mean,
0.381° worst, not one normal over 1°. Quantised positions were worse than
non-conforming, they were tightly packed at six bytes; they are now padded to
eight with `byteStride: 8`. This made `compact` bigger (23.86 → 26.25 MB) and
correct.

**3. Three outputs, and what each costs.**

| | plain | lean | compact |
|---|---|---|---|
| x_t | 41.78 MB | **31.99 MB** (77 %) | 27.10 MB (65 %) |
| STEP | 40.60 MB | **31.03 MB** (76 %) | 26.25 MB (65 %) |
| triangles (x_t) | 2 057 098 | 2 057 098 | 2 055 808 |
| non-manifold (x_t) | 11 | 11 | 237 |
| over 0.2 mm vs OCCT | 2253 | 2253 | 2257 |

`Options::lean()` encodes normals a byte a component and leaves positions
exactly as computed: every measurement above is identical to plain, to the last
digit, and the flat-shaded render is pixel-for-pixel the same file.
`Options::compact()` also puts positions on each mesh's own 16-bit grid — 0.9 µm
mean, 2.6 µm worst, and against OpenCASCADE it measures the same. What it costs
is the finest slivers: about 1 300 triangles narrower than the grid collapse,
which leaves the solid closed but with 237 edges where three faces meet. Lean is
the one to hand on; compact is for delivery where the last quarter matters.

**4. The tools read what we write.** `gltf::import_slice` refuses a file whose
`extensionsRequired` it does not know, and its attribute readers return nothing
for an accessor that is not floating point — so the smallest output was the one
thing no measurement could check. `examples/common/glb_read.rs` opens without
validation and widens integer attributes, honouring `byteStride`; `mesh_diff`,
`glb_audit`, `inspect_glb` and `render` all go through it.

### The material library argues with itself (2026-08-22)

Colour was checked first and is not the problem. Against OpenCASCADE's own
reading of the same STEP, per *instance* — the assembly has two distinct
products both named `105.A1792` and two named `102.A1526`, each pair with
different colours — all 42 named bodies match in both readers, and the
sRGB→linear conversion checks out by hand on three colours. The GLB writes
`baseColorFactor` linear with no extension, which is what glTF asks for.

What is wrong is the finish. The bundled `.sldmat` sets `Shininess` per
material, and it is not maintained:

| entry | shaders it asks for | Shininess | roughness we derived |
|---|---|---|---|
| Copper | `copper` / `CopperPolished` | 0.025 | 0.938 |
| Brass | `polished brass` / `BrassPolished` | 0.025 | 0.938 |
| Nickel | `nickel` / `NickelPolished` | 0.100 | 0.750 |
| Pure Silver | `silver plate` / `silverpure` | — | 0.500 |
| Leaded Commercial Bronze | `polished bronze` | — | 0.500 |
| Plain Carbon Steel | `polished steel` | — | 0.475 |

Polished copper as rough as unfinished concrete — and `Nickel` is what
`representative(Chrome)` returns, so every chrome-plated part in every customer
model came out matte.

Rule (`SldMaterial::roughness`): where a **shader name** states a polish, that
is a statement about how the material looks, which is the thing roughness
encodes, and it is taken as a *ceiling* — an entry whose optics already agree
keeps them. The ceiling is the library's own: the polished entries it does fill
in sensibly, `6061 Alloy` and `AISI 1020`, sit at 0.225 and 0.05, so 0.25 is
the top of the range it uses for a polished finish rather than a number
invented here. Measured: those six drop to 0.25; `6061 Alloy`, `AISI 1020`,
`Gray Cast Iron` and `Rubber` do not move; **the pilot's GLB is byte-identical**,
because its two metals are AISI 1020 and 6061 Alloy, both already consistent.

The judgement is made on `shader_names` — a new field holding the
`pwshader`/`cgshader` names *without* the `swtexture` path. The path is still
in `shaders`, where `is_metal` weighs it, but it cannot speak to finish: every
plastic in the library, rubber included, points at
`plastic\polished\pplastic2.jpg`. Reading finish off that made rubber polished.

The other direction is left alone and the listing says so: half the steels name
a polished shader while pointing at a cast texture, and there is no reading of
that which is obviously right. `cargo run -p cad-ir --example sldmat_info`
prints the remaining 13 disagreements; `--all` prints all 115 entries.

**Still not backed by the library: paint.** It is 10 of the pilot's 14
materials and the visible bulk of the model, and a `.sldmat` carries no paint —
paint is a SolidWorks *appearance*, not a material. Its gloss is ours: 0.30
semi-gloss, or 0.55 where the Parasolid twin's `SDL/TYSA_REFLECTIVITY` says the
designer marked that colour matte, which is 12 of the 14 colours here.

### Paint had no source. It does now: the appearance library (2026-08-22)

`representative(Paint)` returned `None`, so the majority of a painted
assembly's surface — 10 of the pilot's 14 materials — took its gloss from a
number written here: 0.30, or 0.55 where the Parasolid twin said matte. That
was the last value in the material pipeline with nothing behind it.

The source was already in the tree, unread: `native/crates/cad-ir/assets/Materials`,
**619 SolidWorks `.p2m` appearance files**, 332 KB of text between them,
including a `painted/` folder with car, powder-coat and sprayed finishes. A
`.sldmat` says what a part is *made of*; a `.p2m` says how it was *finished*,
which is the question a renderer actually asks. `build.rs` compiles all 619
into the crate, keyed by path, so adding an appearance to the tree is enough.

**`roughness` in a `.p2m` is not glTF roughness**, and taking it for that gets
clear glass wrong — the file states 0.70 for it. The file describes a PhotoView
surface where the reflection is either sharp or blurred and `roughness` only
controls the blur:

| what the file says | reading | calibrating files |
|---|---|---|
| `reflectivity` is 0 | nothing to reflect; the number describes the surface. Taken as stated. | powder coat 0.92 |
| `blurryReflections off`, reflectivity > 0 | a mirror however rough the number looks | clear glass 0.70, high-gloss plastic 0.60, chromium plate 0.70 — all smooth |
| `blurryReflections on` | the number *is* the blur, which is what glTF roughness is | brushed chromium 0.25, burnished chrome 0.50, cast chromium plate 0.80, matte rubber 0.85 |

Every calibrating file is in the bundled tree, and the rule was chosen to fit
them rather than the other way round; four tests pin it, including that clear
glass comes out polished and that exactly 41 files carry `metallic_color`.

Paint now takes `painted/powder coat/dark powdercoat` where the designer marked
the colour matte and `painted/car/gloss blue` where they did not — a machine
casting is powder-coated, not car-painted. Measured on the pilot: every paint
goes **0.55 → 0.92**, identically in both readers; the two metals (AISI 1020,
6061 Alloy) and the rubbers do not move; base colours are untouched and so is
every triangle. 231 tests, no warnings.

`.gitignore` now keeps the `.p2m` and the `.sldmat` — 620 files, 459 KB — and
excludes the 935 MB of textures and HDR environments beside them, which a
`.p2m` names by a path inside a SolidWorks installation that nothing here
resolves.

### A node with four parents (2026-08-22)

Babylon.js refused the quantised output: `/nodes/3: Invalid recursive node
hierarchy`. Node 3 was `214 204 003_dequant`.

A quantised mesh needs a node to carry the scale and offset that undo its grid,
and the writer built **one such node per geometry** and parented it under every
instance — so a part used four times gave its dequantisation node four parents.
glTF's node hierarchy is a forest: a node has at most one parent. Ten nodes in
the pilot had two to four. The plain and lean outputs were unaffected, because
without quantisation the instances reference the shared mesh directly, which
*is* allowed.

Fixed by keeping the transform per geometry and writing a fresh node under each
instance. The mesh is still shared, which is where the size is: 18 extra nodes,
26.25 → 26.26 MB. Verified on all four outputs — no node with two parents, no
cycle, nothing unreachable from the scene — and pinned by
`every_node_has_at_most_one_parent`, which walks the graph for all three
compression settings on a scene whose one bolt is used twice, the smallest
shape that reproduces it.

The lesson worth keeping: **sharing a mesh is legal, sharing a node is not.**
Instancing in glTF is many nodes pointing at one mesh, never many nodes
pointing at one node.

### A closed edge's own sense, and the 270° round (2026-08-22)

The flange round of `102.A1525` is a torus band between two circles. Our STEP
reader and OpenCASCADE both draw it as the short 90° arc — a convex bullnose.
The Parasolid reader took the other way round the tube: 270°, a concave cove
with an overhanging lip, three quarters of a torus buried inside the solid, and
a **2 mm deep annular notch running the whole way round the flange** (outer
silhouette radius at y = 0.2420: 13.48 mm against STEP's 15.47 and OCCT's
15.44). 3060 triangles where 1080 were needed.

Everything about the face agreed between the readers — same torus frame
(`origin [5.4012,0,0] axis [1,0,0] ref [0,0,-1]`, major 14.5, minor 1.0), same
two trim rings at the same `v`, `same_sense` true in both. The one difference
was the direction one ring is traversed: STEP `+1`, Parasolid `−1`. A band's two
rings must run opposite ways — that is what "material on the left" means — and
with both the same, `wrapped_region`'s rule had nothing to decide on and fell
back to the sorted `v` values, which pick the wrong band whenever the right one
crosses the `v` seam.

The file was not ambiguous. Its two `FIN`s carry `-` and `-`, and its two
`CIRCLE`s carry `+` and `-` in field 6 — the geometry's own sense character,
the same one `lower_face` already composes with the face's, and the same one
`intern_edge` already reads to choose which of two arcs an *open* edge is. A
closed edge names no vertices, took the `closed_no_vertex` branch, and that
branch dropped it. Composed, the band walks one ring each way.

Fix in `intern_edge`: `built_reversed = (rebuilt && !forward) ^ curve_reversed`,
where `curve_reversed` is the curve's `-` sense on a closed edge.
`XT_CLOSED_TRACE=1` names each one. **89 of the pilot's 581 closed edges — 15 %
— carried a sense we were dropping.**

Measured, Parasolid path, whole assembly against OpenCASCADE at normal density:

| | before | after |
|---|---|---|
| triangles | 2 057 098 | 2 044 318 |
| theirs → ours, mean | 0.0084 mm | **0.0079 mm** |
| theirs → ours, p99 | 0.1106 mm | **0.1030 mm** |
| theirs → ours, p99.9 | 0.5257 mm | **0.3878 mm** |
| theirs → ours, over 0.2 mm | 2253 | **1790** |
| ours → theirs, p99 | 0.3523 mm | **0.2589 mm** |
| ours → theirs, over 1 mm | 0.247 % | **0.100 %** |
| open / non-manifold | 0 / 11 | 0 / 11 |

`102.A1525`'s band is now 1080 triangles and 0.039807 mm from its torus —
identical to the STEP reading, digit for digit. STEP is untouched (1 970 318
triangles, 0 open, 0 non-manifold, 301 points over 0.2 mm). 232 tests, no
warnings.

New probes: `CAD_TESS_RAWRING` (each ring's wrap, u/v span and 3D walk *before*
normalisation — the print that isolated this), `CAD_TESS_BAND` (the two-ring
torus decision with both travels and the band's share of the period),
`XT_LOOP_TRACE` (a loop's fin senses, resulting half-edge directions, and the
raw records of the curves behind them).

### A blade the part does not have: inverting a surface of revolution (2026-08-22)

The worst thing in the Parasolid mesh was a flat blade standing off the lower
crankcase of `200 201 003-51` — about 324 mm³ of material neither the STEP
reading nor OpenCASCADE has, found by the visual sweep and then confirmed in
the vertex data.

It is one face. `[facesag]` names it: **3.960 mm, 525 triangles, revolution**,
where the whole rest of the body's worst face is 0.845 mm and STEP's
counterpart of this face has 28 triangles. Both readers have 2236 faces on this
body and the identical boundary box (1.115 × 1.026 × 16.646 mm at the same
place); only the surface differs, STEP writing a B-spline where Parasolid
writes a surface of revolution.

`CAD_TESS_FACE=1341` dumps the face's boundary in parameter space beside what
the surface says is there, and every one of its thirteen points read

```
u 3.14257 v 0.00000  boundary [39.6026,-63.5170,-33.3941]
                     surface says [39.0351,-67.4327,-33.5621]  off by 3.9602
```

3.96 mm — twice the 2 mm radius, the far side of the axis, and the same for
every point. `Surface::Revolution::point_at` rotates the **profile** by `u`, so
`u` is measured from the half-plane the profile lies in; `invert` returned the
absolute angle about the axis measured from `frame.ref_dir`. The two agree only
when the profile happens to lie along `ref_dir`. This profile sits at π, so
inverting and evaluating disagreed by half a turn, the boundary inverted to
parameters on the opposite side, and the patch filled in the tube between them:
a 4 mm blade 16.6 mm long.

Fixed in `cad-ir`: `invert` measures `u` from the profile's own angle, taken at
the middle of the profile. Measured on the face: boundary points reproduce to
0.0005 mm (was 3.96), 27 triangles (was 525, STEP has 28), faceting 0.005 mm
(was 3.960). Whole assembly, Parasolid path against OpenCASCADE:

| | before | after |
|---|---|---|
| ours → theirs, max | 3.8970 mm | **2.3791 mm** |
| ours → theirs, over 1 mm | 1314 | **1057** |
| ours → theirs, p99.9 | 1.0025 mm | **0.9218 mm** |
| triangles | 2 044 318 | 2 043 886 |
| open / non-manifold | 0 / 11 | 0 / 11 |

STEP is untouched — it uses the same IR but writes no surface of revolution
here. `a_revolution_inverts_to_the_parameter_it_was_evaluated_at` pins it, with
the profile seated at four different angles including π; the old code passes
only at zero. 232 tests, no warnings.

Also changed while chasing this, and kept: `triangulate_region` builds the
second seam reading when the first **wanders** as well as when it leaves cracks,
and on equal cracks prefers the one that stays nearest its own boundary. A
crack-free reading is not therefore the right one — it can close the boundary
perfectly and draw the complement. It did not fire on this face (the fault was
the surface, not the seam), but "fewer cracks wins" with no second opinion is
how the membrane over the bolt holes was chosen too.

New probe: `CAD_TESS_FACE=<id>` dumps a face's surface, its bounds, and every
boundary point in parameter space beside what the surface evaluates there —
the print that isolated this in one run.

## Delivery: CLI, C ABI, NuGet (2026-08-22)

Until now the pipeline existed only inside `examples/`, hand-assembled twice —
once for each reader. Three crates and a .NET project turn it into something
that can be handed to someone.

```
xt-parser   Parasolid XT text → raw entities        (no deps)
cad-ir      B-Rep, scene, materials, sldmat, p2m    (no deps)
cad-xt      XT → cad-ir            → cad-ir, xt-parser
cad-step    STEP → cad-ir          → cad-ir
cad-tess    cad-ir → mesh          → cad-ir
cad-export  cad-ir → GLB/OBJ/USDZ  → cad-ir
cad-convert read → tessellate → write, in one call   ← the only thing to wrap
cad-cli     `cadconvert`, a binary over it
cad-ffi     `libcadconvert_native`, cdylib + staticlib, C ABI + include/cadconvert.h
dotnet/     CadConvert, netstandard2.0, P/Invoke over that
```

`cad-convert` picks the reader from the file's own header before its extension
— a `.stp` that begins `**ABCDEFGHIJKLMNOPQRSTUVWXYZ` is a Parasolid file
someone renamed, and reading it as STEP fails with a message about nothing.

**The defaults were the hard part, and they were wrong first.** The CLI and the
ABI each invented `0.05 mm` and `20°`. Both are far coarser than
`cad_tess::Options::default()`, which is measured: 0.04 % of the model's own
diagonal and 8°, relative so it scales with the part. Worse, `relative` stayed
true, so `0.05` meant *five per cent of the diagonal*. The pilot came out at
259 600 triangles instead of 2 043 886, with **607 open half-edges** and three
faces that failed to mesh at all. The converter's defaults are the one place the
mesh must not be filed down. Zero now means "the converter's own" all the way
out to `ConvertOptions.SagMillimetres`, which is `double?` and null by default;
a caller who states a distance means a distance, so stating one also turns the
relative reading off.

Verified end to end: a fresh console app, restoring the packed
`CadConvert` from a local feed, converts the pilot to **2 043 886
triangles, 0 open edges, 11 non-manifold, 11 214 of 11 214 faces** — digit for
digit what `xt_mesh` produces.

Two more silent failure modes met on the way, both worth knowing:

* `PackagePath="runtimes"` is a directory and pack appends each file's own
  `{rid}/native/` under it. Writing `runtimes/%(RecursiveDir)` doubles it, and
  the .NET host — which probes `runtimes/{rid}/native` — then finds nothing, at
  the first P/Invoke, with no build error. The first glob pointed one directory
  too high and packed nothing at all, which fails the same way.
* Copying the built library into `runtimes/` by hand is how a stale binary
  ships: the package globs whatever is there and says nothing about its age.
  The first package carried a dylib from before the defaults were fixed and
  reproduced the old numbers exactly. A `pack.sh` was written to rebuild the
  native side first; it has since been retired, because `dotnet build` does
  that itself — see "The .NET side is the container" below.

`netstandard2.0` for the wrapper, which reaches .NET Framework 4.6.1 and
everything after it — a CAD plug-in host is as likely to be one as the other.
It has no `LPUTF8Str` and `LPStr` is the ANSI code page, so paths cross the ABI
as nul-terminated UTF-8 bytes rather than as marshalled strings; anything else
turns a path that is not ASCII into a file the converter cannot find.

The ABI never unwinds: unwinding into C is undefined behaviour, and a converter
that takes a long-running host down over one bad file is not usable. Every entry
point catches a panic and returns `CADCONVERT_ERR_PANIC`. Every string it returns
was allocated by it and must come back to `cadconvert_string_free`; nothing the
caller allocates is ever freed inside.

239 tests, no warnings, `#![forbid(unsafe_code)]` still holds everywhere except
`cad-ffi`, where each `unsafe` block states what makes it sound.

## Where the memory goes (2026-08-22)

Peak resident size, measured with `/usr/bin/time -l`, for the 46-body pilot —
a 32.7 MB STEP and a 36.3 MB Parasolid file:

| | STEP | Parasolid |
|---|---:|---:|
| after reading | 529 MB | 540 MB |
| after meshing | ~700 MB | ~730 MB |
| whole conversion | ~700 MB | ~720 MB |

The meshing figures move by ±80 MB between runs; that phase is `rayon`'s and
its peak depends on how the work lands on threads. Reading is deterministic.

**The scene it all produces holds 19 MB.** `cargo run -p cad-convert --example
scene_bytes` walks it and counts: 11 214 faces, 26 535 edges, 96 111 NURBS
control points, and the largest single item is the arrays themselves at 8.3 MB.
So the several hundred megabytes are not the model — they are what the readers
churn to arrive at it.

`cargo run -p cad-convert --example read_memory` prints resident size at each
step and says where:

```
Parasolid                          after   change
  after reading the bytes           37 MB    +35
  after making it a string          72 MB    +35     ← a second copy
  after parsing the entity graph   488 MB   +417     ← this
  after lowering to a scene        473 MB    +18
```

**476 877 entities with 2 908 642 fields between them cost 417 MB** — about
875 bytes per entity, where the fields themselves account for perhaps 90 MB.
The rest is per-entity allocation: a `RawEntity` owns several small `Vec`s and
each is its own malloc. That is the one number worth attacking, and the shape
of the fix is an arena or a flat field pool indexed by entity rather than a
`Vec` per entity.

The STEP side reads its own file in 58 MB and lowers it in another 21 — 79 MB
in total, which is what the whole conversion ought to cost. It reaches 529 MB
because **it parses the Parasolid twin as well, in full, to recover fourteen
reflectivity flags** — the designer's metal-versus-matte, which the STEP does
not carry. `--no-twin` reads the same STEP in **79 MB**. The colours are
unchanged either way; only the metal/matte split is lost.

Two ways out, neither taken yet: parse the twin with a callback that keeps only
types 80 and 81 and never builds the entity vector, which is an `xt-parser`
change and would help both readers; or cache the fourteen flags beside the file
so the twin is read once ever. The second is a smaller change and the wrong one
— it makes a cache the user has to know about.

One thing was fixed here: `String::from_utf8_lossy` allocates a full second
copy when the bytes are not valid UTF-8, which these files often are not, and
the original was held for the whole parse. Owning the string and dropping the
bytes first took the STEP path from 563 MB to **529 MB**. Also fixed:
`p2m::AppearanceLibrary::bundled()` reparsed all 619 appearance files on every
call and is now built once per process — worth nothing on this file, because
only fourteen materials are resolved, and worth a great deal on a file with
thousands.

The measuring tools lied twice on the way, both times by succeeding at
something else: `dump_types` reported a 36 MB parse of the Parasolid file when
it had in fact panicked on invalid UTF-8 before parsing anything, and sampling
resident size with `ps` at the boundaries of `to_scene_with` showed a flat
80 MB while the peak inside it was 484 MB higher. A phase's cost has to be
measured by stopping after it — `cadconvert --stop-after read|mesh` exists for
exactly that, since peak resident size only ever grows.

## Does a conversion need both files? (2026-08-22)

Measured on the pilot, which ships as a STEP and a Parasolid twin of the same
model.

**For geometry, no.** `mesh_diff` between a STEP conversion with the twin and
one without is **0.0000 mm everywhere**, same vertex and triangle counts. The
twin moves no vertex; it is read only for appearance. Either file alone
produces a complete watertight solid — STEP the better of the two (0
non-manifold edges against 11, and 301 points over 0.2 mm from OpenCASCADE
against 1 790).

**For appearance, the twin carries one thing the STEP does not**: per-face
reflectivity, `SDL/TYSA_REFLECTIVITY`, the designer's own metal-versus-matte.
Colours are not the issue — both readers get all 42 named bodies right on their
own, verified instance by instance against OpenCASCADE. What is lost without it
is the finish, and losing it is not subtle:

| colour | with the twin | without |
|---|---|---|
| 808080, 81888C, 778888, 777788, 555555 | matte paint, metal 0.0 | **steel, metal 1.0, roughness 0.05** |
| 333333 | matte paint | **cast iron, metal 1.0** |
| 0033BB, 22BB88, 30BF94, CC0000 | roughness 0.92 | roughness 0.65 |

Six of fourteen colours change class. Colour inference reads a neutral grey as
machined metal, which is right for a machine part and wrong for a painted one,
and the whole compressor body comes out mirror-polished.

**But it is needed once, not every time.** The twin's whole contribution is
fourteen colour-to-finish facts, and the material table already carries exactly
that. `cadconvert --materials finish.txt --no-twin` reproduces the twin's result
on **twelve of twelve** paint and rubber colours — metal and roughness
identical, 0.92 for the powder coat — and the two metals match on both as well,
differing only in base colour, where naming a library material deliberately
takes the library's measured reflectance instead of blending it with the file's.

```
color 808080 = Toz Boya      # powder coat
color D1D1D1 = 6061 Alloy
color 555759 = AISI 1020
color 000000 = EPDM
```

What made that work is `Finish::from_name`: a trade name usually carries the
finish inside it — *toz boya* **is** powder coat — so the table can now say
everything the twin says. Before it, a table got the class right and the gloss
wrong: paint at 0.65 instead of 0.92.

The cost of not doing this is the whole memory profile of the STEP path:

| | peak after reading |
|---|---:|
| STEP + twin | 530 MB |
| STEP + a fourteen-line table | **80 MB** |
| Parasolid alone | 540 MB |

So: read the twin once, write the table, and every conversion after it is a
sixth of the memory with the same output. The default still reads the twin,
because a user who has both files and no table should get the right answer
without being told to make one.

**If there is only one file**: the Parasolid alone gives both geometry and
finish with nothing else needed, at slightly worse geometry (11 non-manifold
edges, 1 790 points over 0.2 mm against 301). The STEP alone gives the better
geometry and guesses the finish — well on saturated colours, badly on greys.

## The .NET side is the container (2026-08-22)

The repository was a Cargo workspace with a `dotnet/` folder inside it. It is
now the other way round, because the deliverable is the NuGet package and the
thing that has to be true of every build is that the package works:

```
CadConvert.slnx    the container
src/, tests/       the .NET package and its test suite
build/             Native.props (where the library is) and Native.targets (how
                   to build it)
native/            the Rust workspace — Cargo.toml and crates/
runtimes/          staged native libraries; build output, not source
```

**`dotnet build` builds the Rust side.** `build/Native.targets` runs
`cargo build -p cad-ffi` and copies the result to
`runtimes/{rid}/native/`, where both the package and the .NET host look for it.
There is no separate step to remember and no `pack.sh`: one command produces
something that works, which is the whole reason for the layout.

Three things that had to be got right:

* **Incrementality is cargo's.** Reproducing it in MSBuild with
  `Inputs`/`Outputs` means globbing the whole Rust tree on every build to
  answer a question cargo answers better. Measured: a second `dotnet build`
  with nothing changed takes **1.2 seconds**, most of it MSBuild's own
  start-up. The risk of making `dotnet build` do everything is that it slows
  the loop; it does not.
* **Only one project may run cargo.** Both the library and the test suite
  imported `Native.targets` at first, and cargo ran twice per build. The file
  is now split: `Native.props` says *where* the library is and anything may
  import it; `Native.targets` says *how to build it* and only the library
  project imports it. A project reference orders the two.
* **The pack job must not build.** CI builds each runtime on its own machine
  and stages all five before packing, so the pack step passes
  `-p:BuildNative=false` — that machine could only rebuild its own, and would
  need a Rust toolchain to do it.

`Debug` builds cargo's `dev` profile so a native breakpoint is usable;
everything else takes `release`, because a debug build of the tessellator is
roughly twenty times slower and nobody wants that by accident.

Verified after the move: 239 Rust tests, zero compiler warnings, `dotnet build`
and `dotnet pack` each clean, and a fresh console app restoring the packed
`CadConvert` converts the pilot to **2 043 886 triangles, 0 open edges,
11 non-manifold** — the same numbers as before it.

## The bundled material library is the one it claims to be (2026-08-22)

Downloaded again from
<https://ww3.cad.de/foren/ubb/uploads/Taiko/solidworksmaterials_adjusted.sldmat.txt>
and compared against `assets/solidworks-materials.sldmat`: **the decoded text is
identical**. 115 materials, 12 classifications, 115 optical blocks, 47 distinct
`pwshader` names, 17 texture references, on both sides; nothing in one that is
not in the other. The checksums differ only because the download is UTF-16 LE
with a BOM (275 456 bytes) and the bundled copy is UTF-8 (137 565).

Recorded in `assets/PROVENANCE.md` so it is not an open question again. The
appearance library beside it — 619 `.p2m` files, which is where paint lives —
is documented there too.

## The worst face in the model was the ruler (2026-08-22)

`[facesag]` ranked faces 122 and 126 of `201 201 003-51` at **9.28 mm** — six
times worse than anything else in the Parasolid mesh, and the STEP reading of
the same body tops out at 0.85. It looked like the largest defect left.

It is not a defect. The face is a groove swept all the way round, `u_closed`,
98 rows of control points and 102 knots in u. The `[facesag-edge]` line printed
underneath says what happened:

```
[facesag-edge] len 1.0734 uv [1.0000,0.6631]..[0.0476,0.4322]
```

A **1.07 mm** edge whose two ends sit at u = 1.0000 and u = 0.0476: it crosses
the seam. Its midpoint is not on the surface, so finding it means a search, and
the search lands on the far side of the loop — 9.28 mm away. The face's own
boundary points all invert to their own positions **exactly** (0.0000), so the
surface and the trim are right; only the ruler was wrong.

Independently: the whole Parasolid mesh's largest departure from OpenCASCADE
anywhere is 2.38 mm, so no face in it can be 9.28 mm from its own surface.

Averaging the two endpoints' own parameters instead — with the seam taken into
account, so the short way round is taken — was built and measured **worse**:
the parameter midpoint is only the chord's midpoint when the parameterisation
is uniform, and on this body it took faces that read 0.85 mm to 2.71. Reverted.

What stands instead is the rule for reading the number: **when
`[facesag-edge]` shows the two ends a period apart, the sag above it is the
seam and not the mesh.** That is the third time an instrument in this project
has pointed at the wrong thing by succeeding at something else — after
`dump_types` reporting a parse it had panicked before reaching, and `ps`
sampling a flat 80 MB across a call whose peak was 484 MB higher.

### Telling a measured grid from a restated boundary (2026-08-22)

A Parasolid blend has no closed-form lowering, so the reader stores a grid and
the tessellator, seeing a surface that is degree one in both directions, meshes
the face from its boundary instead. That is right for most of them and wrong
for some, and until now nothing distinguished the two:

* a grid the ball **rolled** — solved from it touching both mating surfaces at
  every station, and gated against this very boundary. Evidence about the
  interior.
* an **arc-sectioned** grid — a construction from the record's stated radius.
* a **Coons** patch — the boundary restated. No new information at all.

Offering every grid as the rebuild's interior was tried in July and measured
worse: points over 1 mm against OpenCASCADE 14 → 65, over 0.2 mm 2253 → 2683.
The conclusion drawn then — "the boundary is better information than the
geometry we recover from it" — was right about two of the three and wrong about
the first.

`Solid::measured`, a bool per surface, is the reader saying which. `cad-xt`
sets it only where `from_arc` is false, so only a real roll counts, and clears
it again when `make_each_blend_of_one_mind` puts a face back on its siblings'
interpolation. Every other reader leaves the vector empty, which reads as
"nothing here is measured".

**The corner match was the whole difficulty.** The grid's corners are where the
*reader's* `quad_corners` cut the boundary and the patch's are where the
tessellator's did; they agree about the shape and rarely about which corner is
first. Measured: 1020 faces offered a measured grid, and an identity match took
**14**. Trying all eight ways a square maps onto a square — four turns and
their mirrors, checked by evaluating the grid at the four mapped corners —
takes **195**. Nothing is stretched; it is the same grid read from a different
corner.

| | before | after |
|---|---:|---:|
| faces using their measured interior | 14 | **209** |
| theirs → ours, mean | 0.0079 mm | **0.0076 mm** |
| theirs → ours, p99 | 0.1030 mm | **0.0991 mm** |
| theirs → ours, over 0.05 mm | 13 201 | **12 371** |
| theirs → ours, over 0.2 mm | 1 789 | **1 646** |
| ours → theirs, p99 | 0.2564 mm | **0.2348 mm** |
| ours → theirs, over 0.2 mm | 20 822 | **17 532** |
| non-manifold edges | 11 | **10** |
| triangles, open edges, over 1 mm | 2 043 886, 0, 14 | unchanged |

Two further steps, and where the class ends.

**Cutting the ring at the grid's own corners.** The grid carries them — they
are the four ends of its control net — so rather than hoping two independent
`quad_corners` calls agree, the ring is cut at the points nearest them. That
took the count only from 195 to 209, but it registered the ones already in use
much better, which is where most of the table above comes from.

**Why the other 825 cannot be used, measured.** After cutting at the grid's own
corners, the distance from a grid corner to the nearest point on its face's
ring is **0.52 mm at the median** — on faces whose sides are 1.23 mm. Two
fifths of a side. The grids are not registered to the boundaries they were
gated against, because `grid_holds` asks whether every ring point lies near one
of the grid's *triangles*, which a grid wider than the face satisfies without
sharing a corner with it.

That is also the answer to why July's blanket experiment made things worse:
four fifths of those grids sit beside the face rather than on it, and taking
them as the interior drags it off the boundary they agree with only loosely.

**Landing the far edge on the file's own rail** was the next reading, and it
was wrong. A rolled grid's near edge *is* the file's rail, to the bit; its far
edge is only where the ball was solved to touch the other surface, so putting
each cross-section's far end onto the face's other rail — faded across the
section, both edges then exact — should have closed the drift. Measured: it did
not move the median at all (0.5217 → 0.5363 mm) and it cost accepted grids,
1034 offered → 910 and 209 used → 195. Reverted. **The drift is not in the far
edge.**

What is left, measured but not closed: the reader's ring and the tessellator's
are two samplings of the same boundary. A corner that is exact on one need not
be near any point of the other, and 0.52 mm is the size of that disagreement on
a 1.23 mm side. Closing it means making the two share a sampling — the reader
recording the ring it built the grid on, or both taking the boundary from the
same place — and that is a larger change than anything tried here.

Three readings tried in this class, one kept. `CAD_TESS_MEASURED=1` prints, per
face, whether a measured grid was offered and whether it was taken, and
`[corners]` beside it prints how far the best of the eight orientations misses.

## Production readiness: what was measured (2026-08-22)

**Malformed input.** A converter runs on files someone else produced, and the
contract is not that every one converts — most cannot — but that none of them
panics. A panic in a library is an abort in the CLI and a dead host process
wherever the C ABI is loaded into one.

Twenty-two mutations of the pilot — truncated at 0.1 %, 1 %, 10 %, 50 %, 90 %
and 99.9 %, two hundred bytes flipped in three places, empty files, arbitrary
bytes, and a STEP with a header and no bodies — put through the **whole**
pipeline, read, mesh and write:

```
produced a file: 8    refused cleanly: 14    crashed: 0
```

The eight that produced a file are truncations and bit-flips the readers
recover from by design, and the file from a bit-flipped input is still
watertight — 0 open edges. `crates/cad-convert/tests/malformed.rs` keeps this,
with a real 6 KB Parasolid part bundled beside it as the fixture and a
deterministic scatter of flipped bytes rather than a random one, so a failure
is reproducible from the test alone. 245 tests now.

**Concurrency.** Four threads calling `cadconvert_convert` at once on one process
each produce 3598 triangles from the same file. Nothing in the pipeline is
shared mutable state; the two bundled libraries are `OnceLock`s built once and
read thereafter.

**Panics that remain, by construction.** The C ABI catches unwinding at every
entry point and returns `CADCONVERT_ERR_PANIC`, because unwinding into C is
undefined behaviour. That is a backstop, not the design: the readers return
`Result` throughout and `#![forbid(unsafe_code)]` holds in every crate except
`cad-ffi`.

**Cost, at the default quality.**

| | STEP (32.7 MB) | Parasolid (36.3 MB) |
|---|---:|---:|
| wall clock | 11.2 s | 27.0 s |
| peak resident | 529 MB | 540 MB |
| output, lean | 31.03 MB | 31.99 MB |

The memory is the Parasolid entity graph either way — 417 MB for 476 877
entities — and the STEP path pays it only to read its twin's fourteen
reflectivity flags, which `--materials` replaces at 80 MB. See "Where the
memory goes".

**What is not measured, and is the gap to production:** nothing is committed —
2 033 untracked files and no remote, so CI has never run and no platform but
osx-arm64 has ever built this. The five-runtime matrix is written and untested.

## Memory: 710 MB to 400, and where the rest is (2026-08-22)

Two changes to how an entity is stored, measured on the pilot.

**`FieldVal` was 80 bytes because of a variant used 65 times.** Its largest
arm was `Mat3([f64; 9])` — 72 bytes inline — and an enum is as large as its
largest arm, so every one of the file's 2 908 642 fields cost 80 bytes to hold
values that are almost all eight. The file contains **65 matrices**, two
thousandths of one per cent. Boxing that one arm takes `FieldVal` to 32 bytes
and the field storage from **233.6 MB to 93.4 MB**, at the cost of 65
allocations.

**`RawEntity` was 184 bytes because of five `Vec`s most entities never use.**
`var_f64`, `var_i16`, `var_i32`, `var_ptr` and `var_char` are 120 bytes of
headers on every entity, and an allocation each on every entity that uses one;
55 % of entities have no tail at all. Behind one `Option<Box<VarTail>>` the
struct is **72 bytes** and the graph's structs fall from **87.7 MB to 34.3 MB**.
The five fields became accessors returning slices, so an entity with no tail
answers without allocating.

**And the text was held twice.** `String::from_utf8_lossy` allocates a full
second copy when the bytes are not valid UTF-8, which these files usually are
not, and `scene_from_file` held the original across the whole parse. Owning the
string and dropping the bytes first, then dropping the string before lowering,
is another file's-worth.

| peak resident | before | after |
|---|---:|---:|
| Parasolid, read only | 540 MB | **~295 MB** |
| Parasolid, whole conversion | ~710 MB | ~470 MB |
| STEP, whole conversion | ~700 MB | ~400 MB |
| STEP, read only, no twin | 79 MB | 79 MB |

The mesh is bit-identical throughout — 2 043 886 triangles, 0 open edges,
10 non-manifold, the same distances to OpenCASCADE — and 245 tests pass.

**mimalloc was tried and is worse.** The reading was that millions of small
short-lived allocations set the high-water mark, so a better allocator should
lower it. Measured: STEP whole 346–450 MB → 524–549, Parasolid 395–496 →
440–523. It trades memory for speed by holding freed pages. Reverted.

**What is left is the tessellator, and it levels off.** Reading a STEP without
its twin is 79 MB; converting it is ~400. The scene is 19 MB and the mesh 53,
so ~300 MB is the meshing, and it is spread across rayon's worker threads
rather than held anywhere. Ten conversions in one process:

```
run   1      2      3      4      5  …  10
     399    523    572    598    598 … 598 MB
```

It plateaus at the fourth and does not move again — allocator high-water, not
retention. That is the number a long-running service should budget for, and
`bench/CadConvert.Bench` is how to get it on any platform: peak
resident comes from `getrusage(RUSAGE_SELF)` on Unix and `PeakWorkingSet64` on
Windows, because .NET has no one way to ask and the macOS answer is zero.
