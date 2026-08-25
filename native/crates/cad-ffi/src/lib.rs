//! The C ABI.
//!
//! Everything else in this workspace forbids `unsafe`. This crate cannot: a
//! foreign caller hands over raw pointers and there is no way to read them
//! safely. What it can do is keep the unsafe surface to the five functions
//! below, validate every pointer before it is read, and never let a panic
//! cross the boundary — unwinding into C is undefined behaviour, so each entry
//! point catches one and turns it into an error code.
//!
//! Ownership is explicit and one-directional: every string this library
//! returns was allocated by it and must come back to [`cadconvert_string_free`].
//! Nothing the caller allocates is ever freed here. The one string that goes
//! the other way — the `detail` a progress callback receives — is lent for the
//! duration of that call and is not the caller's to keep or free.

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
static ALLOC: cad_alloc::MiMalloc = cad_alloc::MiMalloc;

use std::ffi::{CStr, CString, c_char, c_void};
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
/// The caller's progress callback asked for it. Outputs written before that
/// point are on disk.
pub const CADCONVERT_ERR_CANCELLED: i32 = 7;

/// Where a conversion is, as a progress callback hears it. Matches
/// `cad_convert::Stage`.
pub const CADCONVERT_STAGE_READ: i32 = 1;
pub const CADCONVERT_STAGE_MESH: i32 = 2;
pub const CADCONVERT_STAGE_WRITE: i32 = 3;

/// Told between units of work, on the calling thread.
///
/// `done` of `total` units of `stage` are finished; `detail` names the unit
/// about to start and is empty when the stage is complete. It is lent for
/// the call only. Return zero to continue and anything else to stop, which
/// makes the conversion return [`CADCONVERT_ERR_CANCELLED`]. The function
/// must not unwind.
pub type CadconvertProgressFn = unsafe extern "C" fn(
    user: *mut c_void,
    stage: i32,
    done: u64,
    total: u64,
    detail: *const c_char,
) -> i32;

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

/// What one conversion produced. `bytes` is every output added up.
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
    let outputs = [output];
    // SAFETY: the contract is this function's own, stated above; the one
    // output pointer is passed on as a one-element array.
    unsafe {
        cadconvert_convert_many(
            input,
            outputs.as_ptr(),
            1,
            options,
            None,
            std::ptr::null_mut(),
            summary,
            message,
        )
    }
}

/// Read `input` once, mesh it once, and write it to every path in `outputs`.
///
/// Each output's extension chooses its container — `.glb` or `.usdz` — so a
/// part wanted in both is one call and one reading. `progress`, when not
/// null, is told where the work is between units, on this thread, with `user`
/// passed back as it was given; see [`CadconvertProgressFn`]. Everything else
/// is as [`cadconvert_convert`].
///
/// # Safety
/// `input` must be nul-terminated UTF-8; `outputs` must point at
/// `output_count` such strings. `options` may be null (the defaults),
/// `progress` may be null (no reports), and `summary` and `message` may be
/// null (not wanted). `user` is never read here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cadconvert_convert_many(
    input: *const c_char,
    outputs: *const *const c_char,
    output_count: usize,
    options: *const CadconvertOptions,
    progress: Option<CadconvertProgressFn>,
    user: *mut c_void,
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

    if input.is_null() {
        say("input is required".into());
        return CADCONVERT_ERR_NULL_ARGUMENT;
    }
    if output_count == 0 || outputs.is_null() {
        say("at least one output is required".into());
        return CADCONVERT_ERR_NULL_ARGUMENT;
    }
    // SAFETY: checked non-null; the caller's contract is that it is
    // nul-terminated.
    let Ok(input) = unsafe { CStr::from_ptr(input) }.to_str() else {
        say("paths must be UTF-8".into());
        return CADCONVERT_ERR_BAD_UTF8;
    };
    let mut paths = Vec::with_capacity(output_count);
    for i in 0..output_count {
        // SAFETY: the caller's contract is that `outputs` holds
        // `output_count` pointers; `i` is below it.
        let p = unsafe { *outputs.add(i) };
        if p.is_null() {
            say(format!("output {i} is null"));
            return CADCONVERT_ERR_NULL_ARGUMENT;
        }
        // SAFETY: checked non-null; nul-terminated by contract.
        let Ok(text) = unsafe { CStr::from_ptr(p) }.to_str() else {
            say("paths must be UTF-8".into());
            return CADCONVERT_ERR_BAD_UTF8;
        };
        paths.push(PathBuf::from(text));
    }
    let opts = if options.is_null() {
        CadconvertOptions::default()
    } else {
        // SAFETY: checked non-null.
        unsafe { *options }
    };
    let input = PathBuf::from(input);

    // The callback crosses back into the caller with a string lent for the
    // call. A detail with a nul inside it — a path cannot have one, a body
    // name from a file could — is cut at the nul rather than refused.
    let mut report = |p: &cad_convert::Progress| -> bool {
        let Some(f) = progress else {
            return true;
        };
        let stage = match p.stage {
            cad_convert::Stage::Read => CADCONVERT_STAGE_READ,
            cad_convert::Stage::Mesh => CADCONVERT_STAGE_MESH,
            cad_convert::Stage::Write => CADCONVERT_STAGE_WRITE,
        };
        let detail = CString::new(p.detail.split('\0').next().unwrap_or("")).unwrap_or_default();
        // SAFETY: the caller supplied the function and promised it does not
        // unwind; `detail` outlives the call; `user` is handed back unread.
        unsafe { f(user, stage, p.done as u64, p.total as u64, detail.as_ptr()) == 0 }
    };

    // A panic must not unwind into the caller: it is undefined behaviour
    // across the ABI, and a converter that takes the host process down over
    // one bad file is not usable from a long-running program.
    let done = std::panic::catch_unwind(AssertUnwindSafe(|| {
        cad_convert::convert_many(&input, &paths, &settings(&opts), &mut report)
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
                cad_convert::Error::Cancelled { .. } => CADCONVERT_ERR_CANCELLED,
                cad_convert::Error::NoOutput => CADCONVERT_ERR_NULL_ARGUMENT,
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

    fn sample() -> CString {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../cad-convert/tests/samples/small.x_t");
        CString::new(path.to_str().unwrap()).unwrap()
    }

    fn scratch(name: &str) -> (std::path::PathBuf, CString) {
        let dir = std::env::temp_dir().join("cad-ffi-tests");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(name);
        let _ = std::fs::remove_file(&path);
        let c = CString::new(path.to_str().unwrap()).unwrap();
        (path, c)
    }

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

    /// What a callback under test records: every report, and what to answer.
    struct Listener {
        seen: Vec<(i32, u64, u64, String)>,
        stop_at: Option<i32>,
    }

    unsafe extern "C" fn listen(
        user: *mut c_void,
        stage: i32,
        done: u64,
        total: u64,
        detail: *const c_char,
    ) -> i32 {
        // SAFETY: the tests below pass a `*mut Listener` and keep it alive
        // for the call; `detail` is nul-terminated by this library's contract.
        let l = unsafe { &mut *(user as *mut Listener) };
        let text = unsafe { CStr::from_ptr(detail) }.to_string_lossy().into_owned();
        l.seen.push((stage, done, total, text));
        (l.stop_at == Some(stage)) as i32
    }

    #[test]
    fn many_outputs_come_from_one_reading_and_the_callback_hears_every_stage() {
        let (glb, glb_c) = scratch("many.glb");
        let (usdz, usdz_c) = scratch("many.usdz");
        let outputs = [glb_c.as_ptr(), usdz_c.as_ptr()];
        let mut listener = Listener {
            seen: Vec::new(),
            stop_at: None,
        };
        let mut summary = CadconvertSummary::default();
        let mut message: *mut c_char = std::ptr::null_mut();
        let code = unsafe {
            cadconvert_convert_many(
                sample().as_ptr(),
                outputs.as_ptr(),
                2,
                std::ptr::null(),
                Some(listen),
                &mut listener as *mut Listener as *mut c_void,
                &mut summary,
                &mut message,
            )
        };
        assert_eq!(code, CADCONVERT_OK, "{}", unsafe { text(message) });
        unsafe { cadconvert_string_free(message) };
        let total = std::fs::metadata(&glb).unwrap().len() + std::fs::metadata(&usdz).unwrap().len();
        assert_eq!(summary.bytes, total, "bytes are every output added up");
        assert!(summary.triangles > 100);

        let stages: Vec<i32> = listener.seen.iter().map(|s| s.0).collect();
        let mut order = stages.clone();
        order.dedup();
        assert_eq!(
            order,
            [CADCONVERT_STAGE_READ, CADCONVERT_STAGE_MESH, CADCONVERT_STAGE_WRITE]
        );
        let last = listener.seen.last().unwrap();
        assert_eq!((last.0, last.1, last.2), (CADCONVERT_STAGE_WRITE, 2, 2));
        assert!(last.3.is_empty());
        assert!(
            listener
                .seen
                .iter()
                .any(|s| s.0 == CADCONVERT_STAGE_WRITE && s.3.ends_with("many.usdz"))
        );
        let _ = std::fs::remove_file(&glb);
        let _ = std::fs::remove_file(&usdz);
    }

    #[test]
    fn a_callback_that_says_stop_gets_cancelled_and_no_file() {
        let (glb, glb_c) = scratch("stopped.glb");
        let outputs = [glb_c.as_ptr()];
        let mut listener = Listener {
            seen: Vec::new(),
            stop_at: Some(CADCONVERT_STAGE_MESH),
        };
        let mut message: *mut c_char = std::ptr::null_mut();
        let code = unsafe {
            cadconvert_convert_many(
                sample().as_ptr(),
                outputs.as_ptr(),
                1,
                std::ptr::null(),
                Some(listen),
                &mut listener as *mut Listener as *mut c_void,
                std::ptr::null_mut(),
                &mut message,
            )
        };
        assert_eq!(code, CADCONVERT_ERR_CANCELLED);
        let why = unsafe { text(message) };
        assert!(why.contains("Mesh"), "{why}");
        unsafe { cadconvert_string_free(message) };
        assert!(!glb.exists());
    }

    #[test]
    fn no_outputs_is_refused_as_a_missing_argument() {
        let mut message: *mut c_char = std::ptr::null_mut();
        let code = unsafe {
            cadconvert_convert_many(
                sample().as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                None,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut message,
            )
        };
        assert_eq!(code, CADCONVERT_ERR_NULL_ARGUMENT);
        unsafe { cadconvert_string_free(message) };
    }

    unsafe fn text(message: *mut c_char) -> String {
        if message.is_null() {
            String::new()
        } else {
            // SAFETY: a non-null message is this library's own nul-terminated
            // string.
            unsafe { CStr::from_ptr(message) }.to_string_lossy().into_owned()
        }
    }
}
