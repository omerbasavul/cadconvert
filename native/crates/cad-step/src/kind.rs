//! Interning of the STEP entity keywords this reader understands.
//!
//! Only the keywords needed to recover B-Rep geometry, the assembly tree,
//! presentation styles and units are interned. Everything else becomes
//! [`Kind::Other`] and keeps its keyword span, so nothing is lost and no file
//! is rejected for containing entities we do not model — a STEP exporter is
//! free to write hundreds of PMI, tolerance and annotation entities we have no
//! reason to look at.

/// Longest modelled keyword, `GEOMETRICALLY_BOUNDED_SURFACE_SHAPE_REPRESENTATION`
/// at 54 bytes. The uppercase fallback buffer is sized from this; a keyword
/// longer than any we model cannot match one, so it short-circuits to
/// [`Kind::Other`] without touching the buffer.
const MAX_KEYWORD_LEN: usize = 64;

macro_rules! kinds {
    ($( $variant:ident => $text:literal ),* $(,)?) => {
        /// An interned STEP entity keyword.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Kind {
            $( $variant, )*
            /// A complex instance: `#N=(A(…)B(…));`.
            Complex,
            /// A keyword outside the modelled set.
            Other,
        }

        impl Kind {
            /// Intern a keyword. Case-insensitive, as Part 21 keywords are.
            ///
            /// The exact-match arm is a slice pattern, so rustc switches on
            /// length and then memcmps only the candidates of that length —
            /// which matters when it runs once per record on a file with half a
            /// million of them. Real exporters write keywords uppercase, so the
            /// lowercase fallback allocates nothing and is effectively never
            /// taken.
            pub fn intern(kw: &[u8]) -> Kind {
                match Kind::intern_exact(kw) {
                    Kind::Other if kw.len() <= MAX_KEYWORD_LEN
                        && kw.iter().any(u8::is_ascii_lowercase) =>
                    {
                        let mut upper = [0u8; MAX_KEYWORD_LEN];
                        let n = kw.len();
                        upper[..n].copy_from_slice(kw);
                        upper[..n].make_ascii_uppercase();
                        Kind::intern_exact(&upper[..n])
                    }
                    k => k,
                }
            }

            fn intern_exact(kw: &[u8]) -> Kind {
                match kw {
                    $( $text => Kind::$variant, )*
                    _ => Kind::Other,
                }
            }

            /// The canonical keyword text, or `""` for [`Kind::Other`].
            pub fn as_str(self) -> &'static str {
                match self {
                    $( Kind::$variant => match std::str::from_utf8($text) {
                        Ok(s) => s,
                        Err(_) => "",
                    }, )*
                    Kind::Complex => "(complex)",
                    Kind::Other => "",
                }
            }
        }
    };
}

kinds! {
    // -- geometry: points, directions, placements -------------------------
    CartesianPoint            => b"CARTESIAN_POINT",
    Direction                 => b"DIRECTION",
    Vector                    => b"VECTOR",
    Axis1Placement            => b"AXIS1_PLACEMENT",
    Axis2Placement2d          => b"AXIS2_PLACEMENT_2D",
    Axis2Placement3d          => b"AXIS2_PLACEMENT_3D",

    // -- geometry: curves --------------------------------------------------
    Line                      => b"LINE",
    Circle                    => b"CIRCLE",
    Ellipse                   => b"ELLIPSE",
    Hyperbola                 => b"HYPERBOLA",
    Parabola                  => b"PARABOLA",
    Polyline                  => b"POLYLINE",
    BSplineCurveWithKnots     => b"B_SPLINE_CURVE_WITH_KNOTS",
    BSplineCurve              => b"B_SPLINE_CURVE",
    BezierCurve               => b"BEZIER_CURVE",
    UniformCurve              => b"UNIFORM_CURVE",
    QuasiUniformCurve         => b"QUASI_UNIFORM_CURVE",
    RationalBSplineCurve      => b"RATIONAL_B_SPLINE_CURVE",
    TrimmedCurve              => b"TRIMMED_CURVE",
    CompositeCurve            => b"COMPOSITE_CURVE",
    CompositeCurveSegment     => b"COMPOSITE_CURVE_SEGMENT",
    OffsetCurve3d             => b"OFFSET_CURVE_3D",
    CurveReplica              => b"CURVE_REPLICA",
    SurfaceCurve              => b"SURFACE_CURVE",
    SeamCurve                 => b"SEAM_CURVE",
    IntersectionCurve         => b"INTERSECTION_CURVE",
    Pcurve                    => b"PCURVE",
    DefinitionalRepresentation => b"DEFINITIONAL_REPRESENTATION",

    // -- geometry: surfaces ------------------------------------------------
    Plane                     => b"PLANE",
    CylindricalSurface        => b"CYLINDRICAL_SURFACE",
    ConicalSurface            => b"CONICAL_SURFACE",
    SphericalSurface          => b"SPHERICAL_SURFACE",
    ToroidalSurface           => b"TOROIDAL_SURFACE",
    DegenerateToroidalSurface => b"DEGENERATE_TOROIDAL_SURFACE",
    BSplineSurfaceWithKnots   => b"B_SPLINE_SURFACE_WITH_KNOTS",
    BSplineSurface            => b"B_SPLINE_SURFACE",
    BezierSurface             => b"BEZIER_SURFACE",
    UniformSurface            => b"UNIFORM_SURFACE",
    QuasiUniformSurface       => b"QUASI_UNIFORM_SURFACE",
    RationalBSplineSurface    => b"RATIONAL_B_SPLINE_SURFACE",
    SurfaceOfRevolution       => b"SURFACE_OF_REVOLUTION",
    SurfaceOfLinearExtrusion  => b"SURFACE_OF_LINEAR_EXTRUSION",
    OffsetSurface             => b"OFFSET_SURFACE",
    RectangularTrimmedSurface => b"RECTANGULAR_TRIMMED_SURFACE",
    SurfaceReplica            => b"SURFACE_REPLICA",

    // -- topology ----------------------------------------------------------
    VertexPoint               => b"VERTEX_POINT",
    VertexLoop                => b"VERTEX_LOOP",
    EdgeCurve                 => b"EDGE_CURVE",
    OrientedEdge              => b"ORIENTED_EDGE",
    EdgeLoop                  => b"EDGE_LOOP",
    PolyLoop                  => b"POLY_LOOP",
    FaceBound                 => b"FACE_BOUND",
    FaceOuterBound            => b"FACE_OUTER_BOUND",
    AdvancedFace              => b"ADVANCED_FACE",
    FaceSurface               => b"FACE_SURFACE",
    OrientedFace              => b"ORIENTED_FACE",
    OpenShell                 => b"OPEN_SHELL",
    ClosedShell               => b"CLOSED_SHELL",
    OrientedClosedShell       => b"ORIENTED_CLOSED_SHELL",
    ManifoldSolidBrep         => b"MANIFOLD_SOLID_BREP",
    BrepWithVoids             => b"BREP_WITH_VOIDS",
    ShellBasedSurfaceModel    => b"SHELL_BASED_SURFACE_MODEL",
    GeometricCurveSet         => b"GEOMETRIC_CURVE_SET",

    // -- product structure and assembly ------------------------------------
    Product                   => b"PRODUCT",
    ProductDefinition         => b"PRODUCT_DEFINITION",
    ProductDefinitionFormation => b"PRODUCT_DEFINITION_FORMATION",
    ProductDefinitionFormationWithSpecifiedSource
                              => b"PRODUCT_DEFINITION_FORMATION_WITH_SPECIFIED_SOURCE",
    ProductDefinitionShape    => b"PRODUCT_DEFINITION_SHAPE",
    ProductRelatedProductCategory => b"PRODUCT_RELATED_PRODUCT_CATEGORY",
    ShapeDefinitionRepresentation => b"SHAPE_DEFINITION_REPRESENTATION",
    ShapeRepresentation       => b"SHAPE_REPRESENTATION",
    AdvancedBrepShapeRepresentation => b"ADVANCED_BREP_SHAPE_REPRESENTATION",
    ManifoldSurfaceShapeRepresentation => b"MANIFOLD_SURFACE_SHAPE_REPRESENTATION",
    GeometricallyBoundedSurfaceShapeRepresentation
                              => b"GEOMETRICALLY_BOUNDED_SURFACE_SHAPE_REPRESENTATION",
    ShapeRepresentationRelationship => b"SHAPE_REPRESENTATION_RELATIONSHIP",
    NextAssemblyUsageOccurrence => b"NEXT_ASSEMBLY_USAGE_OCCURRENCE",
    ContextDependentShapeRepresentation => b"CONTEXT_DEPENDENT_SHAPE_REPRESENTATION",
    ItemDefinedTransformation => b"ITEM_DEFINED_TRANSFORMATION",
    RepresentationRelationshipWithTransformation
                              => b"REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION",
    RepresentationRelationship => b"REPRESENTATION_RELATIONSHIP",
    MappedItem                => b"MAPPED_ITEM",
    RepresentationMap         => b"REPRESENTATION_MAP",
    Representation            => b"REPRESENTATION",

    // -- presentation ------------------------------------------------------
    StyledItem                => b"STYLED_ITEM",
    OverRidingStyledItem      => b"OVER_RIDING_STYLED_ITEM",
    PresentationStyleAssignment => b"PRESENTATION_STYLE_ASSIGNMENT",
    PresentationStyleByContext => b"PRESENTATION_STYLE_BY_CONTEXT",
    SurfaceStyleUsage         => b"SURFACE_STYLE_USAGE",
    SurfaceSideStyle          => b"SURFACE_SIDE_STYLE",
    SurfaceStyleFillArea      => b"SURFACE_STYLE_FILL_AREA",
    SurfaceStyleRendering     => b"SURFACE_STYLE_RENDERING",
    SurfaceStyleRenderingWithProperties => b"SURFACE_STYLE_RENDERING_WITH_PROPERTIES",
    SurfaceStyleTransparent   => b"SURFACE_STYLE_TRANSPARENT",
    FillAreaStyle             => b"FILL_AREA_STYLE",
    FillAreaStyleColour       => b"FILL_AREA_STYLE_COLOUR",
    ColourRgb                 => b"COLOUR_RGB",
    DraughtingPreDefinedColour => b"DRAUGHTING_PRE_DEFINED_COLOUR",
    CurveStyle                => b"CURVE_STYLE",
    PresentationLayerAssignment => b"PRESENTATION_LAYER_ASSIGNMENT",
    MechanicalDesignGeometricPresentationRepresentation
                              => b"MECHANICAL_DESIGN_GEOMETRIC_PRESENTATION_REPRESENTATION",

    ProductCategory           => b"PRODUCT_CATEGORY",
    ProductContext            => b"PRODUCT_CONTEXT",
    ProductDefinitionContext  => b"PRODUCT_DEFINITION_CONTEXT",
    ApplicationContext        => b"APPLICATION_CONTEXT",
    ApplicationProtocolDefinition => b"APPLICATION_PROTOCOL_DEFINITION",

    // -- units and context -------------------------------------------------
    SiUnit                    => b"SI_UNIT",
    NamedUnit                 => b"NAMED_UNIT",
    ConversionBasedUnit       => b"CONVERSION_BASED_UNIT",
    LengthMeasureWithUnit     => b"LENGTH_MEASURE_WITH_UNIT",
    PlaneAngleMeasureWithUnit => b"PLANE_ANGLE_MEASURE_WITH_UNIT",
    UncertaintyMeasureWithUnit => b"UNCERTAINTY_MEASURE_WITH_UNIT",
    DimensionalExponents      => b"DIMENSIONAL_EXPONENTS",
    GeometricRepresentationContext => b"GEOMETRIC_REPRESENTATION_CONTEXT",
    RepresentationContext     => b"REPRESENTATION_CONTEXT",
    GlobalUnitAssignedContext => b"GLOBAL_UNIT_ASSIGNED_CONTEXT",
    GlobalUncertaintyAssignedContext => b"GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT",
    LengthUnit                => b"LENGTH_UNIT",
    PlaneAngleUnit            => b"PLANE_ANGLE_UNIT",
    SolidAngleUnit            => b"SOLID_ANGLE_UNIT",

    // -- properties, where material text can hide --------------------------
    PropertyDefinition        => b"PROPERTY_DEFINITION",
    PropertyDefinitionRepresentation => b"PROPERTY_DEFINITION_REPRESENTATION",
    DescriptiveRepresentationItem => b"DESCRIPTIVE_REPRESENTATION_ITEM",
    ValueRepresentationItem   => b"VALUE_REPRESENTATION_ITEM",
    MaterialDesignation       => b"MATERIAL_DESIGNATION",
    GeneralProperty           => b"GENERAL_PROPERTY",
}

impl Kind {
    /// True for the surface entities a face can be built on.
    pub fn is_surface(self) -> bool {
        matches!(
            self,
            Kind::Plane
                | Kind::CylindricalSurface
                | Kind::ConicalSurface
                | Kind::SphericalSurface
                | Kind::ToroidalSurface
                | Kind::DegenerateToroidalSurface
                | Kind::BSplineSurfaceWithKnots
                | Kind::BSplineSurface
                | Kind::BezierSurface
                | Kind::UniformSurface
                | Kind::QuasiUniformSurface
                | Kind::RationalBSplineSurface
                | Kind::SurfaceOfRevolution
                | Kind::SurfaceOfLinearExtrusion
                | Kind::OffsetSurface
                | Kind::RectangularTrimmedSurface
                | Kind::SurfaceReplica
        )
    }

    /// True for the curve entities an edge can be built on.
    pub fn is_curve(self) -> bool {
        matches!(
            self,
            Kind::Line
                | Kind::Circle
                | Kind::Ellipse
                | Kind::Hyperbola
                | Kind::Parabola
                | Kind::Polyline
                | Kind::BSplineCurveWithKnots
                | Kind::BSplineCurve
                | Kind::BezierCurve
                | Kind::UniformCurve
                | Kind::QuasiUniformCurve
                | Kind::RationalBSplineCurve
                | Kind::TrimmedCurve
                | Kind::CompositeCurve
                | Kind::OffsetCurve3d
                | Kind::CurveReplica
                | Kind::SurfaceCurve
                | Kind::SeamCurve
                | Kind::IntersectionCurve
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interns_known_keywords() {
        assert_eq!(Kind::intern(b"ADVANCED_FACE"), Kind::AdvancedFace);
        assert_eq!(Kind::intern(b"CARTESIAN_POINT"), Kind::CartesianPoint);
        assert_eq!(Kind::intern(b"COLOUR_RGB"), Kind::ColourRgb);
        assert_eq!(
            Kind::intern(b"DEGENERATE_TOROIDAL_SURFACE"),
            Kind::DegenerateToroidalSurface
        );
    }

    #[test]
    fn interning_is_case_insensitive() {
        assert_eq!(Kind::intern(b"advanced_face"), Kind::AdvancedFace);
        assert_eq!(Kind::intern(b"Plane"), Kind::Plane);
    }

    #[test]
    fn unknown_keywords_fall_through() {
        assert_eq!(Kind::intern(b"DIMENSIONAL_LOCATION"), Kind::Other);
        assert_eq!(Kind::intern(b""), Kind::Other);
    }

    #[test]
    fn similar_length_keywords_do_not_collide() {
        // Both 5 bytes; the length dispatch must not stop at the first arm.
        assert_eq!(Kind::intern(b"PLANE"), Kind::Plane);
        assert_eq!(Kind::intern(b"LINE"), Kind::Line);
        // Prefixes must not match their longer relatives.
        assert_eq!(Kind::intern(b"B_SPLINE_CURVE"), Kind::BSplineCurve);
        assert_eq!(
            Kind::intern(b"B_SPLINE_CURVE_WITH_KNOTS"),
            Kind::BSplineCurveWithKnots
        );
    }

    #[test]
    fn round_trips_through_as_str() {
        for kw in [
            &b"ADVANCED_FACE"[..],
            b"CYLINDRICAL_SURFACE",
            b"NEXT_ASSEMBLY_USAGE_OCCURRENCE",
        ] {
            let k = Kind::intern(kw);
            assert_ne!(k, Kind::Other);
            assert_eq!(k.as_str().as_bytes(), kw);
        }
    }

    #[test]
    fn surface_and_curve_predicates_agree_with_interning() {
        assert!(Kind::intern(b"TOROIDAL_SURFACE").is_surface());
        assert!(!Kind::intern(b"TOROIDAL_SURFACE").is_curve());
        assert!(Kind::intern(b"B_SPLINE_CURVE_WITH_KNOTS").is_curve());
        assert!(!Kind::intern(b"ADVANCED_FACE").is_surface());
    }
}
