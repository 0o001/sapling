// Copyright (c) 2017-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

//! Code to deal with deltas received or sent over the wire.

use bytes::{BigEndian, BufMut, BytesMut};

use mercurial_types::delta::{Delta, Fragment};

use errors::*;
use utils::BytesExt;

const DELTA_HEADER_LEN: usize = 12;

/// Decodes this delta. Consumes the entire buffer, so accepts a BytesMut.
pub fn decode_delta(buf: BytesMut) -> Result<Delta> {
    let mut buf = buf;
    let mut frags = vec![];
    let mut remaining = buf.len();

    while remaining >= DELTA_HEADER_LEN {
        // Each delta fragment has:
        // ---
        // start offset: i32
        // end offset: i32
        // new length: i32
        // content (new length bytes)
        // ---
        let start = buf.drain_i32();
        let end = buf.drain_i32();
        let new_len = buf.drain_i32();
        // TODO: handle negative values for all the above

        let delta_len = (new_len as usize) + DELTA_HEADER_LEN;
        if remaining < delta_len {
            Err(ErrorKind::InvalidDelta(format!(
                "expected {} bytes, {} remaining",
                delta_len,
                remaining
            )))?;
        }

        frags.push(Fragment {
            start: start as usize,
            end: end as usize,
            // TODO: avoid copies here by switching this to Bytes
            content: buf.split_to(new_len as usize).to_vec(),
        });

        remaining -= delta_len;
    }

    if remaining != 0 {
        Err(ErrorKind::InvalidDelta(
            format!("{} trailing bytes in encoded delta", remaining),
        ))?;
    }

    Delta::new(frags)
        .with_context(|_| ErrorKind::InvalidDelta("invalid fragment list".into()))
        .map_err(Error::from)
}

pub fn encode_delta<B: BufMut>(delta: &Delta, out: &mut B) {
    for fragment in delta.fragments() {
        out.put_i32::<BigEndian>(fragment.start as i32);
        out.put_i32::<BigEndian>(fragment.end as i32);
        out.put_i32::<BigEndian>(fragment.content.len() as i32);
        out.put_slice(&fragment.content[..]);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use failure;

    #[test]
    fn invalid_deltas() {
        let short_delta = BytesMut::from(&b"\0\0\0\0\0\0\0\0\0\0\0\x20"[..]);
        assert_matches!(
            decode_delta(short_delta).unwrap_err().downcast::<ErrorKind>(),
            Ok(ErrorKind::InvalidDelta(ref msg))
            if msg == "expected 44 bytes, 12 remaining"
        );

        let short_header = BytesMut::from(&b"\0\0\0\0\0\0"[..]);
        assert_matches!(
            decode_delta(short_header).unwrap_err().downcast::<ErrorKind>(),
            Ok(ErrorKind::InvalidDelta(ref msg))
            if msg == "6 trailing bytes in encoded delta"
        );

        // start = 2, end = 0
        let start_after_end = BytesMut::from(&b"\0\0\0\x02\0\0\0\0\0\0\0\0"[..]);
        match decode_delta(start_after_end) {
            Ok(bad) => panic!("unexpected success {:?}", bad),
            Err(err) => match err.downcast::<failure::Context<ErrorKind>>() {
                Ok(ctxt) => match ctxt.get_context() {
                    &ErrorKind::InvalidDelta(..) => (),
                    bad => panic!("Bad ErrorKind {:?}", bad),
                },
                Err(bad) => panic!("Unexpected error {:?}", bad),
            },
        }
    }

    quickcheck! {
        fn roundtrip(delta: Delta) -> bool {
            let mut out = vec![];
            encode_delta(&delta, &mut out);
            delta == decode_delta(out.into()).unwrap()
        }
    }
}
