/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

mononoke_app::subcommands! {
    mod blobstore;
    mod blobstore_unlink;
    mod bookmarks;
    mod commit;
    mod convert;
    mod fetch;
    mod list_repos;
    mod mutable_renames;
    mod redaction;
    mod repo_info;
    mod skiplist;
    mod ephemeral_store;
}
