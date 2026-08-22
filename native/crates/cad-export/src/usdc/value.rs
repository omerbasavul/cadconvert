//! What a field can hold, and how the crate format writes it.
//!
//! Every value in a crate file is a 64-bit representation:
//!
//! ```text
//! bit 63  this is an array
//! bit 62  the payload is the value, not an offset
//! bit 61  the payload is compressed
//! 48..55  which type
//! 0..47   the value, or where it is
//! ```
//!
//! The type numbers are not documented anywhere this project could find them.
//! They were read off files USD wrote: a `.usda` naming one of each type, run
//! through `usdcat`, taken apart by `tools/usdc_decode.py`. That is also why
//! only the types this writer actually emits are here — the rest were never
//! observed and guessing at a number would produce a file that reads as
//! something else.

/// A field's value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// An index into the token table.
    Token(String),
    /// `primChildren` and `properties`, which are metadata a prim carries
    /// about itself.
    TokenVector(Vec<String>),
    /// A `token[]` **attribute**, which is a different thing: a token with the
    /// array bit, the way `int[]` is an int with the array bit. Written as a
    /// token vector instead, a reader hands back the C++ container it was
    /// stored in and the value is lost.
    TokenArray(Vec<String>),
    /// `prepend apiSchemas = [...]`.
    TokenListOpPrepended(Vec<String>),
    /// `def` is 0, `over` 1, `class` 2.
    Specifier(u32),
    /// `varying` is 0, `uniform` 1.
    Variability(u32),
    Bool(bool),
    Int(i32),
    Float(f32),
    Double(f64),
    /// An index into the string table.
    Str(String),
    /// `@path@`.
    Asset(String),
    Vec2f([f32; 2]),
    Vec3f([f32; 3]),
    Vec4f([f32; 4]),
    /// Row-major, the way USD writes one.
    Matrix4d([[f64; 4]; 4]),
    IntArray(Vec<i32>),
    Vec2fArray(Vec<[f32; 2]>),
    Vec3fArray(Vec<[f32; 3]>),
    /// A connection or a relationship target, as explicit paths.
    PathListOp(Vec<String>),
}

// Read off files USD wrote. See the module note.
pub const TYPE_BOOL: u8 = 1;
pub const TYPE_INT: u8 = 3;
pub const TYPE_FLOAT: u8 = 8;
pub const TYPE_DOUBLE: u8 = 9;
pub const TYPE_STRING: u8 = 10;
pub const TYPE_TOKEN: u8 = 11;
pub const TYPE_ASSET: u8 = 12;
pub const TYPE_MATRIX4D: u8 = 15;
pub const TYPE_VEC2F: u8 = 20;
pub const TYPE_VEC3F: u8 = 24;
pub const TYPE_VEC4F: u8 = 28;
pub const TYPE_TOKEN_LIST_OP: u8 = 32;
pub const TYPE_PATH_LIST_OP: u8 = 34;
pub const TYPE_TOKEN_VECTOR: u8 = 41;
pub const TYPE_SPECIFIER: u8 = 42;
pub const TYPE_VARIABILITY: u8 = 44;

/// The bits a list operation's header can carry. Only the two this writes are
/// named; the rest are the other ways USD can compose a list.
pub const LIST_OP_EXPLICIT: u8 = 0x01 | 0x02; // is explicit, and has explicit items
pub const LIST_OP_PREPENDED: u8 = 0x20;

impl Value {
    pub fn type_code(&self) -> u8 {
        match self {
            Value::Token(_) => TYPE_TOKEN,
            Value::TokenVector(_) => TYPE_TOKEN_VECTOR,
            Value::TokenArray(_) => TYPE_TOKEN,
            Value::TokenListOpPrepended(_) => TYPE_TOKEN_LIST_OP,
            Value::Specifier(_) => TYPE_SPECIFIER,
            Value::Variability(_) => TYPE_VARIABILITY,
            Value::Bool(_) => TYPE_BOOL,
            Value::Int(_) => TYPE_INT,
            Value::Float(_) => TYPE_FLOAT,
            Value::Double(_) => TYPE_DOUBLE,
            Value::Str(_) => TYPE_STRING,
            Value::Asset(_) => TYPE_ASSET,
            Value::Vec2f(_) | Value::Vec2fArray(_) => TYPE_VEC2F,
            Value::Vec3f(_) | Value::Vec3fArray(_) => TYPE_VEC3F,
            Value::Vec4f(_) => TYPE_VEC4F,
            Value::Matrix4d(_) => TYPE_MATRIX4D,
            Value::IntArray(_) => TYPE_INT,
            Value::PathListOp(_) => TYPE_PATH_LIST_OP,
        }
    }

    /// Whether the array bit is set. An `int` and an `int[]` share a type
    /// number and are told apart only by this.
    pub fn is_array(&self) -> bool {
        matches!(
            self,
            Value::IntArray(_)
                | Value::Vec2fArray(_)
                | Value::Vec3fArray(_)
                | Value::TokenArray(_)
        )
    }

    /// Every token this value mentions, so the table can be built before
    /// anything is written.
    pub fn tokens(&self) -> Vec<&str> {
        match self {
            Value::Token(t) | Value::Asset(t) => vec![t.as_str()],
            Value::TokenVector(v) | Value::TokenArray(v) | Value::TokenListOpPrepended(v) => {
                v.iter().map(String::as_str).collect()
            }
            // A string is a token too: the string table holds token indices.
            Value::Str(s) => vec![s.as_str()],
            _ => Vec::new(),
        }
    }

    /// The paths this value refers to, for the same reason.
    pub fn paths(&self) -> &[String] {
        match self {
            Value::PathListOp(p) => p,
            _ => &[],
        }
    }
}
