/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use tempfile::tempdir;

use minibench::{bench, elapsed};

use dag::idmap::{IdMap, IdMapAssignHead};
use dag::{Group, IdDag, InProcessIdDag, VertexName};

fn main() {
    println!(
        "benchmarking {} serde",
        std::any::type_name::<InProcessIdDag>()
    );
    let parents = bindag::parse_bindag(bindag::MOZILLA);

    let head_name = VertexName::copy_from(format!("{}", parents.len() - 1).as_bytes());
    let parents_by_name = |name: VertexName| -> dag::Result<Vec<VertexName>> {
        let i = String::from_utf8(name.as_ref().to_vec())
            .unwrap()
            .parse::<usize>()
            .unwrap();
        Ok(parents[i]
            .iter()
            .map(|p| format!("{}", p).as_bytes().to_vec().into())
            .collect())
    };

    let id_map_dir = tempdir().unwrap();
    let mut id_map = IdMap::open(id_map_dir.path()).unwrap();
    let outcome = id_map
        .assign_head(head_name.clone(), &parents_by_name, Group::MASTER)
        .unwrap();
    let mut iddag = IdDag::new_in_process();
    iddag
        .build_segments_volatile_from_assign_head_outcome(&outcome)
        .unwrap();


    let mut blob = Vec::new();
    bench("serializing inprocess iddag with mincode", || {
        elapsed(|| {
            blob = mincode::serialize(&iddag).unwrap();
        })
    });

    println!("mincode serialized blob has {} bytes", blob.len());

    bench("deserializing inprocess iddag with mincode", || {
        elapsed(|| {
            let _new_iddag: InProcessIdDag = mincode::deserialize(&blob).unwrap();
        })
    });
}
