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
    /// Hand back each body's boundary representation as soon as its mesh
    /// exists.
    ///
    /// Nothing downstream of the tessellator reads a `brep` — not the material
    /// resolver, not the UV projection, not either writer. On the pilot
    /// assembly the boundary representations come to 41.9 MB against 63.9 MB
    /// of meshes, and holding all of the first while building all of the
    /// second is what put the tail of a conversion at 105 MB above the read.
    /// Freed body by body, the two never both stand at full height.
    ///
    /// Off by default, because a `Scene` that has lost its exact geometry is a
    /// surprise to a caller who wanted to compare mesh against surface — which
    /// is what every diagnostic in this workspace does. The converters turn it
    /// on; they write a mesh and then have no further use for the surfaces it
    /// came from.
    pub release_brep: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            // On a circle the chord error is exactly 1 − cos(θ/2) of the
            // radius, so the angular limit alone fixes how round a round thing
            // looks — and on this assembly's 11,422 circular edges it is the
            // binding constraint at every radius the model uses, the linear
            // limit never reaching it. Measured: 20° leaves 1.52% of the
            // radius, which reads as flats on a boss and chunky gear teeth;
            // 8° leaves 0.24%, under a pixel at any framing that shows the
            // whole part, at 3.6× the triangles. The linear limit is set to
            // meet it — 0.04% of the model diagonal — so neither dominates.
            linear_deflection: 0.0004,
            angular_deflection: 8f64.to_radians(),
            relative: true,
            min_edge_segments: 1,
            max_depth: 12,
            interior_points: true,
            release_brep: false,
        }
    }
}

/// Options with the relative tolerance resolved against a concrete model size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Resolved {
    /// Absolute linear deflection, in scene units, for a feature the size of
    /// the whole model. Anything smaller is held to less than this — see
    /// [`Resolved::sag_for`].
    pub sag: f64,
    /// The model's own extent, and the fraction of a feature's size that the
    /// chord may depart from it. Together these let a feature be judged
    /// against itself rather than against the assembly it sits in.
    pub model: f64,
    pub relative: f64,
    pub angle: f64,
    pub min_edge_segments: usize,
    pub max_depth: u32,
    pub interior_points: bool,
}

impl Resolved {
    /// The chord tolerance for one feature, judged against its own size.
    ///
    /// A single tolerance for the whole model is right for the model and wrong
    /// for everything small in it. On the pilot assembly — 480 mm across, so
    /// 0.19 mm of sag — 1,217 faces are smaller than five times that, and 104
    /// are smaller than the tolerance itself: embossed lettering, the fillets
    /// around it, thread flanks. They cannot come out as anything but blobs,
    /// because the tolerance says they may be.
    ///
    /// So a feature is held to a fraction of *its own* extent, with the
    /// proportion between it and the model bounded either way: the bound stops
    /// a large face from being refined past what the model asks and a hair of
    /// a face from asking for infinity. This is the rule OpenCASCADE applies
    /// when its deflection is relative, and it is why a viewer built on it
    /// shows lettering that ours smears.
    ///
    /// Sized so that a feature as big as the model gets exactly [`sag`],
    /// which is where that number was measured.
    ///
    /// [`sag`]: Resolved::sag
    pub fn sag_for(&self, extent: f64) -> f64 {
        if !(self.relative > 0.0 && self.model > 0.0 && extent > 0.0 && extent.is_finite()) {
            return self.sag;
        }
        let proportion = (self.model / (2.0 * extent)).clamp(0.5, 2.0);
        // Refined against itself, but not without end. A feature a fiftieth
        // of the model's size would otherwise ask for a fiftieth of its
        // tolerance, and the triangles it costs buy nothing anyone can see:
        // an eighth of the model's sag is 0.024 mm here, which on a two
        // millimetre letter is a hundredth of it. Measured on the pilot
        // assembly, the floor halves the mesh — 2.6 M triangles to 1.3 M on
        // the Parasolid side — and leaves the lettering as sharp, while
        // taking non-manifold edges from 32 back to 16.
        (proportion * extent * self.relative).max(self.sag / 8.0)
    }

    /// A copy of these settings holding one feature to its own size.
    pub fn for_extent(&self, extent: f64) -> Resolved {
        Resolved {
            sag: self.sag_for(extent),
            ..*self
        }
    }
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
            model: if scale.is_finite() && scale > 0.0 { scale } else { 0.0 },
            // Doubled, because the rule below halves it again for a feature
            // the size of the model — which is where this number was tuned.
            relative: if self.relative { self.linear_deflection * 2.0 } else { 0.0 },
            angle: self.angular_deflection.clamp(1e-4, std::f64::consts::PI),
            min_edge_segments: self.min_edge_segments.max(1),
            max_depth: self.max_depth.clamp(1, 24),
            interior_points: self.interior_points,
        }
    }

    /// Preset for a quick preview: coarse, small, fast.
    /// Fast and visibly faceted: 35° leaves 4.6% of the radius as chord
    /// error, which is a silhouette you can count the sides of. For picking a
    /// part out of an assembly, not for looking at one.
    pub fn draft() -> Options {
        Options {
            linear_deflection: 0.01,
            angular_deflection: 35f64.to_radians(),
            ..Options::default()
        }
    }

    /// Preset for output that will be inspected closely.
    /// For close inspection: 4° leaves 0.06% of the radius, which stays
    /// smooth zoomed into a single feature, at eleven times the triangles of
    /// the default.
    pub fn fine() -> Options {
        Options {
            linear_deflection: 0.0001,
            angular_deflection: 4f64.to_radians(),
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
