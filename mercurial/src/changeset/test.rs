// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use std::collections::BTreeMap;

use quickcheck::{QuickCheck, TestResult};

use mercurial_types::{HgBlob, MPath};

use blobnode::HgBlobNode;
use changeset::{escape, serialize_extras, unescape, Extra, RevlogChangeset, Time};
use nodehash::{HgManifestId, HgNodeHash};

use bytes::Bytes;

const CHANGESET: &[u8] = include_bytes!("cset.bin");
const CHANGESET_NOEXTRA: &[u8] = include_bytes!("cset_noextra.bin");

#[test]
fn test_parse() {
    let csid: HgNodeHash = "0849d280663e46b3e247857f4a68fabd2ba503c3".parse().unwrap();
    let p1: HgNodeHash = "169cb9e47f8e86079ee9fd79972092f78fbf68b1".parse().unwrap();
    let node = HgBlobNode::new(HgBlob::Dirty(Bytes::from(CHANGESET)), Some(&p1), None);
    let cset = RevlogChangeset::parse(node.clone()).expect("parsed");

    assert_eq!(node.nodeid().expect("no nodeid"), csid);

    assert_eq!(
        cset,
        RevlogChangeset {
            parents: *node.parents(),
            manifestid: HgManifestId::new(
                "497522ef3706a1665bf4140497c65b467454e962".parse().unwrap()
            ),
            user: "Mads Kiilerich <madski@unity3d.com>".into(),
            time: Time {
                time: 1383910550,
                tz: -3600,
            },
            extra: Extra(
                vec![("branch".into(), "stable".into())]
                    .into_iter()
                    .collect()
            ),
            files: vec![MPath::new(b"mercurial/util.py").unwrap()],
            comments: r#"util: warn when adding paths ending with \

Paths ending with \ will fail the verification introduced in 684a977c2ae0 when
checking out on Windows ... and if it didn't fail it would probably not do what
the user expected."#.into(),
        }
    );

    let csid: HgNodeHash = "526722d24ee5b3b860d4060e008219e083488356".parse().unwrap();
    let p1: HgNodeHash = "db5eb6a86179ce819db03da9ef2090b32f8e3fc4".parse().unwrap();
    let node = HgBlobNode::new(
        HgBlob::Dirty(Bytes::from(CHANGESET_NOEXTRA)),
        Some(&p1),
        None,
    );
    let cset = RevlogChangeset::parse(node.clone()).expect("parsed");

    assert_eq!(node.nodeid().expect("no nodeid"), csid);

    assert_eq!(
        cset,
        RevlogChangeset {
            parents: *node.parents(),
            manifestid: HgManifestId::new(
                "6c0d10b92d045127f9a3846b59480451fe3bbac9".parse().unwrap()
            ),
            user: "jake@edge2.net".into(),
            time: Time {
                time: 1116031690,
                tz: 25200,
            },
            extra: Extra(vec![].into_iter().collect()),
            files: vec![MPath::new(b"hgweb.py").unwrap()],
            comments: r#"reorganize code into classes
clean up html code for w3c validation
"#.into(),
        }
    );
}

#[test]
fn test_generate() {
    fn test(csid: HgNodeHash, p1: Option<&HgNodeHash>, blob: HgBlob, cs: &[u8]) {
        let node = HgBlobNode::new(blob, p1, None);
        let cset = RevlogChangeset::parse(node.clone()).expect("parsed");

        assert_eq!(node.nodeid().expect("no nodeid"), csid);

        let mut new = Vec::new();

        cset.generate(&mut new).expect("generate failed");

        assert_eq!(new, cs);
    }

    let csid: HgNodeHash = "0849d280663e46b3e247857f4a68fabd2ba503c3".parse().unwrap();
    let p1: HgNodeHash = "169cb9e47f8e86079ee9fd79972092f78fbf68b1".parse().unwrap();
    test(
        csid,
        Some(&p1),
        HgBlob::Dirty(Bytes::from(CHANGESET)),
        CHANGESET,
    );

    let csid: HgNodeHash = "526722d24ee5b3b860d4060e008219e083488356".parse().unwrap();
    let p1: HgNodeHash = "db5eb6a86179ce819db03da9ef2090b32f8e3fc4".parse().unwrap();
    test(
        csid,
        Some(&p1),
        HgBlob::Dirty(Bytes::from(CHANGESET_NOEXTRA)),
        CHANGESET_NOEXTRA,
    );
}

quickcheck! {
    fn escape_roundtrip(s: Vec<u8>) -> bool {
        let esc = escape(&s);
        let unesc = unescape(&esc);
        if s != unesc {
            println!("s: {:?}, esc: {:?}, unesc: {:?}", s, esc, unesc)
        }
        s == unesc
    }
}

fn extras_roundtrip_prop(kv: BTreeMap<Vec<u8>, Vec<u8>>) -> TestResult {
    if kv.keys().any(|k| k.contains(&b':')) {
        return TestResult::discard();
    }

    let extra = Extra(kv);
    let mut enc = Vec::new();
    let () = serialize_extras(&extra, &mut enc).expect("enc failed");
    let new = Extra::from_slice(Some(&enc)).expect("parse failed");

    TestResult::from_bool(new == extra)
}

#[test]
fn extras_roundtrip() {
    QuickCheck::new()
        .tests(50)  // more takes too much time
        .quickcheck(extras_roundtrip_prop as fn(BTreeMap<Vec<u8>, Vec<u8>>) -> TestResult);
}

#[test]
#[ignore]
fn extras_roundtrip_long() {
    QuickCheck::new()
        .tests(1000)
        .quickcheck(extras_roundtrip_prop as fn(BTreeMap<Vec<u8>, Vec<u8>>) -> TestResult);
}
