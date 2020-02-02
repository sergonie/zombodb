use crate::elasticsearch::{Elasticsearch, ElasticsearchBulkRequest};
use crate::json::builder::JsonBuilder;
use pgx::*;
use std::ops::DerefMut;

struct Attribute<'a> {
    dropped: bool,
    name: &'a str,
    typoid: PgOid,
}

struct BuildState<'a> {
    ntuples: usize,
    bulk: ElasticsearchBulkRequest,
    tupdesc: &'a PgBox<pg_sys::TupleDescData>,
    attributes: Vec<Attribute<'a>>,
}

impl<'a> BuildState<'a> {
    fn new(url: &'a str, index_name: &'a str, tupdesc: &'a PgBox<pg_sys::TupleDescData>) -> Self {
        let mut attributes = Vec::new();
        for i in 0..tupdesc.natts {
            let attr = tupdesc_get_attr(&tupdesc, i as usize);
            attributes.push(Attribute {
                dropped: attr.attisdropped,
                name: name_data_to_str(&attr.attname),
                typoid: PgOid::from(attr.atttypid),
            });
        }

        BuildState {
            ntuples: 0,
            bulk: Elasticsearch::new(url, index_name).start_bulk(),
            tupdesc: &tupdesc,
            attributes,
        }
    }
}

#[pg_guard]
pub extern "C" fn ambuild(
    heap_relation: pg_sys::Relation,
    index_relation: pg_sys::Relation,
    index_info: *mut pg_sys::IndexInfo,
) -> *mut pg_sys::IndexBuildResult {
    let mut result = PgBox::<pg_sys::IndexBuildResult>::alloc0();
    let result_mut = result.deref_mut();

    let tupdesc = PgBox::from_pg(PgBox::from_pg(index_relation).rd_att);
    // lookup the tuple descriptor for the rowtype we're *indexing*, rather than
    // using the tuple descriptor for the index definition itself
    let tupdesc = PgBox::from_pg(unsafe {
        pg_sys::lookup_rowtype_tupdesc(
            tupdesc_get_typoid(&tupdesc, 1),
            tupdesc_get_typmod(&tupdesc, 1),
        )
    });

    let mut state = BuildState::new("http://localhost:9200/", "test_index", &tupdesc);

    unsafe {
        pg_sys::IndexBuildHeapScan(
            heap_relation,
            index_relation,
            index_info,
            Some(build_callback),
            &mut state,
        );
    }
    if tupdesc.tdrefcount >= 0 {
        unsafe {
            pg_sys::DecrTupleDescRefCount(tupdesc.as_ptr());
        }
    }

    info!("Waiting to finish");
    match state.bulk.wait_for_completion() {
        Ok(cnt) => info!("indexed {} tuples", cnt),
        Err(e) => panic!("{:?}", e),
    }

    info!("ntuples={}", state.ntuples);
    result_mut.heap_tuples = state.ntuples as f64;
    result_mut.index_tuples = state.ntuples as f64;

    result.into_pg()
}

#[pg_guard]
pub extern "C" fn ambuildempty(_index_relation: pg_sys::Relation) {}

#[pg_guard]
pub extern "C" fn aminsert(
    _index_relation: pg_sys::Relation,
    _values: *mut pg_sys::Datum,
    _isnull: *mut bool,
    _heap_tid: pg_sys::ItemPointer,
    _heap_relation: pg_sys::Relation,
    _check_unique: pg_sys::IndexUniqueCheck,
    _index_info: *mut pg_sys::IndexInfo,
) -> bool {
    info!("aminsert");
    false
}

unsafe extern "C" fn build_callback(
    _index: pg_sys::Relation,
    htup: pg_sys::HeapTuple,
    values: *mut pg_sys::Datum,
    _isnull: *mut bool,
    _tuple_is_alive: bool,
    state: *mut std::os::raw::c_void,
) {
    check_for_interrupts!();

    let htup = PgBox::from_pg(htup);
    let mut state = PgBox::from_pg(state as *mut BuildState);
    let values = std::slice::from_raw_parts(values, 1);
    let builder = row_to_json(values[0], &state);

    state
        .bulk
        .insert(htup.t_self, 0, 0, 0, 0, builder)
        .expect("Unable to send tuple for insert");
    state.ntuples += 1;
}

unsafe fn row_to_json<'a>(row: pg_sys::Datum, state: &PgBox<BuildState>) -> JsonBuilder {
    let mut row_data = JsonBuilder::new(state.attributes.len());

    let datums = deconstruct_row_type(state.tupdesc, row);
    for (attr, datum) in state.attributes.iter().zip(datums.iter()) {
        if attr.dropped {
            continue;
        }

        match datum {
            None => {
                // we don't bother to encode null values
            }
            Some(datum) => {
                match &attr.typoid {
                    PgOid::InvalidOid => panic!("Found InvalidOid for attname='{}'", attr.name),
                    PgOid::Custom(oid) => {
                        // TODO:  what to do here?
                        unimplemented!("Found custom oid={}", oid);
                    }
                    PgOid::BuiltIn(oid) => match oid {
                        PgBuiltInOids::TEXTOID | PgBuiltInOids::VARCHAROID => {
                            row_data.add_string(
                                attr.name,
                                String::from_datum(datum, false, attr.typoid.value()).unwrap(),
                            );
                        }
                        PgBuiltInOids::BOOLOID => {
                            row_data.add_bool(
                                attr.name,
                                bool::from_datum(datum, false, attr.typoid.value()).unwrap(),
                            );
                        }
                        PgBuiltInOids::INT2OID => {
                            row_data.add_i16(
                                attr.name,
                                i16::from_datum(datum, false, attr.typoid.value()).unwrap(),
                            );
                        }
                        PgBuiltInOids::INT4OID => {
                            row_data.add_i32(
                                attr.name,
                                i32::from_datum(datum, false, attr.typoid.value()).unwrap(),
                            );
                        }
                        PgBuiltInOids::INT8OID => {
                            row_data.add_i64(
                                attr.name,
                                i64::from_datum(datum, false, attr.typoid.value()).unwrap(),
                            );
                        }
                        PgBuiltInOids::OIDOID | PgBuiltInOids::XIDOID => {
                            row_data.add_u32(
                                attr.name,
                                u32::from_datum(datum, false, attr.typoid.value()).unwrap(),
                            );
                        }
                        PgBuiltInOids::FLOAT4OID => {
                            row_data.add_f32(
                                attr.name,
                                f32::from_datum(datum, false, attr.typoid.value()).unwrap(),
                            );
                        }
                        PgBuiltInOids::FLOAT8OID => {
                            row_data.add_f64(
                                attr.name,
                                f64::from_datum(datum, false, attr.typoid.value()).unwrap(),
                            );
                        }
                        PgBuiltInOids::JSONOID => {
                            row_data.add_json_string(
                                attr.name,
                                pgx::JsonString::from_datum(datum, false, attr.typoid.value())
                                    .unwrap(),
                            );
                        }
                        PgBuiltInOids::JSONBOID => {
                            row_data.add_jsonb(
                                attr.name,
                                JsonB::from_datum(datum, false, attr.typoid.value()).unwrap(),
                            );
                        }

                        PgBuiltInOids::TEXTARRAYOID | PgBuiltInOids::VARCHARARRAYOID => {
                            row_data.add_string_array(
                                attr.name,
                                Vec::<Option<String>>::from_datum(
                                    datum,
                                    false,
                                    attr.typoid.value(),
                                )
                                .unwrap(),
                            );
                        }
                        PgBuiltInOids::BOOLARRAYOID => {
                            row_data.add_bool_array(
                                attr.name,
                                Vec::<Option<bool>>::from_datum(datum, false, attr.typoid.value())
                                    .unwrap(),
                            );
                        }
                        PgBuiltInOids::INT2ARRAYOID => {
                            row_data.add_i16_array(
                                attr.name,
                                Vec::<Option<i16>>::from_datum(datum, false, attr.typoid.value())
                                    .unwrap(),
                            );
                        }
                        PgBuiltInOids::INT4ARRAYOID => {
                            row_data.add_i32_array(
                                attr.name,
                                Vec::<Option<i32>>::from_datum(datum, false, attr.typoid.value())
                                    .unwrap(),
                            );
                        }
                        PgBuiltInOids::INT8ARRAYOID => {
                            row_data.add_i64_array(
                                attr.name,
                                Vec::<Option<i64>>::from_datum(datum, false, attr.typoid.value())
                                    .unwrap(),
                            );
                        }
                        PgBuiltInOids::OIDARRAYOID | PgBuiltInOids::XMLARRAYOID => {
                            row_data.add_u32_array(
                                attr.name,
                                Vec::<Option<u32>>::from_datum(datum, false, attr.typoid.value())
                                    .unwrap(),
                            );
                        }
                        PgBuiltInOids::FLOAT4ARRAYOID => {
                            row_data.add_f32_array(
                                attr.name,
                                Vec::<Option<f32>>::from_datum(datum, false, attr.typoid.value())
                                    .unwrap(),
                            );
                        }
                        PgBuiltInOids::FLOAT8ARRAYOID => {
                            row_data.add_f64_array(
                                attr.name,
                                Vec::<Option<f64>>::from_datum(datum, false, attr.typoid.value())
                                    .unwrap(),
                            );
                        }
                        PgBuiltInOids::JSONARRAYOID => {
                            row_data.add_json_string_array(
                                attr.name,
                                Vec::<Option<pgx::JsonString>>::from_datum(
                                    datum,
                                    false,
                                    attr.typoid.value(),
                                )
                                .unwrap(),
                            );
                        }
                        PgBuiltInOids::JSONBARRAYOID => {
                            row_data.add_jsonb_array(
                                attr.name,
                                Vec::<Option<JsonB>>::from_datum(datum, false, attr.typoid.value())
                                    .unwrap(),
                            );
                        }
                        _ => {
                            // row_data.add_string(attr.name, "UNSUPPORTED TYPE".to_string());
                            row_data.add_bool(attr.name, false);
                        }
                    },
                }
            }
        }
    }

    row_data
}