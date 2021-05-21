use pgx::*;

pg_module_magic!();

/// ```sql
/// CREATE OR REPLACE FUNCTION trigger_example()
///            RETURNS TRIGGER
///            LANGUAGE c
///            AS 'MODULE_PATHNAME', 'trigger_example_wrapper';
/// ```
#[pg_extern]
unsafe fn trigger_example(fcinfo: pg_sys::FunctionCallInfo) -> pg_sys::Datum {
    // we can only be called as a trigger
    if !called_as_trigger(fcinfo) {
        panic!("not called by trigger manager");
    }

    let trigdata: PgBox<pg_sys::TriggerData> = PgBox::from_pg(
        fcinfo.as_ref().expect("fcinfo is NULL").context as *mut pg_sys::TriggerData,
    );

    // and for this example, we're only going to operate as an ON BEFORE INSERT FOR EACH ROW trigger
    if trigger_fired_before(trigdata.tg_event)
        && trigger_fired_by_insert(trigdata.tg_event)
        && trigger_fired_for_row(trigdata.tg_event)
    {
        let tupdesc = PgTupleDesc::from_pg_copy(trigdata.tg_relation.as_ref().unwrap().rd_att);
        let tuple = PgBox::from_pg(trigdata.tg_trigtuple);
        let id = heap_getattr::<i64>(&tuple, 1, &tupdesc);
        let title = heap_getattr::<&str>(&tuple, 2, &tupdesc);
        let description = heap_getattr::<&str>(&tuple, 3, &tupdesc);
        let payload = heap_getattr::<JsonB>(&tuple, 4, &tupdesc);

        warning!(
            "id={:?}, title={:?}, description={:?}, payload={:?}",
            id,
            title,
            description,
            payload
        );

        // return the inserting tuple, unchanged
        trigdata.tg_trigtuple as pg_sys::Datum
    } else {
        panic!("not fired in the ON BEFORE INSERT context");
    }
}

extension_sql!(
    r#"
CREATE TABLE test (
    id serial8 NOT NULL PRIMARY KEY,
    title varchar(50),
    description text,
    payload jsonb
);

CREATE TRIGGER test_trigger BEFORE INSERT ON test FOR EACH ROW EXECUTE FUNCTION trigger_example();
INSERT INTO test (title, description, payload) VALUES ('the title', 'a description', '{"key": "value"}');

"#
);

#[cfg(any(test, feature = "pg_test"))]
mod tests {
    use pgx::*;

    #[pg_test]
    fn test_uuid_set() {
        // Nothing
    }
}

#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {
        // perform one-off initialization when the pg_test framework starts
    }

    pub fn postgresql_conf_options() -> Vec<&'static str> {
        // return any postgresql.conf settings that are required for your tests
        vec![]
    }
}
