// Copyright 2020 ZomboDB, LLC <zombodb@gmail.com>. All rights reserved. Use of this source code is
// governed by the MIT license that can be found in the LICENSE file.

#[macro_use]
extern crate clap;

mod commands;

use crate::commands::connect::connect_psql;
use crate::commands::get::get_property;
use crate::commands::init::init_pgx;
use crate::commands::install::install_extension;
use crate::commands::new::create_crate_template;
use crate::commands::package::package_extension;
use crate::commands::run::run_psql;
use crate::commands::schema;
use crate::commands::start::start_postgres;
use crate::commands::status::status_postgres;
use crate::commands::stop::stop_postgres;
use crate::commands::test::test_extension;
use clap::{App, AppSettings};
use colored::Colorize;
use pgx_utils::handle_result;
use pgx_utils::pg_config::{PgConfig, PgConfigSelector, Pgx};
use pgx_utils::{exit, exit_with_error};
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;

const SUPPORTED_MAJOR_VERSIONS: &[u16] = &[10, 11, 12, 13];

fn main() -> std::result::Result<(), std::io::Error> {
    handle_result!(do_it(), "");
    Ok(())
}

fn do_it() -> std::result::Result<(), std::io::Error> {
    let yaml = load_yaml!("cli.yml");
    let app = App::from(yaml);

    let matches = app
        .version(crate_version!())
        .global_setting(AppSettings::GlobalVersion)
        .get_matches();

    if let Some(extension) = matches.subcommand_matches("pgx") {
        let result = match extension.subcommand() {
            ("init", Some(init)) => {
                let mut versions = HashMap::new();

                for major in SUPPORTED_MAJOR_VERSIONS {
                    let name = format!("pg{}", major);
                    init.value_of(&name).map(|v| versions.insert(name, v));
                }

                if versions.is_empty() {
                    // no arguments specified, so we'll just install our defaults
                    init_pgx(&Pgx::default(SUPPORTED_MAJOR_VERSIONS)?)
                } else {
                    // user specified arguments, so we'll only install those versions of Postgres
                    let mut default_pgx = None;
                    let mut pgx = Pgx::new();

                    for (pgver, pg_config_path) in versions {
                        let config = if pg_config_path == "download" {
                            if default_pgx.is_none() {
                                default_pgx = Some(Pgx::default(SUPPORTED_MAJOR_VERSIONS)?);
                            }
                            default_pgx
                                .as_ref()
                                .unwrap() // We just set this
                                .get(&pgver)
                                .expect(&format!("{} is not a known Postgres version", pgver))
                                .clone()
                        } else {
                            PgConfig::new(pg_config_path.into())
                        };
                        pgx.push(config);
                    }

                    init_pgx(&pgx)
                }
            }
            ("new", Some(new)) => {
                let is_bgworker = new.is_present("bgworker");
                let extname = new
                    .value_of("name")
                    .expect("<NAME> argument to create is required");
                validate_extension_name(extname);
                let path = PathBuf::from_str(&format!("{}/", extname)).unwrap();
                create_crate_template(path, extname, is_bgworker)
            }
            ("start", Some(start)) => {
                let pgver = start.value_of("pg_version").unwrap_or("all");
                let pgx = Pgx::from_config()?;
                for pg_config in pgx.iter(PgConfigSelector::new(pgver)) {
                    start_postgres(pg_config?)?
                }

                Ok(())
            }
            ("stop", Some(stop)) => {
                let pgver = stop.value_of("pg_version").unwrap_or("all");
                let pgx = Pgx::from_config()?;
                for pg_config in pgx.iter(PgConfigSelector::new(pgver)) {
                    stop_postgres(pg_config?)?
                }

                Ok(())
            }
            ("status", Some(status)) => {
                let pgver = status.value_of("pg_version").unwrap_or("all");
                let pgx = Pgx::from_config()?;
                for pg_config in pgx.iter(PgConfigSelector::new(pgver)) {
                    let pg_config = pg_config?;
                    if status_postgres(pg_config)? {
                        println!(
                            "Postgres v{} is {}",
                            pg_config.major_version()?,
                            "running".bold().green()
                        )
                    } else {
                        println!(
                            "Postgres v{} is {}",
                            pg_config.major_version()?,
                            "stopped".bold().red()
                        )
                    }
                }

                Ok(())
            }
            ("install", Some(install)) => {
                let is_release = install.is_present("release");
                let features = install
                    .values_of("features")
                    .map(|v| v.collect())
                    .unwrap_or(vec![]);
                let pg_config = match std::env::var("PGX_TEST_MODE_VERSION") {
                    // for test mode, we want the pg_config specified in PGX_TEST_MODE_VERSION
                    Ok(pgver) => match Pgx::from_config()?.get(&pgver) {
                        Ok(pg_config) => pg_config.clone(),
                        Err(_) => {
                            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput,
                                                           "PGX_TEST_MODE_VERSION does not contain a valid postgres version number"
                                ));
                        }
                    },

                    // otherwise, the user just ran "cargo pgx install", and we use whatever "pg_config" is configured
                    Err(_) => match install.value_of("pg_config") {
                        None => PgConfig::from_path(),
                        Some(config) => PgConfig::new(PathBuf::from(config)),
                    },
                };

                install_extension(&pg_config, is_release, None, features)
            }
            ("package", Some(package)) => {
                let is_debug = package.is_present("debug");
                let features = package
                    .values_of("features")
                    .map(|v| v.collect())
                    .unwrap_or(vec![]);
                let pg_config = match package.value_of("pg_config") {
                    None => PgConfig::from_path(),
                    Some(config) => PgConfig::new(PathBuf::from(config)),
                };
                package_extension(&pg_config, is_debug, features)
            }
            ("run", Some(run)) => {
                let pgver = run
                    .value_of("pg_version")
                    .expect("<PG_VERSION> is required");
                let dbname = run.value_of("dbname").map_or_else(
                    || get_property("extname").expect("could not determine extension name"),
                    |v| v.to_string(),
                );
                let features = run
                    .values_of("features")
                    .map(|v| v.collect())
                    .unwrap_or(vec![]);
                let is_release = run.is_present("release");
                run_psql(
                    Pgx::from_config()?.get(pgver)?,
                    &dbname,
                    is_release,
                    features,
                )
            }
            ("connect", Some(run)) => {
                let pgver = run
                    .value_of("pg_version")
                    .expect("<PG_VERSION> is required");
                let dbname = run.value_of("dbname").map_or_else(
                    || get_property("extname").expect("could not determine extension name"),
                    |v| v.to_string(),
                );
                connect_psql(Pgx::from_config()?.get(pgver)?, &dbname)
            }
            ("test", Some(test)) => {
                let is_release = test.is_present("release");
                let pgver = test.value_of("pg_version").unwrap_or("all");
                let features = test
                    .values_of("features")
                    .map(|v| v.collect())
                    .unwrap_or(vec![]);
                let pgx = Pgx::from_config()?;
                for pg_config in pgx.iter(PgConfigSelector::new(pgver)) {
                    test_extension(pg_config?, is_release, features.clone())?
                }
                Ok(())
            }
            ("schema", Some(schema)) => {
                let (_, extname) = crate::commands::get::find_control_file();
                let out = schema
                    .value_of("out")
                    .map(|x| x.to_string())
                    .unwrap_or_else(|| {
                        format!(
                            "sql/{}-{}.sql",
                            extname,
                            crate::commands::install::get_version()
                        )
                    });
                let dot = schema.value_of("dot").map(|x| x.to_string());
                let is_release = schema.is_present("release");
                let is_release = schema.is_present("release");
                let features = schema
                    .values_of("features")
                    .map(|v| v.collect())
                    .unwrap_or(vec![]);
                let pg_config = match std::env::var("PGX_TEST_MODE_VERSION") {
                    // for test mode, we want the pg_config specified in PGX_TEST_MODE_VERSION
                    Ok(pgver) => match Pgx::from_config()?.get(&pgver) {
                        Ok(pg_config) => pg_config.clone(),
                        Err(_) => {
                            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput,
                                                           "PGX_TEST_MODE_VERSION does not contain a valid postgres version number"
                            ));
                        }
                    },

                    // otherwise, the user just ran "cargo pgx install", and we use whatever "pg_config" is configured
                    Err(_) => match schema.value_of("pg_config") {
                        None => PgConfig::from_path(),
                        Some(config) => PgConfig::new(PathBuf::from(config)),
                    },
                };

                schema::generate_schema(&pg_config, is_release, &features, &out, dot)
            }
            ("get", Some(get)) => {
                let name = get.value_of("name").expect("no property name specified");
                if let Some(value) = get_property(name) {
                    println!("{}", value);
                }
                Ok(())
            }
            _ => exit!(extension.usage()),
        };

        return result;
    } else {
        exit!(matches.usage())
    }
}

fn validate_extension_name(extname: &str) {
    for c in extname.chars() {
        if !c.is_alphanumeric() && c != '_' && !c.is_lowercase() {
            exit_with_error!("Extension name must be in the set of [a-z0-9_]")
        }
    }
}
