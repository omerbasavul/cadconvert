//! Where the entity graph's bytes go: data against allocation count.

#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let p = std::env::args().nth(1).unwrap();
    let bytes = std::fs::read(&p)?;
    let text = xt_parser::decode(bytes);
    let raw = xt_parser::parse_raw_owned(text)?;
    let n = raw.entities.len();
    println!("size_of::<RawEntity>() = {}", size_of::<xt_parser::entity::RawEntity>());
    println!("size_of::<FieldVal>()  = {}", size_of::<xt_parser::entity::FieldVal>());
    println!("{n} entities, {} arena fields", raw.entities.arena_len());

    let (mut boxes, mut tails, mut tail_bytes) = (0usize, 0usize, 0usize);
    for e in raw.entities.iter() {
        for f in raw.entities.fields(e).iter().chain(raw.entities.extra(e)) {
            if matches!(f, xt_parser::entity::FieldVal::Vec3(_)
                         | xt_parser::entity::FieldVal::Interval(_)) { boxes += 1; }
        }
        let v = raw.entities.var_f64(e).len() * 8 + raw.entities.var_i16(e).len() * 2 + raw.entities.var_i32(e).len() * 8
              + raw.entities.var_ptr(e).len() * 8 + raw.entities.var_char(e).len() * 4;
        if v > 0 || raw.entities.var_f64(e).len() + raw.entities.var_i16(e).len() + raw.entities.var_i32(e).len()
                  + raw.entities.var_ptr(e).len() + raw.entities.var_char(e).len() > 0 { tails += 1; }
        tail_bytes += v;
    }
    let entity_mb = n * size_of::<xt_parser::entity::RawEntity>();
    let arena_mb = raw.entities.arena_len() * size_of::<xt_parser::entity::FieldVal>();
    println!("  entity structs      {:>7.1} MB", entity_mb as f64 / 1e6);
    println!("  field arena         {:>7.1} MB", arena_mb as f64 / 1e6);
    println!("  boxed vec3/interval {:>7} of them, {:.1} MB of payload", boxes, boxes as f64 * 24.0 / 1e6);
    println!("  var tails           {:>7} entities have one, {:.1} MB of payload", tails, tail_bytes as f64 / 1e6);
    println!("  ---");
    println!("  allocations for the tails and boxes: {}", boxes + tails * 2);
    println!("  payload counted     {:>7.1} MB",
        (entity_mb + arena_mb) as f64 / 1e6 + boxes as f64 * 24.0 / 1e6 + tail_bytes as f64 / 1e6);
    Ok(())
}
