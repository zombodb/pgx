
// Copyright 2020 ZomboDB, LLC <zombodb@gmail.com>. All rights reserved. Use of this source code is
// governed by the MIT license that can be found in the LICENSE file.


use maplit::*;
use pgx::*;
use serde::*;
use std::collections::HashMap;

mod animals {
    use super::*;

    #[derive(PostgresEnum, Serialize, Deserialize, Debug, Eq, PartialEq)]
    pub(crate) enum BearSpecies {
        Grizzly,
        Polar,
        Black,
        Spirit,
    }

    #[derive(PostgresType, Serialize, Deserialize, Debug, Eq, PartialEq)]
    pub struct Bear {
        pub(crate) species: BearSpecies,
    }
}

use animals::{Bear, BearSpecies};

#[pg_extern]
fn bear_of_species(species: BearSpecies) -> Bear {
    Bear {
        species,
    }
}

#[pg_extern]
fn species_of_bear(bear: Bear) -> BearSpecies {
    bear.species
}

#[pg_extern]
fn grizzly_bear() -> Bear {
    Bear {
        species: BearSpecies::Grizzly,
    }
}

#[cfg(any(test, feature = "pg_test"))]
mod tests {
    use crate::submodule::{BearSpecies, Bear, species_of_bear, grizzly_bear, bear_of_species};
    use maplit::*;
    use pgx::*;

    #[pg_test]
    fn test_species_of_bear_via_spi_wrapper() {
        let bear = Spi::get_one::<Bear>("SELECT grizzly_bear();");

        assert_eq!(bear, Some(grizzly_bear()));

        assert_eq!(
            bear,
            Some(Bear {
                species: BearSpecies::Grizzly,
            })
        )
    }
}
