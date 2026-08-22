//! Reading a GLB that uses `KHR_mesh_quantization`.
//!
//! The `gltf` crate refuses a file whose `extensionsRequired` names an
//! extension it does not know, and even past that its attribute readers hand
//! back nothing for an accessor that is not floating point. The extension only
//! changes how the numbers are stored — positions as 16-bit integers over a
//! box, normals as signed bytes — and the scale and offset that undo the
//! positions sit on the node above, which every tool here already applies. So
//! widening the integers is all that is left to do, and doing it here keeps the
//! smallest output we can write something the measurements can still check.

/// Open a GLB, accepting extensions the validator does not know.
pub fn open(
    bytes: &[u8],
) -> Result<(gltf::Document, Vec<gltf::buffer::Data>), Box<dyn std::error::Error>> {
    open_with_images(bytes).map(|(doc, buffers, _)| (doc, buffers))
}

/// The document, its buffers, and its decoded images.
///
/// `import_slice` refuses anything whose `extensionsRequired` it does not
/// implement, and this project writes `KHR_mesh_quantization` there because a
/// viewer that ignores quantisation draws nonsense. So the fallback is the
/// path that actually runs for our own files — and it has to import the images
/// too. It did not, which is why the first textured render came out
/// byte-identical to the untextured one: the images were silently an empty
/// list, and every material sampled nothing.
pub fn open_with_images(
    bytes: &[u8],
) -> Result<
    (gltf::Document, Vec<gltf::buffer::Data>, Vec<gltf::image::Data>),
    Box<dyn std::error::Error>,
> {
    match gltf::import_slice(bytes) {
        Ok((doc, buffers, images)) => Ok((doc, buffers, images)),
        Err(_) => {
            let file = gltf::Gltf::from_slice_without_validation(bytes)?;
            let blob = file.blob.clone();
            let doc = file.document;
            let buffers = gltf::import_buffers(&doc, None, blob)?;
            let images = gltf::import_images(&doc, None, &buffers).unwrap_or_default();
            Ok((doc, buffers, images))
        }
    }
}

/// A primitive's positions, widened if they are stored quantised.
pub fn positions(p: &gltf::Primitive, buffers: &[gltf::buffer::Data]) -> Vec<[f32; 3]> {
    let reader = p.reader(|b| Some(&buffers[b.index()]));
    match reader.read_positions() {
        Some(it) => it.collect(),
        None => vec3(p.get(&gltf::Semantic::Positions), buffers, false),
    }
}

/// A primitive's normals, widened and rescaled if they are stored quantised.
pub fn normals(p: &gltf::Primitive, buffers: &[gltf::buffer::Data]) -> Vec<[f32; 3]> {
    let reader = p.reader(|b| Some(&buffers[b.index()]));
    match reader.read_normals() {
        Some(it) => it.collect(),
        None => vec3(p.get(&gltf::Semantic::Normals), buffers, true),
    }
}

/// Read a VEC3 accessor of any component type as floats.
///
/// `normalised` follows glTF's own meaning: an integer component stands for a
/// fraction of its own full scale, which is what a byte normal is.
fn vec3(
    accessor: Option<gltf::Accessor>,
    buffers: &[gltf::buffer::Data],
    normalised: bool,
) -> Vec<[f32; 3]> {
    let Some(a) = accessor else { return Vec::new() };
    let Some(view) = a.view() else { return Vec::new() };
    let data = &buffers[view.buffer().index()];
    let base = view.offset() + a.offset();
    let width = match a.data_type() {
        gltf::accessor::DataType::I8 | gltf::accessor::DataType::U8 => 1,
        gltf::accessor::DataType::I16 | gltf::accessor::DataType::U16 => 2,
        _ => 4,
    };
    let stride = view.stride().unwrap_or(width * 3);
    let mut out = Vec::with_capacity(a.count());
    for i in 0..a.count() {
        let at = base + i * stride;
        if at + width * 3 > data.len() {
            break;
        }
        let mut v = [0.0f32; 3];
        for (k, slot) in v.iter_mut().enumerate() {
            let o = at + k * width;
            *slot = match a.data_type() {
                gltf::accessor::DataType::I8 => {
                    let x = data[o] as i8 as f32;
                    if normalised { (x / 127.0).max(-1.0) } else { x }
                }
                gltf::accessor::DataType::U8 => {
                    let x = data[o] as f32;
                    if normalised { x / 255.0 } else { x }
                }
                gltf::accessor::DataType::I16 => {
                    let x = i16::from_le_bytes([data[o], data[o + 1]]) as f32;
                    if normalised { (x / 32767.0).max(-1.0) } else { x }
                }
                gltf::accessor::DataType::U16 => {
                    let x = u16::from_le_bytes([data[o], data[o + 1]]) as f32;
                    if normalised { x / 65535.0 } else { x }
                }
                gltf::accessor::DataType::U32 => {
                    u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]) as f32
                }
                gltf::accessor::DataType::F32 => {
                    f32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
                }
            };
        }
        out.push(v);
    }
    out
}
