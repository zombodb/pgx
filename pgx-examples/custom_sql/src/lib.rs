// Copyright 2020 ZomboDB, LLC <zombodb@gmail.com>. All rights reserved. Use of this source code is
// governed by the MIT license that can be found in the LICENSE file.

use pgx::*;
use serde::{Deserialize, Serialize};

pg_module_magic!();

mod home {
    use super::*;

    #[derive(PostgresEnum, Serialize, Deserialize)]
    pub enum Dog {
        Brandy,
        Nami,
        Koda,
    }

    #[derive(PostgresType, Serialize, Deserialize)]
    pub struct Ball {
        last_chomp: Dog,
    }
}
use home::Dog;

// `extension_sql` allows you to define your own custom SQL.
//
// Since PostgreSQL is is order dependent, you may need to specify a positioning.
//
// Valid options are:
//  * `bootstrap` positions the block before any other generated SQL. It should be unique.
//     Errors if `before`/`after` are also present.
//  * `before = $ident` & `after = $ident` positions the block before/after `$ident`
//    where `$ident` is a string identifier or a path to a SQL entity (such as a type which derives
//    `PostgresType`)
//  * `before = [$ident, $ident2, ident3]` & `after = [$ident]` A variable number of positionings.
//  * `name` is an optional string identifier for the item, in case you need to refer to it in
//    other positioning.
extension_sql!(
    "\n\
        CREATE TABLE extension_sql VALUES (message TEXT);\n\
        INSERT INTO extension_sql VALUES ('bootstrap');\n\
    ",
    name = "bootstrap",
    bootstrap,
);
extension_sql!(
    "\n
        INSERT INTO extension_sql VALUES ('single');\n\
    ",
    name = "single",
    after = ["bootstrap"],
);
extension_sql!(
    "\n\
    INSERT INTO extension_sql VALUES ('multiple');\n\
",
    after = [Dog, home::Ball],
    before = ["single"], // This points to the above `extension_sql!()` with `name = multiple`
);

// `extension_sql_file` does the same as `extension_sql` but automatically sets the `name` to the
// filename (not the full path).
extension_sql_file!("../sql/single.sql", after = ["bootstrap"]);
extension_sql_file!(
    "../sql/multiple.sql",
    after = [Dog, home::Ball],
    before = ["single.sql"],
);
extension_sql_file!("../sql/finalizer.sql", finalize);

#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {
        todo!();
        // perform one-off initialization when the pg_test framework starts
    }

    pub fn postgresql_conf_options() -> Vec<&'static str> {
        // return any postgresql.conf settings that are required for your tests
        vec![]
    }
}
