//! What an entity graph costs, and where.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use xt_parser::entity::{FieldVal, RawEntity};
    println!("  size_of RawEntity {:>4}   size_of FieldVal {:>4}",
        size_of::<RawEntity>(), size_of::<FieldVal>());

    let Some(path) = std::env::args().nth(1) else { return Ok(()) };
    let bytes = std::fs::read(&path)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let (header, body) = xt_parser::header::split_header(&text)?;
    let _ = xt_parser::header::parse_header(header)?;
    let tline = xt_parser::schema::parse_tline(body)?;
    let mut input = tline.body.as_str();
    let partitions = if tline.has_base_schema {
        xt_parser::schema::parse_schema_preamble(&mut input)
            .map(|p| p.partition_count)
            .unwrap_or(0)
    } else {
        0
    };
    let (entities, _) = xt_parser::entity::parse_entities_opt(
        &mut input, partitions, tline.has_base_schema, tline.key_major,
    )?;

    let mut fields = 0usize;
    let mut mat3 = 0usize;
    let mut vec3 = 0usize;
    let mut with_var = 0usize;
    let mut var_items = 0usize;
    let mut extra = 0usize;
    for e in &entities {
        fields += e.fields.len();
        extra += e.extra.len();
        for f in e.fields.iter().chain(&e.extra) {
            match f {
                FieldVal::Mat3(_) => mat3 += 1,
                FieldVal::Vec3(_) => vec3 += 1,
                _ => {}
            }
        }
        let v = e.var_f64().len() + e.var_i16().len() + e.var_i32().len()
            + e.var_ptr().len() + e.var_char().len();
        var_items += v;
        if v > 0 { with_var += 1; }
    }
    let n = entities.len();
    println!("  {n} entities, {fields} fields, {extra} extra");
    println!("    Mat3 fields  {mat3:>8}  ({:.3}% of them)", 100.0 * mat3 as f64 / (fields + extra) as f64);
    println!("    Vec3 fields  {vec3:>8}  ({:.1}%)", 100.0 * vec3 as f64 / (fields + extra) as f64);
    // Every variant, because which of them sizes the enum is the whole
    // question: 24 bytes for a Vec3 makes every field 32, and 95% of fields
    // hold eight or less.
    let mut by_variant = std::collections::BTreeMap::<&str, usize>::new();
    for e in &entities {
        for f in e.fields.iter().chain(e.extra.iter()) {
            *by_variant.entry(match f {
                FieldVal::Int(_) => "Int(8)",
                FieldVal::Float(_) => "Float(8)",
                FieldVal::Short(_) => "Short(2)",
                FieldVal::Char(_) => "Char(4)",
                FieldVal::Bool(_) => "Bool(1)",
                FieldVal::Byte(_) => "Byte(1)",
                FieldVal::Ptr(_) => "Ptr(8)",
                FieldVal::Vec3(_) => "Vec3(24)",
                FieldVal::Interval(_) => "Interval(16)",
                FieldVal::Mat3(_) => "Mat3(boxed)",
            }).or_default() += 1;
        }
    }
    let total: usize = by_variant.values().sum();
    println!("  every field by variant:");
    for (name, n) in &by_variant {
        println!("    {name:<14} {n:>9}  {:>5.1}%", 100.0 * *n as f64 / total as f64);
    }
    println!("    entities with any variable-length data: {with_var} ({:.1}%), {var_items} items between them",
        100.0 * with_var as f64 / n as f64);
    println!("\n  where the bytes are, as it stands:");
    let structs = n * size_of::<RawEntity>();
    let field_bytes = (fields + extra) * size_of::<FieldVal>();
    println!("    the structs themselves  {:>7.1} MB", structs as f64 / 1e6);
    println!("    their fields            {:>7.1} MB", field_bytes as f64 / 1e6);
    println!("    (plus one malloc header per non-empty Vec, seven per entity)");
    Ok(())
}
