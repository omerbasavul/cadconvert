//! The C ABI.
//!
//! Everything else in this workspace forbids `unsafe`. This crate cannot: a
//! foreign caller hands over raw pointers and there is no way to read them
//! safely. What it can do is keep the unsafe surface to the four functions
//! below, validate every pointer before it is read, and never let a panic
//! cross the boundary — unwinding into C is undefined behaviour, so each entry
//! point catches one and turns it into an error code.
//!
//! Ownership is explicit and one-directional: every string this library
//! returns was allocated by it and must come back to [`cadconvert_string_free`].
//! Nothing the caller allocates is ever freed here.

// The allocator this library's own work runs on. See the note in `cad-cli`:
// the converter's Parasolid path is millions of small short-lived allocations
// and then a handful of megabyte mesh buffers, and the system allocator on
// macOS cannot lend the first back to the second. Measured on the pilot,
// 291 MB and 31.9 s become 258 MB and 22.4 s, byte for byte the same file.
//
// A `cdylib` may say this: it is the whole process when the host is .NET
// P/Invoking into it. An `rlib` caller who disagrees turns the feature off.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::ffi::{CStr, CString, c_char};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;

/// What to write. Matches `cad_convert::Target`.
pub const CADCONVERT_TARGET_PLAIN: i32 = 0;
pub const CADCONVERT_TARGET_LEAN: i32 = 1;
pub const CADCONVERT_TARGET_COMPACT: i32 = 2;
pub const CADCONVERT_TARGET_USDZ: i32 = 3;

/// Outcomes. Zero is success; everything else leaves `summary` untouched.
pub const CADCONVERT_OK: i32 = 0;
pub const CADCONVERT_ERR_NULL_ARGUMENT: i32 = 1;
pub const CADCONVERT_ERR_BAD_UTF8: i32 = 2;
pub const CADCONVERT_ERR_UNKNOWN_FORMAT: i32 = 3;
pub const CADCONVERT_ERR_READ: i32 = 4;
pub const CADCONVERT_ERR_WRITE: i32 = 5;
pub const CADCONVERT_ERR_PANIC: i32 = 6;

/// How finely to mesh, and what to write.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CadconvertOptions {
    /// How far the mesh may sit from the true surface, in scene millimetres.
    ///
    /// Zero means the converter's own, which is 0.04% of the model's diagonal
    /// — a fraction rather than a fixed distance, so it scales with the part.
    pub sag_mm: f64,
    /// Largest angle between adjacent facet normals, in degrees. This is what
    /// keeps a small hole from becoming a triangle. Zero means the converter's
    /// own, which is 8°.
    pub angle_deg: f64,
    /// One of the `CADCONVERT_TARGET_*` values.
    pub target: i32,
    /// Read a STEP file's `.x_t` twin for the designer's metal/matte, when one
    /// is beside it. Non-zero for yes.
    pub use_parasolid_twin: i32,
}

impl Default for CadconvertOptions {
    fn default() -> Self {
        CadconvertOptions {
            sag_mm: 0.0,
            angle_deg: 0.0,
            target: CADCONVERT_TARGET_LEAN,
            use_parasolid_twin: 1,
        }
    }
}

/// What one conversion produced.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CadconvertSummary {
    pub bytes: u64,
    pub bodies: u64,
    pub faces: u64,
    pub faces_meshed: u64,
    pub triangles: u64,
}

/// Fill `options` with the defaults. Safe to call with a null pointer, which
/// does nothing.
///
/// # Safety
/// `options` must be null or point to a writable `CadconvertOptions`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cadconvert_default_options(options: *mut CadconvertOptions) {
    if options.is_null() {
        return;
    }
    // SAFETY: checked non-null just above; the caller's contract is that it
    // points at a writable value of this type.
    unsafe { *options = CadconvertOptions::default() };
}

/// The library's version, as a static nul-terminated string. Never freed.
#[unsafe(no_mangle)]
pub extern "C" fn cadconvert_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

/// Read `input`, mesh it, and write it to `output`.
///
/// On success returns [`CADCONVERT_OK`], fills `summary`, and sets `message` to
/// the warnings — one per line — or to null when there were none. On failure
/// returns a `CADCONVERT_ERR_*` code and sets `message` to the reason. Either way
/// a non-null `message` must be given back to [`cadconvert_string_free`].
///
/// # Safety
/// `input` and `output` must be nul-terminated UTF-8. `options` may be null,
/// which means the defaults. `summary` and `message` may be null, which means
/// the caller does not want them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cadconvert_convert(
    input: *const c_char,
    output: *const c_char,
    options: *const CadconvertOptions,
    summary: *mut CadconvertSummary,
    message: *mut *mut c_char,
) -> i32 {
    if !message.is_null() {
        // SAFETY: checked non-null; the caller owns a writable pointer slot.
        unsafe { *message = std::ptr::null_mut() };
    }
    let say = |text: String| {
        if !message.is_null()
            && let Ok(c) = CString::new(text)
        {
            // SAFETY: as above. The string is leaked deliberately — it is the
            // caller's now, and `cadconvert_string_free` takes it back.
            unsafe { *message = c.into_raw() };
        }
    };

    if input.is_null() || output.is_null() {
        say("input and output are required".into());
        return CADCONVERT_ERR_NULL_ARGUMENT;
    }
    // SAFETY: both checked non-null; the caller's contract is that they are
    // nul-terminated.
    let (input, output) = unsafe { (CStr::from_ptr(input), CStr::from_ptr(output)) };
    let (Ok(input), Ok(output)) = (input.to_str(), output.to_str()) else {
        say("paths must be UTF-8".into());
        return CADCONVERT_ERR_BAD_UTF8;
    };
    let opts = if options.is_null() {
        CadconvertOptions::default()
    } else {
        // SAFETY: checked non-null.
        unsafe { *options }
    };

    let (input, output) = (PathBuf::from(input), PathBuf::from(output));
    // A panic must not unwind into the caller: it is undefined behaviour
    // across the ABI, and a converter that takes the host process down over
    // one bad file is not usable from a long-running program.
    let done = std::panic::catch_unwind(AssertUnwindSafe(|| {
        cad_convert::convert(&input, &output, &settings(&opts))
    }));

    match done {
        Ok(Ok(s)) => {
            if !summary.is_null() {
                // SAFETY: checked non-null.
                unsafe {
                    *summary = CadconvertSummary {
                        bytes: s.bytes,
                        bodies: s.bodies as u64,
                        faces: s.faces as u64,
                        faces_meshed: s.faces_meshed as u64,
                        triangles: s.triangles as u64,
                    }
                };
            }
            if !s.warnings.is_empty() {
                say(s.warnings.join("\n"));
            }
            CADCONVERT_OK
        }
        Ok(Err(e)) => {
            let code = match e {
                cad_convert::Error::UnknownFormat(_) => CADCONVERT_ERR_UNKNOWN_FORMAT,
                cad_convert::Error::Read { .. } => CADCONVERT_ERR_READ,
                cad_convert::Error::Write { .. } => CADCONVERT_ERR_WRITE,
            };
            say(e.to_string());
            code
        }
        Err(_) => {
            say("the converter panicked; the file may be malformed in a way it does not yet name".into());
            CADCONVERT_ERR_PANIC
        }
    }
}

/// Give back a string this library returned. Null is accepted and ignored.
///
/// # Safety
/// `text` must be null or a pointer this library returned and that has not
/// already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cadconvert_string_free(text: *mut c_char) {
    if text.is_null() {
        return;
    }
    // SAFETY: the contract above says this came from `CString::into_raw`.
    drop(unsafe { CString::from_raw(text) });
}

fn settings(o: &CadconvertOptions) -> cad_convert::Options {
    // Zero leaves the tessellator's own, which is measured: a fixed 0.05 mm
    // and 20° were tried as the defaults here and left 607 open half-edges in
    // the pilot. A caller who states a distance means a distance, so stating
    // one also turns off the relative reading.
    let mut quality = cad_tess::Options::default();
    if o.sag_mm > 0.0 {
        quality.linear_deflection = o.sag_mm;
        quality.relative = false;
    }
    if o.angle_deg > 0.0 {
        quality.angular_deflection = o.angle_deg.to_radians();
    }
    cad_convert::Options {
        quality,
        target: match o.target {
            CADCONVERT_TARGET_PLAIN => cad_convert::Target::Glb,
            CADCONVERT_TARGET_COMPACT => cad_convert::Target::GlbCompact,
            CADCONVERT_TARGET_USDZ => cad_convert::Target::Usdz,
            _ => cad_convert::Target::GlbLean,
        },
        use_parasolid_twin: o.use_parasolid_twin != 0,
        ..cad_convert::Options::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_path_is_an_error_and_not_a_crash() {
        let mut message: *mut c_char = std::ptr::null_mut();
        let code = unsafe {
            cadconvert_convert(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut message,
            )
        };
        assert_eq!(code, CADCONVERT_ERR_NULL_ARGUMENT);
        assert!(!message.is_null(), "a failure must say why");
        unsafe { cadconvert_string_free(message) };
    }

    #[test]
    fn an_unreadable_file_is_named_rather_than_guessed_at() {
        let input = CString::new("/nonexistent/nothing.x_t").unwrap();
        let output = CString::new("/tmp/nothing.glb").unwrap();
        let mut message: *mut c_char = std::ptr::null_mut();
        let code = unsafe {
            cadconvert_convert(
                input.as_ptr(),
                output.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut message,
            )
        };
        // The extension says Parasolid, so the format is not in doubt — what
        // failed is the reading, and saying so is the difference between "I
        // cannot open this" and "I do not know what this is".
        assert_eq!(code, CADCONVERT_ERR_READ);
        let text = unsafe { CStr::from_ptr(message) }.to_string_lossy().into_owned();
        assert!(text.contains("nothing.x_t"), "{text}");
        unsafe { cadconvert_string_free(message) };
    }

    #[test]
    fn a_file_of_no_known_format_says_so() {
        let dir = std::env::temp_dir();
        let path = dir.join("cadconvert-ffi-unknown.dat");
        std::fs::write(&path, "not a CAD file").unwrap();
        let input = CString::new(path.to_str().unwrap()).unwrap();
        let output = CString::new(dir.join("nothing.glb").to_str().unwrap()).unwrap();
        let mut message: *mut c_char = std::ptr::null_mut();
        let code = unsafe {
            cadconvert_convert(
                input.as_ptr(),
                output.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut message,
            )
        };
        assert_eq!(code, CADCONVERT_ERR_UNKNOWN_FORMAT);
        unsafe { cadconvert_string_free(message) };
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_defaults_are_what_the_header_promises() {
        let mut o = CadconvertOptions {
            sag_mm: 0.0,
            angle_deg: 0.0,
            target: 99,
            use_parasolid_twin: 0,
        };
        unsafe { cadconvert_default_options(&mut o) };
        assert_eq!(o.target, CADCONVERT_TARGET_LEAN);
        assert_eq!(o.use_parasolid_twin, 1);
        // Zero is the statement "use the converter's own", which is measured
        // and relative to the model; a number here would be a worse guess.
        assert_eq!(o.sag_mm, 0.0);
        assert_eq!(o.angle_deg, 0.0);
    }
}
