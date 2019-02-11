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
extern crate context;
extern crate dbbookmarks;
extern crate failure_ext as failure;
extern crate futures;
extern crate futures_ext;
#[macro_use]
extern crate maplit;
extern crate mercurial_types_mocks;
extern crate mononoke_types;
extern crate mononoke_types_mocks;
extern crate tokio;

use bookmarks::{
    Bookmark, BookmarkPrefix, BookmarkUpdateLogEntry, BookmarkUpdateReason, Bookmarks,
    BundleReplayData,
};
use context::CoreContext;
use dbbookmarks::{SqlBookmarks, SqlConstructors};
use futures::{Future, Stream};
use mercurial_types_mocks::nodehash as mercurial_mocks;
use mononoke_types::Timestamp;
use mononoke_types_mocks::changesetid::{
    FIVES_CSID, FOURS_CSID, ONES_CSID, THREES_CSID, TWOS_CSID,
};
use mononoke_types_mocks::repo::{REPO_ONE, REPO_ZERO};

fn create_bookmark(book: &str) -> Bookmark {
    Bookmark::new(book.to_string()).unwrap()
}

fn create_prefix(book: &str) -> BookmarkPrefix {
    BookmarkPrefix::new(book.to_string()).unwrap()
}

fn compare_log_entries(expected: BookmarkUpdateLogEntry, actual: BookmarkUpdateLogEntry) {
    assert_eq!(expected.id, actual.id);
    assert_eq!(expected.repo_id, actual.repo_id);
    assert_eq!(expected.bookmark_name, actual.bookmark_name);
    assert_eq!(expected.to_changeset_id, actual.to_changeset_id);
    assert_eq!(expected.from_changeset_id, actual.from_changeset_id);
    assert_eq!(expected.reason, actual.reason);
}

#[test]
fn test_simple_unconditional_set_get() {
    let ctx = CoreContext::test_mock();
    let bookmarks = SqlBookmarks::with_sqlite_in_memory().unwrap();
    let name_correct = create_bookmark("book");
    let name_incorrect = create_bookmark("book2");

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.force_set(
        &name_correct,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().unwrap());

    assert_eq!(
        bookmarks
            .get(ctx.clone(), &name_correct, REPO_ZERO)
            .wait()
            .unwrap(),
        Some(ONES_CSID)
    );
    assert_eq!(
        bookmarks
            .get(ctx.clone(), &name_incorrect, REPO_ZERO)
            .wait()
            .unwrap(),
        None
    );

    compare_log_entries(
        bookmarks
            .read_next_bookmark_log_entry(ctx.clone(), 0)
            .wait()
            .unwrap()
            .unwrap(),
        BookmarkUpdateLogEntry {
            id: 1,
            repo_id: REPO_ZERO,
            bookmark_name: name_correct,
            to_changeset_id: Some(ONES_CSID),
            from_changeset_id: None,
            reason: BookmarkUpdateReason::TestMove {
                bundle_replay_data: None,
            },
            timestamp: Timestamp::now(),
        },
    );
}

#[test]
fn test_multi_unconditional_set_get() {
    let ctx = CoreContext::test_mock();
    let bookmarks = SqlBookmarks::with_sqlite_in_memory().unwrap();
    let name_1 = create_bookmark("book");
    let name_2 = create_bookmark("book2");

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.force_set(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    txn.force_set(
        &name_2,
        TWOS_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().unwrap());

    assert_eq!(
        bookmarks
            .get(ctx.clone(), &name_1, REPO_ZERO)
            .wait()
            .unwrap(),
        Some(ONES_CSID)
    );

    assert_eq!(
        bookmarks
            .get(ctx.clone(), &name_2, REPO_ZERO)
            .wait()
            .unwrap(),
        Some(TWOS_CSID)
    );
}

#[test]
fn test_unconditional_set_same_bookmark() {
    let ctx = CoreContext::test_mock();
    let bookmarks = SqlBookmarks::with_sqlite_in_memory().unwrap();
    let name_1 = create_bookmark("book");

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.force_set(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().unwrap());

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.force_set(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().unwrap());

    assert_eq!(
        bookmarks
            .get(ctx.clone(), &name_1, REPO_ZERO)
            .wait()
            .unwrap(),
        Some(ONES_CSID)
    );
}

#[test]
fn test_simple_create() {
    let ctx = CoreContext::test_mock();
    let bookmarks = SqlBookmarks::with_sqlite_in_memory().unwrap();
    let name_1 = create_bookmark("book");

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.create(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().unwrap());

    assert_eq!(
        bookmarks
            .get(ctx.clone(), &name_1, REPO_ZERO)
            .wait()
            .unwrap(),
        Some(ONES_CSID)
    );

    compare_log_entries(
        bookmarks
            .read_next_bookmark_log_entry(ctx.clone(), 0)
            .wait()
            .unwrap()
            .unwrap(),
        BookmarkUpdateLogEntry {
            id: 1,
            repo_id: REPO_ZERO,
            bookmark_name: name_1,
            to_changeset_id: Some(ONES_CSID),
            from_changeset_id: None,
            reason: BookmarkUpdateReason::TestMove {
                bundle_replay_data: None,
            },
            timestamp: Timestamp::now(),
        },
    );
}

#[test]
fn test_create_already_existing() {
    let ctx = CoreContext::test_mock();
    let bookmarks = SqlBookmarks::with_sqlite_in_memory().unwrap();
    let name_1 = create_bookmark("book");

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.create(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().unwrap());

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.create(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().is_err());
}

#[test]
fn test_create_change_same_bookmark() {
    let ctx = CoreContext::test_mock();
    let bookmarks = SqlBookmarks::with_sqlite_in_memory().unwrap();
    let name_1 = create_bookmark("book");

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.create(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn
        .force_set(
            &name_1,
            ONES_CSID,
            BookmarkUpdateReason::TestMove {
                bundle_replay_data: None
            }
        )
        .is_err());

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.force_set(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn
        .create(
            &name_1,
            ONES_CSID,
            BookmarkUpdateReason::TestMove {
                bundle_replay_data: None
            }
        )
        .is_err());

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.force_set(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn
        .update(
            &name_1,
            TWOS_CSID,
            ONES_CSID,
            BookmarkUpdateReason::TestMove {
                bundle_replay_data: None
            }
        )
        .is_err());

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.update(
        &name_1,
        TWOS_CSID,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn
        .force_set(
            &name_1,
            ONES_CSID,
            BookmarkUpdateReason::TestMove {
                bundle_replay_data: None
            }
        )
        .is_err());

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.update(
        &name_1,
        TWOS_CSID,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn
        .force_delete(
            &name_1,
            BookmarkUpdateReason::TestMove {
                bundle_replay_data: None
            }
        )
        .is_err());

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.force_delete(
        &name_1,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn
        .update(
            &name_1,
            TWOS_CSID,
            ONES_CSID,
            BookmarkUpdateReason::TestMove {
                bundle_replay_data: None
            }
        )
        .is_err());

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.delete(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn
        .update(
            &name_1,
            TWOS_CSID,
            ONES_CSID,
            BookmarkUpdateReason::TestMove {
                bundle_replay_data: None
            }
        )
        .is_err());

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.update(
        &name_1,
        TWOS_CSID,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn
        .delete(
            &name_1,
            ONES_CSID,
            BookmarkUpdateReason::TestMove {
                bundle_replay_data: None
            }
        )
        .is_err());
}

#[test]
fn test_simple_update_bookmark() {
    let ctx = CoreContext::test_mock();
    let bookmarks = SqlBookmarks::with_sqlite_in_memory().unwrap();
    let name_1 = create_bookmark("book");

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.create(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().unwrap());

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.update(
        &name_1,
        TWOS_CSID,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().unwrap());

    assert_eq!(
        bookmarks
            .get(ctx.clone(), &name_1, REPO_ZERO)
            .wait()
            .unwrap(),
        Some(TWOS_CSID)
    );

    compare_log_entries(
        bookmarks
            .read_next_bookmark_log_entry(ctx.clone(), 1)
            .wait()
            .unwrap()
            .unwrap(),
        BookmarkUpdateLogEntry {
            id: 2,
            repo_id: REPO_ZERO,
            bookmark_name: name_1,
            to_changeset_id: Some(TWOS_CSID),
            from_changeset_id: Some(ONES_CSID),
            reason: BookmarkUpdateReason::TestMove {
                bundle_replay_data: None,
            },
            timestamp: Timestamp::now(),
        },
    );
}

#[test]
fn test_update_non_existent_bookmark() {
    let ctx = CoreContext::test_mock();
    let bookmarks = SqlBookmarks::with_sqlite_in_memory().unwrap();
    let name_1 = create_bookmark("book");

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.update(
        &name_1,
        TWOS_CSID,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert_eq!(txn.commit().wait().unwrap(), false);
}

#[test]
fn test_update_existing_bookmark_with_incorrect_commit() {
    let ctx = CoreContext::test_mock();
    let bookmarks = SqlBookmarks::with_sqlite_in_memory().unwrap();
    let name_1 = create_bookmark("book");

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.create(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().unwrap());

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.update(
        &name_1,
        ONES_CSID,
        TWOS_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert_eq!(txn.commit().wait().unwrap(), false);
}

#[test]
fn test_force_delete() {
    let ctx = CoreContext::test_mock();
    let bookmarks = SqlBookmarks::with_sqlite_in_memory().unwrap();
    let name_1 = create_bookmark("book");

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.force_delete(
        &name_1,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().unwrap());

    assert_eq!(
        bookmarks
            .get(ctx.clone(), &name_1, REPO_ZERO)
            .wait()
            .unwrap(),
        None
    );

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.create(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().unwrap());
    assert!(bookmarks
        .get(ctx.clone(), &name_1, REPO_ZERO)
        .wait()
        .unwrap()
        .is_some());

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.force_delete(
        &name_1,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().unwrap());

    assert_eq!(
        bookmarks
            .get(ctx.clone(), &name_1, REPO_ZERO)
            .wait()
            .unwrap(),
        None
    );

    compare_log_entries(
        bookmarks
            .read_next_bookmark_log_entry(ctx.clone(), 2)
            .wait()
            .unwrap()
            .unwrap(),
        BookmarkUpdateLogEntry {
            id: 3,
            repo_id: REPO_ZERO,
            bookmark_name: name_1,
            to_changeset_id: None,
            from_changeset_id: None,
            reason: BookmarkUpdateReason::TestMove {
                bundle_replay_data: None,
            },
            timestamp: Timestamp::now(),
        },
    );
}

#[test]
fn test_delete() {
    let ctx = CoreContext::test_mock();
    let bookmarks = SqlBookmarks::with_sqlite_in_memory().unwrap();
    let name_1 = create_bookmark("book");

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.delete(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert_eq!(txn.commit().wait().unwrap(), false);

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.create(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().unwrap());
    assert!(bookmarks
        .get(ctx.clone(), &name_1, REPO_ZERO)
        .wait()
        .unwrap()
        .is_some());

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.delete(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().unwrap());

    compare_log_entries(
        bookmarks
            .read_next_bookmark_log_entry(ctx.clone(), 1)
            .wait()
            .unwrap()
            .unwrap(),
        BookmarkUpdateLogEntry {
            id: 2,
            repo_id: REPO_ZERO,
            bookmark_name: name_1,
            to_changeset_id: None,
            from_changeset_id: Some(ONES_CSID),
            reason: BookmarkUpdateReason::TestMove {
                bundle_replay_data: None,
            },
            timestamp: Timestamp::now(),
        },
    );
}

#[test]
fn test_delete_incorrect_hash() {
    let ctx = CoreContext::test_mock();
    let bookmarks = SqlBookmarks::with_sqlite_in_memory().unwrap();
    let name_1 = create_bookmark("book");

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.create(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().unwrap());
    assert!(bookmarks
        .get(ctx.clone(), &name_1, REPO_ZERO)
        .wait()
        .unwrap()
        .is_some());

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.delete(
        &name_1,
        TWOS_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert_eq!(txn.commit().wait().unwrap(), false);
}

#[test]
fn test_list_by_prefix() {
    let ctx = CoreContext::test_mock();
    let bookmarks = SqlBookmarks::with_sqlite_in_memory().unwrap();
    let name_1 = create_bookmark("book1");
    let name_2 = create_bookmark("book2");

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.create(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    txn.create(
        &name_2,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().unwrap());

    let prefix = create_prefix("book");
    let name_1_prefix = create_prefix("book1");
    let name_2_prefix = create_prefix("book2");
    assert_eq!(
        bookmarks
            .list_by_prefix(ctx.clone(), &prefix, REPO_ZERO)
            .collect()
            .wait()
            .unwrap(),
        vec![(name_1.clone(), ONES_CSID), (name_2.clone(), ONES_CSID)]
    );

    assert_eq!(
        bookmarks
            .list_by_prefix(ctx.clone(), &name_1_prefix, REPO_ZERO)
            .collect()
            .wait()
            .unwrap(),
        vec![(name_1.clone(), ONES_CSID)]
    );

    assert_eq!(
        bookmarks
            .list_by_prefix(ctx.clone(), &name_2_prefix, REPO_ZERO)
            .collect()
            .wait()
            .unwrap(),
        vec![(name_2, ONES_CSID)]
    );
}

#[test]
fn test_create_different_repos() {
    let ctx = CoreContext::test_mock();
    let bookmarks = SqlBookmarks::with_sqlite_in_memory().unwrap();
    let name_1 = create_bookmark("book");

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.force_set(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().is_ok());

    // Updating value from another repo, should fail
    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ONE);
    txn.update(
        &name_1,
        TWOS_CSID,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert_eq!(txn.commit().wait().unwrap(), false);

    // Creating value should succeed
    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ONE);
    txn.create(
        &name_1,
        TWOS_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().is_ok());

    assert_eq!(
        bookmarks
            .get(ctx.clone(), &name_1, REPO_ZERO)
            .wait()
            .unwrap(),
        Some(ONES_CSID)
    );
    assert_eq!(
        bookmarks
            .get(ctx.clone(), &name_1, REPO_ONE)
            .wait()
            .unwrap(),
        Some(TWOS_CSID)
    );

    // Force deleting should delete only from one repo
    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ONE);
    txn.force_delete(
        &name_1,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().is_ok());
    assert_eq!(
        bookmarks
            .get(ctx.clone(), &name_1, REPO_ZERO)
            .wait()
            .unwrap(),
        Some(ONES_CSID)
    );

    // delete should fail for another repo
    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ONE);
    txn.delete(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert_eq!(txn.commit().wait().unwrap(), false);
}

#[test]
fn test_log_correct_order() {
    let ctx = CoreContext::test_mock();
    let bookmarks = SqlBookmarks::with_sqlite_in_memory().unwrap();
    let name_1 = create_bookmark("book");

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.force_set(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    assert!(txn.commit().wait().is_ok());

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.update(
        &name_1,
        TWOS_CSID,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    txn.commit().wait().unwrap();

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.update(
        &name_1,
        THREES_CSID,
        TWOS_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    txn.commit().wait().unwrap();

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.update(
        &name_1,
        FOURS_CSID,
        THREES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    txn.commit().wait().unwrap();

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.update(
        &name_1,
        FIVES_CSID,
        FOURS_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: None,
        },
    )
    .unwrap();
    txn.commit().wait().unwrap();

    let log_entry = bookmarks
        .read_next_bookmark_log_entry(ctx.clone(), 0)
        .wait()
        .unwrap()
        .unwrap();
    assert_eq!(log_entry.to_changeset_id.unwrap(), ONES_CSID);

    let log_entry = bookmarks
        .read_next_bookmark_log_entry(ctx.clone(), 1)
        .wait()
        .unwrap()
        .unwrap();
    assert_eq!(log_entry.to_changeset_id.unwrap(), TWOS_CSID);

    let log_entry = bookmarks
        .read_next_bookmark_log_entry(ctx.clone(), 2)
        .wait()
        .unwrap()
        .unwrap();
    assert_eq!(log_entry.to_changeset_id.unwrap(), THREES_CSID);

    let log_entry = bookmarks
        .read_next_bookmark_log_entry(ctx.clone(), 3)
        .wait()
        .unwrap()
        .unwrap();
    assert_eq!(log_entry.to_changeset_id.unwrap(), FOURS_CSID);

    let log_entry = bookmarks
        .read_next_bookmark_log_entry(ctx.clone(), 4)
        .wait()
        .unwrap()
        .unwrap();
    assert_eq!(log_entry.to_changeset_id.unwrap(), FIVES_CSID);
}

#[test]
fn test_log_bundle_replay_data() {
    let ctx = CoreContext::test_mock();
    let bookmarks = SqlBookmarks::with_sqlite_in_memory().unwrap();
    let name_1 = create_bookmark("book");
    let timestamp = Timestamp::now();
    let expected = BundleReplayData {
        bundle_handle: "handle".to_string(),
        commit_timestamps: hashmap! {mercurial_mocks::ONES_CSID => timestamp.clone()},
    };

    let mut txn = bookmarks.create_transaction(ctx.clone(), REPO_ZERO);
    txn.force_set(
        &name_1,
        ONES_CSID,
        BookmarkUpdateReason::TestMove {
            bundle_replay_data: Some(expected.clone()),
        },
    )
    .unwrap();
    assert!(txn.commit().wait().is_ok());

    let log_entry = bookmarks
        .read_next_bookmark_log_entry(ctx.clone(), 0)
        .wait()
        .unwrap()
        .unwrap();
    let bundle_replay_data = match log_entry.reason {
        BookmarkUpdateReason::TestMove { bundle_replay_data } => bundle_replay_data,
        _ => {
            panic!("unexpected reason");
        }
    };

    let actual = bundle_replay_data.unwrap();
    assert_eq!(actual, expected);
}
