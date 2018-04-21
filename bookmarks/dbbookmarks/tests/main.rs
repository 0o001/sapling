// Copyright (c) 2017-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

//! Tests for the Filenodes store.

#![deny(warnings)]
#![feature(never_type)]

extern crate ascii;
extern crate async_unit;
extern crate bookmarks;
extern crate dbbookmarks;
extern crate failure_ext as failure;
extern crate futures;
extern crate futures_ext;
extern crate mercurial_types;
extern crate mercurial_types_mocks;
extern crate tokio;

use ascii::AsciiString;
use dbbookmarks::{MysqlDbBookmarks, SqliteDbBookmarks};
use mercurial_types_mocks::nodehash::{ONES_CSID, TWOS_CSID};
use mercurial_types_mocks::repo::REPO_ZERO;

macro_rules! bookmarks_test_impl {
    ($mod_name: ident => {
        new: $new_cb: expr,
    }) => {
        mod $mod_name {
            use super::*;

            use bookmarks::Bookmarks;
            use futures::future::Future;
            use futures::Stream;

            #[test]
            fn test_simple_unconditional_set_get() {
                let bookmarks = $new_cb();
                let name_correct = AsciiString::from_ascii("book".to_string()).unwrap();
                let name_incorrect = AsciiString::from_ascii("book2".to_string()).unwrap();

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.force_set(&name_correct, &ONES_CSID).unwrap();
                txn.commit().wait().unwrap();

                assert_eq!(
                    bookmarks.get(&name_correct, &REPO_ZERO).wait().unwrap(),
                    Some(ONES_CSID)
                );
                assert_eq!(
                    bookmarks.get(&name_incorrect, &REPO_ZERO).wait().unwrap(),
                    None
                );
            }

            #[test]
            fn test_multi_unconditional_set_get() {
                let bookmarks = $new_cb();
                let name_1 = AsciiString::from_ascii("book".to_string()).unwrap();
                let name_2 = AsciiString::from_ascii("book2".to_string()).unwrap();

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.force_set(&name_1, &ONES_CSID).unwrap();
                txn.force_set(&name_2, &TWOS_CSID).unwrap();
                txn.commit().wait().unwrap();

                assert_eq!(
                    bookmarks.get(&name_1, &REPO_ZERO).wait().unwrap(),
                    Some(ONES_CSID)
                );
                assert_eq!(
                    bookmarks.get(&name_2, &REPO_ZERO).wait().unwrap(),
                    Some(TWOS_CSID)
                );
            }

            #[test]
            fn test_unconditional_set_same_bookmark() {
                let bookmarks = $new_cb();
                let name_1 = AsciiString::from_ascii("book".to_string()).unwrap();

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.force_set(&name_1, &ONES_CSID).unwrap();
                txn.commit().wait().unwrap();

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.force_set(&name_1, &ONES_CSID).unwrap();
                txn.commit().wait().unwrap();

                assert_eq!(
                    bookmarks.get(&name_1, &REPO_ZERO).wait().unwrap(),
                    Some(ONES_CSID)
                );
            }

            #[test]
            fn test_simple_create() {
                let bookmarks = $new_cb();
                let name_1 = AsciiString::from_ascii("book".to_string()).unwrap();

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.create(&name_1, &ONES_CSID).unwrap();
                txn.commit().wait().unwrap();

                assert_eq!(
                    bookmarks.get(&name_1, &REPO_ZERO).wait().unwrap(),
                    Some(ONES_CSID)
                );
            }

            #[test]
            fn test_create_already_existing() {
                let bookmarks = $new_cb();
                let name_1 = AsciiString::from_ascii("book".to_string()).unwrap();

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.create(&name_1, &ONES_CSID).unwrap();
                txn.commit().wait().unwrap();

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.create(&name_1, &ONES_CSID).unwrap();
                assert!(txn.commit().wait().is_err());
            }

            #[test]
            fn test_create_change_same_bookmark() {
                let bookmarks = $new_cb();
                let name_1 = AsciiString::from_ascii("book".to_string()).unwrap();

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.create(&name_1, &ONES_CSID).unwrap();
                assert!(txn.force_set(&name_1, &ONES_CSID).is_err());

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.force_set(&name_1, &ONES_CSID).unwrap();
                assert!(txn.create(&name_1, &ONES_CSID).is_err());

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.force_set(&name_1, &ONES_CSID).unwrap();
                assert!(txn.update(&name_1, &TWOS_CSID, &ONES_CSID).is_err());

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.update(&name_1, &TWOS_CSID, &ONES_CSID).unwrap();
                assert!(txn.force_set(&name_1, &ONES_CSID).is_err());

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.update(&name_1, &TWOS_CSID, &ONES_CSID).unwrap();
                assert!(txn.force_delete(&name_1).is_err());

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.force_delete(&name_1).unwrap();
                assert!(txn.update(&name_1, &TWOS_CSID, &ONES_CSID).is_err());

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.delete(&name_1, &ONES_CSID).unwrap();
                assert!(txn.update(&name_1, &TWOS_CSID, &ONES_CSID).is_err());

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.update(&name_1, &TWOS_CSID, &ONES_CSID).unwrap();
                assert!(txn.delete(&name_1, &ONES_CSID).is_err());
            }

            #[test]
            fn test_simple_update_bookmark() {
                let bookmarks = $new_cb();
                let name_1 = AsciiString::from_ascii("book".to_string()).unwrap();

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.create(&name_1, &ONES_CSID).unwrap();
                txn.commit().wait().unwrap();

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.update(&name_1, &TWOS_CSID, &ONES_CSID).unwrap();
                txn.commit().wait().unwrap();

                assert_eq!(
                    bookmarks.get(&name_1, &REPO_ZERO).wait().unwrap(),
                    Some(TWOS_CSID)
                );
            }

            #[test]
            fn test_update_non_existent_bookmark() {
                let bookmarks = $new_cb();
                let name_1 = AsciiString::from_ascii("book".to_string()).unwrap();

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.update(&name_1, &TWOS_CSID, &ONES_CSID).unwrap();
                assert!(txn.commit().wait().is_err());
            }

            #[test]
            fn test_update_existing_bookmark_with_incorrect_commit() {
                let bookmarks = $new_cb();
                let name_1 = AsciiString::from_ascii("book".to_string()).unwrap();

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.create(&name_1, &ONES_CSID).unwrap();
                txn.commit().wait().unwrap();

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.update(&name_1, &ONES_CSID, &TWOS_CSID).unwrap();
                assert!(txn.commit().wait().is_err());
            }

            #[test]
            fn test_force_delete() {
                let bookmarks = $new_cb();
                let name_1 = AsciiString::from_ascii("book".to_string()).unwrap();

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.force_delete(&name_1).unwrap();
                txn.commit().wait().unwrap();

                assert_eq!(bookmarks.get(&name_1, &REPO_ZERO).wait().unwrap(), None);

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.create(&name_1, &ONES_CSID).unwrap();
                txn.commit().wait().unwrap();
                assert!(bookmarks.get(&name_1, &REPO_ZERO).wait().unwrap().is_some());

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.force_delete(&name_1).unwrap();
                txn.commit().wait().unwrap();

                assert_eq!(bookmarks.get(&name_1, &REPO_ZERO).wait().unwrap(), None);
            }

            #[test]
            fn test_delete() {
                let bookmarks = $new_cb();
                let name_1 = AsciiString::from_ascii("book".to_string()).unwrap();

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.delete(&name_1, &ONES_CSID).unwrap();
                assert!(txn.commit().wait().is_err());

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.create(&name_1, &ONES_CSID).unwrap();
                txn.commit().wait().unwrap();
                assert!(bookmarks.get(&name_1, &REPO_ZERO).wait().unwrap().is_some());

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.delete(&name_1, &ONES_CSID).unwrap();
                txn.commit().wait().unwrap();
            }

            #[test]
            fn test_delete_incorrect_hash() {
                let bookmarks = $new_cb();
                let name_1 = AsciiString::from_ascii("book".to_string()).unwrap();

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.create(&name_1, &ONES_CSID).unwrap();
                txn.commit().wait().unwrap();
                assert!(bookmarks.get(&name_1, &REPO_ZERO).wait().unwrap().is_some());

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.delete(&name_1, &TWOS_CSID).unwrap();
                assert!(txn.commit().wait().is_err());
            }

            #[test]
            fn test_list_by_prefix() {
                let bookmarks = $new_cb();
                let name_1 = AsciiString::from_ascii("book1".to_string()).unwrap();
                let name_2 = AsciiString::from_ascii("book2".to_string()).unwrap();

                let mut txn = bookmarks.create_transaction(&REPO_ZERO);
                txn.create(&name_1, &ONES_CSID).unwrap();
                txn.create(&name_2, &ONES_CSID).unwrap();
                txn.commit().wait().unwrap();

                let prefix = AsciiString::from_ascii("book".to_string()).unwrap();
                assert_eq!(
                    bookmarks
                        .list_by_prefix(&prefix, &REPO_ZERO)
                        .collect()
                        .wait()
                        .unwrap(),
                    vec![(name_1.clone(), ONES_CSID), (name_2.clone(), ONES_CSID)]
                );

                assert_eq!(
                    bookmarks
                        .list_by_prefix(&name_1, &REPO_ZERO)
                        .collect()
                        .wait()
                        .unwrap(),
                    vec![(name_1.clone(), ONES_CSID)]
                );

                assert_eq!(
                    bookmarks
                        .list_by_prefix(&name_2, &REPO_ZERO)
                        .collect()
                        .wait()
                        .unwrap(),
                    vec![(name_2, ONES_CSID)]
                );
            }
        }
    }
}

bookmarks_test_impl!(sqlite_tests => {
     new: create_sqlite,
 });

bookmarks_test_impl!(mysql_tests => {
     new: create_mysql,
 });

fn create_sqlite() -> SqliteDbBookmarks {
    SqliteDbBookmarks::in_memory().unwrap()
}

fn create_mysql() -> MysqlDbBookmarks {
    MysqlDbBookmarks::create_test_db("mononokefilenodestest").unwrap()
}
