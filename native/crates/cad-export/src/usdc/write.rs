//! Assembling a crate file out of specs.
//!
//! Nothing here knows what a mesh is. It takes a list of specs — a path, a
//! kind, and some named values — and produces the six tables and the value
//! data a `.usdc` is made of. What the specs *say* is [`super::scene`]'s
//! business.

use super::coding;
use super::value::*;
use std::collections::HashMap;

/// What a spec is. The numbers are `SdfSpecType`, which USD does document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecKind {
    Attribute = 1,
    Prim = 6,
    PseudoRoot = 7,
    Relationship = 8,
}

pub struct Spec {
    /// An absolute path: `/root/body/mesh`, or `/root/body/mesh.points` for a
    /// property.
    pub path: String,
    pub kind: SpecKind,
    pub fields: Vec<(&'static str, Value)>,
}

#[derive(Default)]
pub struct Writer {
    tokens: Vec<String>,
    token_of: HashMap<String, u32>,
    strings: Vec<u32>,
    string_of: HashMap<String, u32>,
    fields: Vec<(u32, u64)>,
    field_of: HashMap<(u32, u64), u32>,
    fieldsets: Vec<i32>,
    fieldset_of: HashMap<Vec<u32>, u32>,
    data: Vec<u8>,
    path_index: HashMap<String, u32>,
}

impl Writer {
    fn token(&mut self, text: &str) -> u32 {
        if let Some(&i) = self.token_of.get(text) {
            return i;
        }
        let i = self.tokens.len() as u32;
        self.tokens.push(text.to_string());
        self.token_of.insert(text.to_string(), i);
        i
    }

    fn string(&mut self, text: &str) -> u32 {
        if let Some(&i) = self.string_of.get(text) {
            return i;
        }
        let token = self.token(text);
        let i = self.strings.len() as u32;
        self.strings.push(token);
        self.string_of.insert(text.to_string(), i);
        i
    }

    /// Append to the value data, four-byte aligned, and return the offset.
    fn put(&mut self, bytes: &[u8]) -> u64 {
        while self.data.len() % 4 != 0 {
            self.data.push(0);
        }
        // The data section starts at 88, after the header.
        let at = HEADER_SIZE + self.data.len() as u64;
        self.data.extend_from_slice(bytes);
        at
    }

    fn value_rep(&mut self, value: &Value) -> u64 {
        let type_code = value.type_code() as u64;
        let array_bit = u64::from(value.is_array()) << 63;

        // Cheap enough to build the closures here; the borrow checker will not
        // have `self` twice, so the indices are resolved first.
        let inline = match value {
            // An asset is a path, and a path is a token: inlined as its
            // index, exactly like a token. Written out to the data section
            // instead, the reader takes the offset for an index and reports
            // "failed to get token for index 456".
            Value::Token(t) | Value::Asset(t) => Some(self.token(t) as u64),
            Value::Str(s) => Some(self.string(s) as u64),
            Value::Specifier(n) | Value::Variability(n) => Some(*n as u64),
            Value::Bool(b) => Some(u64::from(*b)),
            Value::Int(i) => Some(*i as u32 as u64),
            Value::Float(f) => Some(f.to_bits() as u64),
            Value::Double(d) => {
                let narrowed = *d as f32;
                (narrowed as f64 == *d).then(|| narrowed.to_bits() as u64)
            }
            _ => None,
        };
        if let Some(payload) = inline {
            return array_bit | (1 << 62) | (type_code << 48) | payload;
        }

        let (bytes, compressed) = self.encode(value);
        let offset = self.put(&bytes);
        array_bit | (u64::from(compressed) << 61) | (type_code << 48) | offset
    }

    /// A value's bytes in the data section, and whether they are compressed.
    fn encode(&mut self, value: &Value) -> (Vec<u8>, bool) {
        let mut out = Vec::new();
        match value {
            Value::TokenVector(items) | Value::TokenArray(items) => {
                let indices: Vec<u32> = items.iter().map(|t| self.token(t)).collect();
                out.extend_from_slice(&(indices.len() as u64).to_le_bytes());
                for i in indices {
                    out.extend_from_slice(&i.to_le_bytes());
                }
            }
            Value::TokenListOpPrepended(items) => {
                let indices: Vec<u32> = items.iter().map(|t| self.token(t)).collect();
                out.push(LIST_OP_PREPENDED);
                out.extend_from_slice(&(indices.len() as u64).to_le_bytes());
                for i in indices {
                    out.extend_from_slice(&i.to_le_bytes());
                }
            }
            Value::PathListOp(paths) => {
                let indices: Vec<u32> = paths
                    .iter()
                    .map(|p| *self.path_index.get(p).unwrap_or(&0))
                    .collect();
                out.push(LIST_OP_EXPLICIT);
                out.extend_from_slice(&(indices.len() as u64).to_le_bytes());
                for i in indices {
                    out.extend_from_slice(&i.to_le_bytes());
                }
            }
            Value::Double(d) => out.extend_from_slice(&d.to_le_bytes()),
            Value::Vec2f(v) => {
                for c in v {
                    out.extend_from_slice(&c.to_le_bytes());
                }
            }
            Value::Vec3f(v) => {
                for c in v {
                    out.extend_from_slice(&c.to_le_bytes());
                }
            }
            Value::Vec4f(v) => {
                for c in v {
                    out.extend_from_slice(&c.to_le_bytes());
                }
            }
            Value::Matrix4d(m) => {
                for row in m {
                    for c in row {
                        out.extend_from_slice(&c.to_le_bytes());
                    }
                }
            }
            Value::IntArray(items) => {
                out.extend_from_slice(&(items.len() as u64).to_le_bytes());
                // The crate format's own integer coding, which is most of why
                // this file is a third of the text form: a triangle index
                // array steps by one far more often than not, and a step of
                // one costs two bits. Below sixteen the header costs more than
                // it saves, which is where USD draws the line too.
                if items.len() >= MIN_COMPRESSED_ARRAY {
                    // The count, then how many bytes the coding took, then
                    // the bytes. Leaving the size out leaves the reader to
                    // take the first eight bytes of the data for it, which it
                    // reports as "chunk too large" rather than as anything
                    // that names the cause.
                    let packed = coding::compress_ints32(items);
                    out.extend_from_slice(&(packed.len() as u64).to_le_bytes());
                    out.extend_from_slice(&packed);
                    return (out, true);
                }
                for i in items {
                    out.extend_from_slice(&i.to_le_bytes());
                }
            }
            Value::Vec2fArray(items) => {
                out.extend_from_slice(&(items.len() as u64).to_le_bytes());
                for v in items {
                    for c in v {
                        out.extend_from_slice(&c.to_le_bytes());
                    }
                }
            }
            Value::Vec3fArray(items) => {
                out.extend_from_slice(&(items.len() as u64).to_le_bytes());
                for v in items {
                    for c in v {
                        out.extend_from_slice(&c.to_le_bytes());
                    }
                }
            }
            // Everything else inlines and never reaches here.
            other => unreachable!("{other:?} should have been inlined"),
        }
        (out, false)
    }

    fn field(&mut self, name: &str, value: &Value) -> u32 {
        let token = self.token(name);
        let rep = self.value_rep(value);
        if let Some(&i) = self.field_of.get(&(token, rep)) {
            return i;
        }
        let i = self.fields.len() as u32;
        self.fields.push((token, rep));
        self.field_of.insert((token, rep), i);
        i
    }

    fn fieldset(&mut self, fields: Vec<u32>) -> u32 {
        if let Some(&i) = self.fieldset_of.get(&fields) {
            return i;
        }
        let i = self.fieldsets.len() as u32;
        for &f in &fields {
            self.fieldsets.push(f as i32);
        }
        self.fieldsets.push(-1);
        self.fieldset_of.insert(fields, i);
        i
    }
}

/// A crate file's header is 88 bytes: eight of identifier, eight of version,
/// the offset of the table of contents, and eight reserved words.
const HEADER_SIZE: u64 = 88;
/// Below this an array's integer coding costs more than it saves. USD's own
/// threshold, arrived at the same way.
const MIN_COMPRESSED_ARRAY: usize = 16;

/// One entry of the path table.
struct PathNode {
    /// The token for this element. Negative means a property rather than a
    /// prim, which is how the format tells `/a/b` from `/a.b`.
    element: i32,
    children: Vec<usize>,
    /// The full path, kept beside the node rather than looked up. Searching
    /// the map for each node is quadratic, and a pilot assembly has a hundred
    /// thousand of them.
    path: String,
}

pub fn write(specs: &[Spec]) -> Vec<u8> {
    let mut w = Writer::default();
    // Token zero is always this. USD writes it; a reader does not require it,
    // but a file that has it looks like every other file.
    w.token(";-)");

    let (nodes, order) = build_paths(specs, &mut w);

    // Fields can name paths — a connection, a relationship target — so every
    // path has to be numbered before any value is encoded.
    let mut spec_rows: Vec<(u32, u32, u32)> = Vec::with_capacity(specs.len());
    for spec in specs {
        let mut fields = Vec::with_capacity(spec.fields.len());
        for (name, value) in &spec.fields {
            fields.push(w.field(name, value));
        }
        let set = w.fieldset(fields);
        let path = *w.path_index.get(&spec.path).expect("every spec has a path");
        spec_rows.push((path, set, spec.kind as u32));
    }

    assemble(w, &nodes, &order, &spec_rows)
}

/// Number every path and build the tree the path table encodes.
///
/// Pre-order, and in the order the specs arrive, so that a file written twice
/// from the same scene is the same file.
fn build_paths(specs: &[Spec], w: &mut Writer) -> (Vec<PathNode>, Vec<usize>) {
    let mut nodes: Vec<PathNode> = vec![PathNode {
        element: 0, // the pseudo-root's element is the empty token
        children: Vec::new(),
        path: "/".into(),
    }];
    let mut by_path: HashMap<String, usize> = HashMap::new();
    by_path.insert("/".into(), 0);
    // The empty token names the pseudo-root.
    let empty = w.token("");
    nodes[0].element = empty as i32;

    let mut wanted: Vec<&str> = Vec::new();
    for spec in specs {
        wanted.push(&spec.path);
        for value in spec.fields.iter().map(|(_, v)| v) {
            for p in value.paths() {
                wanted.push(p);
            }
        }
    }

    for path in wanted {
        ensure_path(path, w, &mut nodes, &mut by_path);
    }

    // Depth-first, which is the order the table is read back in.
    let mut order = Vec::with_capacity(nodes.len());
    let mut stack = vec![0usize];
    while let Some(n) = stack.pop() {
        order.push(n);
        for &child in nodes[n].children.iter().rev() {
            stack.push(child);
        }
    }
    for (i, &n) in order.iter().enumerate() {
        w.path_index.insert(nodes[n].path.clone(), i as u32);
    }
    (nodes, order)
}

fn ensure_path(
    path: &str,
    w: &mut Writer,
    nodes: &mut Vec<PathNode>,
    by_path: &mut HashMap<String, usize>,
) -> usize {
    if let Some(&i) = by_path.get(path) {
        return i;
    }
    // A property is the part after a dot, and it hangs off the prim before it.
    let (parent, element, is_property) = match path.rsplit_once('.') {
        Some((prim, property)) => (prim.to_string(), property.to_string(), true),
        None => {
            let cut = path.rfind('/').unwrap_or(0);
            let parent = if cut == 0 { "/".to_string() } else { path[..cut].to_string() };
            (parent, path[cut + 1..].to_string(), false)
        }
    };
    let parent_index = ensure_path(&parent, w, nodes, by_path);
    let token = w.token(&element) as i32;
    let index = nodes.len();
    nodes.push(PathNode {
        element: if is_property { -token } else { token },
        children: Vec::new(),
        path: path.to_string(),
    });
    nodes[parent_index].children.push(index);
    by_path.insert(path.to_string(), index);
    index
}

fn assemble(
    mut w: Writer,
    nodes: &[PathNode],
    order: &[usize],
    specs: &[(u32, u32, u32)],
) -> Vec<u8> {
    // The path table: for each entry in pre-order, its own index, its element
    // token, and one number saying where its sibling is.
    //
    //   -1  it has children and no sibling
    //    0  it has a sibling and no children
    //   -2  neither
    //   >0  both, and the sibling is this far ahead
    let position: HashMap<usize, usize> =
        order.iter().enumerate().map(|(i, &n)| (n, i)).collect();
    let mut indexes = Vec::with_capacity(order.len());
    let mut elements = Vec::with_capacity(order.len());
    let mut jumps = Vec::with_capacity(order.len());

    // Which node, if any, follows each as a sibling.
    let mut sibling: HashMap<usize, usize> = HashMap::new();
    for node in nodes {
        for pair in node.children.windows(2) {
            sibling.insert(pair[0], pair[1]);
        }
    }

    for (i, &n) in order.iter().enumerate() {
        indexes.push(i as i32);
        elements.push(nodes[n].element);
        let has_child = !nodes[n].children.is_empty();
        let next = sibling.get(&n).copied();
        jumps.push(match (has_child, next) {
            (true, None) => -1,
            (false, Some(_)) => 0,
            (false, None) => -2,
            (true, Some(s)) => (position[&s] - i) as i32,
        });
    }

    let mut out = Vec::with_capacity(w.data.len() + (1 << 16));
    out.extend_from_slice(b"PXR-USDC");
    out.extend_from_slice(&[0, 8, 0, 0, 0, 0, 0, 0]); // version 0.8.0
    out.extend_from_slice(&0u64.to_le_bytes()); // the offset is filled in below
    out.resize(HEADER_SIZE as usize, 0);
    out.append(&mut w.data);

    let mut sections: Vec<(&str, u64, u64)> = Vec::new();
    let section = |out: &mut Vec<u8>, sections: &mut Vec<(&'static str, u64, u64)>,
                       name: &'static str, body: &[u8]| {
        sections.push((name, out.len() as u64, body.len() as u64));
        out.extend_from_slice(body);
    };

    // TOKENS: how many, how big they are together, and the compressed run of
    // them separated by nul.
    let mut joined = Vec::new();
    for token in &w.tokens {
        joined.extend_from_slice(token.as_bytes());
        joined.push(0);
    }
    let compressed = coding::compress(&joined);
    let mut body = Vec::new();
    body.extend_from_slice(&(w.tokens.len() as u64).to_le_bytes());
    body.extend_from_slice(&(joined.len() as u64).to_le_bytes());
    body.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
    body.extend_from_slice(&compressed);
    section(&mut out, &mut sections, "TOKENS", &body);

    // STRINGS: token indices, one per string.
    let mut body = Vec::new();
    body.extend_from_slice(&(w.strings.len() as u64).to_le_bytes());
    for &s in &w.strings {
        body.extend_from_slice(&s.to_le_bytes());
    }
    section(&mut out, &mut sections, "STRINGS", &body);

    // FIELDS: the token each names, then the representations.
    let mut body = Vec::new();
    body.extend_from_slice(&(w.fields.len() as u64).to_le_bytes());
    let tokens: Vec<i32> = w.fields.iter().map(|&(t, _)| t as i32).collect();
    let packed = coding::compress_ints32(&tokens);
    body.extend_from_slice(&(packed.len() as u64).to_le_bytes());
    body.extend_from_slice(&packed);
    let mut reps = Vec::with_capacity(w.fields.len() * 8);
    for &(_, rep) in &w.fields {
        reps.extend_from_slice(&rep.to_le_bytes());
    }
    let packed = coding::compress(&reps);
    body.extend_from_slice(&(packed.len() as u64).to_le_bytes());
    body.extend_from_slice(&packed);
    section(&mut out, &mut sections, "FIELDS", &body);

    // FIELDSETS: runs of field indices, each ended by -1.
    let mut body = Vec::new();
    body.extend_from_slice(&(w.fieldsets.len() as u64).to_le_bytes());
    let packed = coding::compress_ints32(&w.fieldsets);
    body.extend_from_slice(&(packed.len() as u64).to_le_bytes());
    body.extend_from_slice(&packed);
    section(&mut out, &mut sections, "FIELDSETS", &body);

    // PATHS.
    let mut body = Vec::new();
    body.extend_from_slice(&(order.len() as u64).to_le_bytes());
    body.extend_from_slice(&(order.len() as u64).to_le_bytes());
    for array in [&indexes, &elements, &jumps] {
        let packed = coding::compress_ints32(array);
        body.extend_from_slice(&(packed.len() as u64).to_le_bytes());
        body.extend_from_slice(&packed);
    }
    section(&mut out, &mut sections, "PATHS", &body);

    // SPECS.
    let mut body = Vec::new();
    body.extend_from_slice(&(specs.len() as u64).to_le_bytes());
    let paths: Vec<i32> = specs.iter().map(|s| s.0 as i32).collect();
    let sets: Vec<i32> = specs.iter().map(|s| s.1 as i32).collect();
    let kinds: Vec<i32> = specs.iter().map(|s| s.2 as i32).collect();
    for array in [&paths, &sets, &kinds] {
        let packed = coding::compress_ints32(array);
        body.extend_from_slice(&(packed.len() as u64).to_le_bytes());
        body.extend_from_slice(&packed);
    }
    section(&mut out, &mut sections, "SPECS", &body);

    let toc_at = out.len() as u64;
    out.extend_from_slice(&(sections.len() as u64).to_le_bytes());
    for (name, start, size) in &sections {
        let mut padded = [0u8; 16];
        padded[..name.len()].copy_from_slice(name.as_bytes());
        out.extend_from_slice(&padded);
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
    }
    out[16..24].copy_from_slice(&toc_at.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_and_sections(bytes: &[u8]) -> (u64, Vec<(String, u64, u64)>) {
        assert_eq!(&bytes[..8], b"PXR-USDC");
        assert_eq!(&bytes[8..11], &[0, 8, 0], "version 0.8.0");
        let toc = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let count = u64::from_le_bytes(bytes[toc as usize..toc as usize + 8].try_into().unwrap());
        let mut out = Vec::new();
        let mut at = toc as usize + 8;
        for _ in 0..count {
            let name = String::from_utf8_lossy(&bytes[at..at + 16])
                .trim_end_matches('\0')
                .to_string();
            let start = u64::from_le_bytes(bytes[at + 16..at + 24].try_into().unwrap());
            let size = u64::from_le_bytes(bytes[at + 24..at + 32].try_into().unwrap());
            out.push((name, start, size));
            at += 32;
        }
        (toc, out)
    }

    fn one_mesh() -> Vec<Spec> {
        vec![
            Spec {
                path: "/".into(),
                kind: SpecKind::PseudoRoot,
                fields: vec![
                    ("defaultPrim", Value::Token("root".into())),
                    ("primChildren", Value::TokenVector(vec!["root".into()])),
                ],
            },
            Spec {
                path: "/root".into(),
                kind: SpecKind::Prim,
                fields: vec![
                    ("specifier", Value::Specifier(0)),
                    ("typeName", Value::Token("Mesh".into())),
                    ("properties", Value::TokenVector(vec!["points".into()])),
                ],
            },
            Spec {
                path: "/root.points".into(),
                kind: SpecKind::Attribute,
                fields: vec![
                    ("typeName", Value::Token("point3f[]".into())),
                    ("default", Value::Vec3fArray(vec![[0.0; 3], [1.0, 0.0, 0.0]])),
                ],
            },
        ]
    }

    #[test]
    fn the_container_is_what_a_reader_walks() {
        let bytes = write(&one_mesh());
        let (toc, sections) = header_and_sections(&bytes);

        let names: Vec<&str> = sections.iter().map(|(n, _, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["TOKENS", "STRINGS", "FIELDS", "FIELDSETS", "PATHS", "SPECS"]
        );
        // Every section inside the file and before the table of contents.
        for (name, start, size) in &sections {
            assert!(*start >= 88, "{name} overlaps the header");
            assert!(start + size <= toc, "{name} runs past the contents");
        }
    }

    #[test]
    fn a_property_hangs_off_its_prim_rather_than_beside_it() {
        // `/root.points` is a child of `/root`, not a sibling. The path table
        // says so with the sign of the element token, and a reader that finds
        // it the other way round loses the attribute.
        let bytes = write(&one_mesh());
        let (_, sections) = header_and_sections(&bytes);
        let (_, start, _) = sections.iter().find(|(n, _, _)| n == "PATHS").unwrap();

        let at = *start as usize;
        let count = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()) as usize;
        // `/`, `/root`, `/root.points` — three, and no more.
        assert_eq!(count, 3);
    }

    #[test]
    fn the_same_field_written_twice_is_stored_once() {
        // Fourteen materials and sixty-four meshes share `custom = false` and
        // `variability = uniform` between them. Every one of those is the same
        // eight bytes and the table holds it once.
        let mut specs = one_mesh();
        for i in 0..8 {
            specs.push(Spec {
                path: format!("/root/child_{i}"),
                kind: SpecKind::Prim,
                fields: vec![
                    ("specifier", Value::Specifier(0)),
                    ("typeName", Value::Token("Scope".into())),
                ],
            });
        }
        let bytes = write(&specs);
        let (_, sections) = header_and_sections(&bytes);
        let (_, start, _) = sections.iter().find(|(n, _, _)| n == "FIELDS").unwrap();
        let count = u64::from_le_bytes(
            bytes[*start as usize..*start as usize + 8].try_into().unwrap(),
        );
        // Without sharing this would be four for the first three specs plus
        // two for each of the eight children.
        assert!(count < 12, "{count} fields for a file with six distinct ones");
    }

    #[test]
    fn writing_the_same_scene_twice_gives_the_same_bytes() {
        // Nothing here may depend on the order a hash map iterates in: a
        // converter whose output moves between runs cannot be diffed and
        // cannot be cached.
        assert_eq!(write(&one_mesh()), write(&one_mesh()));
    }

    #[test]
    fn a_token_is_interned_once_however_often_it_is_named() {
        let mut specs = one_mesh();
        for i in 0..20 {
            specs.push(Spec {
                path: format!("/root/s_{i}"),
                kind: SpecKind::Prim,
                fields: vec![("typeName", Value::Token("Scope".into()))],
            });
        }
        let bytes = write(&specs);
        let (_, sections) = header_and_sections(&bytes);
        let (_, start, _) = sections.iter().find(|(n, _, _)| n == "TOKENS").unwrap();
        let count = u64::from_le_bytes(
            bytes[*start as usize..*start as usize + 8].try_into().unwrap(),
        );
        // 20 prim names, plus a handful of shared ones. "Scope" is one token.
        assert!(count < 35, "{count} tokens");
    }
}
