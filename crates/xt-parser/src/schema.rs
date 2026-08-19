//! Entity schemas and compact-format schema preamble parser.
//!
//! The compact transmit format embeds schema diffs in the file preamble.
//! Each entity type has a base schema (sch_13006) that is modified by annotation
//! characters: C (copy), I (insert), D (delete), A (append).

use winnow::prelude::*;
use winnow::token::take_while;

use crate::token;

// ── Entity type IDs ──────────────────────────────────────────────────────────

pub const PARTITION: u16 = 3;
pub const PMARK: u16 = 4;
pub const MARK: u16 = 9;
pub const ASSEMBLY: u16 = 10;
pub const INSTANCE: u16 = 11;
pub const BODY: u16 = 12;
pub const SHELL: u16 = 13;
pub const FACE: u16 = 14;
pub const LOOP: u16 = 15;
pub const EDGE: u16 = 16;
pub const FIN: u16 = 17;
pub const VERTEX: u16 = 18;
pub const REGION: u16 = 19;
pub const POINT: u16 = 29;
pub const LINE: u16 = 30;
pub const CIRCLE: u16 = 31;
pub const ELLIPSE: u16 = 32;
pub const PARABOLA: u16 = 33;
pub const HYPERBOLA: u16 = 34;
pub const PARACURVE: u16 = 35;
pub const SILHOUETTE: u16 = 39;
pub const CHART: u16 = 40;
pub const LIMIT: u16 = 41;
pub const BSPLINE_CURVE_OLD: u16 = 43;
pub const KNOT_VECTOR: u16 = 44;
pub const BSPLINE_VERTICES: u16 = 45;
pub const OFFSET_CURVE: u16 = 46;
pub const OBSOLETE_CPC: u16 = 36;
pub const CPC: u16 = 48;
pub const PLANE: u16 = 50;
pub const CYLINDER: u16 = 51;
pub const CONE: u16 = 52;
pub const SPHERE: u16 = 53;
pub const TORUS: u16 = 54;
pub const PIPE: u16 = 55;
pub const BLENDED_EDGE: u16 = 56;
pub const BLENDED_VERTEX: u16 = 57;
pub const BLEND_OVERLAP: u16 = 58;
pub const BLEND_BOUND: u16 = 59;
pub const OFFSET_SURF: u16 = 60;
pub const SILH_SURF: u16 = 63;
pub const BASIC_PATCH: u16 = 64;
pub const HULL: u16 = 65;
pub const SWEPT_SURF: u16 = 67;
pub const SPUN_SURF: u16 = 68;
pub const LIST: u16 = 70;
pub const REAL_LIS_BLOCK: u16 = 71;
pub const INTEGER_LIS_BLOCK: u16 = 72;
pub const TAG_LIS_BLOCK: u16 = 73;
pub const POINTER_LIS_BLOCK: u16 = 74;
pub const ATT_DEF_ID: u16 = 79;
pub const ATTRIB_DEF: u16 = 80;
pub const ATTRIBUTE: u16 = 81;
pub const INT_VALUES: u16 = 82;
pub const REAL_VALUES: u16 = 83;
pub const CHAR_VALUES: u16 = 84;
pub const POINT_VALUES: u16 = 85;
pub const VECTOR_VALUES: u16 = 86;
pub const AXIS_VALUES: u16 = 87;
pub const TAG_VALUES: u16 = 88;
pub const DIRECTION_VALUES: u16 = 89;
pub const FEATURE: u16 = 90;
pub const MEMBER_OF_FEATURE: u16 = 91;
pub const TRANSFORM: u16 = 100;
pub const WORLD: u16 = 101;
pub const KEY_NODE: u16 = 102;
pub const PE_SURF: u16 = 120;
pub const INT_PE_DATA: u16 = 121;
pub const EXT_PE_DATA: u16 = 122;
pub const B_SURFACE: u16 = 124;
pub const SURFACE_DATA: u16 = 125;
pub const NURBS_SURF: u16 = 126;
pub const KNOT_MULT: u16 = 127;
pub const KNOT_SET: u16 = 128;
pub const KNOT_MULT_SUM: u16 = 129;
pub const PE_CURVE: u16 = 130;
pub const PCURVE: u16 = 132;
pub const TRIMMED_CURVE: u16 = 133;
pub const B_CURVE: u16 = 134;
pub const CURVE_DATA: u16 = 135;
pub const NURBS_CURVE: u16 = 136;
pub const SP_CURVE: u16 = 137;
pub const INTERSECTION: u16 = 38;
pub const GEOMETRIC_OWNER: u16 = 141;
pub const SESSION: u16 = 172;
pub const PLANE_EXT: u16 = 405;
pub const GEOMETRIC_OWNER_EXT2: u16 = 403;
pub const GEOMETRIC_OWNER_EXT3: u16 = 413;
pub const PLANE_EXT2: u16 = 415;
pub const PS30_EXT_255: u16 = 255;
pub const PS30_EXT_287: u16 = 287;
pub const PS30_EXT_292: u16 = 292;
pub const PS30_EXT_298: u16 = 298;
pub const PS30_EXT_317: u16 = 317;
pub const PS30_EXT_396: u16 = 396;
pub const PS30_EXT_446: u16 = 446;
pub const PS30_EXT_458: u16 = 458;
pub const PS30_EXT_486: u16 = 486;
pub const PS30_EXT_487: u16 = 487;
pub const PS30_EXT_488: u16 = 488;
pub const PS30_EXT_556: u16 = 556;
pub const PS30_EXT_596: u16 = 596;
pub const PS30_EXT_598: u16 = 598;
pub const PS30_EXT_457: u16 = 457;
pub const PS30_EXT_649: u16 = 649;
pub const PS30_EXT_682: u16 = 682;
pub const PS30_EXT_591: u16 = 591;
pub const PS30_EXT_602: u16 = 602;
pub const PS30_EXT_604: u16 = 604;
pub const PS30_EXT_606: u16 = 606;
pub const PS30_EXT_607: u16 = 607;
pub const PS30_EXT_687: u16 = 687;
pub const PS30_EXT_692: u16 = 692;
pub const PS30_EXT_697: u16 = 697;
pub const PS30_EXT_718: u16 = 718;
pub const PS30_EXT_719: u16 = 719;
pub const PS30_EXT_771: u16 = 771;
pub const PS30_EXT_783: u16 = 783;
pub const PS30_EXT_772: u16 = 772;
pub const PS30_EXT_786: u16 = 786;
pub const PS30_EXT_798: u16 = 798;
pub const PS30_EXT_800: u16 = 800;
pub const PS30_EXT_801: u16 = 801;
pub const PS30_EXT_832: u16 = 832;
pub const PS30_EXT_840: u16 = 840;
pub const PS30_EXT_842: u16 = 842;
pub const PS30_EXT_845: u16 = 845;
pub const PS30_EXT_589: u16 = 589;
pub const PS30_EXT_848: u16 = 848;
pub const PS30_EXT_853: u16 = 853;
pub const PS30_EXT_857: u16 = 857;
pub const PS30_EXT_654: u16 = 654;
pub const PS30_EXT_876: u16 = 876;
pub const PS30_EXT_903: u16 = 903;
pub const PS30_EXT_918: u16 = 918;
pub const PS30_EXT_922: u16 = 922;
pub const PS30_EXT_928: u16 = 928;
pub const PS30_EXT_790: u16 = 790;
pub const PS30_EXT_793: u16 = 793;
pub const PS30_EXT_934: u16 = 934;
pub const PS30_EXT_936: u16 = 936;
pub const PS30_EXT_939: u16 = 939;
pub const PS30_EXT_803: u16 = 803;
pub const PS30_EXT_962: u16 = 962;
pub const PS30_EXT_964: u16 = 964;
pub const PS30_EXT_966: u16 = 966;
pub const PS30_EXT_968: u16 = 968;
pub const PS30_EXT_971: u16 = 971;
pub const PS30_EXT_976: u16 = 976;
pub const PS30_EXT_984: u16 = 984;
pub const PS30_EXT_989: u16 = 989;
pub const PS30_EXT_291: u16 = 291;
pub const PS30_EXT_975: u16 = 975;
pub const PS30_EXT_986: u16 = 986;
pub const PS30_EXT_990: u16 = 990;
pub const PS30_EXT_1046: u16 = 1046;
pub const PS30_EXT_1049: u16 = 1049;
pub const PS30_EXT_103: u16 = 103;
pub const PS30_EXT_716: u16 = 716;
pub const PS30_EXT_1081: u16 = 1081;
pub const PS30_EXT_1085: u16 = 1085;
pub const PS30_EXT_1089: u16 = 1089;
pub const PS30_EXT_1487: u16 = 1487;
pub const PS30_EXT_1490: u16 = 1490;
pub const PS30_EXT_8001: u16 = 8001;
pub const PS30_EXT_9000: u16 = 9000;
pub const PS30_EXT_8038: u16 = 8038;
pub const PS30_EXT_8017: u16 = 8017;
pub const PS30_EXT_8040: u16 = 8040;
pub const PS30_LEGACY_6: u16 = 6;
pub const PS30_LEGACY_5: u16 = 5;
pub const PS30_LEGACY_2: u16 = 2;
pub const INTERSECTION_DATA: u16 = 204;
pub const PS30_EXT_308: u16 = 308;
pub const PS30_EXT_315: u16 = 315;
pub const PS30_EXT_599: u16 = 599;
pub const PS30_EXT_1020: u16 = 1020;
pub const PS30_EXT_98: u16 = 98;

// ── Field types ──────────────────────────────────────────────────────────────

/// A single field's type in an entity schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    /// Signed integer (node_id, state, etc.)
    D,
    /// Unsigned byte
    U,
    /// Short (16-bit signed)
    N,
    /// 64-bit float
    F64,
    /// Single character (sense, region_type)
    C,
    /// Boolean / logical
    L,
    /// Entity pointer
    P,
    /// 3-component vector (3 × f64)
    V,
    /// 2-component interval (2 × f64)
    I,
    /// 3×3 matrix (9 × f64)
    F64x9,
    /// 2-element pointer field (e.g. INTERSECTION.surface, BLENDED_EDGE.surface)
    P2,
    /// 3-element pointer field (e.g. BLENDED_VERTEX.surface)
    P3,
    /// 2-element float field (e.g. BLENDED_EDGE.range, CHART.parameter_error)
    F2,
    /// 3-element float field (e.g. BLENDED_VERTEX.range)
    F3,
    /// 2-element char field (e.g. BLEND_OVERLAP.blend_type)
    C2,
    /// Variable-length float array whose element count equals the entity index.
    FVlaIdx,
    /// Whitespace-delimited opaque token.
    S,
}

/// Schema for a single entity type: ordered field types.
/// For variable-length entities, fixed fields come first;
/// variable data is indicated by `is_variable`.
#[derive(Debug, Clone)]
pub struct EntitySchema {
    pub type_id: u16,
    pub fields: Vec<FieldType>,
    /// If true, variable-length data follows the fixed fields.
    /// For classic variable entities (BSPLINE_VERTICES, etc.), `fields` is empty
    /// and the count token precedes the array elements.
    /// For embedded-count entities (CHART), `fields` is non-empty and
    /// `var_count_field_idx` identifies which fixed field holds the element count.
    pub is_variable: bool,
    /// Type of variable-length array elements (if is_variable).
    pub var_type: Option<VarType>,
    /// If Some(i), the count for the variable array comes from fixed fields[i]
    /// rather than as a leading token in the stream (used by CHART).
    pub var_count_field_idx: Option<usize>,
    /// If true, the variable array count equals the entity's logical index
    /// (the integer read immediately after type_id). Used by types such as
    /// LIMIT (type 41) where the entity index encodes the number of points.
    pub entity_index_is_var_count: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarType {
    F64,
    I16,
    /// Signed 32-bit integer. Used by INT_VALUES (type 82) whose schema
    /// field type is `d` (int32).
    I32,
    Ptr,
    Char,
    /// Raw byte char: reads one byte without skipping leading whitespace.
    /// Used for packed string data where the whitespace IS data (e.g. attribute
    /// name "Headphone hinge" stored as 15 consecutive raw bytes).
    RawChar,
    /// Three f64 per element (3D vector). Used by CHART where the count is
    /// stored in a fixed field (var_count_field_idx) rather than as a leading token.
    V3,
}

// ── Base schemas (sch_13006) ─────────────────────────────────────────────────

use FieldType::*;

/// Return the base schema for a given entity type.
///
/// All XT files use sch_13006 as the base for inline schema diffs
/// (reference section 2.1.2: "differences between its schema and a
/// defined base schema (currently V13's SCH_13006)"). Only fields with
/// transmit_flag=1 are included. Fields with extra2>1 are expanded
/// inline (e.g. `surface; p; 1 0 2` → two P fields).
/// Returns `None` for unknown types.
pub fn base_schema(type_id: u16) -> Option<EntitySchema> {
    let (fields, is_variable, var_type) = match type_id {
        // ── Type 1: NULLP ─────────────────────────────────────────
        1 => (vec![], false, None),

        // ── Type 2: WORKSPACE ─────────────────────────────────────
        // ws; c; 1 0 1  (variable char array)
        2 => (vec![], true, Some(VarType::Char)),

        // ── Type 3: PARTITION ─────────────────────────────────────
        // Transmitted: current_pmark(p), highest_id(d)
        PARTITION => (vec![P, D], false, None),

        // ── Type 4: PMARK ─────────────────────────────────────────
        // Transmitted: preceding(p), first_following(p), next_sibling(p),
        //   prev_sibling(p), n_new_nodes(d), n_del_nodes(d),
        //   n_copy_mod_nodes(d), delta_key(d), delta_is_forward(l), id(d)
        PMARK => (vec![P, P, P, P, D, D, D, D, L, D], false, None),

        // ── Type 10: ASSEMBLY ─────────────────────────────────────
        // Transmitted: highest_node_id(d), attributes_features(p),
        //   attribute_chains(p), list(p), surface(p), curve(p), point(p),
        //   key(p), res_size(f), res_linear(f), ref_instance(p), next(p),
        //   previous(p), state(u), owner(p), type(u), sub_instance(p)
        ASSEMBLY => (
            vec![D, P, P, P, P, P, P, P, F64, F64, P, P, P, U, P, U, P],
            false,
            None,
        ),

        // ── Type 11: INSTANCE ─────────────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), type(u), part(p),
        //   transform(p), assembly(p), next_in_part(p), prev_in_part(p),
        //   next_of_part(p), prev_of_part(p)
        INSTANCE => (
            vec![D, P, U, P, P, P, P, P, P, P],
            false,
            None,
        ),

        // ── Type 12: BODY ─────────────────────────────────────────
        // Transmitted: highest_node_id(d), attributes_features(p),
        //   attribute_chains(p), surface(p), curve(p), point(p), key(p),
        //   res_size(f), res_linear(f), ref_instance(p), next(p), previous(p),
        //   state(u), owner(p), body_type(u), nom_geom_state(u), shell(p),
        //   boundary_surface(p), boundary_curve(p), boundary_point(p),
        //   region(p), edge(p), vertex(p)
        BODY => (
            vec![
                D, P, P, P, P, P, P, F64, F64, P, P, P, U, P, U, U, P, P, P, P, P, P, P,
            ],
            false,
            None,
        ),

        // ── Type 13: SHELL ────────────────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), body(p), next(p),
        //   face(p), edge(p), vertex(p), region(p), front_face(p)
        SHELL => (vec![D, P, P, P, P, P, P, P, P], false, None),

        // ── Type 14: FACE ─────────────────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), tolerance(f),
        //   next(p), previous(p), loop(p), shell(p), surface(p), sense(c),
        //   next_on_surface(p), previous_on_surface(p), next_front(p),
        //   previous_front(p), front_shell(p)
        FACE => (
            vec![D, P, F64, P, P, P, P, P, C, P, P, P, P, P],
            false,
            None,
        ),

        // ── Type 15: LOOP ─────────────────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), halfedge(p),
        //   face(p), next(p)
        LOOP => (vec![D, P, P, P, P], false, None),

        // ── Type 16: EDGE ─────────────────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), tolerance(f),
        //   halfedge(p), previous(p), next(p), curve(p), next_on_curve(p),
        //   previous_on_curve(p), owner(p)
        EDGE => (vec![D, P, F64, P, P, P, P, P, P, P], false, None),

        // ── Type 17: HALFEDGE (FIN) ───────────────────────────────
        // node_id transmit=0; transmitted: attributes_features(p), loop(p),
        //   forward(p), backward(p), vertex(p), other(p), edge(p), curve(p),
        //   next_at_vx(p), sense(c)
        FIN => (vec![P, P, P, P, P, P, P, P, P, C], false, None),

        // ── Type 18: VERTEX ───────────────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), halfedge(p),
        //   previous(p), next(p), point(p), tolerance(f), owner(p)
        VERTEX => (vec![D, P, P, P, P, P, F64, P], false, None),

        // ── Type 19: REGION ───────────────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), body(p), next(p),
        //   previous(p), shell(p), type(c)
        REGION => (vec![D, P, P, P, P, P, C], false, None),

        // ── Type 29: POINT ────────────────────────────────────────
        // All 6 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), pvec(v)
        POINT => (vec![D, P, P, P, P, V], false, None),

        // ── Type 30: LINE ─────────────────────────────────────────
        // All 9 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), pvec(v),
        //   direction(v)
        LINE => (vec![D, P, P, P, P, P, C, V, V], false, None),

        // ── Type 31: CIRCLE ───────────────────────────────────────
        // All 11 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), centre(v),
        //   normal(v), x_axis(v), radius(f)
        CIRCLE => (vec![D, P, P, P, P, P, C, V, V, V, F64], false, None),

        // ── Type 32: ELLIPSE ──────────────────────────────────────
        // All 12 transmitted (sch_13006.s_t): node_id(d), attributes_features(p),
        //   owner(p), next(p), previous(p), geometric_owner(p), sense(c),
        //   centre(v), normal(v), x_axis(v), major_radius(f), minor_radius(f)
        // NOTE: sch_13006.s_t has sense BEFORE centre (same as other curves).
        // The reference doc C struct (5.2.1.3) shows a different order but the
        // schema file is authoritative for the transmit format.
        ELLIPSE => (vec![D, P, P, P, P, P, C, V, V, V, F64, F64], false, None),

        // ── Type 33: PARABOLA ─────────────────────────────────────
        // All 11 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), origin(v),
        //   normal(v), x_axis(v), focal_length(f)
        PARABOLA => (vec![D, P, P, P, P, P, C, V, V, V, F64], false, None),

        // ── Type 34: HYPERBOLA ────────────────────────────────────
        // All 12 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), origin(v),
        //   normal(v), x_axis(v), transverse_radius(f), conjugate_radius(f)
        HYPERBOLA => (vec![D, P, P, P, P, P, C, V, V, V, F64, F64], false, None),

        // ── Type 35: PARACURVE ────────────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), owner(p), next(p),
        //   previous(p), geometric_owner(p), sense(c), seg(d), cpc(p)
        PARACURVE => (vec![D, P, P, P, P, P, C, D, P], false, None),

        // ── Type 36: OBSOLETE_CPC ─────────────────────────────────
        // Transmitted fixed: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c),
        //   vertex_dimension(n), segment_count(n), order(n)
        // Variable: segment(p, extra2=1)
        OBSOLETE_CPC => (
            vec![D, P, P, P, P, P, C, N, N, N],
            true,
            Some(VarType::Ptr),
        ),

        // ── Type 37: PATCH_BOUND ──────────────────────────────────
        // All 10 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), boundary(c),
        //   lh_patch(p), rh_patch(p)
        37 => (vec![D, P, P, P, P, P, C, C, P, P], false, None),

        // ── Type 38: INTERSECTION ─────────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), owner(p), next(p),
        //   previous(p), geometric_owner(p), sense(c), surface(p, extra2=2→P2),
        //   chart(p), start(p), end(p)
        // NOTE: P2 reads both surface pointers from stream but only stores
        // the first in entity.fields[7]. surf2 is discarded. chart lands at [8].
        // scale; f; transmit=0 → omitted
        INTERSECTION => (
            vec![D, P, P, P, P, P, C, P2, P, P, P],
            false,
            None,
        ),

        // ── Type 39: SILHOUETTE ───────────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), owner(p), next(p),
        //   previous(p), geometric_owner(p), sense(c), analytic_root(c),
        //   from_infinity(l), surface(p), start(p), end(p), eye(v)
        SILHOUETTE => (
            vec![D, P, P, P, P, P, C, C, L, P, P, P, V],
            false,
            None,
        ),

        // ── Type 40: CHART ────────────────────────────────────────
        // Transmitted fixed: base_parameter(f), base_scale(f), chart_count(d),
        //   chordal_error(f), angular_error(f), parameter_error(f, extra2=2→2f)
        // Variable: hvec(h, extra2=1) — hvec treated as V3 (see entity.rs 'h' reader)
        // parameter_error has count=2 → F2 (2 floats). hvec(h) is the variable tail.
        CHART => (
            vec![F64, F64, D, F64, F64, F2, V],
            true,
            Some(VarType::V3),
        ),

        // ── Type 41: LIMIT ────────────────────────────────────────
        // Transmitted: type(c), hvec(h, extra2=1 variable)
        // hvec treated as V3 (see entity.rs 'h' reader).
        // entity_index_is_var_count=true: count comes from entity index.
        // LIMIT: type(c) + hvec(h, variable). has_version=true reads version
        // (always 1) before entity_idx. var_count = version-1 = 0 extra elements
        // (the single h-type is in fixed V).
        LIMIT => (vec![C, V], true, Some(VarType::V3)),

        // ── Type 43: BSPLINE_CURVE ────────────────────────────────
        // Transmitted: knot_vector(p), vertex_dimension(n), vertex_count(d),
        //   order(n), bspline_vertices(p)
        BSPLINE_CURVE_OLD => (vec![P, N, D, N, P], false, None),

        // ── Type 44: KNOT_VECTOR ──────────────────────────────────
        // Transmitted fixed: periodic(l), knot_count(d)
        // Variable: knots(f, extra2=1)
        KNOT_VECTOR => (vec![L, D], true, Some(VarType::F64)),

        // ── Type 45: BSPLINE_VERTICES ────────────────────────────
        // Single variable field: vertices(f, extra2=1)
        BSPLINE_VERTICES => (vec![], true, Some(VarType::F64)),

        // ── Type 46: OFFSET_CURVE ────────────────────────────────
        // All 10 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), surface(p),
        //   curve(p), offset(f)
        OFFSET_CURVE => (vec![D, P, P, P, P, P, C, P, P, F64], false, None),

        // ── Type 48: CPC ─────────────────────────────────────────
        // All 9 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), bezier(p),
        //   bspline(p)
        CPC => (vec![D, P, P, P, P, P, C, P, P], false, None),

        // ── Type 49: OBSOLETE_SP_CURVE ───────────────────────────
        // Transmitted fixed: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c),
        //   const_param(l), segment_count(n), order(n), surface(p)
        // Variable: bezier_vertices(f, extra2=1)
        49 => (
            vec![D, P, P, P, P, P, C, L, N, N, P],
            true,
            Some(VarType::F64),
        ),

        // ── Type 50: PLANE ───────────────────────────────────────
        // All 10 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), pvec(v),
        //   normal(v), x_axis(v)
        PLANE => (vec![D, P, P, P, P, P, C, V, V, V], false, None),

        // ── Type 51: CYLINDER ────────────────────────────────────
        // All 11 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), pvec(v),
        //   axis(v), radius(f), x_axis(v)
        CYLINDER => (vec![D, P, P, P, P, P, C, V, V, F64, V], false, None),

        // ── Type 52: CONE ────────────────────────────────────────
        // All 13 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), pvec(v),
        //   axis(v), radius(f), sin_half_angle(f), cos_half_angle(f), x_axis(v)
        CONE => (
            vec![D, P, P, P, P, P, C, V, V, F64, F64, F64, V],
            false,
            None,
        ),

        // ── Type 53: SPHERE ──────────────────────────────────────
        // All 11 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), centre(v),
        //   radius(f), axis(v), x_axis(v)
        SPHERE => (vec![D, P, P, P, P, P, C, V, F64, V, V], false, None),

        // ── Type 54: TORUS ───────────────────────────────────────
        // All 12 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), centre(v),
        //   axis(v), major_radius(f), minor_radius(f), x_axis(v)
        TORUS => (
            vec![D, P, P, P, P, P, C, V, V, F64, F64, V],
            false,
            None,
        ),

        // ── Type 55: PIPE ────────────────────────────────────────
        // All 9 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), spine(p),
        //   radius(f)
        PIPE => (vec![D, P, P, P, P, P, C, P, F64], false, None),

        // ── Type 56: BLENDED_EDGE ────────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), owner(p), next(p),
        //   previous(p), geometric_owner(p), sense(c), blend_type(c),
        //   surface(p, extra2=2→P,P), spine(p), range(f, extra2=2→F64,F64),
        //   thumb_weight(f, extra2=2→F64,F64), boundary(p, extra2=2→P,P),
        //   start(p), end(p)
        BLENDED_EDGE => (
            vec![
                D, P, P, P, P, P, C, C,
                P, P,       // surface×2
                P,          // spine
                F64, F64,   // range×2
                F64, F64,   // thumb_weight×2
                P, P,       // boundary×2
                P,          // start
                P,          // end
            ],
            false,
            None,
        ),

        // ── Type 57: BLENDED_VERTEX ──────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), owner(p), next(p),
        //   previous(p), geometric_owner(p), sense(c), blend_type(c),
        //   surface(p, extra2=3→P,P,P), sub_surface(p, extra2=3→P,P,P),
        //   boundary(p, extra2=3→P,P,P), range(f, extra2=3→F64,F64,F64),
        //   thumb_weight(f, extra2=3→F64,F64,F64), centre(v)
        BLENDED_VERTEX => (
            vec![
                D, P, P, P, P, P, C, C,
                P, P, P,          // surface×3
                P, P, P,          // sub_surface×3
                P, P, P,          // boundary×3
                F64, F64, F64,    // range×3
                F64, F64, F64,    // thumb_weight×3
                V,                // centre
            ],
            false,
            None,
        ),

        // ── Type 58: BLEND_OVERLAP ───────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), owner(p), next(p),
        //   previous(p), geometric_owner(p), sense(c),
        //   surface(p, extra2=2→P,P), sub_surface(p, extra2=4→P,P,P,P),
        //   range(f, extra2=4→F64,F64,F64,F64),
        //   thumb_weight(f, extra2=4→F64,F64,F64,F64),
        //   blend_type(c, extra2=2→C,C), overlap_type(c), swap_u_v(l)
        BLEND_OVERLAP => (
            vec![
                D, P, P, P, P, P, C,
                P, P,                   // surface×2
                P, P, P, P,             // sub_surface×4
                F64, F64, F64, F64,     // range×4
                F64, F64, F64, F64,     // thumb_weight×4
                C, C,                   // blend_type×2
                C,                      // overlap_type
                L,                      // swap_u_v
            ],
            false,
            None,
        ),

        // ── Type 59: BLEND_BOUND ─────────────────────────────────
        // All 9 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), boundary(n),
        //   blend(p)
        BLEND_BOUND => (vec![D, P, P, P, P, P, C, N, P], false, None),

        // ── Type 60: OFFSET_SURF ─────────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), owner(p), next(p),
        //   previous(p), geometric_owner(p), sense(c), check(c),
        //   true_offset(l), surface(p), offset(f), scale(f)
        OFFSET_SURF => (
            vec![D, P, P, P, P, P, C, C, L, P, F64, F64],
            false,
            None,
        ),

        // ── Type 61: PARASURF ────────────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), owner(p), next(p),
        //   previous(p), geometric_owner(p), sense(c), col(d), row(d), cps(p)
        61 => (vec![D, P, P, P, P, P, C, D, D, P], false, None),

        // ── Type 62: OBSOLETE_CPS ────────────────────────────────
        // Transmitted fixed: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c),
        //   vertex_dimension(n), col_count(n), row_count(n), u_order(n),
        //   v_order(n)
        // Variable: patch(p, extra2=1)
        62 => (
            vec![D, P, P, P, P, P, C, N, N, N, N, N],
            true,
            Some(VarType::Ptr),
        ),

        // ── Type 63: SILH_SURF ───────────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), owner(p), next(p),
        //   previous(p), geometric_owner(p), sense(c), from_infinity(l),
        //   surface(p), eye(v)
        SILH_SURF => (vec![D, P, P, P, P, P, C, L, P, V], false, None),

        // ── Type 64: BASIC_PATCH ─────────────────────────────────
        // entity transmit=0; but fields that are transmitted:
        //   u_length(f), v_length(f), bezier_vertices(f, variable)
        // Since entity-level transmit=0, this type is not written standalone;
        // included only for completeness in sub-entity references.
        BASIC_PATCH => (vec![F64, F64], true, Some(VarType::F64)),

        // ── Type 65: HULL ────────────────────────────────────────
        // entity transmit=0; transmitted: dimension(n), plane_count(n),
        //   corner_count(n), vecs(v, variable)
        HULL => (vec![N, N, N], true, Some(VarType::V3)),

        // ── Type 66: BSPLINE_SURF ────────────────────────────────
        // All 8 transmitted: row_knots(p), col_knots(p),
        //   vertex_dimension(n), col_count(d), row_count(d), u_order(n),
        //   v_order(n), bspline_vertices(p)
        66 => (vec![P, P, N, D, D, N, N, P], false, None),

        // ── Type 67: SWEPT_SURF ──────────────────────────────────
        // All 10 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), section(p),
        //   sweep(v), scale(f)
        SWEPT_SURF => (vec![D, P, P, P, P, P, C, P, V, F64], false, None),

        // ── Type 68: SPUN_SURF ───────────────────────────────────
        // All 16 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), profile(p),
        //   base(v), axis(v), start(v), end(v), start_param(f), end_param(f),
        //   x_axis(v), scale(f)
        SPUN_SURF => (
            vec![D, P, P, P, P, P, C, P, V, V, V, V, F64, F64, V, F64],
            false,
            None,
        ),

        // ── Type 69: CPS ─────────────────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), owner(p), next(p),
        //   previous(p), geometric_owner(p), sense(c), bezier(p), bspline(p)
        69 => (vec![D, P, P, P, P, P, C, P, P], false, None),

        // ── Type 70: LIST ────────────────────────────────────────
        // Transmitted: node_id(d), owner(p), next(p), previous(p),
        //   list_type(d), list_length(d), block_length(d), size_of_entry(d),
        //   list_block(p), finger_block(p), finger_index(d), notransmit(l)
        LIST => (
            vec![D, P, P, P, D, D, D, D, P, P, D, L],
            false,
            None,
        ),

        // ── Type 71: REAL_LIS_BLOCK ──────────────────────────────
        // Transmitted fixed: n_entries(d), next_block(p)
        // Variable: entries(f, extra2=1)
        REAL_LIS_BLOCK => (vec![D, P], true, Some(VarType::F64)),

        // ── Type 72: INTEGER_LIS_BLOCK ───────────────────────────
        // Transmitted fixed: n_entries(d), next_block(p)
        // Variable: entries(d, extra2=1)
        INTEGER_LIS_BLOCK => (vec![D, P], true, Some(VarType::I16)),

        // ── Type 73: TAG_LIS_BLOCK ───────────────────────────────
        // Transmitted fixed: n_entries(d), next_block(p)
        // Variable: entries(t, extra2=1) — tag treated as pointer
        TAG_LIS_BLOCK => (vec![D, P], true, Some(VarType::Ptr)),

        // ── Type 74: POINTER_LIS_BLOCK ───────────────────────────
        // Transmitted fixed: n_entries(d), next_block(p)
        // Variable: entries(p, extra2=1)
        POINTER_LIS_BLOCK => (vec![D, P], true, Some(VarType::Ptr)),

        // ── Type 79: ATT_DEF_ID ──────────────────────────────────
        // Variable: string(c, extra2=1) — entity_idx raw bytes
        ATT_DEF_ID => (vec![], true, Some(VarType::RawChar)),

        // ── Type 80: ATTRIB_DEF ──────────────────────────────────
        // Transmitted fixed: next(p), identifier(p), type_id(d),
        //   actions(u, extra2=8 → 8 U bytes expanded inline), field_names(p),
        //   legal_owners(l, extra2=14 → 14 L booleans expanded inline)
        // Variable: fields(u, extra2=1)
                // next(p), identifier(p), type_id(d), actions(u×8),
        // callbacks(p) — transmitted in PS30 despite sch_30100 transmit=0,
        // field_names(p), legal_owners(l×14), fields(u×1)
        // NOT variable-length: fields has extra2=1 = fixed 1-element array.
        // sch_13006: 8 fields, callbacks(p) has transmit=0 → skip.
        // Last field 'fields' has extra2=1 → variable-length uint array.
        // Transmitted fixed: next(p), identifier(p), type_id(d), actions(u×8),
        //   field_names(p), legal_owners(l×14)
        // Variable tail: fields(u) with count from VERSION int.
        ATTRIB_DEF => (
            vec![
                P, P, D,
                U, U, U, U, U, U, U, U,  // actions×8
                P,                         // field_names
                L, L, L, L, L, L, L, L, L, L, L, L, L, L,  // legal_owners×14
            ],
            true,
            Some(VarType::I16),  // fields: uint values stored as i16
        ),

        // Type 78 (ATTRIB_CALLBACKS) omitted: transmit=0 in sch_13006,
        // so it uses Path B (full inline schema) when encountered.

        // ── Type 81: ATTRIBUTE ───────────────────────────────────
        // Transmitted fixed: node_id(d), definition(p), owner(p), next(p),
        //   previous(p), next_of_type(p), previous_of_type(p)
        // Variable: fields(p, extra2=1)
        // 8 fields: node_id(d), definition(p), owner(p), next(p),
        //   previous(p), next_of_type(p), previous_of_type(p), fields(p)
        // sch_13006: 8 fields, last field 'fields' has extra2=1 (variable-length).
        // The VERSION int before entity_index IS the variable array count.
        // 7 fixed fields (D + 6P), then var_count × pointer values.
        // PS30 annotation may add fields but n_new_fields=255 means base schema.
        ATTRIBUTE => (
            vec![D, P, P, P, P, P, P],
            true,
            Some(VarType::Ptr),
        ),

        // ── Types 82-89: Attribute value entities ──────────────
        // Pure variable: VERSION int = array count, then entity_handle, then data.
        // No fixed fields — the version/count is handled by has_version logic.
        // Reference sections 5.4.7–5.4.15.
        INT_VALUES => (vec![], true, Some(VarType::I32)),         // 82: INT_VALUES (d→int32)
        REAL_VALUES => (vec![], true, Some(VarType::F64)),        // 83: REAL_VALUES (f)
        CHAR_VALUES => (vec![], true, Some(VarType::RawChar)),    // 84: CHAR_VALUES (c)
        POINT_VALUES | VECTOR_VALUES | AXIS_VALUES | DIRECTION_VALUES =>
            (vec![], true, Some(VarType::V3)),                    // 85-87,89: $v[] types
        TAG_VALUES => (vec![], true, Some(VarType::Ptr)),         // 88: TAG_VALUES (t→ptr)

        // ── Type 90: FEATURE ─────────────────────────────────────
        // All 7 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), type(u), first_member(p)
        FEATURE => (vec![D, P, P, P, P, U, P], false, None),

        // ── Type 91: MEMBER_OF_FEATURE ───────────────────────────
        // All 7 transmitted: dummy_node_id(d), owning_feature(p), owner(p),
        //   next(p), previous(p), next_member(p), previous_member(p)
        MEMBER_OF_FEATURE => (vec![D, P, P, P, P, P, P], false, None),

        // ── Type 96: SHORT_VALUES ────────────────────────────────
        // Variable: values(n, extra2=1)
        96 => (vec![], true, Some(VarType::I16)),

        // ── Type 97: BOX_VALUES ──────────────────────────────────
        // Variable: values(b, extra2=1) — box=6 doubles, treat as V
        // Note: b type = box (6 doubles); mapped to V for compatibility.
        97 => (vec![], true, Some(VarType::V3)),

        // ── Type 98: UNICODE_VALUES ──────────────────────────────
        // Variable: values(w, extra2=1) — unicode short
        98 => (vec![], true, Some(VarType::I16)),

        // ── Type 99: FIELD_NAMES ─────────────────────────────────
        // Variable: names(p, extra2=1)
        99 => (vec![], true, Some(VarType::Ptr)),

        // ── Type 100: TRANSFORM ──────────────────────────────────
        // Transmitted: node_id(d), owner(p), next(p), previous(p),
        //   rotation_matrix(f, extra2=9→F64x9), translation_vector(v),
        //   scale(f), flag(d), perspective_vector(v)
        TRANSFORM => (
            vec![D, P, P, P, F64x9, V, F64, D, V],
            false,
            None,
        ),

        // ── Type 101: WORLD ──────────────────────────────────────
        // Transmitted: assembly(p), attribute(p), body(p), transform(p),
        //   surface(p), curve(p), point(p), alive(l), attrib_def(p),
        //   highest_id(d), current_id(d)
        WORLD => (
            vec![P, P, P, P, P, P, P, L, P, D, D],
            false,
            None,
        ),

        // ── Type 102: KEY_NODE ───────────────────────────────────
        // Variable: string(c, extra2=1) — key string chars
        KEY_NODE => (vec![], true, Some(VarType::Char)),

        // ── Type 103: BEZIER_CURVE ───────────────────────────────
        // Transmitted fixed: vertex_dimension(n), segment_count(d), order(n),
        //   check(c)
        // Variable: segment(p, extra2=1)
        103 => (vec![N, D, N, C], true, Some(VarType::Ptr)),

        // ── Type 104: BEZIER_SURF ────────────────────────────────
        // Transmitted fixed: vertex_dimension(n), col_count(d), row_count(d),
        //   u_order(n), v_order(n), check(c)
        // Variable: patch(p, extra2=1)
        104 => (vec![N, D, D, N, N, C], true, Some(VarType::Ptr)),

        // ── Type 110: SET_ELEMENT_TAG ────────────────────────────
        // All 6 transmitted: next(p), forward(p), backward(p), class(d),
        //   set(p), node(p)
        110 => (vec![P, P, P, D, P, P], false, None),

        // ── Type 111: FACE_SET ───────────────────────────────────
        // All 5 transmitted: tag(p), next(p), class(d), he_set(p),
        //   surfaces(p)
        111 => (vec![P, P, D, P, P], false, None),

        // ── Type 112: HALFEDGE_SET ───────────────────────────────
        // All 6 transmitted: tag(p), next(p), previous(p), class(d),
        //   fa_set(p), co_he_set(p)
        112 => (vec![P, P, P, D, P, P], false, None),

        // ── Type 120: PE_SURF ────────────────────────────────────
        // Transmitted fixed: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), type(c),
        //   data(p), tf(p)
        // Variable: internal_geom(p, extra2=1)
        PE_SURF => (
            vec![D, P, P, P, P, P, C, C, P, P],
            true,
            Some(VarType::Ptr),
        ),

        // ── Type 121: INT_PE_DATA ────────────────────────────────
        // All 3 transmitted: geom_type(d), real_array(p), int_array(p)
        INT_PE_DATA => (vec![D, P, P], false, None),

        // ── Type 122: EXT_PE_DATA ────────────────────────────────
        // Transmitted fixed: key(p), real_array(p), int_array(p)
        // Variable: data(f, extra2=1) — transmit=0, so no variable part
        EXT_PE_DATA => (vec![P, P, P], false, None),

        // ── Type 124: B_SURFACE ──────────────────────────────────
        // All 9 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), nurbs(p),
        //   data(p)
        B_SURFACE => (vec![D, P, P, P, P, P, C, P, P], false, None),

        // ── Type 125: SURFACE_DATA ───────────────────────────────
        // Transmitted: original_uint(i), original_vint(i), extended_uint(i),
        //   extended_vint(i), self_int(u), original_u_start(c),
        //   original_u_end(c), original_v_start(c), original_v_end(c),
        //   extended_u_start(c), extended_u_end(c), extended_v_start(c),
        //   extended_v_end(c), analytic_form_type(c), swept_form_type(c),
        //   spun_form_type(c), blend_form_type(c), analytic_form(p),
        //   swept_form(p), spun_form(p), blend_form(p)
        SURFACE_DATA => (
            vec![
                I, I, I, I, U,
                C, C, C, C,   // original u/v start/end
                C, C, C, C,   // extended u/v start/end
                C, C, C, C,   // analytic/swept/spun/blend form types
                P, P, P, P,   // analytic/swept/spun/blend form ptrs
            ],
            false,
            None,
        ),

        // ── Type 126: NURBS_SURF ─────────────────────────────────
        // All 22 transmitted: u_periodic(l), v_periodic(l), u_degree(n),
        //   v_degree(n), n_u_vertices(d), n_v_vertices(d), u_knot_type(u),
        //   v_knot_type(u), n_u_knots(d), n_v_knots(d), rational(l),
        //   u_closed(l), v_closed(l), surface_form(u), vertex_dim(n),
        //   bspline_vertices(p), u_knot_mult(p), v_knot_mult(p), u_knots(p),
        //   v_knots(p); u_knot_mult_sum and v_knot_mult_sum are transmit=0
        NURBS_SURF => (
            vec![L, L, N, N, D, D, U, U, D, D, L, L, L, U, N, P, P, P, P, P],
            false,
            None,
        ),

        // ── Type 127: KNOT_MULT ──────────────────────────────────
        // Variable: mult(n, extra2=1)
        KNOT_MULT => (vec![], true, Some(VarType::I16)),

        // ── Type 128: KNOT_SET ───────────────────────────────────
        // Variable: knots(f, extra2=1)
        KNOT_SET => (vec![], true, Some(VarType::F64)),

        // ── Type 130: PE_CURVE ───────────────────────────────────
        // Transmitted fixed: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), type(c),
        //   data(p), tf(p)
        // Variable: internal_geom(p, extra2=1)
        PE_CURVE => (
            vec![D, P, P, P, P, P, C, C, P, P],
            true,
            Some(VarType::Ptr),
        ),

        // ── Type 132: PCURVE ─────────────────────────────────────
        // All 10 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), bsp_parms(l),
        //   surface(p), bspline(p)
        PCURVE => (vec![D, P, P, P, P, P, C, L, P, P], false, None),

        // ── Type 133: TRIMMED_CURVE ──────────────────────────────
        // All 12 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c),
        //   basis_curve(p), point_1(v), point_2(v), parm_1(f), parm_2(f)
        TRIMMED_CURVE => (
            vec![D, P, P, P, P, P, C, P, V, V, F64, F64],
            false,
            None,
        ),

        // ── Type 134: B_CURVE ────────────────────────────────────
        // All 9 transmitted: node_id(d), attributes_features(p), owner(p),
        //   next(p), previous(p), geometric_owner(p), sense(c), nurbs(p),
        //   data(p)
        B_CURVE => (vec![D, P, P, P, P, P, C, P, P], false, None),

        // ── Type 135: CURVE_DATA ─────────────────────────────────
        // Transmitted: self_int(u), analytic_form(p)
        CURVE_DATA => (vec![U, P], false, None),

        // ── Type 136: NURBS_CURVE ────────────────────────────────
        // Transmitted: degree(n), n_vertices(d), vertex_dim(n), n_knots(d),
        //   knot_type(u), periodic(l), closed(l), rational(l), curve_form(u),
        //   bspline_vertices(p), knot_mult(p), knots(p)
        //   (knot_mult_sum is transmit=0)
        NURBS_CURVE => (
            vec![N, D, N, D, U, L, L, L, U, P, P, P],
            false,
            None,
        ),

        // ── Type 137: SP_CURVE ───────────────────────────────────
        // Transmitted: node_id(d), attributes_features(p), owner(p), next(p),
        //   previous(p), geometric_owner(p), sense(c), surface(p), b_curve(p),
        //   original(p), tolerance_to_original(f)
        SP_CURVE => (vec![D, P, P, P, P, P, C, P, P, P, F64], false, None),

        // ── Type 141: GEOMETRIC_OWNER ────────────────────────────
        // All 4 transmitted: owner(p), next(p), previous(p),
        //   shared_geometry(p)
        GEOMETRIC_OWNER => (vec![P, P, P, P], false, None),

        // ── Type 150-163: analytic form types ────────────────────
        // These are sub-entities referenced from SURFACE_DATA.
        150 => (vec![V, V], false, None),             // PLANE_FORM: pvec, normal
        151 => (vec![V, V, F64, C], false, None),     // CYLINDER_FORM: pvec,axis,radius,sense
        152 => (vec![V, V, F64, F64, F64, C], false, None), // CONE_FORM
        153 => (vec![V, F64, C], false, None),        // SPHERE_FORM: centre,radius,sense
        154 => (vec![V, V, F64, F64, C], false, None), // TORUS_FORM
        155 => (vec![V, C], false, None),              // SWEPT_FORM: sweep,subtype
        156 => (vec![V, V, C, C], false, None),        // SWEPT_UV_FORM
        157 => (vec![V, V, C], false, None),           // SPUN_FORM: base,axis,subtype
        158 => (vec![F64, F64, C, C], false, None),    // VAR_RADIUS_PIPE_FORM
        163 => (                                        // HELIX_SU_FORM
            vec![V, V, C, I, F64, F64, F64],
            false,
            None,
        ),

        // ── Type 172: SESSION_DATA ───────────────────────────────
        // Transmitted: attrib_def(p), parameter_check_on(l), journal_on(l),
        //   roll_forward(l), journal_open(d), rollback_size(d), tag_limit(d)
        SESSION => (vec![P, L, L, L, D, D, D], false, None),

        // ── Type 184: HELIX_CU_FORM ──────────────────────────────
        // All 7 transmitted: axis_pt(v), axis_dir(v), point(v), hand(c),
        //   turns(i), pitch(f), tol(f)
        184 => (vec![V, V, V, C, I, F64, F64], false, None),

        // Type 204 (INTERSECTION_DATA) is PS30-specific — NOT in sch_13006.
        // It uses Path B (full inline schema) when encountered.

        _ => return None,
    };

    // CHART (type 40): var count is in field[2] (D, the chart_count field).
    // REAL_LIS_BLOCK (71): var count is in field[0] (n_entries).
    // INTEGER_LIS_BLOCK (72): var count is in field[0].
    // TAG_LIS_BLOCK (73) and POINTER_LIS_BLOCK (74): var count in field[0].
    let var_count_field_idx = if type_id == CHART {
        Some(2)
    } else if type_id == REAL_LIS_BLOCK || type_id == INTEGER_LIS_BLOCK
        || type_id == TAG_LIS_BLOCK || type_id == POINTER_LIS_BLOCK
    {
        Some(0)
    } else {
        None
    };

    // NOTE: entity_index_is_var_count is NOT used by entity.rs — the variable
    // array count always comes from the VERSION token read before entity_index,
    // not from entity_index itself. This field is retained only as documentation.
    let entity_index_is_var_count = false;

    Some(EntitySchema {
        type_id,
        fields,
        is_variable,
        var_type,
        var_count_field_idx,
        entity_index_is_var_count,
    })
}


/// Return the version=0 schema for entity types that use a different field
/// layout when the inline schema annotation uses the `?N` optional-ptr prefix.
///
/// BLENDED_EDGE (56): PS0.15 layout omits `sense` and `blend_type` chars and
/// uses range/thumb_weight floats immediately after the 6 header pointers.
///
/// BLENDED_VERTEX (57) and BLEND_OVERLAP (58) use `D P P P P P V` when
/// version=0, versus their version>0 layout (V first, no node_id) cached in
/// base_schema.
pub fn base_version0_schema(type_id: u16) -> Option<EntitySchema> {
    let (fields, is_variable, var_type, var_count_field_idx) = match type_id {
        CHART => (
            // Old CHART layout: chart_id (D) precedes the float fields.
            // count is in field[3] (D). Followed by P P hvec pointers.
            // The `??` inline-data prefix is consumed by P P reading as null ptrs.
            vec![
                D,   // 0: chart_id (old field, removed in PS13)
                F64, // 1: base_parameter
                F64, // 2: base_scale
                D,   // 3: chart_count (number of V3 points)
                F64, // 4: chordal_error
                F64, // 5: angular_error
                P,   // 6: hvec1 pointer (consumes first `?` of `??` inline prefix)
                P,   // 7: hvec2 pointer (consumes second `?` of `??` inline prefix)
            ],
            true,
            Some(VarType::V3),
            Some(3usize),
        ),
        BLENDED_EDGE => (
            vec![
                D,   // node_id
                P,   // attributes_features
                P,   // owner
                P,   // next
                P,   // previous
                P,   // geometric_owner
                F64, // range[0] (no sense/blend_type chars in version=0 layout)
                F64, // range[1]
                F64, // thumb_weight
                P,   // surface[0]
                P,   // surface[1]
                P,   // spine
                P,   // start (→ LIMIT)
                P,   // end (→ LIMIT)
            ],
            false,
            None,
            None,
        ),
        BLENDED_VERTEX => (
            vec![
                D,   // node_id
                P,   // attributes_features
                P,   // owner
                P,   // surface[0]
                P,   // surface[1]
                P,   // boundary
                V,   // centre (3D position)
            ],
            false,
            None,
            None,
        ),
        BLEND_OVERLAP => (
            vec![
                D,   // node_id
                P,   // attributes_features
                P,   // owner
                P,   // surface[0]
                P,   // surface[1]
                P,   // boundary
                V,   // centre (3D position)
            ],
            false,
            None,
            None,
        ),
        _ => return None,
    };
    Some(EntitySchema {
        type_id,
        fields,
        is_variable,
        var_type,
        var_count_field_idx,
        entity_index_is_var_count: false,
    })
}

/// Return the effective schema for files that carry no inline schema section.
///
/// When a T-line schema key has no base-schema group (`SCH_<modeller>_<schema>`
/// rather than `SCH_<modeller>_<schema>_13006`), the file names a complete
/// standard schema and writes entities straight against it.
///
/// `key_major` is the major version of the key's first group. Schemas below V20
/// match sch_13006 for every type seen so far. V20 and later carry the layout
/// changes that SW-era files spell out as inline annotation diffs — recovered
/// by dumping those diffs from `500.076.x_t` / `500.078.x_t` / `500.081.x_t`,
/// where only BODY (12), LIST (70) and POINTER_LIS_BLOCK (74) differ; every
/// other type annotates as `255` (base as-is).
pub fn standard_schema(type_id: u16, key_major: u32) -> Option<EntitySchema> {
    if key_major < 20 {
        return match type_id {
            // FIN carries no attributes_features pointer before V20 — the
            // leading `p` of the sch_13006 layout is simply absent, everything
            // else lines up. Compare the same entity indices in 500.079UB.x_t
            // (V10) and 500.076.x_t (V24, writes FIN as `255` = base as-is):
            //   V24  17 255 18 | 0 20 18 18 0 21 9 0 0 +   (10 fields)
            //   V10  17     18 |   20 18 18 0 21 9 0 0 +   (9 fields)
            FIN => Some(EntitySchema {
                type_id,
                fields: vec![P, P, P, P, P, P, P, P, C],
                is_variable: false,
                var_type: None,
                var_count_field_idx: None,
                entity_index_is_var_count: false,
            }),

            // ATTRIB_DEF has no field_names pointer before V20, and 13
            // legal_owners rather than 14. All three ATTRIB_DEFs in
            // 500.079UB.x_t agree, and the actions octet is identical to the
            // V20 file's for the same attribute type_id 8004:
            //   V20  80 2 144 | 146 147 8004 | 0 0 0 0 3 5 0 0 | 0 | FFTFFFFFFFFFFF
            //   V10  80 2  10 | 137 138 8004 | 0 0 0 0 3 5 0 0 |   | FFTFFFFFFFFFF
            ATTRIB_DEF => Some(EntitySchema {
                type_id,
                fields: vec![
                    P, P, D,
                    U, U, U, U, U, U, U, U, // actions×8
                    L, L, L, L, L, L, L, L, L, L, L, L, L, // legal_owners×13
                ],
                is_variable: true,
                var_type: Some(VarType::I16),
                var_count_field_idx: None,
                entity_index_is_var_count: false,
            }),

            _ => base_schema(type_id),
        };
    }

    match type_id {
        // BODY: four fields appended after the sch_13006 layout —
        // index_map_offset(d), index_map(p), node_id_index_map(p),
        // schema_embedding_map(p).
        BODY => {
            let mut schema = base_schema(BODY)?;
            schema.fields.extend_from_slice(&[D, P, P, P]);
            Some(schema)
        }

        // LIST: list_type(u) and notransmit(l) are inserted after node_id,
        // size_of_entry and one length field are dropped, and finger_index /
        // finger_block move ahead of list_block:
        //   node_id(d), list_type(u), notransmit(l), owner(p), next(p),
        //   previous(p), list_length(d), block_length(d), finger_index(d),
        //   finger_block(p), list_block(p)
        LIST => Some(EntitySchema {
            type_id,
            fields: vec![D, U, L, P, P, P, D, D, D, P, P],
            is_variable: false,
            var_type: None,
            var_count_field_idx: None,
            entity_index_is_var_count: false,
        }),

        // POINTER_LIS_BLOCK: one extra leading field before the base
        // n_entries(d), next_block(p) pair; still a variable pointer array.
        POINTER_LIS_BLOCK => Some(EntitySchema {
            type_id,
            fields: vec![D, D, P],
            is_variable: true,
            var_type: Some(VarType::Ptr),
            var_count_field_idx: None,
            entity_index_is_var_count: false,
        }),

        _ => base_schema(type_id),
    }
}

/// Return the schema for PS30+ extended entity types that appear via the
/// path-B compact path-A fallback (i.e., types not in base_schema whose
/// first occurrence in a partition is written in path-B format, but whose
/// actual field layout is known empirically).
///
/// The returned schema is used by the path-B compact path-A branch when
/// `type_id` matches a known compact type. Falls back to `None` when the
/// type is unknown (caller uses the PS30_EXT_446 12-field default).
pub fn ps30_compact_schema(type_id: u16) -> Option<EntitySchema> {
    // Variable-schema types must be returned early before the fixed-schema
    // match block because that block always produces is_variable=false schemas.
    if type_id == PS30_LEGACY_2 {
        // Legacy entity type 2. Starts with non-integer data (TF prefix on first
        // float — two char fields T, F immediately followed by the float value).
        //
        // Fixed header: [C, C, V, V, D, D, D, D]
        //   C('T') + C('F') packed with first float, 2 V3 vectors, then 4 D fields.
        //   D[0]=pointer to INTERSECTION_DATA (204), D[1]=float count,
        //   D[2]=entity pointer, D[3]=additional count.
        //
        // Variable body: D[1] floats (intersection parameter + error pairs).
        // var_count_field_idx=Some(5) references field index 5, the second D
        // field in the 8-field fixed list (C,C,V,V,D,D,D,D).
        return Some(EntitySchema {
            type_id,
            fields: vec![C, C, V, V, D, D, D, D],
            is_variable: true,
            var_type: Some(VarType::F64),
            var_count_field_idx: Some(5),
            entity_index_is_var_count: false,
        });
    }

    let fields: Vec<FieldType> = match type_id {
        PS30_EXT_98 => {
            // Entity type 98 (unknown, likely an extended attribute container).
            // Empirically determined 17-field schema from ABC dataset (Part Studio 1).
            // 7 pointer fields, 1 V, 1 F64, 1 V, then 7 integer fields = 21 tokens.
            vec![P, P, P, P, P, P, P, V, F64, V, D, D, D, D, D, D, D]
        }
        PS30_EXT_103 => {
            // PS30+ extended entity type 103. Appears in attribute-definition clusters
            // as an extended ATTRIB_DEF container. Compact path-B reads n_fields as
            // entity_idx (typically 101). Empirically determined 16-field schema:
            //   D×15 (int fields: type_code, flag, ptrs, 8017 code, 9 zeros)
            //   + S (attribute flag string, e.g. "TTTTTTTTTTTTF3")
            // After consuming 16 fields the stream resumes with type=84 (INT_VALUES).
            vec![D, D, P, P, P, P, D, D, D, D, D, D, D, D, D, S]
        }
        PS30_EXT_1020 => {
            // PS30+ extended entity type 1020. Empirically determined 81-field schema.
            // All tokens are integer-valued (entity handles, sense markers, optional ptrs).
            // After consuming 81 tokens the stream resumes at block-end 0 0 1 then type=81.
            vec![
                P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P,
                P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P,
                P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P,
            ]
        }
        PS30_EXT_315 => {
            // PS30+ extended entity type 315. Empirically determined 46-field schema.
            // Appears in wire topology clusters after LOOP (type 15) entities.
            // All 46 tokens are integer-valued (entity handles, sense markers, indices).
            // Sense markers include -17 (reversed FIN), +16 (forward EDGE), ?69 (optional ptr).
            // After consuming 46 tokens the stream resumes at block-end 0 0 1 then type=17 (FIN).
            vec![
                P, P, P, P, P, P, P, P, P, P, P, P, // fields 0-11
                P, P, P, P, P, P, P, P, P, P, P, P, // fields 12-23
                P, P, P, P, P, P, P, P, P, P, P, P, // fields 24-35
                P, P, P, P, P, P, P, P, P, P,       // fields 36-45
            ]
        }
        PS30_EXT_599 => {
            // PS30+ extended entity type 599. Empirically determined 20-field schema.
            // Appears in LOOP/FIN clusters. Compact path-B reads n_fields as entity_idx.
            // Layout: 7 P fields, V3 (3 float tokens), F64, V3, D, D, D, P, D, P, P, P, P, P
            // = 20 fields consuming 24 stream tokens (7 + 3 + 1 + 3 + 1+1+1+1+1+1+1+1+1+1).
            // After consuming 24 tokens the stream resumes at block-end 0 0 1
            // then type=17 (FIN).
            vec![P, P, P, P, P, P, P, V, F64, V, D, D, D, P, D, P, P, P, P, P]
        }
        PS30_EXT_308 => {
            // PS30+ extended entity type 308. Empirically determined 49-field schema.
            // Appears in wire-body contexts after LOOP/EDGE entities, encodes
            // curve evaluation data: positions, tangent vectors, and edge ptrs.
            //
            // Structure:
            //   Header:  P×12 + D (13 fields, 13 tokens)
            //   Record 1: V + D + F64 + F64 + D + P + D + D + P + P + P (11 fields, 13 tokens)
            //   Record 2: D + V + D + F64 + F64 + D + P + D + D + P + P + P (12 fields, 14 tokens)
            //   Record 3: D + V + F64 + D + D + D + P + D + P + P + P + P + P (12 fields, 13 tokens)
            // Total: 49 fields, 55 stream tokens.
            // Confirmed: after consuming 55 tokens the next token is block-end 0 0 1
            // and the stream resumes at type=30 (LINE).
            vec![
                P, P, D, D, D, P, P, D, D, P, P, P,  // header: 12 fields (12 tokens)
                D,                                     // field 12: D=0 (1 token)
                V,                                     // field 13: V (3 tokens, sense-prefix stripped)
                D, F64, F64,                           // fields 14-16 (3 tokens)
                D, P, D, D, P, P,                      // fields 17-22 (6 tokens)
                P,                                     // field 23: P=319 (1 token)
                D,                                     // field 24: D=0 (1 token)
                V,                                     // field 25: V (3 tokens)
                D, F64, F64,                           // fields 26-28 (3 tokens)
                D, P, D, D, P, P, P,                   // fields 29-35 (7 tokens)
                D,                                     // field 36: D=0 (1 token)
                V,                                     // field 37: V (3 tokens)
                F64,                                   // field 38: F64=-1 (1 token)
                D, D, D,                               // fields 39-41 (3 tokens)
                P, D, P, P, P, P, P,                   // fields 42-48 (7 tokens)
            ]
        }
        PS30_EXT_488 => {
            // PS30+ extended entity type 488. Empirically determined 20-field schema.
            // Data: D×4 (int header: 83 3 attrib_def_ptr 1) + F64×2 (color R,G)
            //       + D (topology count=14) + P×6 (entity ptrs) + D (edge count=6)
            //       + P×4 + P (ptr) + D (flag=28).
            // Confirmed by tracing from entity_idx=1085 to next ATTRIBUTE(81) boundary.
            vec![D, D, D, D, F64, F64, D, P, P, P, P, P, P, D, P, P, P, P, P, D]
        }
        PS30_EXT_598 => {
            // PS30+ extended entity type 598. Empirically determined 30-field
            // all-pointer schema (consuming 30 stream tokens). Appears after
            // LOOP (type 15) entities; likely an extended loop-data record.
            // All 30 tokens are integer-valued (pointers or signed node ids);
            // none are floats. After consuming 30 tokens the stream resumes at
            // type_id=0 (partition boundary) then type_id=596.
            vec![P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P]
        }
        PS30_EXT_457 => {
            // PS30+ extended entity type 457. Empirically determined 30-field
            // all-pointer schema (consuming 30 stream tokens). Same layout as
            // PS30_EXT_598. All tokens are integer-valued (pointers / signed
            // node ids). After 30 tokens the stream resumes at a new type_id.
            vec![P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P]
        }
        PS30_EXT_649 => {
            // PS30+ extended entity type 649. Empirically determined 19-field
            // all-pointer schema (consuming 19 stream tokens). Appears in the
            // per-face topology cluster after type 4515 entities. After 19
            // tokens the stream reaches a new type_id (type 4524 or similar).
            vec![P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P]
        }
        PS30_EXT_591 => {
            // PS30+ extended entity type 591. Empirically determined 23-field
            // all-pointer schema (consuming 23 stream tokens). Appears after
            // type 607 entities. All tokens are integer-valued. After 23 tokens
            // the stream reaches type_id=38 (INTERSECTION).
            vec![P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P]
        }
        PS30_EXT_604 => {
            // PS30+ extended entity type 604. Empirically determined 17-field
            // all-pointer schema (consuming 17 stream tokens). Appears after
            // type 596 entities. All tokens are integer-valued (pointers or
            // signed node ids). After 17 tokens the stream reaches type_id=607.
            vec![P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P]
        }
        PS30_EXT_682 => {
            // PS30+ extended entity type 682. Empirically determined 13-field
            // schema with a sense char (C) at field[8] instead of F64.
            // Stream at field[8] has '+-.006885...' where '+' is the C field
            // and '-.006885 .037989 0' is the V field that follows.
            // After 13 fields (P×7 + V + C + V + D + D + D) the stream reaches
            // type_id=51 (CYLINDER). The third D field holds -1.
            vec![P, P, P, P, P, P, P, V, C, V, D, D, D]
        }
        PS30_EXT_687 => {
            // PS30+ extended entity type 687. Empirically determined 11-field
            // schema. Stream: P×5 + C + V + V + D + D + D. The C field
            // consumes the '+' orientation prefix from '+-.10544...' and the
            // first V reads the vector, second V reads direction, D×3 follows.
            // After 11 fields the stream reaches type_id=141 (GEOMETRIC_OWNER).
            vec![P, P, P, P, P, C, V, V, D, D, D]
        }
        PS30_EXT_692 | PS30_EXT_697 | PS30_EXT_718 | PS30_EXT_771 => {
            // PS30+ extended entity types 692, 697, 718. Empirically determined
            // 10-field schema. Stream: P×5 + C + V + V + V + D. The C field
            // consumes the '+' orientation prefix from '+-.NNN...' and three V
            // fields hold the axis, direction and auxiliary vectors. The final D
            // is a flags or state integer. After 10 fields the stream reaches
            // type_id=14 (FACE) or type_id=141 (GEOMETRIC_OWNER).
            vec![P, P, P, P, P, C, V, V, V, D]
        }
        PS30_EXT_719 => {
            // PS30+ extended entity type 719. Empirically determined 9-field
            // schema. Stream: P×5 + C + V + V + V. No trailing D field; after
            // the three vectors the stream reaches type_id=141 (GEOMETRIC_OWNER).
            vec![P, P, P, P, P, C, V, V, V]
        }
        PS30_EXT_783 => {
            // PS30+ extended entity type 783. Empirically determined 12-field
            // schema. Stream: P×7 + V×5. No C or F64 fields; five 3-component
            // vectors follow the seven pointer fields. After 22 stream tokens
            // the stream reaches type_id=133 (TRIMMED_CURVE).
            vec![P, P, P, P, P, P, P, V, V, V, V, V]
        }
        PS30_EXT_798 => {
            // PS30+ extended entity type 798. Empirically determined 20-field
            // schema. Stream: P×15 + C + V + V + V + F64. After consuming 26
            // stream tokens the stream reaches type_id=141 (GEOMETRIC_OWNER).
            vec![P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, C, V, V, V, F64]
        }
        PS30_EXT_800 => {
            // PS30+ extended entity type 800. Empirically determined 6-field
            // schema. Stream: P×2 + V + V + D + D. After consuming 10 stream
            // tokens the stream reaches type_id=14 (FACE).
            vec![P, P, V, V, D, D]
        }
        PS30_EXT_786 => {
            // PS30+ extended entity type 786. Empirically determined 16-field
            // all-pointer schema. After 16 P tokens the stream reaches the
            // next type_id. The first occurrence is followed by type=0/1
            // block-boundary tokens; the second is followed by type=800.
            vec![P, P, P, P, P, P, P, P, P, P, P, P, P, P, P, P]
        }
        PS30_EXT_832 => {
            // PS30+ extended entity type 832. Empirically determined 15-field
            // all-pointer schema. After 15 P tokens the stream reaches
            // type_id=801 (PS30+ extended).
            vec![P, P, P, P, P, P, P, P, P, P, P, P, P, P, P]
        }
        PS30_EXT_801 => {
            // PS30+ extended entity type 801. Empirically determined 37-field
            // all-pointer schema. After 37 P tokens and a type=1 block-end,
            // the stream reaches type_id=840 (PS30+ extended).
            vec![
                P, P, P, P, P, P, P, P, P, P,
                P, P, P, P, P, P, P, P, P, P,
                P, P, P, P, P, P, P, P, P, P,
                P, P, P, P, P, P, P,
            ]
        }
        PS30_EXT_840 | PS30_EXT_842 | PS30_EXT_845 => {
            // PS30+ extended entity types 840, 842, 845. Empirically determined
            // 22-field all-pointer schema. After 22 P tokens and a type=1
            // block-end, the stream reaches the next type_id.
            vec![
                P, P, P, P, P, P, P, P, P, P,
                P, P, P, P, P, P, P, P, P, P,
                P, P,
            ]
        }
        PS30_EXT_848 => {
            // PS30+ extended entity type 848. Empirically determined 4-field
            // all-pointer schema. Appears in the 848/857/654 cluster after
            // type 845 entities. After 4 P tokens the stream reaches either
            // type_id=17 (FIN, same block) or the next block type_id.
            // Verified by exact token-count analysis of the cluster block.
            vec![P, P, P, P]
        }
        PS30_EXT_853 => {
            // PS30+ extended entity type 853. Empirically determined 26-field
            // all-pointer schema. After 26 P tokens and a type=1 block-end,
            // the stream reaches type_id=17 (FIN).
            vec![
                P, P, P, P, P, P, P, P, P, P,
                P, P, P, P, P, P, P, P, P, P,
                P, P, P, P, P, P,
            ]
        }
        PS30_EXT_857 => {
            // PS30+ extended entity type 857. Empirically determined 19-field
            // all-pointer schema. Appears after FIN (type 17) entities in the
            // 848/857/654 cluster. After 19 P tokens the stream reaches
            // type_id=654. Verified by exact token-count analysis.
            vec![
                P, P, P, P, P, P, P, P, P, P,
                P, P, P, P, P, P, P, P, P,
            ]
        }
        PS30_EXT_654 => {
            // PS30+ extended entity type 654. Empirically determined 22-field
            // all-pointer schema. Appears after type 857 entities. After 22 P
            // tokens and a type=1 block-end, the stream reaches type_id=17
            // (FIN). Verified by exact token-count analysis of the cluster block.
            vec![
                P, P, P, P, P, P, P, P, P, P,
                P, P, P, P, P, P, P, P, P, P,
                P, P,
            ]
        }
        PS30_EXT_589 => {
            // PS30+ extended entity type 589. Empirically determined 37-field
            // all-pointer schema. Block-end '1' follows at field index 37.
            vec![
                P, P, P, P, P, P, P, P, P, P,
                P, P, P, P, P, P, P, P, P, P,
                P, P, P, P, P, P, P, P, P, P,
                P, P, P, P, P, P, P,
            ]
        }
        PS30_EXT_876 => {
            // PS30+ extended entity type 876. Empirically determined 18-field
            // all-pointer schema. Appears after ATTRIBUTE (type 81) entities.
            // After 18 P tokens and a type=1 block-end, the stream reaches
            // type_id=16 (EDGE).
            vec![
                P, P, P, P, P, P, P, P, P, P,
                P, P, P, P, P, P, P, P,
            ]
        }
        PS30_EXT_922 => {
            // PS30+ extended entity type 922. Empirically determined 2-field
            // all-pointer schema. Confirmed by stream analysis: exactly 2 P
            // tokens follow entity_idx before the block-end '1'.
            vec![P, P]
        }
        PS30_EXT_928 => {
            // PS30+ extended entity type 928. Empirically determined 30-field
            // schema: 5P, V, V, F64, V, 8P, V, V, V, 10P = 42 stream tokens.
            // Follows FACE (type 14) entities. Contains a coordinate frame
            // (position vec3 + two orientation vec3s + tolerance F64) and
            // a second frame, surrounded by pointer fields.
            vec![
                P, P, P, P, P,
                V, V, F64, V,
                P, P, P, P, P, P, P, P,
                V, V, V,
                P, P, P, P, P, P, P, P, P, P,
            ]
        }
        PS30_EXT_934 => {
            // PS30+ extended entity type 934. Empirically determined 15-field
            // schema: P, P, V, V, V, P×10 = 21 stream tokens. Appears after
            // type 928 entities. The two leading pointers are followed by three
            // vec3 fields (coordinate frame), then 10 pointer fields.
            vec![P, P, V, V, V, P, P, P, P, P, P, P, P, P, P]
        }
        PS30_EXT_936 => {
            // PS30+ extended entity type 936. Empirically determined 31-field
            // all-pointer schema. After 31 P tokens the stream continues.
            // Appears after type 934 entities.
            vec![
                P, P, P, P, P, P, P, P, P, P,
                P, P, P, P, P, P, P, P, P, P,
                P, P, P, P, P, P, P, P, P, P,
                P,
            ]
        }
        PS30_EXT_790 => {
            // PS30+ extended entity type 790. Empirically determined 16-field
            // schema: P×11 + C + V + V + F64 + V = 22 stream tokens.
            // The C field consumes the '+' orientation prefix from '+.NNN',
            // and the three V3 vectors + F64 hold the geometric frame data.
            // After 22 tokens the stream reaches type_id=141 (GEOMETRIC_OWNER).
            vec![P, P, P, P, P, P, P, P, P, P, P, C, V, V, F64, V]
        }
        PS30_EXT_793 => {
            // PS30+ extended entity type 793. Empirically determined 1-field
            // schema: a single pointer field. Entity data is always a single
            // integer (0 = null pointer) followed by a block-end '1'.
            vec![P]
        }
        PS30_EXT_939 => {
            // PS30+ extended entity type 939. Data starts directly with a
            // character token ('L') — no integer schema header. The non-digit
            // start is detected by read_inline_schema_path_b and routed here
            // with entity_idx=0.
            //
            // Empirically determined 4-field schema from stream analysis:
            //   C('L') + C('?') + V3 (3 floats) + D(41)
            // before block-end '1'. Same character-prefix pattern as LIMIT(41).
            vec![C, C, V, D]
        }
        PS30_EXT_803 => {
            // PS30+ extended entity type 803. Empirically determined 7-field
            // all-pointer schema. Appears after LOOP (type 15) entities in the
            // loop-annotation cluster. The default 12-field schema was consuming
            // subsequent LOOP and ATTRIB_DEF entity data as its own fields;
            // correct count is 7 pointer fields.
            vec![P, P, P, P, P, P, P]
        }
        PS30_EXT_975 => {
            // PS30+ extended entity type 975. Appears in partition-2 attribute
            // clusters following FACE topology. Compact path-B reads entity_idx
            // from stream (large handle); remaining 7 pointer fields link to
            // surrounding attribute and topology entities.
            vec![P, P, P, P, P, P, P]
        }
        PS30_EXT_716
        | PS30_EXT_966 | PS30_EXT_971 | PS30_EXT_976 | PS30_EXT_984 | PS30_EXT_989
        | PS30_EXT_986 | PS30_EXT_990
        | PS30_EXT_1046 | PS30_EXT_1049 | PS30_EXT_1081 | PS30_EXT_1085
        | PS30_EXT_1089 | PS30_EXT_1487 | PS30_EXT_1490 => {
            // PS30+ extended entity types in the color/attribute-value family.
            // These all encode a 3-component real attribute value:
            //   D(component_count=3) + P(attrib_def ptr) + D(flag=1) + F64 + F64
            // Compact path-B reads n_fields as entity_idx (typically 83).
            // After consuming 5 fields the stream resumes with an ATTRIBUTE or
            // similar topology entity.
            vec![D, P, D, F64, F64]
        }
        t if (960..=999).contains(&t) => {
            // PS30+ extended entity types in the 960-999 range. These appear as
            // standalone entities interspersed with the B-rep topology. The
            // compact path-B fallback reads the next entity's type_id as
            // entity_idx; the remaining 10 pointer fields must be consumed.
            // Confirmed for types 962, 964, 968, 974.
            vec![P, P, P, P, P, P, P, P, P, P]
        }
        PS30_EXT_8001 | PS30_EXT_9000 | PS30_EXT_8038 | PS30_EXT_8017 | PS30_EXT_8040 => {
            // PS30+ attribute-definition entity types (8001, 9000, 8038, 8017,
            // 8040). Empirically determined 9-field schema: 8 pointer/integer
            // fields followed by one opaque attribute-flag token (e.g.
            // `FFFFTFTFFFFFFF2`, `TTTTTTTTTTTTTF10`). The flag token is a
            // whitespace-delimited string of T/F chars + a digit suffix; its
            // length varies and must be read as a single token (field type S).
            //
            // Note: the compact path-B fallback reads the first stream integer
            // (always 0) as n_fields=0 and uses it as entity_idx=0. So the
            // remaining entity data is 8 integers + 1 S token (not 9+1).
            vec![P, P, P, P, P, P, P, P, S]
        }
        PS30_LEGACY_6 => {
            // Legacy entity type 6. Appears with float-first data (no integer
            // schema header in the stream). The float-start detection in
            // read_inline_schema_path_b routes here with entity_idx=0.
            //
            // Empirically determined 11-field schema from stream analysis:
            //   F64 × 4 (includes 2 NaN from '??' token) + V × 6 + D
            // = 23 stream reads (3 floats + 2 NaN from ??.xxx + 18 floats + 1 int)
            // before block-end '1'. The final D field consumes the integer '41'.
            vec![F64, F64, F64, F64, V, V, V, V, V, V, D]
        }
        PS30_LEGACY_5 => {
            // Legacy entity type 5 (old Parasolid TOPOL or extended pointer node).
            // Appears in PS30 compact format with n_fields=0 (compact path-A) and
            // entity_idx=0. Empirically determined 2-field schema: two null pointer
            // fields. After reading 2 tokens the stream reaches the PS30 attribute
            // flag token (TTTFFFFFFFFFFF2) followed by the next entity (type 83).
            vec![P, P]
        }
        _ => return None,
    };
    Some(EntitySchema {
        type_id,
        fields,
        is_variable: false,
        var_type: None,
        var_count_field_idx: None,
        entity_index_is_var_count: false,
    })
}

// ── Schema preamble parser ───────────────────────────────────────────────────

/// An annotation operation from the schema preamble.
#[derive(Debug, Clone)]
pub enum Annotation {
    /// Copy field from base schema.
    Copy,
    /// Delete field from base schema.
    Delete,
    /// Insert a new field (name, type_id, default fields to skip).
    Insert {
        name: String,
        field_type_id: u16,
        n_defaults: usize,
    },
    /// Append a new field (same as Insert, placed at end).
    Append {
        name: String,
        field_type_id: u16,
        n_defaults: usize,
    },
}

/// Result of parsing the schema preamble: per-type annotation lists.
#[derive(Debug, Clone)]
pub struct SchemaPreamble {
    pub entries: Vec<SchemaEntry>,
    /// n_secondary from the T-line header (partition_count, usually 0).
    pub partition_count: usize,
}

#[derive(Debug, Clone)]
pub struct SchemaEntry {
    pub type_id: u16,
    pub n_annotations: u32,
    pub annotations: Vec<Annotation>,
}

/// Parse the T-line to extract version int and position past it.
/// Input should be the raw body text (newlines NOT stripped yet).
/// Returns (format_version, modeller_version, remaining_body_with_newlines_stripped).
/// Result of parsing the T-line.
#[derive(Debug, Clone)]
pub struct TLine {
    /// Internal text-transmit format version (the `51` in `T51`).
    pub fmt_version: u32,
    /// Packed modeller version from the comment (major*10^7 + ...).
    pub modeller_version: u64,
    /// Major version of the schema key's first group, e.g. `20` for
    /// `SCH_2000000_200000`. This selects the field layouts to read with.
    pub key_major: u32,
    /// True when the key carries a third group (`SCH_<modeller>_<schema>_<base>`,
    /// e.g. `SCH_2401231_20000_13006`). That group names the base schema the
    /// file's inline schema diffs are written against; such files carry a
    /// two-int preamble (n_types, partition_count) followed by per-type inline
    /// schema annotations.
    ///
    /// When the key has only two groups (e.g. `SCH_2000000_200000`) the file's
    /// schema *is* the named schema: there is no preamble and no inline schema
    /// section, and every entity's fields follow the standard layout directly.
    pub has_base_schema: bool,
    /// The body with newlines stripped, positioned just past the T-line.
    pub body: String,
}

/// Parse the T-line.
pub fn parse_tline(body: &str) -> crate::error::Result<TLine> {
    // The T-line is the first line of the body.
    // Find 'T' followed by version int.
    let body = body.trim_start();
    if !body.starts_with('T') {
        return Err(crate::error::XtError::InvalidHeader(
            "expected T-line after header".into(),
        ));
    }

    // Strip ALL newlines from the entire body to create seamless stream.
    // This matches Parasolid's buffer refill behavior: newlines are stripped
    // and long tokens (17-digit floats) intentionally span line breaks.
    // The X_T writer ensures token-terminating spaces are placed so that
    // concatenation only happens within a single token, not between tokens.
    let stripped: String = body
        .chars()
        .filter(|c| *c != '\n' && *c != '\r')
        .collect();

    let mut input = stripped.as_str();

    // Parse T<version>
    'T'.parse_next(&mut input)
        .map_err(|_: winnow::error::ErrMode<winnow::error::ContextError>| {
            crate::error::XtError::Parse {
                offset: 0,
                detail: "expected 'T'".into(),
            }
        })?;

    let fmt_version: u32 = winnow::ascii::dec_uint
        .parse_next(&mut input)
        .map_err(|_: winnow::error::ErrMode<winnow::error::ContextError>| {
            crate::error::XtError::Parse {
                offset: 1,
                detail: "expected format version integer".into(),
            }
        })?;

    // Skip the comment part: " : TRANSMIT FILE created by modeller version "
    // Find the modeller version int (last number before 'SCH_')
    let sch_pos = input.find("SCH_").ok_or_else(|| {
        crate::error::XtError::InvalidHeader("missing SCH_ in T-line".into())
    })?;

    // Extract modeller version: the token just before SCH_
    let before_sch = input[..sch_pos].trim_end();
    let modeller_str = before_sch.rsplit(' ').next().unwrap_or("0");
    let modeller_version: u64 = modeller_str.parse().unwrap_or(0);

    // Advance input past SCH_ and read the schema key
    input = &input[sch_pos..];
    // Read SCH_<digits>_<digits> optionally _<digits>
    // Skip "SCH_"
    input = &input[4..];
    // Read first group of digits
    let key_first: &str = take_while::<_, _, winnow::error::ContextError>(1.., |c: char| {
        c.is_ascii_digit()
    })
    .parse_next(&mut input)
    .map_err(|_| {
        crate::error::XtError::Parse {
            offset: 0,
            detail: "schema key: expected digits".into(),
        }
    })?;
    // The key's first group is a packed modeller version: major*100000 + ...
    let key_major = (key_first.parse::<u32>().unwrap_or(0)) / 100_000;

    // _
    winnow::Parser::<&str, char, winnow::error::ContextError>::parse_next(&mut '_', &mut input)
        .map_err(|_| {
            crate::error::XtError::Parse {
                offset: 0,
                detail: "schema key: expected '_'".into(),
            }
        })?;
    // Second group
    let _d2: &str = take_while::<_, _, winnow::error::ContextError>(1.., |c: char| {
        c.is_ascii_digit()
    })
    .parse_next(&mut input)
    .map_err(|_| {
        crate::error::XtError::Parse {
            offset: 0,
            detail: "schema key: expected digits".into(),
        }
    })?;

    // Optional third group (build number) — tricky because it may run into
    // n_types, because newline stripping can concatenate them. Only where the
    // split falls matters: the build digits themselves are never used, the
    // group's mere presence is what says a base schema is named.
    if input.starts_with('_') {
        input = &input[1..];
        // Read ALL consecutive digits greedily
        let all_digits: &str =
            take_while::<_, _, winnow::error::ContextError>(1.., |c: char| c.is_ascii_digit())
                .parse_next(&mut input)
                .map_err(|_| {
                    crate::error::XtError::Parse {
                        offset: 0,
                        detail: "schema key: expected build digits".into(),
                    }
                })?;

        // Heuristic: split so that n_types < 1000 and build ≤ 5 digits.
        // Newline stripping can concatenate the build with subsequent tokens,
        // so we always try to find the best split.
        // Prefer the LONGEST valid build (don't break on first match).
        let mut best_split = all_digits.len(); // default: all is build
        for split in 1..all_digits.len() {
            let build_part = &all_digits[..split];
            if build_part.len() > 5 {
                break; // builds are at most 5 digits
            }
            let rest = &all_digits[split..];
            if rest.starts_with('0') && rest.len() > 1 {
                continue; // leading zero in n_types
            }
            if let Ok(n) = rest.parse::<u32>() {
                if n < 1000 {
                    best_split = split; // keep going for longer builds
                }
            }
        }
        // Put the remaining digits back into input
        let remaining = &all_digits[best_split..];
        // We need to reconstruct input with the remaining digits prepended
        let new_input = format!("{remaining}{input}");
        // We'll return this as the remaining body
        return Ok(TLine {
            fmt_version,
            modeller_version,
            key_major,
            has_base_schema: true,
            body: new_input,
        });
    }

    // Skip any remaining whitespace
    let rest = input.trim_start().to_string();
    Ok(TLine {
        fmt_version,
        modeller_version,
        key_major,
        has_base_schema: false,
        body: rest,
    })
}

/// Parse the schema preamble from the body stream (after T-line processing).
///
/// The preamble consists of exactly TWO fields after the schema key:
///   N_types (uint16)  — max entity type slot count for the schema table
///   entity_count (uint32) — partition array count (usually 0)
///
/// Schema entries are NOT parsed here. They are parsed LAZILY by the entity
/// loop when each entity type is first encountered (pk_read_inline_schema).
pub fn parse_schema_preamble(input: &mut &str) -> ModalResult<SchemaPreamble> {
    let _n_types = token::xt_uint(input)?;
    let partition_count = token::xt_uint(input)? as usize;

    Ok(SchemaPreamble {
        entries: Vec::new(),
        partition_count,
    })
}

/// Apply preamble annotations to a PS13 base schema, producing the effective
/// field list for this file.
pub fn apply_annotations(base: &EntitySchema, entry: &SchemaEntry) -> EntitySchema {
    let mut fields = Vec::new();
    let mut base_idx = 0;

    for ann in &entry.annotations {
        match ann {
            Annotation::Copy => {
                if base_idx < base.fields.len() {
                    fields.push(base.fields[base_idx]);
                }
                base_idx += 1;
            }
            Annotation::Delete => {
                // Skip one base field
                base_idx += 1;
            }
            Annotation::Insert { field_type_id, .. } => {
                // Insert new field (type derived from field_type_id)
                fields.push(field_type_from_id(*field_type_id));
                // Don't advance base_idx
            }
            Annotation::Append { field_type_id, .. } => {
                // Append new field at current position
                fields.push(field_type_from_id(*field_type_id));
            }
        }
    }

    EntitySchema {
        type_id: entry.type_id,
        fields,
        is_variable: base.is_variable,
        var_type: base.var_type,
        var_count_field_idx: base.var_count_field_idx,
        entity_index_is_var_count: base.entity_index_is_var_count,
    }
}

/// Map field_type_id from preamble to FieldType.
/// These IDs correspond to entity type IDs that hold the field data,
/// or 0 for generic integer/float fields.
fn field_type_from_id(id: u16) -> FieldType {
    match id {
        0 => D,     // generic integer
        12 => P,    // pointer to BODY
        82 => P,    // pointer to ATTRIBUTE_HOLDER
        206 => P,   // pointer to mesh_offset_data
        1006 => P,  // pointer to mesh
        1008 => P,  // pointer to polyline
        1012 => P,  // pointer to finger_block
        1040 => P,  // pointer to owner
        _ => D,     // fallback: treat as integer
    }
}
