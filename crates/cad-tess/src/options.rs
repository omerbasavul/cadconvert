//! Tessellation tolerances.

/// How finely to tessellate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// Maximum distance between the mesh and the true surface.
    ///
    /// In scene units when [`Options::relative`] is false, or as a fraction of
    /// the model's bounding-box diagonal when it is true.
    pub linear_deflection: f64,
    /// Maximum angle, in radians, between adjacent facet normals.
    ///
    /// This is what keeps a 1 mm hole from becoming a triangle: a linear
    /// tolerance scales with the feature, so on a small radius it is satisfied
    /// by almost no subdivision at all.
    pub angular_deflection: f64,
    /// Interpret `linear_deflection` as a fraction of the model size.
    pub relative: bool,
    /// Lower bound on the segments an edge is split into.
    pub min_edge_segments: usize,
    /// Cap on recursive subdivision, so a pathological curve cannot run away.
    pub max_depth: u32,
    /// Add interior points to curved faces rather than triangulating the
    /// boundary alone.
    ///
    /// Without this a cylinder's lateral face becomes a ribbon of long thin
    /// triangles spanning its whole height — geometrically inside tolerance,
    /// and visibly wrong once it is shaded.
    pub interior_points: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            // 0.1% of the model diagonal, which is the setting most CAD
            // viewers ship as "fine" and is invisible at any normal zoom.
            linear_deflection: 0.001,
            angular_deflection: 20f64.to_radians(),
            relative: true,
            min_edge_segments: 1,
            max_depth: 12,
            interior_points: true,
        }
    }
}

/// Options with the relative tolerance resolved against a concrete model size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Resolved {
    /// Absolute linear deflection, in scene units.
    pub sag: f64,
    pub angle: f64,
    pub min_edge_segments: usize,
    pub max_depth: u32,
    pub interior_points: bool,
}

impl Options {
    /// Resolve against a model whose bounding-box diagonal is `scale`.
    ///
    /// A scale of zero — an empty or single-point model — falls back to the
    /// absolute interpretation, because a fraction of nothing is nothing and
    /// would ask for infinite subdivision.
    pub fn resolve(&self, scale: f64) -> Resolved {
        let sag = if self.relative && scale.is_finite() && scale > 0.0 {
            self.linear_deflection * scale
        } else {
            self.linear_deflection
        };
        Resolved {
            sag: sag.max(f64::MIN_POSITIVE),
            angle: self.angular_deflection.clamp(1e-4, std::f64::consts::PI),
            min_edge_segments: self.min_edge_segments.max(1),
            max_depth: self.max_depth.clamp(1, 24),
            interior_points: self.interior_points,
        }
    }

    /// Preset for a quick preview: coarse, small, fast.
    pub fn draft() -> Options {
        Options {
            linear_deflection: 0.01,
            angular_deflection: 35f64.to_radians(),
            ..Options::default()
        }
    }

    /// Preset for output that will be inspected closely.
    pub fn fine() -> Options {
        Options {
            linear_deflection: 0.0002,
            angular_deflection: 10f64.to_radians(),
            ..Options::default()
        }
    }
}

impl Resolved {
    /// The angular step that keeps a circle of radius `r` within the sag
    /// tolerance, also honouring the angular limit.
    ///
    /// A chord subtending angle θ on radius `r` departs from the arc by
    /// `r(1 − cos(θ/2))`; solving for θ gives the sag-limited step.
    pub fn angle_step_for_radius(&self, r: f64) -> f64 {
        let r = r.abs();
        let by_sag = if r > self.sag {
            2.0 * (1.0 - self.sag / r).clamp(-1.0, 1.0).acos()
        } else {
            // The whole feature is smaller than the tolerance; the angular
            // limit is all that is left to respect.
            self.angle
        };
        by_sag.min(self.angle).max(1e-3)
    }

    /// Segments needed to sweep `total` radians on radius `r`.
    pub fn segments_for_arc(&self, r: f64, total: f64) -> usize {
        let step = self.angle_step_for_radius(r);
        ((total.abs() / step).ceil() as usize)
            .max(self.min_edge_segments)
            .max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_tolerance_scales_with_the_model() {
        let o = Options {
            linear_deflection: 0.001,
            relative: true,
            ..Options::default()
        };
        assert_eq!(o.resolve(1000.0).sag, 1.0);
        assert_eq!(o.resolve(10.0).sag, 0.01);
    }

    #[test]
    fn absolute_tolerance_ignores_the_model_size() {
        let o = Options {
            linear_deflection: 0.05,
            relative: false,
            ..Options::default()
        };
        assert_eq!(o.resolve(1000.0).sag, 0.05);
        assert_eq!(o.resolve(1.0).sag, 0.05);
    }

    #[test]
    fn a_zero_scale_model_does_not_ask_for_infinite_detail() {
        let o = Options::default();
        assert!(o.resolve(0.0).sag > 0.0);
        assert!(o.resolve(f64::NAN).sag > 0.0);
    }

    #[test]
    fn the_sag_limited_angle_matches_the_chord_formula() {
        let r = Options {
            linear_deflection: 0.1,
            relative: false,
            angular_deflection: std::f64::consts::PI,
            ..Options::default()
        }
        .resolve(1.0);
        // On radius 10 with sag 0.1, the true departure at the chosen step
        // must be at or just under the tolerance.
        let step = r.angle_step_for_radius(10.0);
        let departure = 10.0 * (1.0 - (step / 2.0).cos());
        assert!(departure <= 0.1 + 1e-9, "departure {departure}");
        assert!(departure > 0.09, "step {step} is needlessly small");
    }

    #[test]
    fn the_angular_limit_bounds_a_large_radius() {
        let r = Options {
            linear_deflection: 10.0,
            relative: false,
            angular_deflection: 15f64.to_radians(),
            ..Options::default()
        }
        .resolve(1.0);
        // A generous sag on a small radius would allow a huge step; the
        // angular limit is what stops a hole becoming a triangle.
        assert!(r.angle_step_for_radius(1.0) <= 15f64.to_radians() + 1e-12);
    }

    #[test]
    fn a_feature_smaller_than_the_tolerance_still_gets_segments() {
        let r = Options {
            linear_deflection: 5.0,
            relative: false,
            ..Options::default()
        }
        .resolve(1.0);
        assert!(r.segments_for_arc(0.1, std::f64::consts::TAU) >= 1);
        assert!(r.angle_step_for_radius(0.1).is_finite());
    }

    #[test]
    fn a_full_circle_needs_more_segments_than_a_quarter() {
        let r = Options::default().resolve(100.0);
        let full = r.segments_for_arc(10.0, std::f64::consts::TAU);
        let quarter = r.segments_for_arc(10.0, std::f64::consts::FRAC_PI_2);
        assert!(full > quarter * 3);
    }
}
