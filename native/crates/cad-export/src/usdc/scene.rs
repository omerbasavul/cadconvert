//! A scene as crate specs.
//!
//! The same prims, the same fields and the same values the text writer emits —
//! see [`crate::usd`] for why each is what it is. This says nothing new about
//! USD; it says it in the encoding that does not cost four times the size.
//!
//! One difference, and it is deliberate. The text form writes each mesh once
//! under a class and references it from every placement. This writes the mesh
//! at each placement instead, because a reference is a composition arc with an
//! encoding of its own that has not been read off a file yet. It costs less
//! than it sounds: the pilot places 46 meshes 64 times, but the meshes that
//! repeat are the small ones, and the triangles go from 1 970 388 to
//! 2 121 018 — under eight per cent.

use super::value::Value;
use super::write::{Spec, SpecKind};
use cad_ir::material::Material;
use cad_ir::mesh::Mesh;
use cad_ir::math::Transform;
use cad_ir::scene::{NodeId, Scene};
use crate::Options;

/// Attribute variability: 0 varying, 1 uniform.
const VARYING: u32 = 0;
const UNIFORM: u32 = 1;
/// Prim specifier: `def`.
const DEF: u32 = 0;

pub struct Builder<'a> {
    scene: &'a Scene,
    specs: Vec<Spec>,
    image_paths: &'a [String],
}

pub fn specs(scene: &Scene, options: &Options, image_paths: &[String]) -> Vec<Spec> {
    let mut b = Builder {
        scene,
        specs: Vec::new(),
        image_paths,
    };

    let mut root_children: Vec<String> = Vec::new();
    if !scene.materials.is_empty() {
        root_children.push("Looks".into());
    }

    // Names have to be settled before the root's primChildren can be written,
    // and a prim's children are named in it.
    // Every placement, with the transform that gets it there. The scene keeps
    // a transform per node relative to its parent and nothing computes the
    // product, so it is accumulated on the way down. The prims come out flat:
    // a placement is a mesh at a matrix, and the tree it came from carried no
    // other meaning.
    let mut names = crate::usd::Names::default();
    let mut placements: Vec<(NodeId, String, Transform)> = Vec::new();
    for &root in &scene.roots {
        b.collect(root, Transform::IDENTITY, &mut names, &mut placements);
    }
    for (_, name, _) in &placements {
        root_children.push(name.clone());
    }

    b.specs.push(Spec {
        path: "/".into(),
        kind: SpecKind::PseudoRoot,
        fields: vec![
            ("defaultPrim", Value::Token("root".into())),
            ("metersPerUnit", Value::Double(1.0)),
            ("upAxis", Value::Token("Y".into())),
            ("primChildren", Value::TokenVector(vec!["root".into()])),
        ],
    });

    let m = options.root_transform().m;
    b.specs.push(Spec {
        path: "/root".into(),
        kind: SpecKind::Prim,
        fields: vec![
            ("specifier", Value::Specifier(DEF)),
            ("typeName", Value::Token("Xform".into())),
            ("kind", Value::Token("component".into())),
            ("primChildren", Value::TokenVector(root_children)),
            (
                "properties",
                Value::TokenVector(vec!["xformOp:transform".into(), "xformOpOrder".into()]),
            ),
        ],
    });
    b.matrix("/root.xformOp:transform", [
        [m[0][0], m[1][0], m[2][0], 0.0],
        [m[0][1], m[1][1], m[2][1], 0.0],
        [m[0][2], m[1][2], m[2][2], 0.0],
        [m[0][3], m[1][3], m[2][3], 1.0],
    ]);
    b.attribute(
        "/root.xformOpOrder",
        "token[]",
        UNIFORM,
        Value::TokenArray(vec!["xformOp:transform".into()]),
    );

    if !scene.materials.is_empty() {
        b.materials();
    }
    for (id, name, transform) in &placements {
        b.placement(*id, name, transform);
    }
    b.specs
}

impl Builder<'_> {
    /// Every node that will become a prim, named uniquely.
    fn collect(
        &self,
        id: NodeId,
        parent: Transform,
        names: &mut crate::usd::Names,
        out: &mut Vec<(NodeId, String, Transform)>,
    ) {
        let node = self.scene.node(id);
        let here = node.transform.then(&parent);
        let draws = node
            .geometry
            .and_then(|g| self.scene.geometry[g.index()].mesh.as_ref())
            .is_some_and(|m| !m.is_empty());
        if draws {
            out.push((id, names.unique(&node.name), here));
        }
        for &child in &node.children {
            self.collect(child, here, names, out);
        }
    }

    fn attribute(&mut self, path: &str, type_name: &str, variability: u32, value: Value) {
        self.specs.push(Spec {
            path: path.to_string(),
            kind: SpecKind::Attribute,
            fields: vec![
                ("custom", Value::Bool(false)),
                ("typeName", Value::Token(type_name.to_string())),
                ("variability", Value::Variability(variability)),
                ("default", value),
            ],
        });
    }

    /// An attribute with an interpolation, which is how a primvar says whether
    /// its values are per vertex or per face.
    fn primvar(&mut self, path: &str, type_name: &str, value: Value) {
        self.specs.push(Spec {
            path: path.to_string(),
            kind: SpecKind::Attribute,
            fields: vec![
                ("custom", Value::Bool(false)),
                ("typeName", Value::Token(type_name.to_string())),
                ("variability", Value::Variability(VARYING)),
                ("default", value),
                ("interpolation", Value::Token("vertex".into())),
            ],
        });
    }

    fn matrix(&mut self, path: &str, m: [[f64; 4]; 4]) {
        self.attribute(path, "matrix4d", VARYING, Value::Matrix4d(m));
    }

    fn connect(&mut self, path: &str, type_name: &str, target: &str) {
        self.specs.push(Spec {
            path: path.to_string(),
            kind: SpecKind::Attribute,
            fields: vec![
                ("custom", Value::Bool(false)),
                ("typeName", Value::Token(type_name.to_string())),
                ("variability", Value::Variability(VARYING)),
                ("connectionPaths", Value::PathListOp(vec![target.to_string()])),
            ],
        });
    }

    /// An output has a type and nothing else; what reads it names it.
    fn output(&mut self, path: &str, type_name: &str) {
        self.specs.push(Spec {
            path: path.to_string(),
            kind: SpecKind::Attribute,
            fields: vec![
                ("custom", Value::Bool(false)),
                ("typeName", Value::Token(type_name.to_string())),
                ("variability", Value::Variability(VARYING)),
            ],
        });
    }

    fn relationship(&mut self, path: &str, target: &str) {
        self.specs.push(Spec {
            path: path.to_string(),
            kind: SpecKind::Relationship,
            fields: vec![
                ("custom", Value::Bool(false)),
                ("variability", Value::Variability(UNIFORM)),
                ("targetPaths", Value::PathListOp(vec![target.to_string()])),
            ],
        });
    }

    fn materials(&mut self) {
        let names: Vec<String> = (0..self.scene.materials.len())
            .map(|i| crate::usd::material_name(self.scene, i as u32))
            .collect();
        self.specs.push(Spec {
            path: "/root/Looks".into(),
            kind: SpecKind::Prim,
            fields: vec![
                ("specifier", Value::Specifier(DEF)),
                ("typeName", Value::Token("Scope".into())),
                ("primChildren", Value::TokenVector(names.clone())),
            ],
        });
        for (i, name) in names.iter().enumerate() {
            let material = self.scene.materials[i].clone();
            self.material(name, &material);
        }
    }

    fn material(&mut self, name: &str, material: &Material) {
        let base = format!("/root/Looks/{name}");
        let textures = material.textures;
        let tile = textures.tile_mm();

        let mut children: Vec<String> = Vec::new();
        if !textures.is_empty() {
            children.push("stReader".into());
            if tile.is_some() {
                children.push("stTransform".into());
            }
        }
        if textures.base_colour.is_some() {
            children.push("diffuseTexture".into());
        }
        if textures.normal.is_some() {
            children.push("normalTexture".into());
        }
        children.push("surface".into());

        self.specs.push(Spec {
            path: base.clone(),
            kind: SpecKind::Prim,
            fields: vec![
                ("specifier", Value::Specifier(DEF)),
                ("typeName", Value::Token("Material".into())),
                ("primChildren", Value::TokenVector(children)),
                ("properties", Value::TokenVector(vec!["outputs:surface".into()])),
            ],
        });
        self.connect(
            &format!("{base}.outputs:surface"),
            "token",
            &format!("{base}/surface.outputs:surface"),
        );

        if !textures.is_empty() {
            self.shader(
                &format!("{base}/stReader"),
                "UsdPrimvarReader_float2",
                vec![
                    ("inputs:varname", "string", Value::Str("st".into())),
                ],
                vec![],
                vec![("outputs:result", "float2")],
            );
            if let Some([w, h]) = tile {
                self.shader(
                    &format!("{base}/stTransform"),
                    "UsdTransform2d",
                    vec![("inputs:scale", "float2", Value::Vec2f([1.0 / w, 1.0 / h]))],
                    vec![(
                        "inputs:in",
                        "float2",
                        format!("{base}/stReader.outputs:result"),
                    )],
                    vec![("outputs:result", "float2")],
                );
            }
        }

        let st_source = if tile.is_some() {
            format!("{base}/stTransform.outputs:result")
        } else {
            format!("{base}/stReader.outputs:result")
        };

        if let Some(id) = textures.base_colour {
            let c = material.base_color;
            self.texture(
                &format!("{base}/diffuseTexture"),
                self.image_paths.get(id.index()).cloned().unwrap_or_default(),
                &st_source,
                "sRGB",
                Some([c[0], c[1], c[2], 1.0]),
                None,
            );
        }
        if let Some(id) = textures.normal {
            self.texture(
                &format!("{base}/normalTexture"),
                self.image_paths.get(id.index()).cloned().unwrap_or_default(),
                &st_source,
                // Never sRGB: these are vectors, not colours.
                "raw",
                // An 8-bit normal map holds 0..1 and a normal runs -1..1.
                Some([2.0, 2.0, 2.0, 1.0]),
                Some([-1.0, -1.0, -1.0, 0.0]),
            );
        }

        let mut values: Vec<(&'static str, &'static str, Value)> = vec![
            ("inputs:metallic", "float", Value::Float(material.metallic)),
            ("inputs:roughness", "float", Value::Float(material.roughness)),
        ];
        if (material.ior - 1.5).abs() > 1e-3 {
            values.push(("inputs:ior", "float", Value::Float(material.ior)));
        }
        if material.alpha < 1.0 {
            values.push(("inputs:opacity", "float", Value::Float(material.alpha)));
        }
        if material.emissive.iter().any(|&c| c > 0.0) {
            values.push((
                "inputs:emissiveColor",
                "color3f",
                Value::Vec3f(material.emissive),
            ));
        }
        let mut connections: Vec<(&'static str, &'static str, String)> = Vec::new();
        if textures.base_colour.is_some() {
            connections.push((
                "inputs:diffuseColor",
                "color3f",
                format!("{base}/diffuseTexture.outputs:rgb"),
            ));
        } else {
            values.push((
                "inputs:diffuseColor",
                "color3f",
                Value::Vec3f(material.base_color),
            ));
        }
        if textures.normal.is_some() {
            connections.push((
                "inputs:normal",
                "normal3f",
                format!("{base}/normalTexture.outputs:rgb"),
            ));
        }
        self.shader(
            &format!("{base}/surface"),
            "UsdPreviewSurface",
            values,
            connections,
            vec![("outputs:surface", "token")],
        );
    }

    #[allow(clippy::type_complexity)]
    fn shader(
        &mut self,
        path: &str,
        id: &str,
        values: Vec<(&'static str, &'static str, Value)>,
        connections: Vec<(&'static str, &'static str, String)>,
        outputs: Vec<(&'static str, &'static str)>,
    ) {
        let mut properties: Vec<String> = vec!["info:id".into()];
        properties.extend(values.iter().map(|(n, _, _)| (*n).to_string()));
        properties.extend(connections.iter().map(|(n, _, _)| (*n).to_string()));
        properties.extend(outputs.iter().map(|(n, _)| (*n).to_string()));

        self.specs.push(Spec {
            path: path.to_string(),
            kind: SpecKind::Prim,
            fields: vec![
                ("specifier", Value::Specifier(DEF)),
                ("typeName", Value::Token("Shader".into())),
                ("properties", Value::TokenVector(properties)),
            ],
        });
        self.attribute(
            &format!("{path}.info:id"),
            "token",
            UNIFORM,
            Value::Token(id.to_string()),
        );
        for (name, type_name, value) in values {
            self.attribute(&format!("{path}.{name}"), type_name, VARYING, value);
        }
        for (name, type_name, target) in connections {
            self.connect(&format!("{path}.{name}"), type_name, &target);
        }
        for (name, type_name) in outputs {
            self.output(&format!("{path}.{name}"), type_name);
        }
    }

    fn texture(
        &mut self,
        path: &str,
        file: String,
        st_source: &str,
        colour_space: &str,
        scale: Option<[f32; 4]>,
        bias: Option<[f32; 4]>,
    ) {
        let mut values: Vec<(&'static str, &'static str, Value)> = vec![
            ("inputs:file", "asset", Value::Asset(file)),
            ("inputs:wrapS", "token", Value::Token("repeat".into())),
            ("inputs:wrapT", "token", Value::Token("repeat".into())),
            (
                "inputs:sourceColorSpace",
                "token",
                Value::Token(colour_space.to_string()),
            ),
        ];
        if let Some(s) = scale {
            values.push(("inputs:scale", "float4", Value::Vec4f(s)));
        }
        if let Some(b) = bias {
            values.push(("inputs:bias", "float4", Value::Vec4f(b)));
        }
        self.shader(
            path,
            "UsdUVTexture",
            values,
            vec![("inputs:st", "float2", st_source.to_string())],
            vec![("outputs:rgb", "float3")],
        );
    }

    fn placement(&mut self, id: NodeId, name: &str, transform: &Transform) {
        let node = self.scene.node(id);
        let Some(g) = node.geometry else { return };
        let Some(mesh) = self.scene.geometry[g.index()].mesh.as_ref() else {
            return;
        };
        let path = format!("/root/{name}");
        let transform = *transform;

        let mut properties: Vec<String> = vec![
            "faceVertexCounts".into(),
            "faceVertexIndices".into(),
            "points".into(),
        ];
        if mesh.normals.len() == mesh.positions.len() {
            properties.push("normals".into());
        }
        if mesh.uvs.len() == mesh.positions.len() {
            properties.push("primvars:st".into());
        }
        properties.push("subdivisionScheme".into());

        let double_sided = mesh
            .parts
            .iter()
            .filter_map(|p| self.scene.materials.get(p.material as usize))
            .any(|m| m.double_sided);
        if double_sided {
            properties.push("doubleSided".into());
        }
        let single = mesh.parts.len() == 1;
        if single {
            properties.push("material:binding".into());
        }
        if !transform.is_identity(1e-12) {
            properties.push("xformOp:transform".into());
            properties.push("xformOpOrder".into());
        }

        let subsets: Vec<String> = if single {
            Vec::new()
        } else {
            (0..mesh.parts.len()).map(|i| format!("part_{i}")).collect()
        };

        let mut fields: Vec<(&'static str, Value)> = vec![
            ("specifier", Value::Specifier(DEF)),
            ("typeName", Value::Token("Mesh".into())),
            ("properties", Value::TokenVector(properties)),
        ];
        if single {
            fields.push((
                "apiSchemas",
                Value::TokenListOpPrepended(vec!["MaterialBindingAPI".into()]),
            ));
        }
        if !subsets.is_empty() {
            fields.push(("primChildren", Value::TokenVector(subsets.clone())));
        }
        self.specs.push(Spec {
            path: path.clone(),
            kind: SpecKind::Prim,
            fields,
        });

        self.geometry(&path, mesh);

        if !transform.is_identity(1e-12) {
            let m = transform.m;
            self.matrix(&format!("{path}.xformOp:transform"), [
                [m[0][0], m[1][0], m[2][0], 0.0],
                [m[0][1], m[1][1], m[2][1], 0.0],
                [m[0][2], m[1][2], m[2][2], 0.0],
                [m[0][3], m[1][3], m[2][3], 1.0],
            ]);
            self.attribute(
                &format!("{path}.xformOpOrder"),
                "token[]",
                UNIFORM,
                Value::TokenArray(vec!["xformOp:transform".into()]),
            );
        }
        if double_sided {
            self.attribute(
                &format!("{path}.doubleSided"),
                "bool",
                UNIFORM,
                Value::Bool(true),
            );
        }
        if single {
            let material = crate::usd::material_name(self.scene, mesh.parts[0].material);
            self.relationship(
                &format!("{path}.material:binding"),
                &format!("/root/Looks/{material}"),
            );
        } else {
            self.attribute(
                &format!("{path}.subsetFamily:materialBind:familyType"),
                "token",
                UNIFORM,
                Value::Token("partition".into()),
            );
            for (i, part) in mesh.parts.iter().enumerate() {
                let subset = format!("{path}/part_{i}");
                let first = part.start as usize / 3;
                let indices: Vec<i32> =
                    (0..part.count as usize / 3).map(|k| (first + k) as i32).collect();
                self.specs.push(Spec {
                    path: subset.clone(),
                    kind: SpecKind::Prim,
                    fields: vec![
                        ("specifier", Value::Specifier(DEF)),
                        ("typeName", Value::Token("GeomSubset".into())),
                        (
                            "properties",
                            Value::TokenVector(vec![
                                "elementType".into(),
                                "familyName".into(),
                                "indices".into(),
                                "material:binding".into(),
                            ]),
                        ),
                        (
                            "apiSchemas",
                            Value::TokenListOpPrepended(vec!["MaterialBindingAPI".into()]),
                        ),
                    ],
                });
                self.attribute(
                    &format!("{subset}.elementType"),
                    "token",
                    UNIFORM,
                    Value::Token("face".into()),
                );
                self.attribute(
                    &format!("{subset}.familyName"),
                    "token",
                    UNIFORM,
                    Value::Token("materialBind".into()),
                );
                self.attribute(
                    &format!("{subset}.indices"),
                    "int[]",
                    VARYING,
                    Value::IntArray(indices),
                );
                let material = crate::usd::material_name(self.scene, part.material);
                self.relationship(
                    &format!("{subset}.material:binding"),
                    &format!("/root/Looks/{material}"),
                );
            }
        }
    }

    fn geometry(&mut self, path: &str, mesh: &Mesh) {
        self.attribute(
            &format!("{path}.faceVertexCounts"),
            "int[]",
            VARYING,
            Value::IntArray(vec![3; mesh.triangle_count()]),
        );
        self.attribute(
            &format!("{path}.faceVertexIndices"),
            "int[]",
            VARYING,
            Value::IntArray(mesh.indices.iter().map(|&i| i as i32).collect()),
        );
        self.attribute(
            &format!("{path}.points"),
            "point3f[]",
            VARYING,
            Value::Vec3fArray(mesh.positions.clone()),
        );
        if mesh.normals.len() == mesh.positions.len() {
            self.primvar(
                &format!("{path}.normals"),
                "normal3f[]",
                Value::Vec3fArray(mesh.normals.clone()),
            );
        }
        if mesh.uvs.len() == mesh.positions.len() {
            // glTF's texture origin is at the top left and USD's is at the
            // bottom left; the exporter negated v once on the way out.
            self.primvar(
                &format!("{path}.primvars:st"),
                "texCoord2f[]",
                Value::Vec2fArray(mesh.uvs.iter().map(|uv| [uv[0], -uv[1]]).collect()),
            );
        }
        self.attribute(
            &format!("{path}.subdivisionScheme"),
            "token",
            UNIFORM,
            Value::Token("none".into()),
        );
    }
}
