/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use crate::{fetch_blame, BlameError};
use anyhow::{anyhow, Error};
use blobrepo_override::DangerousOverride;
use borrowed::borrowed;
use bytes::Bytes;
use context::CoreContext;
use fbinit::FacebookInit;
use maplit::{btreemap, hashmap};
use metaconfig_types::DerivedDataConfig;
use mononoke_types::{Blame, ChangesetId, MPath};
use std::collections::HashMap;
use tests_utils::{create_commit, store_files, store_rename, CreateCommitContext};

// File with multiple changes and a merge
const F0: &[&str] = &[
    // c0
    r#"|
1 0
1 1
"#,
    // c1
    r#"|
2 0
1 0
2 1
"#,
    // c2
    r#"|
2 0
1 0
3 0
3 1
2 1
3 2
"#,
    // c3
    r#"|
1 0
1 1
3 2
4 0
"#,
    // c4
    r#"|
2 0
1 0
3 0
3 1
2 1
3 2
4 0
"#,
];

const F0_AT_C4: &str = r#"c0: |
c1: 2 0
c0: 1 0
c2: 3 0
c2: 3 1
c1: 2 1
c2: 3 2
c3: 4 0
"#;

// file with multiple change only in one parent and a merge
const F1: &[&str] = &[
    // c0
    r#"|
1 0
1 1
"#,
    // c3
    r#"|
1 0
4 0
1 1
"#,
];

const F1_AT_C4: &str = r#"c0: |
c0: 1 0
c3: 4 0
c0: 1 1
"#;

// renamed file
const F2: &[&str] = &[
    // c0 as _f2
    r#"|
1 0
1 1
"#,
    // c1 as _f2 => f2
    r#"|
1 0
2 0
1 1
"#,
    // c3 as new f2
    r#"|
1 0
4 0
1 1
"#,
    // c4 as f2
    r#"|
5 0
1 0
2 0
4 0
1 1
"#,
];

const F2_AT_C4: &str = r#"c0: |
c4: 5 0
c0: 1 0
c1: 2 0
c3: 4 0
c0: 1 1
"#;

#[fbinit::test]
fn test_blame(fb: FacebookInit) -> Result<(), Error> {
    // Commits structure
    //
    //   0
    //  / \
    // 1   3
    // |   |
    // 2   |
    //  \ /
    //   4
    //
    async_unit::tokio_unit_test(async move {
        let ctx = CoreContext::test_mock(fb);
        let repo = blobrepo_factory::new_memblob_empty(None)?;
        borrowed!(ctx, repo);

        let c0 = create_commit(
            ctx.clone(),
            repo.clone(),
            vec![],
            store_files(
                ctx,
                btreemap! {
                    "f0" => Some(F0[0]),
                    "f1" => Some(F1[0]),
                    "_f2" => Some(F2[0]),
                },
                repo,
            )
            .await,
        )
        .await;

        let mut c1_changes = store_files(ctx, btreemap! {"f0" => Some(F0[1])}, repo).await;
        let (f2_path, f2_change) =
            store_rename(ctx, (MPath::new("_f2")?, c0), "f2", F2[1], repo).await;
        c1_changes.insert(f2_path, f2_change);
        let c1 = create_commit(ctx.clone(), repo.clone(), vec![c0], c1_changes).await;

        let c2 = create_commit(
            ctx.clone(),
            repo.clone(),
            vec![c1],
            store_files(ctx, btreemap! {"f0" => Some(F0[2])}, repo).await,
        )
        .await;

        let c3 = create_commit(
            ctx.clone(),
            repo.clone(),
            vec![c0],
            store_files(
                ctx,
                btreemap! {
                    "f0" => Some(F0[3]),
                    "f1" => Some(F1[1]),
                    "f2" => Some(F2[2]),
                },
                repo,
            )
            .await,
        )
        .await;

        let c4 = create_commit(
            ctx.clone(),
            repo.clone(),
            vec![c2, c3],
            store_files(
                ctx,
                btreemap! {
                    "f0" => Some(F0[4]),
                    "f1" => Some(F1[1]), // did not change after c3
                    "f2" => Some(F2[3]),
                },
                repo,
            )
            .await,
        )
        .await;

        let names = hashmap! {
            c0 => "c0",
            c1 => "c1",
            c2 => "c2",
            c3 => "c3",
            c4 => "c4",
        };

        let (content, blame) = fetch_blame(ctx, repo, c4, MPath::new("f0")?).await?;
        assert_eq!(annotate(content, blame, &names)?, F0_AT_C4);

        let (content, blame) = fetch_blame(ctx, repo, c4, MPath::new("f1")?).await?;
        assert_eq!(annotate(content, blame, &names)?, F1_AT_C4);

        let (content, blame) = fetch_blame(ctx, repo, c4, MPath::new("f2")?).await?;
        assert_eq!(annotate(content, blame, &names)?, F2_AT_C4);

        Ok(())
    })
}

#[fbinit::test]
async fn test_blame_file_size_limit_rejected(fb: FacebookInit) -> Result<(), Error> {
    let ctx = CoreContext::test_mock(fb);
    let repo = blobrepo_factory::new_memblob_empty(None)?;
    borrowed!(ctx, repo);
    let file1 = "file1";
    let content = "content";
    let c1 = CreateCommitContext::new_root(&ctx, &repo)
        .add_file(file1, content)
        .commit()
        .await?;

    // Default file size is 10Mb, so blame should be computed
    // without problems.
    fetch_blame(ctx, repo, c1, MPath::new(file1)?).await?;

    let repo = repo.dangerous_override(|mut derived_data_config: DerivedDataConfig| {
        derived_data_config.override_blame_filesize_limit = Some(4);
        derived_data_config
    });

    let file2 = "file2";
    let c2 = CreateCommitContext::new_root(ctx, &repo)
        .add_file(file2, content)
        .commit()
        .await?;

    // We decreased the limit, so derivation should fail now
    let res = fetch_blame(ctx, &repo, c2, MPath::new(file2)?).await;

    match res {
        Err(BlameError::Rejected(_)) => {}
        _ => {
            return Err(anyhow!("unexpected result: {:?}", res));
        }
    }

    Ok(())
}

fn annotate(
    content: Bytes,
    blame: Blame,
    names: &HashMap<ChangesetId, &'static str>,
) -> Result<String, Error> {
    let content = std::str::from_utf8(content.as_ref())?;
    let mut result = String::new();
    let mut ranges = blame.ranges().iter();
    let mut range = ranges
        .next()
        .ok_or_else(|| Error::msg("empty blame for non empty content"))?;
    for (index, line) in content.lines().enumerate() {
        if index as u32 >= range.offset + range.length {
            range = ranges
                .next()
                .ok_or_else(|| Error::msg("not enough ranges in a blame"))?;
        }
        let name = names
            .get(&range.csid)
            .ok_or_else(|| Error::msg("unresolved csid"))?;
        result.push_str(name);
        result.push_str(&": ");
        result.push_str(line);
        result.push_str("\n");
    }
    Ok(result)
}
