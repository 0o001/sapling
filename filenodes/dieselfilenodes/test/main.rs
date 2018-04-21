// Copyright (c) 2017-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

//! Tests for the Filenodes store.

#![deny(warnings)]
#![feature(never_type)]

extern crate async_unit;
extern crate dieselfilenodes;
extern crate failure_ext as failure;
extern crate filenodes;
extern crate futures;
extern crate futures_ext;
extern crate mercurial_types;
extern crate mercurial_types_mocks;
extern crate tokio;

use dieselfilenodes::{MysqlFilenodes, SqliteFilenodes};
use filenodes::{FilenodeInfo, Filenodes};
use futures::future::Future;
use futures_ext::StreamExt;
use mercurial_types::{DFileNodeId, RepoPath, RepositoryId};
use mercurial_types_mocks::nodehash::{ONES_CSID, ONES_FNID, THREES_CSID, THREES_FNID, TWOS_CSID,
                                      TWOS_FNID};
use mercurial_types_mocks::repo::{REPO_ONE, REPO_ZERO};

fn root_first_filenode() -> FilenodeInfo {
    FilenodeInfo {
        path: RepoPath::root(),
        filenode: ONES_FNID,
        p1: None,
        p2: None,
        copyfrom: None,
        linknode: ONES_CSID,
    }
}

fn root_second_filenode() -> FilenodeInfo {
    FilenodeInfo {
        path: RepoPath::root(),
        filenode: TWOS_FNID,
        p1: Some(ONES_FNID),
        p2: None,
        copyfrom: None,
        linknode: TWOS_CSID,
    }
}

fn root_merge_filenode() -> FilenodeInfo {
    FilenodeInfo {
        path: RepoPath::root(),
        filenode: THREES_FNID,
        p1: Some(ONES_FNID),
        p2: Some(TWOS_FNID),
        copyfrom: None,
        linknode: THREES_CSID,
    }
}

fn file_a_first_filenode() -> FilenodeInfo {
    FilenodeInfo {
        path: RepoPath::file("a").unwrap(),
        filenode: ONES_FNID,
        p1: None,
        p2: None,
        copyfrom: None,
        linknode: ONES_CSID,
    }
}

fn file_b_first_filenode() -> FilenodeInfo {
    FilenodeInfo {
        path: RepoPath::file("b").unwrap(),
        filenode: TWOS_FNID,
        p1: None,
        p2: None,
        copyfrom: None,
        linknode: TWOS_CSID,
    }
}

fn copied_from_filenode() -> FilenodeInfo {
    FilenodeInfo {
        path: RepoPath::file("copiedfrom").unwrap(),
        filenode: ONES_FNID,
        p1: None,
        p2: None,
        copyfrom: None,
        linknode: TWOS_CSID,
    }
}

fn copied_filenode() -> FilenodeInfo {
    FilenodeInfo {
        path: RepoPath::file("copiedto").unwrap(),
        filenode: TWOS_FNID,
        p1: None,
        p2: None,
        copyfrom: Some((RepoPath::file("copiedfrom").unwrap(), ONES_FNID)),
        linknode: TWOS_CSID,
    }
}

fn do_add_filenodes(filenodes: &Filenodes, to_insert: Vec<FilenodeInfo>, repo_id: &RepositoryId) {
    let stream = futures::stream::iter_ok(to_insert.into_iter()).boxify();
    filenodes.add_filenodes(stream, repo_id).wait().unwrap();
}

fn do_add_filenode(filenodes: &Filenodes, node: FilenodeInfo, repo_id: &RepositoryId) {
    do_add_filenodes(filenodes, vec![node], repo_id);
}

fn assert_no_filenode(
    filenodes: &Filenodes,
    path: &RepoPath,
    hash: &DFileNodeId,
    repo_id: &RepositoryId,
) {
    let res = filenodes
        .get_filenode(path, hash, repo_id)
        .wait()
        .expect("error while fetching filenode");
    assert!(res.is_none());
}

fn assert_filenode(
    filenodes: &Filenodes,
    path: &RepoPath,
    hash: &DFileNodeId,
    repo_id: &RepositoryId,
    expected: FilenodeInfo,
) {
    let res = filenodes
        .get_filenode(path, hash, repo_id)
        .wait()
        .expect("error while fetching filenode")
        .expect(&format!("not found: {}", hash));
    assert_eq!(res, expected);
}

macro_rules! filenodes_test_impl {
    ($mod_name: ident => {
        new: $new_cb: expr,
    }) => {
        mod $mod_name {
            use super::*;

            #[test]
            fn test_simple_filenode_insert_and_get() {
                async_unit::tokio_unit_test(|| -> Result<_, !> {
                    let filenodes = &$new_cb();

                    do_add_filenode(filenodes, root_first_filenode(), &REPO_ZERO);
                    assert_filenode(
                        filenodes,
                        &RepoPath::root(),
                        &ONES_FNID,
                        &REPO_ZERO,
                        root_first_filenode()
                    );

                    assert_no_filenode(filenodes, &RepoPath::root(), &TWOS_FNID, &REPO_ZERO);
                    assert_no_filenode(filenodes, &RepoPath::root(), &ONES_FNID, &REPO_ONE);
                    Ok(())
                }).expect("test failed");
            }

            #[test]
            fn test_insert_identical_in_batch() {
                async_unit::tokio_unit_test(|| -> Result<_, !> {
                    let filenodes = &$new_cb();
                    do_add_filenodes(
                        filenodes,
                        vec![root_first_filenode(), root_first_filenode()],
                        &REPO_ZERO,
                    );
                    Ok(())
                }).expect("test failed");
            }

            #[test]
            fn test_filenode_insert_twice() {
                async_unit::tokio_unit_test(|| -> Result<_, !> {
                    let filenodes = &$new_cb();
                    do_add_filenode(filenodes, root_first_filenode(), &REPO_ZERO);
                    do_add_filenode(filenodes, root_first_filenode(), &REPO_ZERO);
                    Ok(())
                }).expect("test failed");
            }

            #[test]
            fn test_insert_filenode_with_parent() {
                async_unit::tokio_unit_test(|| -> Result<_, !> {
                    let filenodes = &$new_cb();
                    do_add_filenode(filenodes, root_first_filenode(), &REPO_ZERO);
                    do_add_filenode(filenodes, root_second_filenode(), &REPO_ZERO);
                    assert_filenode(
                        filenodes,
                        &RepoPath::root(),
                        &ONES_FNID,
                        &REPO_ZERO,
                        root_first_filenode()
                    );
                    assert_filenode(
                        filenodes,
                        &RepoPath::root(),
                        &TWOS_FNID,
                        &REPO_ZERO,
                        root_second_filenode()
                    );
                    Ok(())
                }).expect("test failed");
            }

            #[test]
            fn test_insert_root_filenode_with_two_parents() {
                async_unit::tokio_unit_test(|| -> Result<_, !> {
                    let filenodes = &$new_cb();
                    do_add_filenode(filenodes, root_first_filenode(), &REPO_ZERO);
                    do_add_filenode(filenodes, root_second_filenode(), &REPO_ZERO);
                    do_add_filenode(filenodes, root_merge_filenode(), &REPO_ZERO);

                    assert_filenode(
                        filenodes,
                        &RepoPath::root(),
                        &THREES_FNID,
                        &REPO_ZERO,
                        root_merge_filenode(),
                    );
                    Ok(())
                }).expect("test failed");
            }

            #[test]
            fn test_insert_file_filenode() {
                async_unit::tokio_unit_test(|| -> Result<_, !> {
                    let filenodes = &$new_cb();
                    do_add_filenode(filenodes, file_a_first_filenode(), &REPO_ZERO);
                    do_add_filenode(filenodes, file_b_first_filenode(), &REPO_ZERO);

                    assert_no_filenode(
                        filenodes,
                        &RepoPath::file("non-existent").unwrap(),
                        &ONES_FNID,
                        &REPO_ZERO,
                    );
                    assert_filenode(
                        filenodes,
                        &RepoPath::file("a").unwrap(),
                        &ONES_FNID,
                        &REPO_ZERO,
                        file_a_first_filenode(),
                    );
                    assert_filenode(
                        filenodes,
                        &RepoPath::file("b").unwrap(),
                        &TWOS_FNID,
                        &REPO_ZERO,
                        file_b_first_filenode(),
                    );
                    Ok(())
                }).expect("test failed");
            }

            #[test]
            fn test_insert_different_repo() {
                async_unit::tokio_unit_test(|| -> Result<_, !> {
                    let filenodes = &$new_cb();
                    do_add_filenode(filenodes, root_first_filenode(), &REPO_ZERO);
                    do_add_filenode(filenodes, root_second_filenode(), &REPO_ONE);

                    assert_filenode(
                        filenodes,
                        &RepoPath::root(),
                        &ONES_FNID,
                        &REPO_ZERO,
                        root_first_filenode(),
                    );

                    assert_no_filenode(
                        filenodes,
                        &RepoPath::root(),
                        &ONES_FNID,
                        &REPO_ONE,
                    );

                    assert_filenode(
                        filenodes,
                        &RepoPath::root(),
                        &TWOS_FNID,
                        &REPO_ONE,
                        root_second_filenode(),
                    );
                    Ok(())
                }).expect("test failed");
            }

            #[test]
            fn test_insert_parent_and_child_in_same_batch() {
                async_unit::tokio_unit_test(|| -> Result<_, !> {
                    let filenodes = &$new_cb();

                    do_add_filenodes(
                        filenodes,
                        vec![root_first_filenode(), root_second_filenode()],
                        &REPO_ZERO
                    );

                    assert_filenode(
                        filenodes,
                        &RepoPath::root(),
                        &ONES_FNID,
                        &REPO_ZERO,
                        root_first_filenode(),
                    );

                    assert_filenode(
                        filenodes,
                        &RepoPath::root(),
                        &TWOS_FNID,
                        &REPO_ZERO,
                        root_second_filenode(),
                    );
                    Ok(())
                }).expect("test failed");
            }

            #[test]
            fn insert_copied_file() {
                async_unit::tokio_unit_test(|| -> Result<_, !> {
                    let filenodes = &$new_cb();

                    do_add_filenodes(
                        filenodes,
                        vec![copied_from_filenode(), copied_filenode()],
                        &REPO_ZERO
                    );
                    assert_filenode(
                        filenodes,
                        &RepoPath::file("copiedto").unwrap(),
                        &TWOS_FNID,
                        &REPO_ZERO,
                        copied_filenode(),
                    );
                    Ok(())
                }).expect("test failed");
            }

            #[test]
            fn insert_same_copied_file() {
                async_unit::tokio_unit_test(|| -> Result<_, !> {
                    let filenodes = &$new_cb();

                    do_add_filenodes(filenodes, vec![copied_from_filenode()], &REPO_ZERO);
                    do_add_filenodes(
                        filenodes,
                        vec![copied_filenode(), copied_filenode()],
                        &REPO_ZERO
                    );
                    Ok(())
                }).expect("test failed");
            }

            #[test]
            fn insert_copied_file_to_different_repo() {
                async_unit::tokio_unit_test(|| -> Result<_, !> {
                    let filenodes = &$new_cb();

                    let copied = FilenodeInfo {
                        path: RepoPath::file("copiedto").unwrap(),
                        filenode: TWOS_FNID,
                        p1: None,
                        p2: None,
                        copyfrom: Some((RepoPath::file("copiedfrom").unwrap(), ONES_FNID)),
                        linknode: TWOS_CSID,
                    };

                    let notcopied = FilenodeInfo {
                        path: RepoPath::file("copiedto").unwrap(),
                        filenode: TWOS_FNID,
                        p1: None,
                        p2: None,
                        copyfrom: None,
                        linknode: TWOS_CSID,
                    };

                    do_add_filenodes(
                        filenodes,
                        vec![copied_from_filenode(), copied.clone()],
                        &REPO_ZERO
                    );
                    do_add_filenodes(filenodes, vec![notcopied.clone()], &REPO_ONE);
                    assert_filenode(
                        filenodes,
                        &RepoPath::file("copiedto").unwrap(),
                        &TWOS_FNID,
                        &REPO_ZERO,
                        copied,
                    );

                    assert_filenode(
                        filenodes,
                        &RepoPath::file("copiedto").unwrap(),
                        &TWOS_FNID,
                        &REPO_ONE,
                        notcopied,
                    );
                    Ok(())
                }).expect("test failed");
            }
        }
    }
}

filenodes_test_impl!(sqlite_tests => {
     new: create_sqlite,
 });

filenodes_test_impl!(mysql_tests => {
     new: create_mysql,
 });

fn create_sqlite() -> SqliteFilenodes {
    SqliteFilenodes::in_memory().unwrap()
}

fn create_mysql() -> MysqlFilenodes {
    MysqlFilenodes::create_test_db("mononokefilenodestest").unwrap()
}
