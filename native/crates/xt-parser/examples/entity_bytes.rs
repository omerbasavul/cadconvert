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
