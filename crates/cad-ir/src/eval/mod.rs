//! Evaluating the IR's geometry: points, derivatives, normals and inversion.
//!
//! Readers need this to place an edge's end points on its curve; the
//! tessellator needs it for everything. Keeping it in the IR crate rather than
//! either side means both are provably evaluating the same geometry.

pub mod nurbs;
