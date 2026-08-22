//! The format-neutral intermediate representation every reader targets and
//! every writer consumes.
//!
//! Readers (`cad-step`, `xt-parser`, …) lower a file into a [`Scene`] of
//! [`Solid`] boundary representations. The tessellator fills each geometry's
//! [`Mesh`]. Writers (OBJ, GLB, USDZ) read only the [`Scene`]. Nothing in this
//! crate knows about any file format, which is what keeps the *n* readers and
//! *m* writers from becoming *n × m* converters.
//!
//! Conventions the whole pipeline relies on:
//!
//! * Coordinates are millimetres, right-handed, Z up — mechanical CAD's
//!   convention, and the one both source formats already use. Writers convert.
//! * Triangles wind counter-clockwise seen from outside the solid.
//! * Colours are **linear** RGB. Readers convert from whatever their format
//!   stores; writers emit linear, because glTF and USD both want it.

#![forbid(unsafe_code)]

pub mod brep;
pub mod eval;
pub mod image;
pub mod material;
pub mod material_resolve;
pub mod math;
pub mod mesh;
pub mod p2m;
pub mod scene;
pub mod sldmat;

pub use brep::{
    Bound, BodyType, Curve, Curve2, Edge, EdgeId, Face, FaceId, HalfEdge, NurbsCurve, NurbsCurve2,
    NurbsSurface, Shell, Solid, Surface, SurfaceId, CurveId, VertexId,
};
pub use material::{Material, MaterialClass, MaterialSource};
pub use material_resolve::{ColourEvidence, MaterialResolver, MaterialTable};
pub use math::{Aabb, Frame, Interval, Transform, Vec2, Vec3};
pub use mesh::{Mesh, MeshPart};
pub use scene::{Geometry, GeometryId, Instance, MaterialId, Meta, Node, NodeId, Scene, Unit};
