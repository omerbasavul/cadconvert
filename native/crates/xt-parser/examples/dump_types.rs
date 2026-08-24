fn main() {
    let path = std::env::args().nth(1).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let (_hdr, body) = xt_parser::header::split_header(&text).unwrap();
    let tline = xt_parser::schema::parse_tline(body).unwrap();
    let mut input = tline.body.as_str();

    // Only files whose T-line names a base schema to diff against carry a
    // preamble and per-type inline schema annotations.
    let partition_count = if tline.has_base_schema {
        xt_parser::schema::parse_schema_preamble(&mut input)
            .unwrap()
            .partition_count
    } else {
        0
    };
    let (entities, truncated) = xt_parser::entity::parse_entities_opt(
        &mut input,
        partition_count,
        tline.has_base_schema,
        tline.key_major,
    )
    .unwrap();

    if let Some(t) = truncated {
        eprintln!("[xt-parser] {t}");
    }
    let n = entities.len();
    for (i, e) in entities.iter().enumerate().skip(n.saturating_sub(5)) {
        eprintln!("[{:3}] type={:3} idx={:4} fields={} var_f64={} var_i16={} var_ptr={} var_char={}",
            i, e.type_id, e.index, entities.fields(e).len(),
            e.var_f64().len(), e.var_i16().len(), e.var_ptr().len(), e.var_char().len());
    }
}
