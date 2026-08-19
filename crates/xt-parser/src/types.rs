use std::collections::HashMap;

// ── File-level ───────────────────────────────────────────────────────────────

/// Parsed XT file.
#[derive(Debug, Clone)]
pub struct XtFile {
    pub header: XtHeader,
    pub bodies: Vec<XtBody>,
}

/// Metadata extracted from the XT header block.
#[derive(Debug, Clone, Default)]
pub struct XtHeader {
    /// Parasolid version string, e.g. "30.1.168".
    pub version: String,
    /// Writing application, e.g. "Onshape", "SolidWorks".
    pub application: String,
    /// Schema key from PART2, e.g. "SCH_3001168_30100".
    pub schema_key: String,
    /// ISO-8601 date if present.
    pub date: Option<String>,
    /// User field size from PART2 (USFLD_SIZE). Each entity has this many
    /// extra integer words after its regular fields. 0 = no user fields.
    pub user_field_size: u32,
}

// ── Topology ─────────────────────────────────────────────────────────────────

/// A single B-Rep body.
#[derive(Debug, Clone)]
pub struct XtBody {
    pub node_id: i64,
    pub body_type: XtBodyType,
    /// Size resolution (units).
    pub res_size: f64,
    /// Linear tolerance (positional accuracy).
    pub res_linear: f64,
    pub regions: Vec<XtRegion>,
    pub shells: Vec<XtShell>,
    /// Geometry maps keyed by XT entity index.
    pub surfaces: HashMap<usize, XtSurface>,
    pub curves: HashMap<usize, XtCurve>,
    pub points: HashMap<usize, [f64; 3]>,
    pub vertices: HashMap<usize, XtVertex>,
    pub edges: HashMap<usize, XtEdge>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XtBodyType {
    Solid,
    Sheet,
    Wire,
    Acorn,
    General,
    Unknown(u8),
}

#[derive(Debug, Clone)]
pub struct XtRegion {
    pub is_solid: bool,
    pub shell_indices: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct XtShell {
    pub faces: Vec<XtFace>,
}

#[derive(Debug, Clone)]
pub struct XtFace {
    pub node_id: i64,
    pub tolerance: f64,
    /// Key into `body.surfaces`.
    pub surface_key: usize,
    pub sense: XtSense,
    pub loops: Vec<XtLoop>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XtSense {
    Forward,
    Reversed,
}

#[derive(Debug, Clone)]
pub struct XtLoop {
    pub kind: XtLoopKind,
    pub fins: Vec<XtFin>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XtLoopKind {
    Outer,
    Inner,
    Unknown,
}

/// Half-edge (fin).
#[derive(Debug, Clone)]
pub struct XtFin {
    /// Key into `body.edges`.
    pub edge_key: usize,
    /// Key into `body.vertices` (start vertex of this half-edge direction).
    pub vertex_key: Option<usize>,
    pub sense: XtSense,
    /// Optional parameter-space curve key into `body.curves`.
    pub pcurve_key: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct XtEdge {
    pub node_id: i64,
    /// Key into `body.curves`.
    pub curve_key: Option<usize>,
    pub tolerance: f64,
    pub sense: XtSense,
}

#[derive(Debug, Clone)]
pub struct XtVertex {
    pub node_id: i64,
    /// Key into `body.points`.
    pub point_key: usize,
    pub tolerance: f64,
}

// ── Geometry — Surfaces ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum XtSurface {
    Plane(XtPlane),
    Cylinder(XtCylinder),
    Cone(XtCone),
    Sphere(XtSphere),
    Torus(XtTorus),
    BSpline(XtBSplineSurface),
    Offset(XtOffsetSurface),
    Swept(XtSweptSurface),
    Spun(XtSpunSurface),
    Blend(XtBlendSurface),
}

/// Orthonormal basis set: location + axis + ref_direction.
/// Y is derived as axis × ref_direction.
#[derive(Debug, Clone, Copy)]
pub struct XtBasisSet {
    pub location: [f64; 3],
    pub axis: [f64; 3],
    pub ref_direction: [f64; 3],
}

/// R(u,v) = P + u·X + v·Y
#[derive(Debug, Clone)]
pub struct XtPlane {
    pub basis: XtBasisSet,
}

/// R(u,v) = P + r·X·cos(u) + r·Y·sin(u) + v·A
#[derive(Debug, Clone)]
pub struct XtCylinder {
    pub basis: XtBasisSet,
    pub radius: f64,
}

/// R(u,v) = P + v·A + (X·cos(u) + Y·sin(u))·(r + v·tan(a))
#[derive(Debug, Clone)]
pub struct XtCone {
    pub basis: XtBasisSet,
    pub radius: f64,
    pub semi_angle: f64,
}

/// R(u,v) = C + r·(X·cos(u)·cos(v) + Y·sin(u)·cos(v) + A·sin(v))
#[derive(Debug, Clone)]
pub struct XtSphere {
    pub basis: XtBasisSet,
    pub radius: f64,
}

/// R(u,v) = C + (X·cos(u) + Y·sin(u))·(a + b·cos(v)) + b·A·sin(v)
#[derive(Debug, Clone)]
pub struct XtTorus {
    pub basis: XtBasisSet,
    pub major_radius: f64,
    pub minor_radius: f64,
}

#[derive(Debug, Clone)]
pub struct XtBSplineSurface {
    pub u_degree: u32,
    pub v_degree: u32,
    pub n_u_vertices: usize,
    pub n_v_vertices: usize,
    /// Expanded knot vector (with repeated knots per multiplicity).
    pub u_knots: Vec<f64>,
    pub v_knots: Vec<f64>,
    /// Control points in row-major order: u varies fastest.
    pub poles: Vec<[f64; 3]>,
    /// Weights for rational NURBS. `None` for non-rational.
    pub weights: Option<Vec<f64>>,
    pub u_periodic: bool,
    pub v_periodic: bool,
    pub u_closed: bool,
    pub v_closed: bool,
}

#[derive(Debug, Clone)]
pub struct XtOffsetSurface {
    /// XT index of the base surface.
    pub base_surface_key: usize,
    pub offset_distance: f64,
}

#[derive(Debug, Clone)]
pub struct XtSweptSurface {
    /// XT index of the profile curve.
    pub profile_curve_key: usize,
    pub direction: [f64; 3],
}

#[derive(Debug, Clone)]
pub struct XtSpunSurface {
    /// XT index of the profile curve.
    pub profile_curve_key: usize,
    pub axis_point: [f64; 3],
    pub axis_direction: [f64; 3],
}

#[derive(Debug, Clone)]
pub struct XtBlendSurface {
    /// XT index of the spine curve.
    pub spine_curve_key: usize,
    /// XT indices of the two support surfaces.
    pub support_surface_keys: [usize; 2],
    /// Lower bound of the blend parameter range (from sch_13006 `range` field).
    pub range_start: f64,
}

// ── Geometry — Curves ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum XtCurve {
    Line(XtLine),
    Circle(XtCircle),
    Ellipse(XtEllipse),
    BSpline(XtBSplineCurve),
    Intersection(XtIntersectionCurve),
    SPCurve(XtSPCurve),
    Trimmed(XtTrimmedCurve),
}

/// R(t) = P + t·D
#[derive(Debug, Clone)]
pub struct XtLine {
    pub position: [f64; 3],
    pub direction: [f64; 3],
}

/// R(t) = C + r·X·cos(t) + r·Y·sin(t)
#[derive(Debug, Clone)]
pub struct XtCircle {
    pub basis: XtBasisSet,
    pub radius: f64,
}

/// R(t) = C + R1·X·cos(t) + R2·Y·sin(t)
#[derive(Debug, Clone)]
pub struct XtEllipse {
    pub basis: XtBasisSet,
    pub major_radius: f64,
    pub minor_radius: f64,
}

#[derive(Debug, Clone)]
pub struct XtBSplineCurve {
    pub degree: u32,
    /// Expanded knot vector.
    pub knots: Vec<f64>,
    pub poles: Vec<[f64; 3]>,
    /// `None` for non-rational.
    pub weights: Option<Vec<f64>>,
    pub periodic: bool,
    pub closed: bool,
}

#[derive(Debug, Clone)]
pub struct XtIntersectionCurve {
    /// XT indices of the two intersecting surfaces.
    pub surface_keys: [usize; 2],
    /// Polyline approximation points.
    pub approx_points: Vec<[f64; 3]>,
}

/// Surface-parameter curve: a 2D B-spline in (u,v) space on a surface.
#[derive(Debug, Clone)]
pub struct XtSPCurve {
    /// XT index of the host surface.
    pub surface_key: usize,
    /// The 2D B-spline curve in parameter space.
    pub curve_2d: XtBSplineCurve,
}

#[derive(Debug, Clone)]
pub struct XtTrimmedCurve {
    /// XT index of the basis curve.
    pub basis_curve_key: usize,
    pub t_start: f64,
    pub t_end: f64,
}
