//! Length and angle units, and the model tolerance.
//!
//! Every coordinate in the file is expressed in the context's length unit, so
//! nothing downstream can be trusted until this is resolved. Solid Edge writes
//! millimetres, SolidWorks writes millimetres, older AP203 files often write
//! inches through a `CONVERSION_BASED_UNIT` — and the difference is a model
//! that is 25.4× too small.

use crate::error::Result;
use crate::kind::Kind;
use crate::{Entity, StepFile};

/// The unit system and tolerance a representation context declares.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Units {
    /// Multiply a file coordinate by this to get millimetres.
    pub length_to_mm: f64,
    /// Multiply a file angle by this to get radians.
    pub angle_to_rad: f64,
    /// The context's `UNCERTAINTY_MEASURE_WITH_UNIT`, in file length units.
    ///
    /// This is the modelling tolerance the exporter guaranteed: two points
    /// closer than this are the same point. It is the floor for any
    /// tessellation tolerance — asking for finer output than the source
    /// geometry is accurate to only produces noise.
    pub uncertainty: f64,
    /// Whether a length unit was actually found, as opposed to assumed.
    pub resolved: bool,
}

impl Default for Units {
    fn default() -> Self {
        // Millimetres, the near-universal choice of mechanical CAD exporters,
        // with a 1 µm tolerance. `resolved` records that this is a guess.
        Units {
            length_to_mm: 1.0,
            angle_to_rad: 1.0,
            uncertainty: 1e-3,
            resolved: false,
        }
    }
}

/// Resolve the units of the file's geometric representation context.
///
/// A file may declare several contexts; in practice every shape shares one, so
/// the first context carrying a length unit wins. When contexts disagree the
/// caller can resolve per-context with [`units_of_context`].
pub fn resolve(file: &StepFile) -> Result<Units> {
    for e in file.entities() {
        if context_dimension(file, e)?.is_none() {
            continue;
        }
        let u = units_of_context(file, e)?;
        if u.resolved {
            return Ok(u);
        }
    }
    Ok(Units::default())
}

/// The `coordinate_space_dimension` of a representation context, if `e` is one.
///
/// A geometric context is nearly always a complex instance bundling
/// `GEOMETRIC_REPRESENTATION_CONTEXT` with the unit and uncertainty contexts.
fn context_dimension(file: &StepFile, e: &Entity) -> Result<Option<i64>> {
    if e.kind == Kind::GeometricRepresentationContext {
        return Ok(file.args_of(e).next_i64().ok());
    }
    if e.kind == Kind::Complex
        && let Some(mut a) = file.complex_part(e, Kind::GeometricRepresentationContext)?
    {
        return Ok(a.next_i64().ok());
    }
    Ok(None)
}

/// Resolve the units declared by one representation context entity.
pub fn units_of_context(file: &StepFile, ctx: &Entity) -> Result<Units> {
    let mut units = Units::default();

    let mut unit_refs = Vec::new();
    if let Some(mut a) = global_unit_part(file, ctx)? {
        a.next_ref_list(&mut unit_refs)?;
    }

    for &u in &unit_refs {
        let Some(ue) = file.get(u) else { continue };
        match unit_role(file, ue)? {
            Some(Role::Length) => {
                if let Some(f) = length_factor_to_mm(file, ue)? {
                    units.length_to_mm = f;
                    units.resolved = true;
                }
            }
            Some(Role::PlaneAngle) => {
                if let Some(f) = angle_factor_to_rad(file, ue)? {
                    units.angle_to_rad = f;
                }
            }
            _ => {}
        }
    }

    if let Some(mut a) = uncertainty_part(file, ctx)? {
        let mut refs = Vec::new();
        a.next_ref_list(&mut refs)?;
        for &r in &refs {
            if file.kind_of(r) == Kind::UncertaintyMeasureWithUnit
                && let Ok(mut ua) = file.args(r)
                && let Ok(v) = ua.next_measure_f64()
            {
                units.uncertainty = v;
                break;
            }
        }
    }

    Ok(units)
}

fn global_unit_part<'a>(file: &'a StepFile, ctx: &Entity) -> Result<Option<crate::Args<'a>>> {
    if ctx.kind == Kind::GlobalUnitAssignedContext {
        return Ok(Some(file.args_of(ctx)));
    }
    file.complex_part(ctx, Kind::GlobalUnitAssignedContext)
}

fn uncertainty_part<'a>(file: &'a StepFile, ctx: &Entity) -> Result<Option<crate::Args<'a>>> {
    if ctx.kind == Kind::GlobalUncertaintyAssignedContext {
        return Ok(Some(file.args_of(ctx)));
    }
    file.complex_part(ctx, Kind::GlobalUncertaintyAssignedContext)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Length,
    PlaneAngle,
    SolidAngle,
}

/// What a unit entity measures.
///
/// A unit is a complex instance whose parts name both the family
/// (`LENGTH_UNIT`) and the definition (`SI_UNIT` or `CONVERSION_BASED_UNIT`).
fn unit_role(file: &StepFile, e: &Entity) -> Result<Option<Role>> {
    if e.kind == Kind::Complex {
        for (k, _) in file.complex_parts(e)? {
            match k {
                Kind::LengthUnit => return Ok(Some(Role::Length)),
                Kind::PlaneAngleUnit => return Ok(Some(Role::PlaneAngle)),
                Kind::SolidAngleUnit => return Ok(Some(Role::SolidAngle)),
                _ => {}
            }
        }
        return Ok(None);
    }
    Ok(match e.kind {
        Kind::LengthUnit => Some(Role::Length),
        Kind::PlaneAngleUnit => Some(Role::PlaneAngle),
        Kind::SolidAngleUnit => Some(Role::SolidAngle),
        _ => None,
    })
}

/// Factor turning this length unit into millimetres.
fn length_factor_to_mm(file: &StepFile, e: &Entity) -> Result<Option<f64>> {
    if let Some(mut si) = si_part(file, e)? {
        let prefix = si.next_enum().ok();
        let name = si.next_enum().unwrap_or("METRE");
        if !name.eq_ignore_ascii_case("METRE") {
            return Ok(None);
        }
        // A bare `.METRE.` is 1000 mm; `.MILLI.` scales that.
        return Ok(Some(1000.0 * si_prefix_factor(prefix)));
    }

    if let Some(mut cbu) = conversion_part(file, e)? {
        cbu.skip()?; // name
        let factor = cbu.next_ref()?;
        return Ok(measure_in_mm(file, factor)?);
    }

    Ok(None)
}

/// Factor turning this plane-angle unit into radians.
fn angle_factor_to_rad(file: &StepFile, e: &Entity) -> Result<Option<f64>> {
    if let Some(mut si) = si_part(file, e)? {
        let prefix = si.next_enum().ok();
        let name = si.next_enum().unwrap_or("RADIAN");
        if !name.eq_ignore_ascii_case("RADIAN") {
            return Ok(None);
        }
        return Ok(Some(si_prefix_factor(prefix)));
    }

    if let Some(mut cbu) = conversion_part(file, e)? {
        cbu.skip()?; // name
        let factor = cbu.next_ref()?;
        // `PLANE_ANGLE_MEASURE_WITH_UNIT(PLANE_ANGLE_MEASURE(0.0174…), #radian)`
        if let Ok(mut m) = file.args(factor)
            && let Ok(v) = m.next_measure_f64()
        {
            return Ok(Some(v));
        }
    }

    Ok(None)
}

fn si_part<'a>(file: &'a StepFile, e: &Entity) -> Result<Option<crate::Args<'a>>> {
    if e.kind == Kind::SiUnit {
        return Ok(Some(file.args_of(e)));
    }
    file.complex_part(e, Kind::SiUnit)
}

fn conversion_part<'a>(file: &'a StepFile, e: &Entity) -> Result<Option<crate::Args<'a>>> {
    if e.kind == Kind::ConversionBasedUnit {
        return Ok(Some(file.args_of(e)));
    }
    file.complex_part(e, Kind::ConversionBasedUnit)
}

/// A `LENGTH_MEASURE_WITH_UNIT(LENGTH_MEASURE(25.4), #mm)` resolved to mm.
fn measure_in_mm(file: &StepFile, id: u32) -> Result<Option<f64>> {
    let Some(e) = file.get(id) else {
        return Ok(None);
    };
    let mut a = if e.kind == Kind::Complex {
        match file.complex_part(e, Kind::LengthMeasureWithUnit)? {
            Some(a) => a,
            None => return Ok(None),
        }
    } else {
        file.args_of(e)
    };
    let Ok(value) = a.next_measure_f64() else {
        return Ok(None);
    };
    let Ok(base) = a.next_ref() else {
        return Ok(None);
    };
    let Some(base_e) = file.get(base) else {
        return Ok(None);
    };
    // The base is itself a length unit, so recursion terminates at an SI unit.
    Ok(length_factor_to_mm(file, base_e)?.map(|f| value * f))
}

/// The multiplier for an SI prefix enumeration; `None` and `$` mean unity.
fn si_prefix_factor(prefix: Option<&str>) -> f64 {
    match prefix {
        Some(p) => match p.to_ascii_uppercase().as_str() {
            "EXA" => 1e18,
            "PETA" => 1e15,
            "TERA" => 1e12,
            "GIGA" => 1e9,
            "MEGA" => 1e6,
            "KILO" => 1e3,
            "HECTO" => 1e2,
            "DECA" => 1e1,
            "DECI" => 1e-1,
            "CENTI" => 1e-2,
            "MILLI" => 1e-3,
            "MICRO" => 1e-6,
            "NANO" => 1e-9,
            "PICO" => 1e-12,
            "FEMTO" => 1e-15,
            "ATTO" => 1e-18,
            _ => 1.0,
        },
        None => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(data: &str) -> StepFile {
        let src = format!("ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n{data}\nENDSEC;\nEND-ISO-10303-21;\n");
        StepFile::from_bytes(src.into_bytes()).unwrap()
    }

    const MILLIMETRE_CONTEXT: &str = "\
#1=(NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.)LENGTH_UNIT());
#2=(NAMED_UNIT(*)SI_UNIT($,.RADIAN.)PLANE_ANGLE_UNIT());
#3=(NAMED_UNIT(*)SI_UNIT($,.STERADIAN.)SOLID_ANGLE_UNIT());
#4=UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-5),#1,'closure','');
#5=(GEOMETRIC_REPRESENTATION_CONTEXT(3)GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#4))\
GLOBAL_UNIT_ASSIGNED_CONTEXT((#1,#2,#3))REPRESENTATION_CONTEXT('',''));";

    #[test]
    fn resolves_millimetres() {
        let f = parse(MILLIMETRE_CONTEXT);
        let u = resolve(&f).unwrap();
        assert!(u.resolved);
        assert_eq!(u.length_to_mm, 1.0);
        assert_eq!(u.angle_to_rad, 1.0);
        assert!((u.uncertainty - 1e-5).abs() < 1e-18);
    }

    #[test]
    fn resolves_bare_metres_as_a_thousand_millimetres() {
        let f = parse(
            "#1=(NAMED_UNIT(*)SI_UNIT($,.METRE.)LENGTH_UNIT());\n\
             #5=(GEOMETRIC_REPRESENTATION_CONTEXT(3)GLOBAL_UNIT_ASSIGNED_CONTEXT((#1))\
             REPRESENTATION_CONTEXT('',''));",
        );
        assert_eq!(resolve(&f).unwrap().length_to_mm, 1000.0);
    }

    #[test]
    fn resolves_inches_through_a_conversion_unit() {
        let f = parse(
            "#1=(NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.)LENGTH_UNIT());\n\
             #2=LENGTH_MEASURE_WITH_UNIT(LENGTH_MEASURE(25.4),#1);\n\
             #3=(CONVERSION_BASED_UNIT('INCH',#2)LENGTH_UNIT()NAMED_UNIT(*));\n\
             #5=(GEOMETRIC_REPRESENTATION_CONTEXT(3)GLOBAL_UNIT_ASSIGNED_CONTEXT((#3))\
             REPRESENTATION_CONTEXT('',''));",
        );
        let u = resolve(&f).unwrap();
        assert!(u.resolved);
        assert_eq!(u.length_to_mm, 25.4);
    }

    #[test]
    fn resolves_degrees_through_a_conversion_unit() {
        let f = parse(
            "#1=(NAMED_UNIT(*)SI_UNIT($,.RADIAN.)PLANE_ANGLE_UNIT());\n\
             #2=PLANE_ANGLE_MEASURE_WITH_UNIT(PLANE_ANGLE_MEASURE(0.01745329251994328),#1);\n\
             #3=(CONVERSION_BASED_UNIT('DEGREE',#2)PLANE_ANGLE_UNIT()NAMED_UNIT(*));\n\
             #4=(NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.)LENGTH_UNIT());\n\
             #5=(GEOMETRIC_REPRESENTATION_CONTEXT(3)GLOBAL_UNIT_ASSIGNED_CONTEXT((#4,#3))\
             REPRESENTATION_CONTEXT('',''));",
        );
        let u = resolve(&f).unwrap();
        assert!((u.angle_to_rad - 0.01745329251994328).abs() < 1e-15);
    }

    #[test]
    fn a_file_with_no_context_falls_back_to_millimetres_and_says_so() {
        let f = parse("#1=CARTESIAN_POINT('',(0.,0.,0.));");
        let u = resolve(&f).unwrap();
        assert!(!u.resolved);
        assert_eq!(u.length_to_mm, 1.0);
    }
}
