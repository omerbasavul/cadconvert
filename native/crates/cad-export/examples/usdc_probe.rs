//! Write a crate file by hand and let USD read it back.
//!
//! `cargo run -p cad-export --example usdc_probe -- out.usdc`

use cad_export::usdc::{write, Spec, SpecKind, Value};

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "probe.usdc".into());

    let specs = vec![
        Spec {
            path: "/".into(),
            kind: SpecKind::PseudoRoot,
            fields: vec![
                ("defaultPrim", Value::Token("root".into())),
                ("metersPerUnit", Value::Double(1.0)),
                ("upAxis", Value::Token("Y".into())),
                ("primChildren", Value::TokenVector(vec!["root".into()])),
            ],
        },
        Spec {
            path: "/root".into(),
            kind: SpecKind::Prim,
            fields: vec![
                ("specifier", Value::Specifier(0)),
                ("typeName", Value::Token("Xform".into())),
                ("primChildren", Value::TokenVector(vec!["m".into()])),
            ],
        },
        Spec {
            path: "/root/m".into(),
            kind: SpecKind::Prim,
            fields: vec![
                ("specifier", Value::Specifier(0)),
                ("typeName", Value::Token("Mesh".into())),
                (
                    "properties",
                    Value::TokenVector(vec![
                        "faceVertexCounts".into(),
                        "faceVertexIndices".into(),
                        "points".into(),
                        "subdivisionScheme".into(),
                    ]),
                ),
            ],
        },
        Spec {
            path: "/root/m.faceVertexCounts".into(),
            kind: SpecKind::Attribute,
            fields: vec![
                ("custom", Value::Bool(false)),
                ("typeName", Value::Token("int[]".into())),
                ("variability", Value::Variability(0)),
                ("default", Value::IntArray(vec![3])),
            ],
        },
        Spec {
            path: "/root/m.faceVertexIndices".into(),
            kind: SpecKind::Attribute,
            fields: vec![
                ("custom", Value::Bool(false)),
                ("typeName", Value::Token("int[]".into())),
                ("variability", Value::Variability(0)),
                ("default", Value::IntArray(vec![0, 1, 2])),
            ],
        },
        Spec {
            path: "/root/m.points".into(),
            kind: SpecKind::Attribute,
            fields: vec![
                ("custom", Value::Bool(false)),
                ("typeName", Value::Token("point3f[]".into())),
                ("variability", Value::Variability(0)),
                (
                    "default",
                    Value::Vec3fArray(vec![
                        [0.0, 0.0, 0.0],
                        [1.0, 0.0, 0.0],
                        [0.0, 1.0, 0.0],
                    ]),
                ),
            ],
        },
        Spec {
            path: "/root/m.subdivisionScheme".into(),
            kind: SpecKind::Attribute,
            fields: vec![
                ("custom", Value::Bool(false)),
                ("typeName", Value::Token("token".into())),
                ("variability", Value::Variability(1)),
                ("default", Value::Token("none".into())),
            ],
        },
    ];

    let bytes = write(&specs);
    std::fs::write(&out, &bytes).expect("write");
    println!("{out}  {} bytes", bytes.len());
}
