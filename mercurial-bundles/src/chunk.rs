// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use bytes::{BigEndian, BufMut, Bytes, BytesMut};
use tokio_io::codec::{Decoder, Encoder};

use errors::*;
use utils::BytesExt;

/// A bundle2 chunk.
///
/// Chunks underlie the bundle2 protocol. A chunk is a series of bytes and is
/// encoded as:
///
/// i32 chunk_size
/// [u8] data (chunk_size bytes)
///
/// Normally chunk_size > 0.
///
/// There are two special kinds of chunks:
///
/// 1. An "empty chunk", which is simply a chunk of size 0. This is represented
///    as a Normal chunk below with an empty Bytes.
/// 2. An "error chunk", which is a chunk with size -1 and no data. Error chunks
///    interrupt a chunk stream and are followed by a new part.
#[derive(Clone, Debug, PartialEq)]
pub struct Chunk(ChunkInner);

// This is separate to prevent constructing chunks with unexpected Bytes objects.
#[derive(Clone, Debug, PartialEq)]
enum ChunkInner {
    Normal(Bytes),
    Error,
}

impl Chunk {
    pub fn new<T: Into<Bytes>>(val: T) -> Result<Self> {
        let bytes: Bytes = val.into();
        if bytes.len() > i32::max_value() as usize {
            bail!(ErrorKind::Bundle2Chunk(format!(
                "chunk of length {} exceeds maximum {}",
                bytes.len(),
                i32::max_value()
            )));
        }
        Ok(Chunk(ChunkInner::Normal(bytes)))
    }

    pub fn empty() -> Self {
        Chunk(ChunkInner::Normal(Bytes::new()))
    }

    pub fn error() -> Self {
        Chunk(ChunkInner::Error)
    }

    pub fn is_normal(&self) -> bool {
        match self.0 {
            ChunkInner::Normal(_) => true,
            ChunkInner::Error => false,
        }
    }

    pub fn is_empty(&self) -> bool {
        match self.0 {
            ChunkInner::Normal(ref bytes) => bytes.is_empty(),
            ChunkInner::Error => false,
        }
    }

    pub fn is_error(&self) -> bool {
        match self.0 {
            ChunkInner::Normal(_) => false,
            ChunkInner::Error => true,
        }
    }

    /// Convert a chunk into its inner bytes.
    ///
    /// Returns an error if this chunk was an error chunk, since those do not
    /// have any bytes associated with them.
    pub fn into_bytes(self) -> Result<Bytes> {
        match self.0 {
            ChunkInner::Normal(bytes) => Ok(bytes),
            ChunkInner::Error => bail!("error chunk, no associated bytes"),
        }
    }
}

/// Encode a bundle2 chunk into a bytestream.
#[derive(Debug)]
pub struct ChunkEncoder;

impl Encoder for ChunkEncoder {
    type Item = Chunk;
    type Error = Error;

    fn encode(&mut self, item: Chunk, dst: &mut BytesMut) -> Result<()> {
        match item.0 {
            ChunkInner::Normal(bytes) => {
                dst.reserve(4 + bytes.len());
                dst.put_i32::<BigEndian>(bytes.len() as i32);
                dst.put_slice(&bytes);
            }
            ChunkInner::Error => {
                dst.reserve(4);
                dst.put_i32::<BigEndian>(-1);
            }
        }
        Ok(())
    }
}

/// Decode a bytestream into bundle2 chunks.
#[allow(dead_code)]
pub struct ChunkDecoder;

impl Decoder for ChunkDecoder {
    type Item = Chunk;
    type Error = Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Chunk>> {
        if src.len() < 4 {
            return Ok(None);
        }

        let len = src.peek_i32();
        if len == -1 {
            src.drain_i32();
            return Ok(Some(Chunk::error()));
        }
        if len < 0 {
            bail!(ErrorKind::Bundle2Chunk(
                format!("chunk length must be >= -1, found {}", len)
            ));
        }

        let len = len as usize;
        if src.len() < 4 + len {
            return Ok(None);
        }

        src.drain_i32();
        let chunk = Chunk::new(src.split_to(len))?;
        Ok(Some(chunk))
    }
}

#[cfg(test)]
mod test {
    use std::io::Cursor;

    use futures::{stream, Sink, Stream};
    use quickcheck::{quickcheck, TestResult};
    use tokio_core::reactor::Core;
    use tokio_io::codec::{FramedRead, FramedWrite};

    use super::*;

    #[test]
    fn test_empty_chunk() {
        let mut buf = BytesMut::with_capacity(4);
        buf.put_i32::<BigEndian>(0);

        let mut decoder = ChunkDecoder;
        let chunk = decoder.decode(&mut buf).unwrap().unwrap();

        assert_eq!(chunk, Chunk::empty());
        assert!(chunk.is_normal());
        assert!(chunk.is_empty());
        assert!(!chunk.is_error());
    }

    #[test]
    fn test_error_chunk() {
        let mut buf = BytesMut::with_capacity(4);
        buf.put_i32::<BigEndian>(-1);

        let mut decoder = ChunkDecoder;
        let chunk = decoder.decode(&mut buf).unwrap().unwrap();

        assert_eq!(chunk, Chunk::error());
        assert!(!chunk.is_normal());
        assert!(!chunk.is_empty());
        assert!(chunk.is_error());
    }

    #[test]
    fn test_invalid_chunk() {
        let mut buf = BytesMut::with_capacity(4);
        buf.put_i32::<BigEndian>(-2);

        let mut decoder = ChunkDecoder;
        let chunk_err = decoder.decode(&mut buf);

        let err = chunk_err.unwrap_err();
        assert_matches!(err.kind(), &ErrorKind::Bundle2Chunk(_));
    }

    #[test]
    fn test_roundtrip() {
        // Avoid using the quickcheck! macro because it eats up line numbers in
        // stack traces.
        quickcheck(roundtrip as fn(Vec<Option<Vec<u8>>>) -> TestResult);
    }

    fn roundtrip(data: Vec<Option<Vec<u8>>>) -> TestResult {
        let count = data.len();
        // Treat Some(bytes) as a normal chunk, None as an error chunk.
        let chunks: Vec<Chunk> = data.into_iter()
            .map(|x| match x {
                Some(b) => Chunk::new(b).unwrap(),
                None => Chunk::error(),
            })
            .collect();
        // Make a clone so we can use chunks later.
        let chunks_res: Vec<Result<Chunk>> = chunks.clone().into_iter().map(|x| Ok(x)).collect();

        let cursor = Cursor::new(Vec::with_capacity(32 * 1024));
        let sink = FramedWrite::new(cursor, ChunkEncoder);

        let encode_fut = sink.send_all(stream::iter(chunks_res));

        let mut core = Core::new().unwrap();

        // Perform encoding.
        let (sink, _) = core.run(encode_fut).unwrap();

        let mut cursor = sink.into_inner();
        cursor.set_position(0);

        // cursor will now have the encoded byte stream. Run it through the decoder.
        let stream = FramedRead::new(cursor, ChunkDecoder);

        let collector: Vec<Chunk> = Vec::with_capacity(count);
        let decode_fut = collector.send_all(stream.map_err(|err| {
            panic!("Unexpected error: {}", err);
        }));

        // Perform decoding.
        let (collector, _) = core.run(decode_fut).unwrap();

        assert_eq!(collector, chunks);

        TestResult::passed()
    }
}
