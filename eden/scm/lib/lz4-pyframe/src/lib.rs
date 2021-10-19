/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

mod lz4;

pub use crate::lz4::compress;
pub use crate::lz4::compresshc;
pub use crate::lz4::decompress;
pub use crate::lz4::decompress_into;
pub use crate::lz4::decompress_size;

pub use lz4::LZ4Error as Error;
pub type Result<T> = std::result::Result<T, Error>;
