//! Raw entity representation and compact transmit format entity parser.
//!
//! Entity stream format (from reverse engineering of pskernel.dll):
//!   - After the 2-field preamble (N_types, entity_count), entities follow
//!   - Each entity: <type_id> <lazy_inline_schema> <entity_index> <fields...>
//!   - Schema is read lazily on first encounter of each type_id
//!   - type_id == 1 is the stream terminator
//!   - Newlines must be stripped from input before calling

#![allow(unused)]

use std::collections::HashMap;
use winnow::prelude::*;
use winnow::ascii;
use winnow::token::any;

use crate::error::{Result, XtError};
use crate::schema::{self, EntitySchema, FieldType, VarType};
use crate::token;

// ── Raw entity types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum FieldVal {
    Int(i64),
    Float(f64),
    Short(i16),
    Char(char),
    Bool(bool),
    Byte(u8),
    Ptr(usize),
    /// Boxed for the reason the matrix below is boxed, one variant down and
    /// three orders of magnitude bigger. A `[f64; 3]` is 24 bytes and it sized
    /// the whole enum to 32; on the pilot **95% of 2 908 642 fields carry
    /// eight bytes or less** and every one of them paid for these. Behind a
    /// box the enum is 16 and the field storage falls from 93.4 MB to 46.7,
    /// against 3.4 MB of boxes and 144 357 more allocations.
    ///
    /// `as_vec3` returns by value, so none of its thirty-four callers moved.
    Vec3(Box<[f64; 3]>),
    Interval(Box<[f64; 2]>),
    /// Boxed on purpose. An unboxed `[f64; 9]` is 72 bytes and it sizes the
    /// whole enum: every one of a file's fields then costs 80 bytes where
    /// almost all of them hold eight. The pilot has 2 908 642 fields and
    /// **65 matrices** — two thousandths of one per cent — so inlining them
    /// cost 140 MB to save 65 allocations.
    Mat3(Box<[f64; 9]>),
}

impl FieldVal {
    pub fn as_ptr(&self) -> usize {
        match self {
            FieldVal::Ptr(p) => *p,
            FieldVal::Int(i) => *i as usize,
            _ => 0,
        }
    }
    pub fn as_i64(&self) -> i64 {
        match self {
            FieldVal::Int(i) => *i,
            FieldVal::Short(s) => *s as i64,
            FieldVal::Byte(b) => *b as i64,
            FieldVal::Ptr(p) => *p as i64,
            FieldVal::Float(f) => *f as i64,
            _ => 0,
        }
    }
    pub fn as_f64(&self) -> f64 {
        match self {
            FieldVal::Float(f) => *f,
            FieldVal::Int(i) => *i as f64,
            FieldVal::Short(s) => *s as f64,
            FieldVal::Byte(b) => *b as f64,
            _ => 0.0,
        }
    }
    pub fn as_char(&self) -> char {
        match self {
            FieldVal::Char(c) => *c,
            _ => '?',
        }
    }
    pub fn as_bool(&self) -> bool {
        match self {
            FieldVal::Bool(b) => *b,
            FieldVal::Int(i) => *i != 0,
            _ => false,
        }
    }
    pub fn as_vec3(&self) -> [f64; 3] {
        match self {
            FieldVal::Vec3(v) => **v,
            _ => [0.0; 3],
        }
    }
    pub fn as_mat3(&self) -> Option<[f64; 9]> {
        match self {
            FieldVal::Mat3(m) => Some(**m),
            _ => None,
        }
    }
    pub fn as_byte(&self) -> u8 {
        match self {
            FieldVal::Byte(b) => *b,
            FieldVal::Int(i) => *i as u8,
            _ => 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RawEntity {
    pub type_id: u16,
    pub index: usize,
    /// Where this entity's fields sit in [`Entities::arena`].
    ///
    /// A range rather than a `Vec` of its own. Half a million entities each
    /// owning a small vector is half a million allocations, and the allocator
    /// never gives that memory back to the kernel afterwards: it is freed into
    /// free lists of a hundred bytes each, and the tessellator that runs next
    /// asks for buffers of megabytes. Measured on the pilot, dropping the
    /// entity graph returned **nothing** while dropping the meshes — which are
    /// a few large buffers — returned 148 MB.
    ///
    /// One arena is two allocations for the whole graph, and they come back.
    fields: Span,
    /// The variable-length tails, allocated only for the entities that have
    /// any.
    ///
    /// Five `Vec`s inline cost 120 bytes on every entity whether it uses them
    /// or not, and five allocations on every entity that uses one of them. On
    /// the pilot, 55 % of entities have no tail at all and the five sit empty;
    /// behind one box the struct falls from 184 bytes to 72 and the allocation
    /// count with it.
    var: Span,
    var_kind: VarKind,
    /// The elements of fixed arrays past the first, in the order they were
    /// read. A field written as two pointers — an intersection curve's two
    /// surfaces — occupies one slot in `fields`, and the second pointer would
    /// otherwise be consumed from the stream and thrown away. Without it an
    /// intersection can only be taken from the sparse chart the file also
    /// carries, which states its own error in millimetres.
    extra: Span,
}

/// A run of fields inside the arena.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Span {
    start: u32,
    len: u32,
}

impl Span {
    fn of(start: usize, len: usize) -> Span {
        Span {
            start: start as u32,
            len: len as u32,
        }
    }
}

/// Every entity of a file, and the one array their fields live in.
///
/// Indexing goes through here rather than through the entity because the
/// entity does not own its fields — see [`RawEntity::fields`]. It derefs to
/// the entities, so anything that only wants to walk them can.
#[derive(Debug, Clone, Default)]
pub struct Entities {
    items: Vec<RawEntity>,
    arena: Vec<FieldVal>,
    /// The variable-length tails, one array per element type.
    ///
    /// Same argument as the field arena above, and a larger count: 213 166 of
    /// the pilot's entities carry a tail, and each was a `Vec` inside a `Box`
    /// — 426 332 allocations holding 6.5 MB between them. An entity now names
    /// a run in one of these five.
    f64s: Vec<f64>,
    i16s: Vec<i16>,
    i64s: Vec<i64>,
    ptrs: Vec<usize>,
    chars: Vec<char>,
}

impl Entities {
    pub fn with_capacity(entities: usize, fields: usize) -> Entities {
        Entities {
            items: Vec::with_capacity(entities),
            arena: Vec::with_capacity(fields),
            ..Default::default()
        }
    }

    /// The fields of one entity.
    pub fn fields(&self, e: &RawEntity) -> &[FieldVal] {
        let (s, n) = (e.fields.start as usize, e.fields.len as usize);
        &self.arena[s..s + n]
    }

    /// The elements of fixed arrays past the first. See [`RawEntity::extra`].
    pub fn extra(&self, e: &RawEntity) -> &[FieldVal] {
        let (s, n) = (e.extra.start as usize, e.extra.len as usize);
        &self.arena[s..s + n]
    }

    /// The fields of the entity at `i`, for a caller holding an index rather
    /// than a reference.
    pub fn fields_at(&self, i: usize) -> &[FieldVal] {
        self.fields(&self.items[i])
    }

    pub fn arena_len(&self) -> usize {
        self.arena.len()
    }

    /// How many elements sit in the five tail arrays between them.
    pub fn tail_len(&self) -> usize {
        self.f64s.len() + self.i16s.len() + self.i64s.len() + self.ptrs.len() + self.chars.len()
    }

    /// Read-only views of one entity's tail. Empty where the entity has none,
    /// which is most of them, and empty where it has one of another type —
    /// exactly what the five methods on `RawEntity` did before the tails moved
    /// into these arrays.
    pub fn var_f64(&self, e: &RawEntity) -> &[f64] {
        match e.var_kind {
            VarKind::F64 => &self.f64s[e.var.range()],
            _ => &[],
        }
    }
    pub fn var_i16(&self, e: &RawEntity) -> &[i16] {
        match e.var_kind {
            VarKind::I16 => &self.i16s[e.var.range()],
            _ => &[],
        }
    }
    pub fn var_i32(&self, e: &RawEntity) -> &[i64] {
        match e.var_kind {
            VarKind::I32 => &self.i64s[e.var.range()],
            _ => &[],
        }
    }
    pub fn var_ptr(&self, e: &RawEntity) -> &[usize] {
        match e.var_kind {
            VarKind::Ptr => &self.ptrs[e.var.range()],
            _ => &[],
        }
    }
    pub fn var_char(&self, e: &RawEntity) -> &[char] {
        match e.var_kind {
            VarKind::Char => &self.chars[e.var.range()],
            _ => &[],
        }
    }

    fn push(&mut self, mut entity: RawEntity, fields: &[FieldVal], extra: &[FieldVal], tail: &Tail) {
        entity.fields = Span::of(self.arena.len(), fields.len());
        self.arena.extend_from_slice(fields);
        entity.extra = Span::of(self.arena.len(), extra.len());
        self.arena.extend_from_slice(extra);
        entity.var_kind = tail.kind;
        entity.var = match tail.kind {
            VarKind::None => Span::default(),
            VarKind::F64 => {
                let s = Span::of(self.f64s.len(), tail.f64s.len());
                self.f64s.extend_from_slice(&tail.f64s);
                s
            }
            VarKind::I16 => {
                let s = Span::of(self.i16s.len(), tail.i16s.len());
                self.i16s.extend_from_slice(&tail.i16s);
                s
            }
            VarKind::I32 => {
                let s = Span::of(self.i64s.len(), tail.i64s.len());
                self.i64s.extend_from_slice(&tail.i64s);
                s
            }
            VarKind::Ptr => {
                let s = Span::of(self.ptrs.len(), tail.ptrs.len());
                self.ptrs.extend_from_slice(&tail.ptrs);
                s
            }
            VarKind::Char => {
                let s = Span::of(self.chars.len(), tail.chars.len());
                self.chars.extend_from_slice(&tail.chars);
                s
            }
        };
        self.items.push(entity);
    }
}

impl Span {
    fn range(self) -> std::ops::Range<usize> {
        self.start as usize..self.start as usize + self.len as usize
    }
}

impl std::ops::Deref for Entities {
    type Target = [RawEntity];
    fn deref(&self) -> &[RawEntity] {
        &self.items
    }
}

/// Which of [`Entities`]'s five tail arrays an entity's tail is in.
///
/// One byte beside the span, where a `Box<VarTail>` was a pointer and an
/// allocation. The schema names a single `var_type` and the reader matches it
/// once, so an entity has a tail of exactly one type or none at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VarKind {
    #[default]
    None,
    F64,
    I16,
    /// `i64`, not `i32`: the reader widens on the way in and `var_i32` hands
    /// back `&[i64]`.
    I32,
    Ptr,
    Char,
}

/// One entity's tail while it is being read, before it is copied into the
/// arrays. Reused for every entity, like the field scratch beside it.
#[derive(Debug, Default)]
struct Tail {
    kind: VarKind,
    f64s: Vec<f64>,
    i16s: Vec<i16>,
    i64s: Vec<i64>,
    ptrs: Vec<usize>,
    chars: Vec<char>,
}

impl Tail {
    fn clear(&mut self) {
        self.kind = VarKind::None;
        self.f64s.clear();
        self.i16s.clear();
        self.i64s.clear();
        self.ptrs.clear();
        self.chars.clear();
    }
}

// ── Inline schema descriptor ────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct FieldDesc {
    type_char: char,
    entity_type_id: u16,
    array_count: i32,
    element_type: bool,
}

#[derive(Debug, Clone)]
struct InlineSchema {
    fields: Vec<FieldDesc>,
    is_variable: bool,
    var_type: Option<VarType>,
}

// ── Entity stream parser ────────────────────────────────────────────────────

/// Parse all entities from the compact transmit format.
/// `input` must have newlines already stripped.
/// `partition_count` is from the preamble (usually 0).
pub fn parse_entities(input: &mut &str, partition_count: usize) -> Result<Entities> {
    parse_entities_opt(input, partition_count, true, 0).map(|(e, _)| e)
}

/// Like [`parse_entities`], but `inline_schemas` selects the stream dialect.
///
/// When false, the file names a complete schema in its T-line rather than a
/// base to diff against: no per-type inline schema annotations are present, so
/// each entity's fields are read straight from the standard layout for
/// `key_major` (see [`schema::standard_schema`]).
pub fn parse_entities_opt(
    input: &mut &str,
    partition_count: usize,
    inline_schemas: bool,
    key_major: u32,
) -> Result<(Entities, Option<Truncation>)> {
    parse_entities_keeping(input, partition_count, inline_schemas, key_major, None)
}

/// Like [`parse_entities_opt`], but keeps only the entity types asked for.
///
/// Every entity is still *read* — the stream has no index and the only way to
/// reach the next entity is through the one before it — but an entity whose
/// type is not wanted is dropped as soon as it has been read instead of being
/// kept for the caller.
///
/// This is for a reader that wants a handful of facts out of a whole file. The
/// appearance walk is the case it was written for: it needs four kinds of
/// entity out of 476 877 to recover fourteen colour-to-finish lines, and
/// keeping the rest cost 209 MB while the STEP file that asked for them was
/// still resident.
///
/// `keep` is a sorted slice of type ids. `None` keeps everything.
pub fn parse_entities_keeping(
    input: &mut &str,
    partition_count: usize,
    inline_schemas: bool,
    key_major: u32,
    keep: Option<&[u16]>,
) -> Result<(Entities, Option<Truncation>)> {
    let mut schema_cache: HashMap<u16, InlineSchema> = HashMap::new();
    // Sized from the text rather than grown into. One arena that doubles is
    // one realloc that holds the old buffer and the new at once, and at 46 MB
    // that transient is bigger than anything it saves: the peak went *up* when
    // the arena went in, until this line.
    //
    // The stream is one token per field and a token averages a dozen bytes, so
    // the length of the body is the estimate. Being wrong costs a realloc,
    // which is what would have happened anyway.
    // Sized from the text when everything is kept, and barely at all when a
    // filter is in play: the appearance walk keeps four kinds of entity out of
    // half a million, and reserving the whole file's worth for it was 46 MB
    // asked for and never used.
    let mut entities = match keep {
        None => Entities::with_capacity(input.len() / 70, input.len() / 12),
        Some(_) => Entities::default(),
    };
    // Reused for every entity; see `read_entity_fields`.
    let (mut scratch, mut scratch_extra) = (Vec::new(), Vec::new());
    let mut tail = Tail::default();
    // How many were read, which stops being the same as how many were kept the
    // moment a filter is in play. The truncation report is about the stream,
    // so it counts what was read.
    let mut read = 0usize;
    let mut truncated: Option<Truncation> = None;

    loop {
        token::ws(input).map_err(|_| XtError::UnexpectedEof)?;
        if input.is_empty() {
            break;
        }

        // Read type_id (space-delimited decimal uint16).
        let type_id = match read_uint16(input) {
            Ok(t) => t,
            Err(e) => {
                eprintln!(
                    "[xt-parser] type_id read failed after {} entities: {} (next: {:?})",
                    entities.len(), e, &input[..30.min(input.len())]
                );
                break;
            }
        };

        // type_id == 1 is the stream terminator.
        if type_id == 1 {
            let _partition_idx = read_int32(input)?;
            break;
        }

        // Lazily read and cache inline schema. On error, stop gracefully
        // and return what we have — incomplete base schemas will cause errors
        // for some entity types.
        let checkpoint = *input;
        let result: Result<RawEntity> = (|| {
            if !schema_cache.contains_key(&type_id) {
                let s = if inline_schemas {
                    read_inline_schema(input, type_id)?
                } else {
                    let base = schema::standard_schema(type_id, key_major).ok_or_else(|| {
                        XtError::Parse {
                            offset: 0,
                            detail: format!("no base schema for entity type {}", type_id),
                        }
                    })?;
                    inline_schema_from_base(&base)
                };
                schema_cache.insert(type_id, s);
            }
            let schema = schema_cache[&type_id].clone();

            // From Ghidra RE (pk_receive_entity_typed + pk_read_inline_schema):
            // If the last field has array_count==1 (variable-length), a VERSION/COUNT
            // int is read before entity_index. For pure-variable entities (BSPLINE_VERTICES,
            // KNOT_MULT, REAL_VALUES, ATT_DEF_ID, etc.), this count IS the array length.
            // For ATTRIBUTE (7 fixed fields + variable pointer tail), a VERSION int
            // is also read before entity_index; var_count pointer values follow
            // the fixed fields.
            let has_version = schema.is_variable;
            let var_count = if has_version {
                read_int32(input)? as usize
            } else {
                0
            };

            let entity_index = read_int32(input)? as usize;
            let entity = read_entity_fields(
                input,
                type_id,
                entity_index,
                var_count,
                &schema,
                &mut scratch,
                &mut scratch_extra,
                &mut tail,
            )?;
            for _ in 0..partition_count {
                let _ = read_int32(input)?;
            }
            Ok(entity)
        })();

        match result {
            Ok(entity) => {
                if std::env::var_os("XT_TRACE").is_some() {
                    eprintln!(
                        "[trace] #{} type={} idx={} fields={} next={:?}",
                        read, entity.type_id, entity.index, scratch.len(),
                        &input[..40.min(input.len())],
                    );
                }
                read += 1;
                if keep.is_none_or(|k| k.binary_search(&entity.type_id).is_ok()) {
                    entities.push(entity, &scratch, &scratch_extra, &tail);
                }
            }
            Err(e) => {
                truncated = Some(Truncation {
                    type_id,
                    entities_read: read,
                    detail: e.to_string(),
                });
                break;
            }
        }
    }

    Ok((entities, truncated))
}

/// Where an entity stream stopped short, and why.
///
/// The stream is read until it ends or an entity cannot be understood. Stopping
/// early is not the same as reaching the end: the entities read so far are
/// usable, but the topology built from them will be missing whatever came
/// after. Reporting that only on stderr — as this did — let a caller checking
/// the `Result` see a clean success over a body with no faces at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Truncation {
    /// The entity type that could not be read.
    pub type_id: u16,
    /// How many entities were read before it.
    pub entities_read: usize,
    /// The parse error.
    pub detail: String,
}

impl std::fmt::Display for Truncation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "stopped at type_id={} after {} entities: {}",
            self.type_id, self.entities_read, self.detail
        )
    }
}

// ── Inline schema reading ───────────────────────────────────────────────────

fn read_inline_schema(input: &mut &str, type_id: u16) -> Result<InlineSchema> {
    if let Some(base) = schema::base_schema(type_id) {
        read_inline_schema_path_a(input, type_id, &base)
    } else {
        read_inline_schema_path_b(input, type_id)
    }
}

/// Path A: type has base schema (sch_13006) → read annotation diff.
fn read_inline_schema_path_a(
    input: &mut &str,
    type_id: u16,
    base: &EntitySchema,
) -> Result<InlineSchema> {
    let n_new_fields = read_uint8(input)?;

    // 255 (0xFF) or 0 = use base schema as-is (no annotation diffs).
    if n_new_fields == 255 || n_new_fields == 0 {
        return Ok(inline_schema_from_base(base));
    }

    let mut fields: Vec<FieldDesc> = Vec::new();

    // The annotation chars index the base's *logical* fields — one per schema
    // declaration — while `base.fields` stores multi-element declarations
    // pre-expanded (ATTRIB_DEF's `actions; u; 1 0 8` is eight slots). Walk the
    // two in step: `logical_idx` counts declarations, `slot_idx` counts
    // expanded slots, and each logical field covers `span(logical_idx)` slots.
    // Advancing one slot per `C` — the old behaviour — desynchronised after
    // the first multi-element field, which is what stopped every Solid Edge
    // export at its first ATTRIB_DEF.
    let span = |logical_idx: usize| -> usize {
        base.logical_spans
            .as_ref()
            .and_then(|s| s.get(logical_idx).copied())
            .unwrap_or(1)
    };
    let n_logical = base
        .logical_spans
        .as_ref()
        .map(|s| s.len())
        .unwrap_or(base.fields.len());
    let mut logical_idx: usize = 0;
    let mut slot_idx: usize = 0;

    loop {
        let ch = read_raw_byte(input)?;
        match ch {
            'C' => {
                if logical_idx < n_logical {
                    for k in 0..span(logical_idx) {
                        fields.push(field_desc_from_base(base.fields[slot_idx + k]));
                    }
                } else if base.is_variable && logical_idx == n_logical {
                    // `base.fields` holds only the fixed fields; a variable base
                    // declares one more, the trailing array. A `C` landing on it
                    // copies that array, which read_entity_fields reads on its
                    // own from var_count — so push nothing. Pushing a field here
                    // would consume the array's first element twice.
                    //
                    // POINTER_LIS_BLOCK (74) in 500.076.x_t: base [D, P] + ptr
                    // array, annotated `C I(index_map_offset,d) C C Z` → fixed
                    // fields d d p, then the 20-element array.
                } else {
                    // Base schema incomplete — default to pointer.
                    fields.push(FieldDesc {
                        type_char: 'p',
                        entity_type_id: 0,
                        array_count: 0,
                        element_type: false,
                    });
                }
                slot_idx += span(logical_idx);
                logical_idx += 1;
            }
            'D' => {
                slot_idx += span(logical_idx);
                logical_idx += 1;
            }
            'I' | 'A' => {
                let fd = read_field_descriptor(input)?;
                fields.push(fd);
            }
            'Z' => {
                break;
            }
            other => {
                return Err(XtError::Parse {
                    offset: 0,
                    detail: format!(
                        "inline schema type {}: unexpected annotation char {:?} (0x{:02x})",
                        type_id, other, other as u8
                    ),
                });
            }
        }
    }

    if std::env::var_os("XT_DUMP_SCHEMA").is_some() {
        let sig: String = fields
            .iter()
            .map(|f| {
                if f.array_count > 1 {
                    format!("{}x{}", f.type_char, f.array_count)
                } else {
                    f.type_char.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!("[schema] type={} n={} : {}", type_id, fields.len(), sig);
    }

    Ok(InlineSchema {
        fields,
        is_variable: base.is_variable,
        var_type: base.var_type,
    })
}

/// Path B: completely new type → read full field descriptor list.
fn read_inline_schema_path_b(input: &mut &str, type_id: u16) -> Result<InlineSchema> {
    let n_fields = read_uint8(input)?;
    let _type_name = read_type_name(input)?;
    let _alias_name = read_type_name(input)?;

    let mut fields = Vec::with_capacity(n_fields as usize);
    for _ in 0..n_fields {
        fields.push(read_field_descriptor(input)?);
    }

    // If the last field has array_count == 1, the entity is variable-length.
    // The VERSION int (read before entity_index) is the array element count,
    // and the variable array is read as a trailing block after all fixed fields.
    // Remove the variable field from the fixed field list.
    let (is_variable, var_type) = match fields.last() {
        Some(fd) if fd.array_count == 1 => {
            let vt = match fd.type_char {
                'f' => Some(VarType::F64),
                'p' | 't' => Some(VarType::Ptr),
                'd' => Some(VarType::I16),
                'n' | 'w' => Some(VarType::I16),
                'c' => Some(VarType::Char),
                'v' => Some(VarType::V3),
                'u' => Some(VarType::I16),
                _ => None,
            };
            (true, vt)
        }
        _ => (false, None),
    };
    if is_variable {
        fields.pop();
    }

    Ok(InlineSchema {
        fields,
        is_variable,
        var_type,
    })
}

fn read_field_descriptor(input: &mut &str) -> Result<FieldDesc> {
    let _field_type_name = read_type_name(input)?;
    let entity_type_id = read_uint16(input)?;
    let array_count = read_int32(input)? as i32;

    // entity_type_id == 0: raw data field → sub_name gives element type
    // entity_type_id > 0: pointer to entity → type is 'p'
    let type_char = if entity_type_id == 0 {
        let sub_name = read_type_name(input)?;
        sub_name.chars().next().unwrap_or('d')
    } else {
        'p'
    };

    let element_type = if array_count == 1 {
        read_bool_tf(input)?
    } else {
        false
    };

    Ok(FieldDesc {
        type_char,
        entity_type_id,
        array_count,
        element_type,
    })
}

// ── Entity field reading ────────────────────────────────────────────────────

/// Read one entity into `fields` and `extra`, which the caller reuses.
///
/// Scratch rather than fresh vectors: an entity's fields end up in the arena,
/// and a `Vec` per entity on the way there is the half-million allocations
/// this arrangement exists to avoid.
fn read_entity_fields(
    input: &mut &str,
    type_id: u16,
    index: usize,
    var_count: usize,
    schema: &InlineSchema,
    fields: &mut Vec<FieldVal>,
    extra: &mut Vec<FieldVal>,
    tail: &mut Tail,
) -> Result<RawEntity> {
    fields.clear();
    extra.clear();
    tail.clear();
    fields.reserve(schema.fields.len());
    let mut entity = RawEntity {
        type_id,
        index,
        fields: Span::default(),
        var: Span::default(),
        var_kind: VarKind::None,
        extra: Span::default(),
    };

    // Read fixed fields first.
    for fd in &schema.fields {
        let val = read_field_value(input, fd, extra)?;
        fields.push(val);
    }

    // If the entity has a trailing variable-length array, read it.
    // Variable-length entities: the var_count (from the VERSION field read
    // before entity_index) determines the array element count.
    // For entities with fixed V/h-type fields (like CHART, LIMIT), the first
    // h-type element is in the fixed section, so variable count = version - 1.
    if schema.is_variable {
        let has_fixed_hvec = schema.fields.iter().any(|f| f.type_char == 'v');
        let _ = &fields;
        let count = if has_fixed_hvec && var_count > 0 {
            var_count - 1
        } else {
            var_count
        };
        // Sized from the count in hand rather than grown into. The tails held
        // 12.2 MB of capacity for 6.5 MB of data and 48 798 reallocations to
        // get there.
        //
        // Nothing is stored when the count is zero: a variable entity with an
        // empty tail keeps `var: None` and costs no box, which is what the old
        // `tail()`-inside-the-loop shape did by accident and this does on
        // purpose.
        if count > 0 {
            match schema.var_type {
                Some(VarType::F64) => {
                    tail.kind = VarKind::F64;
                    tail.f64s.reserve(count);
                    for _ in 0..count {
                        tail.f64s.push(read_f64(input)?);
                    }
                }
                Some(VarType::I16) => {
                    tail.kind = VarKind::I16;
                    tail.i16s.reserve(count);
                    for _ in 0..count {
                        tail.i16s.push(read_int16(input)?);
                    }
                }
                Some(VarType::I32) => {
                    tail.kind = VarKind::I32;
                    tail.i64s.reserve(count);
                    for _ in 0..count {
                        tail.i64s.push(read_int32(input)? as i64);
                    }
                }
                Some(VarType::Ptr) => {
                    tail.kind = VarKind::Ptr;
                    tail.ptrs.reserve(count);
                    for _ in 0..count {
                        tail.ptrs.push(read_int32(input)? as usize);
                    }
                }
                Some(VarType::Char) | Some(VarType::RawChar) => {
                    tail.kind = VarKind::Char;
                    tail.chars.reserve(count);
                    for _ in 0..count {
                        tail.chars.push(read_raw_byte(input)?);
                    }
                }
                // Three doubles an element, into the same array the F64 arm
                // fills: a kind of its own would hide every CHART, LIMIT and
                // HULL point from `var_f64`.
                Some(VarType::V3) => {
                    tail.kind = VarKind::F64;
                    tail.f64s.reserve(count * 3);
                    for _ in 0..count {
                        tail.f64s.push(read_f64(input)?);
                        tail.f64s.push(read_f64(input)?);
                        tail.f64s.push(read_f64(input)?);
                    }
                }
                None => {
                    for _ in 0..count {
                        let _ = read_int32(input)?;
                    }
                }
            }
        }
    }

    Ok(entity)
}

fn read_field_value(
    input: &mut &str,
    fd: &FieldDesc,
    extra: &mut Vec<FieldVal>,
) -> Result<FieldVal> {
    let count = match fd.array_count {
        0 => 1usize,
        1 => read_int32(input)? as usize, // variable-length
        n => n as usize,                   // fixed array
    };

    // A 9-float fixed array is a rotation matrix (TRANSFORM), and collapsing
    // it to its first element throws away the rotation — every instanced body
    // would land unrotated at its translation. Keep all nine.
    if fd.type_char == 'f' && count == 9 {
        let mut m = [0.0f64; 9];
        for slot in &mut m {
            *slot = read_f64(input)?;
        }
        return Ok(FieldVal::Mat3(Box::new(m)));
    }

    // The first element stays in the field's own slot, so nothing downstream
    // has to move; the rest are kept aside rather than dropped.
    let first = read_single_field(input, fd.type_char)?;
    for _ in 1..count {
        extra.push(read_single_field(input, fd.type_char)?);
    }
    Ok(first)
}

fn read_single_field(input: &mut &str, type_char: char) -> Result<FieldVal> {
    match type_char {
        'p' | 't' => Ok(FieldVal::Ptr(read_ptr(input)?)),
        'd' => Ok(FieldVal::Int(read_int32(input)?)),
        'f' => Ok(FieldVal::Float(read_f64(input)?)),
        'c' => Ok(FieldVal::Char(read_raw_byte(input)?)),
        'u' => {
            // Read as int32 and truncate to u8 — text format can have values > 255
            // for packed multi-byte fields like ATTRIB_DEF.actions×8.
            let v = read_int32(input)? as u8;
            Ok(FieldVal::Byte(v))
        }
        'v' => {
            // Vector: 3 doubles, OR a single `?` fills all 3 with NaN.
            // The Parasolid text_read_vector (0x182055440) checks for `?` once
            // at the start; if found, all 3 components are NaN and only the `?`
            // byte is consumed from the stream.
            token::ws(input).map_err(|_| XtError::UnexpectedEof)?;
            if input.starts_with('?') {
                *input = &input[1..];
                consume_space(input);
                Ok(FieldVal::Vec3(Box::new([f64::NAN; 3])))
            } else {
                let x = read_f64(input)?;
                let y = read_f64(input)?;
                let z = read_f64(input)?;
                Ok(FieldVal::Vec3(Box::new([x, y, z])))
            }
        }
        'b' => {
            // Box: 6 doubles, OR a single `?` fills all 6 with NaN.
            token::ws(input).map_err(|_| XtError::UnexpectedEof)?;
            if input.starts_with('?') {
                *input = &input[1..];
                consume_space(input);
                Ok(FieldVal::Vec3(Box::new([f64::NAN; 3])))
            } else {
                let x1 = read_f64(input)?;
                let y1 = read_f64(input)?;
                let z1 = read_f64(input)?;
                let _x2 = read_f64(input)?;
                let _y2 = read_f64(input)?;
                let _z2 = read_f64(input)?;
                Ok(FieldVal::Vec3(Box::new([x1, y1, z1])))
            }
        }
        'n' | 'w' => Ok(FieldVal::Short(read_int16(input)?)),
        'l' => Ok(FieldVal::Bool(read_bool_tf(input)?)),
        'i' => {
            // Interval: 2 doubles, OR a single `?` fills both with NaN.
            token::ws(input).map_err(|_| XtError::UnexpectedEof)?;
            if input.starts_with('?') {
                *input = &input[1..];
                consume_space(input);
                Ok(FieldVal::Interval(Box::new([f64::NAN; 2])))
            } else {
                let lo = read_f64(input)?;
                let hi = read_f64(input)?;
                Ok(FieldVal::Interval(Box::new([lo, hi])))
            }
        }
        'q' => {
            // Quaternion: NOT read from stream, zeroed.
            Ok(FieldVal::Float(0.0))
        }
        's' => {
            // Opaque skip token: read and discard one whitespace-delimited token.
            token::ws(input).map_err(|_| XtError::UnexpectedEof)?;
            let _ = winnow::token::take_while::<_, _, winnow::error::ContextError>(
                1.., |c: char| !c.is_ascii_whitespace()
            ).parse_next(input).map_err(|_| XtError::UnexpectedEof)?;
            consume_space(input);
            Ok(FieldVal::Int(0))
        }
        'h' => {
            // Only pvec (3 doubles) is transmitted; other hvec components
            // are recalculated by Parasolid on load. Reference section 2.1.4:
            // "only the position vector is written to XT data".
            let x = read_f64(input)?;
            let y = read_f64(input)?;
            let z = read_f64(input)?;
            Ok(FieldVal::Vec3(Box::new([x, y, z])))
        }
        other => Err(XtError::Parse {
            offset: 0,
            detail: format!("unknown field type char {:?}", other),
        }),
    }
}

// ── Schema helpers ──────────────────────────────────────────────────────────

fn inline_schema_from_base(base: &EntitySchema) -> InlineSchema {
    InlineSchema {
        fields: base.fields.iter().map(|ft| field_desc_from_base(*ft)).collect(),
        is_variable: base.is_variable,
        var_type: base.var_type,
    }
}

fn field_desc_from_base(ft: FieldType) -> FieldDesc {
    let type_char = match ft {
        FieldType::D => 'd',
        FieldType::U => 'u',
        FieldType::N => 'n',
        FieldType::F64 => 'f',
        FieldType::C => 'c',
        FieldType::L => 'l',
        FieldType::P => 'p',
        FieldType::V => 'v',
        FieldType::I => 'i',
        FieldType::F64x9 => 'f',
        FieldType::P2 => 'p',
        FieldType::P3 => 'p',
        FieldType::F2 => 'f',
        FieldType::F3 => 'f',
        FieldType::C2 => 'c',
        FieldType::FVlaIdx => 'f',
        FieldType::S => 's',
    };
    let array_count = match ft {
        FieldType::F64x9 => 9,
        FieldType::P2 | FieldType::F2 | FieldType::C2 => 2,
        FieldType::P3 | FieldType::F3 => 3,
        _ => 0,
    };
    FieldDesc {
        type_char,
        entity_type_id: 0,
        array_count,
        element_type: false,
    }
}

// ── Low-level readers ───────────────────────────────────────────────────────
// All decimal readers consume the trailing space (matching Parasolid behavior).

fn read_raw_byte(input: &mut &str) -> Result<char> {
    any::<&str, winnow::error::ContextError>
        .parse_next(input)
        .map_err(|_| XtError::UnexpectedEof)
}

/// Read an entity pointer, handling optional `?` prefix (optional/absent pointer).
fn read_ptr(input: &mut &str) -> Result<usize> {
    token::ws(input).map_err(|_| XtError::UnexpectedEof)?;
    // Skip optional '?' prefix
    if input.starts_with('?') {
        *input = &input[1..];
    }
    let v = ascii::dec_int::<&str, i64, winnow::error::ContextError>
        .parse_next(input)
        .map_err(|_| XtError::UnexpectedEof)?;
    consume_space(input);
    Ok(v.max(0) as usize)
}

fn read_uint16(input: &mut &str) -> Result<u16> {
    token::ws(input).map_err(|_| XtError::UnexpectedEof)?;
    let v = ascii::dec_uint::<&str, u16, winnow::error::ContextError>
        .parse_next(input)
        .map_err(|_| XtError::UnexpectedEof)?;
    consume_space(input);
    Ok(v)
}

fn read_int32(input: &mut &str) -> Result<i64> {
    token::ws(input).map_err(|_| XtError::UnexpectedEof)?;
    // `?` = unset sentinel for integers (-32764 in Parasolid).
    // Reference section 3.2: "represented in a text transmit file as
    // the question mark '?'".
    if input.starts_with('?') {
        *input = &input[1..];
        consume_space(input);
        return Ok(-32764);
    }
    let v = ascii::dec_int::<&str, i64, winnow::error::ContextError>
        .parse_next(input)
        .map_err(|_| XtError::UnexpectedEof)?;
    consume_space(input);
    Ok(v)
}

fn read_uint8(input: &mut &str) -> Result<u8> {
    token::ws(input).map_err(|_| XtError::UnexpectedEof)?;
    let v = ascii::dec_uint::<&str, u8, winnow::error::ContextError>
        .parse_next(input)
        .map_err(|_| XtError::UnexpectedEof)?;
    consume_space(input);
    Ok(v)
}

fn read_int16(input: &mut &str) -> Result<i16> {
    token::ws(input).map_err(|_| XtError::UnexpectedEof)?;
    if input.starts_with('?') {
        *input = &input[1..];
        consume_space(input);
        return Ok(-32764);
    }
    let v = ascii::dec_int::<&str, i16, winnow::error::ContextError>
        .parse_next(input)
        .map_err(|_| XtError::UnexpectedEof)?;
    consume_space(input);
    Ok(v)
}

fn read_f64(input: &mut &str) -> Result<f64> {
    token::ws(input).map_err(|_| XtError::UnexpectedEof)?;
    if input.starts_with('?') {
        // `?` alone = NaN sentinel for optional/absent float.
        // The `?` consumes only itself; any following digits belong to the
        // NEXT field value (e.g. `?20` = NaN for this field, then `20` is
        // the next pointer/int field).
        *input = &input[1..];
        consume_space(input);
        return Ok(f64::NAN);
    }
    let v = token::xt_float(input).map_err(|_| XtError::UnexpectedEof)?;
    consume_space(input);
    Ok(v)
}

fn read_bool_tf(input: &mut &str) -> Result<bool> {
    token::ws(input).map_err(|_| XtError::UnexpectedEof)?;
    let ch = read_raw_byte(input)?;
    match ch {
        'T' | '1' => Ok(true),
        'F' | '0' => Ok(false),
        other => Err(XtError::Parse {
            offset: 0,
            detail: format!("expected T/F/1/0 boolean, got {:?}", other),
        }),
    }
}

fn read_type_name(input: &mut &str) -> Result<String> {
    let len = read_uint8(input)? as usize;
    let mut name = String::with_capacity(len);
    for _ in 0..len {
        name.push(read_raw_byte(input)?);
    }
    Ok(name)
}

fn consume_space(input: &mut &str) {
    if input.starts_with(' ') {
        *input = &input[1..];
    }
}
