//! A fast reader for ISO 10303-21 (STEP "Part 21") exchange files.
//!
//! The design goal is to open a 32 MB AP214 assembly — half a million entity
//! instances, 186 k cartesian points — in tens of milliseconds and then let
//! callers pull only what they need out of it. That rules out materialising an
//! owned object per instance, so the reader works in two layers:
//!
//! 1. [`scan`] walks the bytes once and records where every instance's keyword
//!    and arguments live. Nothing is decoded.
//! 2. Callers ask for an entity by id and get an [`Args`] cursor that decodes
//!    straight out of the file buffer.
//!
//! Unmodelled entities cost a keyword intern and nothing else, so a file full
//! of PMI and tolerance data this reader has no use for is not a problem.
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let file = cad_step::StepFile::open("assembly.stp")?;
//! for e in file.by_kind(cad_step::Kind::AdvancedFace) {
//!     let mut args = file.args_of(e);
//!     let name = args.next_str()?;
//!     println!("face {} named {name:?}", e.id);
//! }
//! # Ok(()) }
//! ```

#![forbid(unsafe_code)]

pub mod error;
pub mod kind;
pub mod lower;
pub mod presentation;
pub mod scan;
pub mod units;
pub mod value;

pub use error::{Result, StepError};
pub use kind::Kind;
pub use presentation::{Appearance, Styles};
pub use units::Units;
pub use value::{Args, Value};

use std::ops::Range;
use std::path::Path;

/// One entity instance, located but not decoded.
#[derive(Debug, Clone)]
pub struct Entity {
    /// The instance name, the `N` in `#N=`.
    pub id: u32,
    /// The interned keyword, or [`Kind::Complex`] for a complex instance.
    pub kind: Kind,
    keyword: Range<u32>,
    args: Range<u32>,
}

/// A `KEYWORD(args);` record from the header section.
#[derive(Debug, Clone)]
pub struct HeaderEntry {
    keyword: Range<u32>,
    args: Range<u32>,
}

/// A parsed Part 21 file.
pub struct StepFile {
    buf: Vec<u8>,
    entities: Vec<Entity>,
    /// `id` → index into `entities`, or [`ABSENT`] where no such id exists.
    by_id: Vec<u32>,
    header: Vec<HeaderEntry>,
}

/// Sentinel in [`StepFile::by_id`] for an id that no instance defines.
const ABSENT: u32 = u32::MAX;

impl StepFile {
    /// Read and scan a file from disk.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<StepFile> {
        let path = path.as_ref();
        let buf = std::fs::read(path).map_err(|source| StepError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        StepFile::from_bytes(buf)
    }

    /// Scan an in-memory Part 21 file.
    ///
    /// Takes ownership because every [`Args`] cursor borrows from this buffer;
    /// the file stays resident for as long as the [`StepFile`] does.
    pub fn from_bytes(buf: Vec<u8>) -> Result<StepFile> {
        let scanned = scan::scan(&buf)?;

        let mut entities = Vec::with_capacity(scanned.records.len());
        let mut by_id = vec![ABSENT; scanned.max_id as usize + 1];

        for rec in scanned.records {
            let kind = if rec.complex {
                Kind::Complex
            } else {
                Kind::intern(&buf[rec.keyword.start as usize..rec.keyword.end as usize])
            };
            // A duplicate instance name is illegal; the last definition wins,
            // which is what every other reader does and keeps us from failing a
            // file over a defect that does not affect the geometry we want.
            by_id[rec.id as usize] = entities.len() as u32;
            entities.push(Entity {
                id: rec.id,
                kind,
                keyword: rec.keyword,
                args: rec.args,
            });
        }

        let header = scanned
            .header
            .into_iter()
            .map(|h| HeaderEntry {
                keyword: h.keyword,
                args: h.args,
            })
            .collect();

        Ok(StepFile {
            buf,
            entities,
            by_id,
            header,
        })
    }

    /// Number of entity instances in the data section.
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Every entity, in file order.
    pub fn entities(&self) -> &[Entity] {
        &self.entities
    }

    /// Look up an instance by name.
    pub fn get(&self, id: u32) -> Option<&Entity> {
        let slot = *self.by_id.get(id as usize)?;
        if slot == ABSENT {
            None
        } else {
            self.entities.get(slot as usize)
        }
    }

    /// Look up an instance by name, or report it as dangling.
    pub fn require(&self, id: u32) -> Result<&Entity> {
        self.get(id).ok_or(StepError::DanglingRef { id })
    }

    /// The interned keyword of `id`, or [`Kind::Other`] when it is undefined.
    pub fn kind_of(&self, id: u32) -> Kind {
        self.get(id).map_or(Kind::Other, |e| e.kind)
    }

    /// The raw keyword text of an entity — useful for [`Kind::Other`].
    pub fn keyword(&self, e: &Entity) -> &str {
        std::str::from_utf8(&self.buf[e.keyword.start as usize..e.keyword.end as usize])
            .unwrap_or("")
    }

    /// An argument cursor over an entity's parameters.
    pub fn args_of(&self, e: &Entity) -> Args<'_> {
        Args::new(&self.buf, e.args.clone())
    }

    /// An argument cursor for an instance name, or a dangling-reference error.
    pub fn args(&self, id: u32) -> Result<Args<'_>> {
        Ok(self.args_of(self.require(id)?))
    }

    /// An argument cursor for `id`, checked to be of kind `want`.
    pub fn args_checked(&self, id: u32, want: Kind) -> Result<Args<'_>> {
        let e = self.require(id)?;
        if e.kind != want {
            return Err(StepError::WrongKind {
                id,
                actual: if e.kind == Kind::Other {
                    self.keyword(e).to_string()
                } else {
                    e.kind.as_str().to_string()
                },
                expected: want.as_str(),
            });
        }
        Ok(self.args_of(e))
    }

    /// Every entity of one kind, in file order.
    pub fn by_kind(&self, kind: Kind) -> impl Iterator<Item = &Entity> {
        self.entities.iter().filter(move |e| e.kind == kind)
    }

    /// The sub-records of a complex instance: `#N=(A(…)B(…));`.
    ///
    /// Returns an empty vector for a simple instance.
    pub fn complex_parts(&self, e: &Entity) -> Result<Vec<(Kind, Args<'_>)>> {
        if e.kind != Kind::Complex {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut i = e.args.start as usize;
        let end = e.args.end as usize;
        while i < end {
            while i < end && (self.buf[i].is_ascii_whitespace() || self.buf[i] == b',') {
                i += 1;
            }
            if i >= end {
                break;
            }
            let Some(off) = memchr::memchr(b'(', &self.buf[i..end]) else {
                break;
            };
            let open = i + off;
            let close = scan::matching_paren(&self.buf, open)?;
            let mut kw = i..open;
            while kw.end > kw.start && self.buf[kw.end - 1].is_ascii_whitespace() {
                kw.end -= 1;
            }
            out.push((
                Kind::intern(&self.buf[kw.clone()]),
                Args::new(&self.buf, (open + 1) as u32..close as u32),
            ));
            i = close + 1;
        }
        Ok(out)
    }

    /// The first sub-record of a complex instance matching `kind`.
    pub fn complex_part(&self, e: &Entity, kind: Kind) -> Result<Option<Args<'_>>> {
        Ok(self
            .complex_parts(e)?
            .into_iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, a)| a))
    }

    /// An argument cursor over a header record such as `FILE_NAME`.
    pub fn header_args(&self, keyword: &str) -> Option<Args<'_>> {
        let kw = keyword.as_bytes();
        self.header
            .iter()
            .find(|h| {
                self.buf[h.keyword.start as usize..h.keyword.end as usize].eq_ignore_ascii_case(kw)
            })
            .map(|h| Args::new(&self.buf, h.args.clone()))
    }

    /// The `name` field of `FILE_NAME`, empty when absent.
    pub fn file_name(&self) -> String {
        self.header_args("FILE_NAME")
            .and_then(|mut a| a.next_str().ok())
            .map(|s| s.into_owned())
            .unwrap_or_default()
    }

    /// The schema identifiers from `FILE_SCHEMA`, e.g. `AUTOMOTIVE_DESIGN {…}`.
    pub fn schemas(&self) -> Vec<String> {
        let Some(mut a) = self.header_args("FILE_SCHEMA") else {
            return Vec::new();
        };
        match a.next_value() {
            Ok(Value::List(items)) => items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            Ok(Value::Str(s)) => vec![s.into_owned()],
            _ => Vec::new(),
        }
    }

    /// The `originating_system` field of `FILE_NAME`, empty when absent.
    ///
    /// This names the CAD system that wrote the file, which decides how its
    /// quirks should be read.
    pub fn originating_system(&self) -> String {
        self.header_args("FILE_NAME")
            .and_then(|mut a| {
                // name, time_stamp, author, organization, preprocessor_version,
                // originating_system, authorisation
                a.skip_n(5).ok()?;
                a.next_str().ok()
            })
            .map(|s| s.into_owned())
            .unwrap_or_default()
    }

    /// Count of instances per kind, for diagnostics.
    pub fn kind_histogram(&self) -> Vec<(Kind, usize)> {
        let mut map: rustc_hash::FxHashMap<Kind, usize> = rustc_hash::FxHashMap::default();
        for e in &self.entities {
            *map.entry(e.kind).or_default() += 1;
        }
        let mut v: Vec<_> = map.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v
    }

    /// Count of instances per raw keyword, including unmodelled ones.
    pub fn keyword_histogram(&self) -> Vec<(String, usize)> {
        let mut map: rustc_hash::FxHashMap<&str, usize> = rustc_hash::FxHashMap::default();
        for e in &self.entities {
            let name = if e.kind == Kind::Complex {
                "(complex)"
            } else {
                self.keyword(e)
            };
            *map.entry(name).or_default() += 1;
        }
        let mut v: Vec<_> = map.into_iter().map(|(k, n)| (k.to_string(), n)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "\
ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('a shape'),'2;1');
FILE_NAME('part','2026-08-19T00:00:00',('me'),('org'),'pp v1','Solid Edge','auth');
FILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 3 1 1 }'));
ENDSEC;
DATA;
#1=CARTESIAN_POINT('',(0.,0.,0.));
#2=DIRECTION('',(0.,0.,1.));
#3=DIRECTION('',(1.,0.,0.));
#4=AXIS2_PLACEMENT_3D('',#1,#2,#3);
#5=PLANE('face surface',#4);
#6=COLOUR_RGB('',0.8,0.1,0.05);
#7=(NAMED_UNIT(*)SI_UNIT($,.METRE.)LENGTH_UNIT());
ENDSEC;
END-ISO-10303-21;
";

    fn file() -> StepFile {
        StepFile::from_bytes(SRC.as_bytes().to_vec()).unwrap()
    }

    #[test]
    fn reads_header_fields() {
        let f = file();
        assert_eq!(f.file_name(), "part");
        assert_eq!(f.originating_system(), "Solid Edge");
        assert_eq!(f.schemas(), vec!["AUTOMOTIVE_DESIGN { 1 0 10303 214 3 1 1 }"]);
    }

    #[test]
    fn indexes_entities_by_instance_name() {
        let f = file();
        assert_eq!(f.len(), 7);
        assert_eq!(f.kind_of(5), Kind::Plane);
        assert_eq!(f.kind_of(999), Kind::Other);
        assert!(f.get(999).is_none());
        assert!(matches!(
            f.require(999),
            Err(StepError::DanglingRef { id: 999 })
        ));
    }

    #[test]
    fn decodes_a_cartesian_point() {
        let f = file();
        let mut a = f.args_checked(1, Kind::CartesianPoint).unwrap();
        assert_eq!(a.next_str().unwrap(), "");
        let mut xyz = Vec::new();
        a.next_f64_list(&mut xyz).unwrap();
        assert_eq!(xyz, vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn decodes_a_colour() {
        let f = file();
        let mut a = f.args_checked(6, Kind::ColourRgb).unwrap();
        a.skip().unwrap();
        let rgb = [
            a.next_f64().unwrap(),
            a.next_f64().unwrap(),
            a.next_f64().unwrap(),
        ];
        assert_eq!(rgb, [0.8, 0.1, 0.05]);
    }

    #[test]
    fn a_wrong_kind_lookup_names_both_kinds() {
        let f = file();
        match f.args_checked(5, Kind::AdvancedFace) {
            Err(StepError::WrongKind { id, actual, expected }) => {
                assert_eq!(id, 5);
                assert_eq!(actual, "PLANE");
                assert_eq!(expected, "ADVANCED_FACE");
            }
            other => panic!("expected a WrongKind error, got {other:?}"),
        }
    }

    #[test]
    fn splits_a_complex_instance_into_its_parts() {
        let f = file();
        let e = f.require(7).unwrap();
        assert_eq!(e.kind, Kind::Complex);
        let parts = f.complex_parts(e).unwrap();
        let kinds: Vec<_> = parts.iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, vec![Kind::NamedUnit, Kind::SiUnit, Kind::LengthUnit]);

        let mut si = f.complex_part(e, Kind::SiUnit).unwrap().unwrap();
        si.skip().unwrap(); // prefix, unset here
        assert_eq!(si.next_enum().unwrap(), "METRE");
    }

    #[test]
    fn by_kind_finds_every_instance_of_a_kind() {
        let f = file();
        let dirs: Vec<u32> = f.by_kind(Kind::Direction).map(|e| e.id).collect();
        assert_eq!(dirs, vec![2, 3]);
    }

    #[test]
    fn histograms_cover_every_entity() {
        let f = file();
        let total: usize = f.kind_histogram().iter().map(|(_, n)| n).sum();
        assert_eq!(total, f.len());
        let total: usize = f.keyword_histogram().iter().map(|(_, n)| n).sum();
        assert_eq!(total, f.len());
    }
}
