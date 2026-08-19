# xt-parser — Parasolid PS30+ XT Text Format Reference

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

# xt-winnow output
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
