// Copyright 2018 Facebook, Inc.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

//! Index support for `log`.
//!
//! See [Index] for the main structure.

// File format:
//
// ```plain,ignore
// INDEX       := HEADER + ENTRY_LIST
// HEADER      := '\0'  (takes offset 0, so 0 is not a valid offset for ENTRY)
// ENTRY_LIST  := RADIX | ENTRY_LIST + ENTRY
// ENTRY       := RADIX | LEAF | LINK | KEY | ROOT + REVERSED(VLQ(ROOT_LEN))
// RADIX       := '\2' + RADIX_FLAG (1 byte) + BITMAP (2 bytes) +
//                PTR2(RADIX | LEAF) * popcnt(BITMAP) + PTR2(LINK)
// LEAF        := '\3' + PTR(KEY | EXT_KEY) + PTR(LINK)
// LINK        := '\4' + VLQ(VALUE) + PTR(NEXT_LINK | NULL)
// KEY         := '\5' + VLQ(KEY_LEN) + KEY_BYTES
// EXT_KEY     := '\6' + VLQ(KEY_START) + VLQ(KEY_LEN)
// INLINE_LEAF := '\7' + EXT_KEY + LINK
// ROOT        := '\1' + PTR(RADIX) + VLQ(META_LEN) + META
//
// PTR(ENTRY)  := VLQ(the offset of ENTRY)
// PTR2(ENTRY) := the offset of ENTRY, in 0 or 4, or 8 bytes depending on BITMAP and FLAGS
//
// RADIX_FLAG := USE_64_BIT (1 bit) + RESERVED (6 bits) + HAVE_LINK (1 bit)
// ```
//
// Some notes about the format:
//
// - A "RADIX" entry has 16 children. This is mainly for source control hex hashes. The "N"
//   in a radix entry could be less than 16 if some of the children are missing (ex. offset = 0).
//   The corresponding jump table bytes of missing children are 0s. If child i exists, then
//   `jumptable[i]` is the relative (to the beginning of radix entry) offset of PTR(child offset).
// - A "ROOT" entry its length recorded as the last byte. Normally the root entry is written
//   at the end. This makes it easier for the caller - it does not have to record the position
//   of the root entry. The caller could optionally provide a root location.
// - An entry has a 1 byte "type". This makes it possible to do a linear scan from the
//   beginning of the file, instead of having to go through a root. Potentially useful for
//   recovery purpose, or adding new entry types (ex. tree entries other than the 16-children
//   radix entry, value entries that are not u64 linked list, key entries that refers external
//   buffer).
// - The "EXT_KEY" type has a logically similar function with "KEY". But it refers to an external
//   buffer. This is useful to save spaces if the index is not a source of truth and keys are
//   long.
// - The "INLINE_LEAF" type is basically an inlined version of EXT_KEY and LINK, to save space.
// - The "ROOT_LEN" is reversed so it can be read byte-by-byte from the end of a file.

use byteorder::{ByteOrder, LittleEndian, WriteBytesExt};
use std::fmt::{self, Debug, Formatter};
use std::fs::{self, File};
use std::io::{self, Seek, SeekFrom, Write};
use std::mem::size_of;
use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;

use std::io::ErrorKind::InvalidData;

use base16::Base16Iter;
use checksum_table::ChecksumTable;
use lock::ScopedFileLock;
use utils::mmap_readonly;

use fs2::FileExt;
use memmap::Mmap;
use vlqencoding::{VLQDecodeAt, VLQEncode};

//// Structures and serialization

#[derive(Clone, PartialEq, Default)]
struct MemRadix {
    pub offsets: [Offset; 16],
    pub link_offset: LinkOffset,
}

#[derive(Clone, PartialEq)]
struct MemLeaf {
    pub key_offset: Offset,
    pub link_offset: LinkOffset,
}

#[derive(Clone, PartialEq)]
struct MemKey {
    pub key: Box<[u8]>, // base256
}

#[derive(Clone, PartialEq)]
struct MemExtKey {
    pub start: u64,
    pub len: u64,
}

#[derive(Clone, PartialEq)]
struct MemLink {
    pub value: u64,
    pub next_link_offset: LinkOffset,
    pub unused: bool,
}

#[derive(Clone, PartialEq)]
struct MemRoot {
    pub radix_offset: RadixOffset,
    pub meta: Box<[u8]>,
}

// Shorter alias to `Option<ChecksumTable>`
type Checksum = Option<ChecksumTable>;

// Helper method to do checksum
#[inline]
fn verify_checksum(checksum: &Checksum, start: u64, length: u64) -> io::Result<()> {
    if let &Some(ref table) = checksum {
        if !table.check_range(start, length) {
            return Err(integrity_error());
        }
    }
    Ok(())
}

#[inline]
fn integrity_error() -> io::Error {
    io::Error::new(InvalidData, "integrity check failed")
}

/// Read reversed vlq at the given end offset (exclusive).
/// Return the decoded integer and the bytes used by the VLQ integer.
fn read_vlq_reverse(buf: &[u8], end_offset: usize) -> io::Result<(u64, usize)> {
    let buf = buf.as_ref();
    let mut int_buf = Vec::new();
    for i in (0..end_offset).rev() {
        int_buf.push(buf[i]);
        if buf[i] <= 127 {
            break;
        }
    }
    let (value, vlq_size) = int_buf.read_vlq_at(0)?;
    assert_eq!(vlq_size, int_buf.len());
    Ok((value, vlq_size))
}

// Offsets that are >= DIRTY_OFFSET refer to in-memory entries that haven't been
// written to disk. Offsets < DIRTY_OFFSET are on-disk offsets.
const DIRTY_OFFSET: u64 = 1u64 << 63;

const TYPE_HEAD: u8 = 0;
const TYPE_ROOT: u8 = 1;
const TYPE_RADIX: u8 = 2;
const TYPE_LEAF: u8 = 3;
const TYPE_LINK: u8 = 4;
const TYPE_KEY: u8 = 5;
const TYPE_EXT_KEY: u8 = 6;
const TYPE_INLINE_LEAF: u8 = 7;

// Bits needed to represent the above type integers.
const TYPE_BITS: usize = 3;

// Size constants. Do not change.
const TYPE_BYTES: usize = 1;
const RADIX_FLAG_BYTES: usize = 1;
const RADIX_BITMAP_BYTES: usize = 2;

// Bit flags used by radix
const RADIX_FLAG_USE_64BIT: u8 = 1;
const RADIX_FLAG_HAVE_LINK: u8 = 1 << 7;

/// Offset to an entry. The type of the entry is yet to be resolved.
#[derive(Copy, Clone, PartialEq, PartialOrd, Default)]
pub struct Offset(u64);

// Typed offsets. Constructed after verifying types.
// `LinkOffset` is public since it's exposed by some APIs.

#[derive(Copy, Clone, PartialEq, PartialOrd, Default)]
struct RadixOffset(Offset);
#[derive(Copy, Clone, PartialEq, PartialOrd, Default)]
struct LeafOffset(Offset);

/// Offset to a linked list entry.
///
/// The entry stores a [u64] integer and optionally, the next [LinkOffset].
#[derive(Copy, Clone, PartialEq, PartialOrd, Default)]
pub struct LinkOffset(Offset);
#[derive(Copy, Clone, PartialEq, PartialOrd, Default)]
struct KeyOffset(Offset);
#[derive(Copy, Clone, PartialEq, PartialOrd, Default)]
struct ExtKeyOffset(Offset);

#[derive(Copy, Clone)]
enum TypedOffset {
    Radix(RadixOffset),
    Leaf(LeafOffset),
    Link(LinkOffset),
    Key(KeyOffset),
    ExtKey(ExtKeyOffset),
}

impl Offset {
    /// Convert `io::Result<u64>` read from disk to a non-dirty `Offset`.
    /// Return `InvalidData` error if the offset is dirty.
    #[inline]
    fn from_disk(value: u64) -> io::Result<Self> {
        if value >= DIRTY_OFFSET {
            Err(InvalidData.into())
        } else {
            Ok(Offset(value))
        }
    }

    /// Convert a possibly "dirty" offset to a non-dirty offset.
    /// Useful when writing offsets to disk.
    #[inline]
    fn to_disk(self, offset_map: &OffsetMap) -> u64 {
        offset_map.get(self)
    }

    /// Convert to `TypedOffset`.
    #[inline]
    fn to_typed(self, buf: &[u8], checksum: &Checksum) -> io::Result<TypedOffset> {
        let type_int = self.type_int(buf, checksum)?;
        match type_int {
            TYPE_RADIX => Ok(TypedOffset::Radix(RadixOffset(self))),
            TYPE_LEAF => Ok(TypedOffset::Leaf(LeafOffset(self))),
            TYPE_LINK => Ok(TypedOffset::Link(LinkOffset(self))),
            TYPE_KEY => Ok(TypedOffset::Key(KeyOffset(self))),
            TYPE_EXT_KEY => Ok(TypedOffset::ExtKey(ExtKeyOffset(self))),
            // LeafOffset handles inline transparently.
            TYPE_INLINE_LEAF => Ok(TypedOffset::Leaf(LeafOffset(self))),
            _ => Err(InvalidData.into()),
        }
    }

    /// Read the `type_int` value.
    #[inline]
    fn type_int(self, buf: &[u8], checksum: &Checksum) -> io::Result<u8> {
        if self.is_null() {
            Err(InvalidData.into())
        } else if self.is_dirty() {
            Ok(((self.0 - DIRTY_OFFSET) & ((1 << TYPE_BITS) - 1)) as u8)
        } else {
            verify_checksum(checksum, self.0, TYPE_BYTES as u64)?;
            match buf.get(self.0 as usize) {
                Some(x) => Ok(*x as u8),
                _ => return Err(InvalidData.into()),
            }
        }
    }

    /// Test whether the offset is null (0).
    #[inline]
    fn is_null(self) -> bool {
        self.0 == 0
    }

    /// Test whether the offset points to an in-memory entry.
    #[inline]
    fn is_dirty(self) -> bool {
        self.0 >= DIRTY_OFFSET
    }
}

// Common methods shared by typed offset structs.
trait TypedOffsetMethods: Sized {
    #[inline]
    fn dirty_index(self) -> usize {
        debug_assert!(self.to_offset().is_dirty());
        ((self.to_offset().0 - DIRTY_OFFSET) >> TYPE_BITS) as usize
    }

    #[inline]
    fn from_offset(offset: Offset, buf: &[u8], checksum: &Checksum) -> io::Result<Self> {
        if offset.is_null() {
            Ok(Self::from_offset_unchecked(offset))
        } else {
            let type_int = offset.type_int(buf, checksum)?;
            if type_int == Self::type_int() {
                Ok(Self::from_offset_unchecked(offset))
            } else {
                Err(InvalidData.into())
            }
        }
    }

    #[inline]
    fn from_dirty_index(index: usize) -> Self {
        Self::from_offset_unchecked(Offset(
            (((index as u64) << TYPE_BITS) | Self::type_int() as u64) + DIRTY_OFFSET,
        ))
    }

    #[inline]
    fn type_int() -> u8;

    #[inline]
    fn from_offset_unchecked(offset: Offset) -> Self;

    #[inline]
    fn to_offset(&self) -> Offset;
}

// Implement traits for typed offset structs.
macro_rules! impl_offset {
    ($type:ident, $type_int:expr, $name:expr) => {
        impl TypedOffsetMethods for $type {
            #[inline]
            fn type_int() -> u8 {
                $type_int
            }

            #[inline]
            fn from_offset_unchecked(offset: Offset) -> Self {
                $type(offset)
            }

            #[inline]
            fn to_offset(&self) -> Offset {
                self.0
            }
        }

        impl Deref for $type {
            type Target = Offset;

            #[inline]
            fn deref(&self) -> &Offset {
                &self.0
            }
        }

        impl Debug for $type {
            fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
                if self.is_null() {
                    write!(f, "None")
                } else {
                    if self.is_dirty() {
                        write!(f, "{}[{}]", $name, self.dirty_index())
                    } else {
                        // `Offset` will print "Disk[{}]".
                        self.0.fmt(f)
                    }
                }
            }
        }

        impl From<$type> for Offset {
            #[inline]
            fn from(x: $type) -> Offset {
                x.0
            }
        }

        impl From<$type> for u64 {
            #[inline]
            fn from(x: $type) -> u64 {
                (x.0).0
            }
        }

        impl From<$type> for usize {
            #[inline]
            fn from(x: $type) -> usize {
                (x.0).0 as usize
            }
        }
    };
}

impl_offset!(RadixOffset, TYPE_RADIX, "Radix");
impl_offset!(LeafOffset, TYPE_LEAF, "Leaf");
impl_offset!(LinkOffset, TYPE_LINK, "Link");
impl_offset!(KeyOffset, TYPE_KEY, "Key");
impl_offset!(ExtKeyOffset, TYPE_EXT_KEY, "ExtKey");

impl RadixOffset {
    /// Link offset of a radix entry.
    #[inline]
    fn link_offset(self, index: &Index) -> io::Result<LinkOffset> {
        if self.is_dirty() {
            Ok(index.dirty_radixes[self.dirty_index()].link_offset)
        } else {
            let flag_start = TYPE_BYTES + usize::from(self);
            let flag = *index.buf.get(flag_start).ok_or_else(integrity_error)?;
            index.verify_checksum(
                flag_start as u64,
                (RADIX_FLAG_BYTES + RADIX_BITMAP_BYTES) as u64,
            )?;

            if Self::parse_have_link_from_flag(flag) {
                let bitmap_start = flag_start + RADIX_FLAG_BYTES;
                let bitmap = Self::read_bitmap_unchecked(&index.buf, bitmap_start)?;
                let int_size = Self::parse_int_size_from_flag(flag);
                let link_offset =
                    bitmap_start + RADIX_BITMAP_BYTES + bitmap.count_ones() as usize * int_size;
                index.verify_checksum(link_offset as u64, int_size as u64)?;
                let raw_offset = Self::read_raw_int_unchecked(&index.buf, int_size, link_offset)?;
                LinkOffset::from_offset(Offset::from_disk(raw_offset)?, &index.buf, &index.checksum)
            } else {
                Ok(LinkOffset::default())
            }
        }
    }

    /// Lookup the `i`-th child inside a radix entry.
    /// Return stored offset, or `Offset(0)` if that child does not exist.
    #[inline]
    fn child(self, index: &Index, i: u8) -> io::Result<Offset> {
        debug_assert!(i < 16);
        if self.is_dirty() {
            Ok(index.dirty_radixes[self.dirty_index()].offsets[i as usize])
        } else {
            let flag_start = TYPE_BYTES + usize::from(self);
            let bitmap_start = flag_start + RADIX_FLAG_BYTES;
            // Integrity of "bitmap" is checked below to reduce calls to verify_checksum, since
            // this is a hot path.
            let bitmap = Self::read_bitmap_unchecked(&index.buf, bitmap_start)?;
            let has_child = (1u16 << i) & bitmap != 0;
            if has_child {
                let flag = *index.buf.get(flag_start).ok_or_else(integrity_error)?;
                let int_size = Self::parse_int_size_from_flag(flag);
                let skip_child_count = (((1u16 << i) - 1) & bitmap).count_ones() as usize;
                let child_offset = bitmap_start + RADIX_BITMAP_BYTES + skip_child_count * int_size;
                index.verify_checksum(
                    flag_start as u64,
                    (child_offset + int_size - flag_start) as u64,
                )?;
                let raw_offset = Self::read_raw_int_unchecked(&index.buf, int_size, child_offset)?;
                Ok(Offset::from_disk(raw_offset)?)
            } else {
                index.verify_checksum(bitmap_start as u64, RADIX_BITMAP_BYTES as u64)?;
                Ok(Offset::default())
            }
        }
    }

    /// Copy an on-disk entry to memory so it can be modified. Return new offset.
    /// If the offset is already in-memory, return it as-is.
    #[inline]
    fn copy(self, index: &mut Index) -> io::Result<RadixOffset> {
        if self.is_dirty() {
            Ok(self)
        } else {
            let entry = MemRadix::read_from(&index.buf, u64::from(self), &index.checksum)?;
            let len = index.dirty_radixes.len();
            index.dirty_radixes.push(entry);
            Ok(RadixOffset::from_dirty_index(len))
        }
    }

    /// Change a child of `MemRadix`. Panic if the offset points to an on-disk entry.
    #[inline]
    fn set_child(self, index: &mut Index, i: u8, value: Offset) {
        assert!(i < 16);
        if self.is_dirty() {
            index.dirty_radixes[self.dirty_index()].offsets[i as usize] = value;
        } else {
            panic!("bug: set_child called on immutable radix entry");
        }
    }

    /// Change link offset of `MemRadix`. Panic if the offset points to an on-disk entry.
    #[inline]
    fn set_link(self, index: &mut Index, value: LinkOffset) {
        if self.is_dirty() {
            index.dirty_radixes[self.dirty_index()].link_offset = value.into();
        } else {
            panic!("bug: set_link called on immutable radix entry");
        }
    }

    /// Create a new in-memory radix entry.
    #[inline]
    fn create(index: &mut Index, radix: MemRadix) -> RadixOffset {
        let len = index.dirty_radixes.len();
        index.dirty_radixes.push(radix);
        RadixOffset::from_dirty_index(len)
    }

    /// Parse whether link offset exists from a flag.
    #[inline]
    fn parse_have_link_from_flag(flag: u8) -> bool {
        flag & RADIX_FLAG_HAVE_LINK != 0
    }

    /// Parse int size (in bytes) from a flag.
    #[inline]
    fn parse_int_size_from_flag(flag: u8) -> usize {
        if flag & RADIX_FLAG_USE_64BIT == 0 {
            size_of::<u32>()
        } else {
            size_of::<u64>()
        }
    }

    /// Read bitmap from the given offset without integrity check.
    #[inline]
    fn read_bitmap_unchecked(buf: &[u8], bitmap_offset: usize) -> io::Result<u16> {
        debug_assert_eq!(RADIX_BITMAP_BYTES, size_of::<u16>());
        Ok(LittleEndian::read_u16(buf.get(
            bitmap_offset..bitmap_offset + RADIX_BITMAP_BYTES,
        ).ok_or_else(integrity_error)?))
    }

    /// Read integer from the given offset without integrity check.
    #[inline]
    fn read_raw_int_unchecked(buf: &[u8], int_size: usize, offset: usize) -> io::Result<u64> {
        Ok(match int_size {
            4 => LittleEndian::read_u32(buf.get(offset..offset + 4).ok_or_else(integrity_error)?)
                as u64,
            8 => LittleEndian::read_u64(buf.get(offset..offset + 8).ok_or_else(integrity_error)?),
            _ => unreachable!(),
        })
    }
}

/// Extract key_content from an untyped Offset. Internal use only.
fn extract_key_content(index: &Index, key_offset: Offset) -> io::Result<&[u8]> {
    let typed_offset = key_offset.to_typed(&index.buf, &index.checksum)?;
    match typed_offset {
        TypedOffset::Key(x) => Ok(x.key_content(index)?),
        TypedOffset::ExtKey(x) => Ok(x.key_content(index)?),
        _ => Err(InvalidData.into()),
    }
}

impl LeafOffset {
    /// Key content and link offsets of a leaf entry.
    #[inline]
    fn key_and_link_offset(self, index: &Index) -> io::Result<(&[u8], LinkOffset)> {
        if self.is_dirty() {
            let e = &index.dirty_leafs[self.dirty_index()];
            let key_content = extract_key_content(index, e.key_offset)?;
            Ok((key_content, e.link_offset))
        } else {
            let (key_content, raw_link_offset) = match index.buf[usize::from(self)] {
                TYPE_INLINE_LEAF => {
                    let raw_key_offset = u64::from(self) + TYPE_BYTES as u64;
                    let key_offset = ExtKeyOffset::from_offset(
                        Offset::from_disk(raw_key_offset)?,
                        &index.buf,
                        &None,
                    )?;
                    // Avoid using key_content. Skip one checksum check.
                    let (key_content, key_entry_size) =
                        key_offset.key_content_and_entry_size_unchecked(index)?;
                    let key_entry_size = key_entry_size.unwrap();
                    let raw_link_offset = raw_key_offset + key_entry_size as u64;
                    index.verify_checksum(
                        u64::from(self),
                        raw_link_offset as u64 - u64::from(self),
                    )?;
                    (key_content, raw_link_offset)
                }
                TYPE_LEAF => {
                    let (raw_key_offset, vlq_len): (u64, _) =
                        index.buf.read_vlq_at(usize::from(self) + TYPE_BYTES)?;
                    let key_offset = Offset::from_disk(raw_key_offset)?;
                    let key_content = extract_key_content(index, key_offset)?;
                    let (raw_link_offset, vlq_len2) = index
                        .buf
                        .read_vlq_at(usize::from(self) + TYPE_BYTES + vlq_len)?;
                    index.verify_checksum(
                        u64::from(self),
                        (TYPE_BYTES + vlq_len + vlq_len2) as u64,
                    )?;
                    (key_content, raw_link_offset)
                }
                _ => unreachable!("bug: LeafOffset constructed with non-leaf types"),
            };
            let link_offset = LinkOffset::from_offset(
                Offset::from_disk(raw_link_offset as u64)?,
                &index.buf,
                &index.checksum,
            )?;
            Ok((key_content, link_offset))
        }
    }

    /// Create a new in-memory leaf entry. The key entry cannot be null.
    #[inline]
    fn create(index: &mut Index, link_offset: LinkOffset, key_offset: Offset) -> LeafOffset {
        debug_assert!(!key_offset.is_null());
        let len = index.dirty_leafs.len();
        index.dirty_leafs.push(MemLeaf {
            link_offset,
            key_offset,
        });
        LeafOffset::from_dirty_index(len)
    }

    /// Update link_offset of a leaf entry in-place. Copy on write. Return the new leaf_offset
    /// if it's copied from disk.
    ///
    /// Note: the old leaf is expected to be no longer needed. If that's not true, don't call
    /// this function.
    #[inline]
    fn set_link(self, index: &mut Index, link_offset: LinkOffset) -> io::Result<LeafOffset> {
        if self.is_dirty() {
            index.dirty_leafs[self.dirty_index()].link_offset = link_offset;
            Ok(self)
        } else {
            let entry = MemLeaf::read_from(&index.buf, u64::from(self), &index.checksum)?;
            Ok(Self::create(index, link_offset, entry.key_offset))
        }
    }

    /// Mark the entry as unused. An unused entry won't be written to disk.
    /// No effect on an on-disk entry.
    fn mark_unused(self, index: &mut Index) {
        if self.is_dirty() {
            let key_offset = index.dirty_leafs[self.dirty_index()].key_offset;
            match key_offset.to_typed(&index.buf, &index.checksum) {
                Ok(TypedOffset::Key(x)) => x.mark_unused(index),
                Ok(TypedOffset::ExtKey(x)) => x.mark_unused(index),
                _ => (),
            };
            index.dirty_leafs[self.dirty_index()].mark_unused()
        }
    }
}

/// Iterator for values in the linked list
pub struct LeafValueIter<'a> {
    index: &'a Index,
    offset: LinkOffset,
    errored: bool,
}

impl<'a> Iterator for LeafValueIter<'a> {
    type Item = io::Result<u64>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset.is_null() || self.errored {
            None
        } else {
            match self.offset.value_and_next(self.index) {
                Ok((value, next)) => {
                    self.offset = next;
                    Some(Ok(value))
                }
                Err(e) => {
                    self.errored = true;
                    Some(Err(e))
                }
            }
        }
    }
}

impl LinkOffset {
    /// Iterating through values referred by this linked list.
    pub fn values<'a>(self, index: &'a Index) -> LeafValueIter<'a> {
        LeafValueIter {
            errored: false,
            index,
            offset: self,
        }
    }

    /// Get value, and the next link offset.
    #[inline]
    fn value_and_next(self, index: &Index) -> io::Result<(u64, LinkOffset)> {
        if self.is_dirty() {
            let e = &index.dirty_links[self.dirty_index()];
            Ok((e.value, e.next_link_offset))
        } else {
            let (value, vlq_len) = index.buf.read_vlq_at(usize::from(self) + TYPE_BYTES)?;
            let (next_link, vlq_len2) = index
                .buf
                .read_vlq_at(usize::from(self) + TYPE_BYTES + vlq_len)?;
            index.verify_checksum(u64::from(self), (TYPE_BYTES + vlq_len + vlq_len2) as u64)?;
            let next_link = LinkOffset::from_offset(
                Offset::from_disk(next_link)?,
                &index.buf,
                &index.checksum,
            )?;
            Ok((value, next_link))
        }
    }

    /// Create a new link entry that chains this entry.
    /// Return new `LinkOffset`
    fn create(self, index: &mut Index, value: u64) -> LinkOffset {
        let new_link = MemLink {
            value,
            next_link_offset: self.into(),
            unused: false,
        };
        let len = index.dirty_links.len();
        index.dirty_links.push(new_link);
        LinkOffset::from_dirty_index(len)
    }
}

impl KeyOffset {
    /// Key content of a key entry.
    #[inline]
    fn key_content(self, index: &Index) -> io::Result<&[u8]> {
        if self.is_dirty() {
            Ok(&index.dirty_keys[self.dirty_index()].key[..])
        } else {
            let (key_len, vlq_len): (usize, _) =
                index.buf.read_vlq_at(usize::from(self) + TYPE_BYTES)?;
            let start = usize::from(self) + TYPE_BYTES + vlq_len;
            let end = start + key_len;
            index.verify_checksum(u64::from(self), end as u64 - u64::from(self))?;
            if end > index.buf.len() {
                Err(InvalidData.into())
            } else {
                Ok(&index.buf[start..end])
            }
        }
    }

    /// Create a new in-memory key entry. The key cannot be empty.
    #[inline]
    fn create(index: &mut Index, key: &[u8]) -> KeyOffset {
        debug_assert!(key.len() > 0);
        let len = index.dirty_keys.len();
        index.dirty_keys.push(MemKey {
            key: Vec::from(key).into_boxed_slice(),
        });
        KeyOffset::from_dirty_index(len)
    }

    /// Mark the entry as unused. An unused entry won't be written to disk.
    /// No effect on an on-disk entry.
    fn mark_unused(self, index: &mut Index) {
        if self.is_dirty() {
            index.dirty_keys[self.dirty_index()].mark_unused();
        }
    }
}

impl ExtKeyOffset {
    /// Key content of a key entry.
    #[inline]
    fn key_content(self, index: &Index) -> io::Result<&[u8]> {
        let (key_content, entry_size) = self.key_content_and_entry_size_unchecked(index)?;
        if let Some(entry_size) = entry_size {
            index.verify_checksum(u64::from(self), entry_size as u64)?;
        }
        Ok(key_content)
    }

    /// Key content and key entry size. Used internally.
    #[inline]
    fn key_content_and_entry_size_unchecked(
        self,
        index: &Index,
    ) -> io::Result<(&[u8], Option<usize>)> {
        let (start, len, entry_size) = if self.is_dirty() {
            let e = &index.dirty_ext_keys[self.dirty_index()];
            (e.start, e.len, None)
        } else {
            let (start, vlq_len1): (u64, _) =
                index.buf.read_vlq_at(usize::from(self) + TYPE_BYTES)?;
            let (len, vlq_len2): (u64, _) = index
                .buf
                .read_vlq_at(usize::from(self) + TYPE_BYTES + vlq_len1)?;
            (start, len, Some(TYPE_BYTES + vlq_len1 + vlq_len2))
        };
        let key_buf = index.key_buf.as_ref().as_ref();
        Ok((&key_buf[start as usize..(start + len) as usize], entry_size))
    }

    /// Create a new in-memory external key entry. The key cannot be empty.
    #[inline]
    fn create(index: &mut Index, start: u64, len: u64) -> ExtKeyOffset {
        debug_assert!(len > 0);
        let i = index.dirty_ext_keys.len();
        index.dirty_ext_keys.push(MemExtKey { start, len });
        ExtKeyOffset::from_dirty_index(i)
    }

    /// Mark the entry as unused. An unused entry won't be written to disk.
    /// No effect on an on-disk entry.
    fn mark_unused(self, index: &mut Index) {
        if self.is_dirty() {
            index.dirty_ext_keys[self.dirty_index()].mark_unused();
        }
    }
}

/// Check type for an on-disk entry
fn check_type(buf: &[u8], offset: usize, expected: u8) -> io::Result<()> {
    let typeint = *(buf.get(offset).ok_or(InvalidData)?);
    if typeint != expected {
        Err(InvalidData.into())
    } else {
        Ok(())
    }
}

impl MemRadix {
    fn read_from<B: AsRef<[u8]>>(buf: B, offset: u64, checksum: &Checksum) -> io::Result<Self> {
        let buf = buf.as_ref();
        let offset = offset as usize;
        let mut pos = 0;

        // Integrity check is done at the end to reduce overhead.
        check_type(buf, offset, TYPE_RADIX)?;
        pos += TYPE_BYTES;

        let flag = *buf.get(offset + pos).ok_or_else(integrity_error)?;
        pos += RADIX_FLAG_BYTES;

        let bitmap = RadixOffset::read_bitmap_unchecked(buf, offset + pos)?;
        pos += RADIX_BITMAP_BYTES;

        let int_size = RadixOffset::parse_int_size_from_flag(flag);

        let mut offsets = [Offset::default(); 16];
        for i in 0..16 {
            if (bitmap >> i) & 1 == 1 {
                offsets[i] = Offset::from_disk(RadixOffset::read_raw_int_unchecked(
                    buf,
                    int_size,
                    offset + pos,
                )?)?;
                pos += int_size;
            }
        }

        let link_offset = if RadixOffset::parse_have_link_from_flag(flag) {
            let raw_offset = RadixOffset::read_raw_int_unchecked(buf, int_size, offset + pos)?;
            pos += int_size;
            LinkOffset::from_offset(Offset::from_disk(raw_offset)?, buf, checksum)?
        } else {
            LinkOffset::default()
        };

        verify_checksum(checksum, offset as u64, pos as u64)?;

        Ok(MemRadix {
            offsets,
            link_offset,
        })
    }

    fn write_to<W: Write>(&self, writer: &mut W, offset_map: &OffsetMap) -> io::Result<()> {
        // Prepare data to write
        let mut flag = 0;
        let mut bitmap = 0;
        let u32_max = ::std::u32::MAX as u64;

        let link_offset = if !self.link_offset.is_null() {
            flag |= RADIX_FLAG_HAVE_LINK;
            let link_offset = self.link_offset.to_disk(offset_map);
            if link_offset > u32_max {
                flag |= RADIX_FLAG_USE_64BIT;
            }
            link_offset
        } else {
            0
        };

        let mut child_offsets = [0u64; 16];
        for i in 0..16 {
            let child_offset = self.offsets[i];
            if !child_offset.is_null() {
                bitmap |= 1u16 << i;
                let child_offset = child_offset.to_disk(offset_map);
                if child_offset > u32_max {
                    flag |= RADIX_FLAG_USE_64BIT;
                }
                child_offsets[i] = child_offset;
            }
        }

        // Write them
        writer.write_all(&[TYPE_RADIX, flag])?;
        writer.write_u16::<LittleEndian>(bitmap)?;

        if flag & RADIX_FLAG_USE_64BIT != 0 {
            for &child_offset in child_offsets.iter() {
                if child_offset > 0 {
                    writer.write_u64::<LittleEndian>(child_offset)?;
                }
            }
            if link_offset > 0 {
                writer.write_u64::<LittleEndian>(link_offset)?;
            }
        } else {
            for &child_offset in child_offsets.iter() {
                if child_offset > 0 {
                    writer.write_u32::<LittleEndian>(child_offset as u32)?;
                }
            }
            if link_offset > 0 {
                writer.write_u32::<LittleEndian>(link_offset as u32)?;
            }
        }
        Ok(())
    }
}

impl MemLeaf {
    fn read_from<B: AsRef<[u8]>>(buf: B, offset: u64, checksum: &Checksum) -> io::Result<Self> {
        let buf = buf.as_ref();
        let offset = offset as usize;
        match buf.get(offset) {
            Some(&TYPE_INLINE_LEAF) => {
                let key_offset = offset + TYPE_BYTES;
                // Skip the key part
                let offset = key_offset + TYPE_BYTES;
                let (_key_start, vlq_len): (u64, _) = buf.read_vlq_at(offset)?;
                let offset = offset + vlq_len;
                let (_key_len, vlq_len): (u64, _) = buf.read_vlq_at(offset)?;
                let offset = offset + vlq_len;
                // Checksum will be verified by ExtKey and Leaf nodes
                let key_offset = Offset::from_disk(key_offset as u64)?;
                let link_offset =
                    LinkOffset::from_offset(Offset::from_disk(offset as u64)?, buf, checksum)?;
                Ok(MemLeaf {
                    key_offset,
                    link_offset,
                })
            }
            Some(&TYPE_LEAF) => {
                let (key_offset, len1) = buf.read_vlq_at(offset + TYPE_BYTES)?;
                let key_offset = Offset::from_disk(key_offset)?;
                let (link_offset, len2) = buf.read_vlq_at(offset + TYPE_BYTES + len1)?;
                let link_offset =
                    LinkOffset::from_offset(Offset::from_disk(link_offset)?, buf, checksum)?;
                verify_checksum(checksum, offset as u64, (TYPE_BYTES + len1 + len2) as u64)?;
                Ok(MemLeaf {
                    key_offset,
                    link_offset,
                })
            }
            _ => Err(integrity_error()),
        }
    }

    /// If the entry is suitable for writing inline, write a inline entry, mark dependent
    /// entries as "unused", and return `true`. Otherwise do nothing and return `false`.
    ///
    /// The caller probably wants to set this entry to "unused" to prevent writing twice,
    /// if true is returned.
    fn maybe_write_inline_to<W: Write>(
        &self,
        writer: &mut W,
        buf: &[u8],
        buf_offset: u64,
        dirty_ext_keys: &mut Vec<MemExtKey>,
        dirty_links: &mut Vec<MemLink>,
        offset_map: &mut OffsetMap,
    ) -> io::Result<bool> {
        debug_assert!(!self.is_unused());

        // Conditions to be inlined:
        // - Both Key and Link are dirty (in-memory). Otherwise this might waste space.
        // - Key is ExtKey. This is just to make implemenation easier. Owned key support might be
        // added in the future.
        // - Link does not refer to another in-memory link that hasn't been written yet (i.e.
        //   does not exist in offset_map). This is just to make implemenation easier.

        let are_dependencies_dirty = self.key_offset.is_dirty() && self.link_offset.is_dirty();

        if are_dependencies_dirty {
            if let Ok(TypedOffset::ExtKey(key_offset)) = self.key_offset.to_typed(buf, &None) {
                let ext_key_index = key_offset.dirty_index();
                let link_index = self.link_offset.dirty_index();
                let mut ext_key = dirty_ext_keys.get_mut(ext_key_index).unwrap();
                let mut link = dirty_links.get_mut(link_index).unwrap();

                let next_link_offset = link.next_link_offset;
                if next_link_offset.is_dirty()
                    && offset_map.link_map[next_link_offset.dirty_index()] == 0
                {
                    // Dependent Link is not written yet.
                    return Ok(false);
                }

                // Header
                writer.write_all(&[TYPE_INLINE_LEAF])?;

                // Inlined ExtKey
                let offset = buf.len() as u64 + buf_offset;
                offset_map.ext_key_map[ext_key_index] = offset;
                ext_key.write_to(writer, offset_map)?;

                // Inlined Link
                let offset = buf.len() as u64 + buf_offset;
                offset_map.link_map[ext_key_index] = offset;
                link.write_to(writer, offset_map)?;

                ext_key.mark_unused();
                link.mark_unused();

                Ok(true)
            } else {
                // InlineLeaf only supports ExtKey, not embeeded Key.
                Ok(false)
            }
        } else {
            Ok(false)
        }
    }

    /// Write a Leaf entry.
    fn write_noninline_to<W: Write>(
        &self,
        writer: &mut W,
        offset_map: &OffsetMap,
    ) -> io::Result<()> {
        debug_assert!(!self.is_unused());
        writer.write_all(&[TYPE_LEAF])?;
        writer.write_vlq(self.key_offset.to_disk(offset_map))?;
        writer.write_vlq(self.link_offset.to_disk(offset_map))?;
        Ok(())
    }

    /// Mark the entry as unused. An unused entry won't be written to disk.
    fn mark_unused(&mut self) {
        self.key_offset = Offset::default();
    }

    #[inline]
    fn is_unused(&self) -> bool {
        self.key_offset.is_null()
    }
}

impl MemLink {
    fn read_from<B: AsRef<[u8]>>(buf: B, offset: u64, checksum: &Checksum) -> io::Result<Self> {
        let buf = buf.as_ref();
        let offset = offset as usize;
        check_type(buf, offset, TYPE_LINK)?;
        let (value, len1) = buf.read_vlq_at(offset + 1)?;
        let (next_link_offset, len2) = buf.read_vlq_at(offset + TYPE_BYTES + len1)?;
        let next_link_offset =
            LinkOffset::from_offset(Offset::from_disk(next_link_offset)?, buf, checksum)?;
        verify_checksum(checksum, offset as u64, (TYPE_BYTES + len1 + len2) as u64)?;
        Ok(MemLink {
            value,
            next_link_offset,
            unused: false,
        })
    }

    fn write_to<W: Write>(&self, writer: &mut W, offset_map: &OffsetMap) -> io::Result<()> {
        writer.write_all(&[TYPE_LINK])?;
        writer.write_vlq(self.value)?;
        writer.write_vlq(self.next_link_offset.to_disk(offset_map))?;
        Ok(())
    }

    /// Mark the entry as unused. An unused entry won't be written to disk.
    fn mark_unused(&mut self) {
        self.unused = true;
    }

    #[inline]
    fn is_unused(&self) -> bool {
        self.unused
    }
}

impl MemKey {
    fn read_from<B: AsRef<[u8]>>(buf: B, offset: u64, checksum: &Checksum) -> io::Result<Self> {
        let buf = buf.as_ref();
        let offset = offset as usize;
        check_type(buf, offset, TYPE_KEY)?;
        let (key_len, len): (usize, _) = buf.read_vlq_at(offset + 1)?;
        let key = Vec::from(buf.get(
            offset + TYPE_BYTES + len..offset + TYPE_BYTES + len + key_len,
        ).ok_or(InvalidData)?)
            .into_boxed_slice();
        verify_checksum(checksum, offset as u64, (TYPE_BYTES + len + key_len) as u64)?;
        Ok(MemKey { key })
    }

    fn write_to<W: Write>(&self, writer: &mut W, _: &OffsetMap) -> io::Result<()> {
        writer.write_all(&[TYPE_KEY])?;
        writer.write_vlq(self.key.len())?;
        writer.write_all(&self.key)?;
        Ok(())
    }

    /// Mark the entry as unused. An unused entry won't be written to disk.
    fn mark_unused(&mut self) {
        self.key = Vec::new().into_boxed_slice();
    }

    #[inline]
    fn is_unused(&self) -> bool {
        self.key.len() == 0
    }
}

impl MemExtKey {
    fn read_from<B: AsRef<[u8]>>(buf: B, offset: u64, checksum: &Checksum) -> io::Result<Self> {
        let buf = buf.as_ref();
        let offset = offset as usize;
        check_type(buf, offset, TYPE_EXT_KEY)?;
        let (start, vlq_len1) = buf.read_vlq_at(offset + TYPE_BYTES)?;
        let (len, vlq_len2) = buf.read_vlq_at(offset + TYPE_BYTES + vlq_len1)?;
        verify_checksum(
            checksum,
            offset as u64,
            (TYPE_BYTES + vlq_len1 + vlq_len2) as u64,
        )?;
        Ok(MemExtKey { start, len })
    }

    fn write_to<W: Write>(&self, writer: &mut W, _: &OffsetMap) -> io::Result<()> {
        writer.write_all(&[TYPE_EXT_KEY])?;
        writer.write_vlq(self.start)?;
        writer.write_vlq(self.len)
    }

    /// Mark the entry as unused. An unused entry won't be written to disk.
    fn mark_unused(&mut self) {
        self.len = 0;
    }

    #[inline]
    fn is_unused(&self) -> bool {
        self.len == 0
    }
}

impl MemRoot {
    fn read_from<B: AsRef<[u8]>>(buf: B, offset: u64, checksum: &Checksum) -> io::Result<Self> {
        let buf = buf.as_ref();
        let offset = offset as usize;
        let mut cur = offset;
        check_type(buf, offset, TYPE_ROOT)?;
        cur += TYPE_BYTES;

        let (radix_offset, vlq_len) = buf.read_vlq_at(cur)?;
        cur += vlq_len;

        let radix_offset =
            RadixOffset::from_offset(Offset::from_disk(radix_offset)?, buf, checksum)?;

        let (meta_len, vlq_len): (usize, _) = buf.read_vlq_at(cur)?;
        cur += vlq_len;

        let meta = match buf.get(cur..cur + meta_len) {
            Some(meta) => meta,
            None => return Err(InvalidData.into()),
        };
        cur += meta_len;

        verify_checksum(checksum, offset as u64, (cur - offset) as u64)?;
        Ok(MemRoot {
            radix_offset,
            meta: meta.to_vec().into_boxed_slice(),
        })
    }

    fn read_from_end<B: AsRef<[u8]>>(buf: B, end: u64, checksum: &Checksum) -> io::Result<Self> {
        if end > 1 {
            let (root_size, vlq_size) = read_vlq_reverse(buf.as_ref(), end as usize)?;
            let vlq_size = vlq_size as u64;
            verify_checksum(checksum, end - vlq_size, vlq_size)?;
            Self::read_from(buf, end - vlq_size - root_size, checksum)
        } else {
            Err(InvalidData.into())
        }
    }

    fn write_to<W: Write>(&self, writer: &mut W, offset_map: &OffsetMap) -> io::Result<()> {
        let mut buf = Vec::with_capacity(16);
        buf.write_all(&[TYPE_ROOT])?;
        buf.write_vlq(self.radix_offset.to_disk(offset_map))?;
        buf.write_vlq(self.meta.len())?;
        buf.write_all(&self.meta)?;
        let len = buf.len();
        let mut reversed_vlq = Vec::new();
        reversed_vlq.write_vlq(len)?;
        reversed_vlq.reverse();
        buf.write_all(&reversed_vlq)?;
        writer.write_all(&buf)
    }
}

#[derive(Default)]
struct OffsetMap {
    radix_len: usize,
    radix_map: Vec<u64>,
    leaf_map: Vec<u64>,
    link_map: Vec<u64>,
    key_map: Vec<u64>,
    ext_key_map: Vec<u64>,
}

impl OffsetMap {
    fn empty_for_index(index: &Index) -> OffsetMap {
        let radix_len = index.dirty_radixes.len();
        OffsetMap {
            radix_len,
            radix_map: vec![0; radix_len],
            leaf_map: vec![0; index.dirty_leafs.len()],
            link_map: vec![0; index.dirty_links.len()],
            key_map: vec![0; index.dirty_keys.len()],
            ext_key_map: vec![0; index.dirty_ext_keys.len()],
        }
    }

    #[inline]
    fn get(&self, offset: Offset) -> u64 {
        if offset.is_dirty() {
            let result = match offset.to_typed(&b""[..], &None).unwrap() {
                // Radix entries are pushed in the reversed order. So the index needs to be
                // reversed.
                TypedOffset::Radix(x) => self.radix_map[self.radix_len - 1 - x.dirty_index()],
                TypedOffset::Leaf(x) => self.leaf_map[x.dirty_index()],
                TypedOffset::Link(x) => self.link_map[x.dirty_index()],
                TypedOffset::Key(x) => self.key_map[x.dirty_index()],
                TypedOffset::ExtKey(x) => self.ext_key_map[x.dirty_index()],
            };
            // result == 0 means an entry marked "unused" is actually used. It's a logic error.
            debug_assert!(result > 0);
            result
        } else {
            // No need to translate.
            offset.0
        }
    }
}

//// Main Index

/// Insertion-only mapping from `bytes` to a list of [u64]s.
///
/// An [Index] is backed by an append-only file in the filesystem. Internally,
/// it uses base16 radix trees for keys and linked list for [u64] values. The
/// file format was designed to be able to support other types of indexes (ex.
/// non-radix-trees). Though none of them are implemented.
pub struct Index {
    // For locking and low-level access.
    file: File,

    // For efficient and shared random reading.
    buf: Mmap,

    // Logical length. Could be different from `buf.len()`.
    len: u64,

    // Whether "file" was opened as read-only.
    // Only affects "flush". Do not affect in-memory writes.
    read_only: bool,

    // In-memory entries. The root entry is always in-memory.
    root: MemRoot,
    dirty_radixes: Vec<MemRadix>,
    dirty_leafs: Vec<MemLeaf>,
    dirty_links: Vec<MemLink>,
    dirty_keys: Vec<MemKey>,
    dirty_ext_keys: Vec<MemExtKey>,

    // Optional checksum table.
    checksum: Checksum,
    checksum_chunk_size: u64,

    // Additional buffer for external keys.
    key_buf: Arc<AsRef<[u8]> + Send + Sync>,
}

/// Key to insert. Used by [Index::insert_advanced].
pub enum InsertKey<'a> {
    /// Embedded key.
    Embed(&'a [u8]),

    /// Reference (`[start, end)`) to `key_buf`.
    Reference((u64, u64)),
}

/// Options used to configured how an [Index] is opened.
///
/// Similar to [std::fs::OpenOptions], to use this, first call `new`, then
/// chain calls to methods to set each option, finally call `open` to get
/// an [Index] structure.
#[derive(Clone)]
pub struct OpenOptions {
    checksum_chunk_size: u64,
    len: Option<u64>,
    write: Option<bool>,
    key_buf: Option<Arc<AsRef<[u8]> + Send + Sync>>,
}

impl OpenOptions {
    /// Create [OpenOptions] with default configuration:
    /// - no checksum
    /// - no external key buffer
    /// - read root entry from the end of the file
    /// - open as read-write but fallback to read-only
    pub fn new() -> OpenOptions {
        OpenOptions {
            checksum_chunk_size: 0,
            len: None,
            write: None,
            key_buf: None,
        }
    }

    /// Set checksum behavior.
    ///
    /// If `checksum_chunk_size` is set to 0, do not use checksums. Otherwise,
    /// it's the size of a chunk to be checksumed, in bytes. Rounded to `2 ** n`
    /// for performance reasons.
    ///
    /// Disabling checksum can help with performance.
    pub fn checksum_chunk_size(&mut self, checksum_chunk_size: u64) -> &mut Self {
        self.checksum_chunk_size = checksum_chunk_size;
        self
    }

    /// Set whether writing is required:
    ///
    /// - `None`: open as read-write but fallback to read-only. `flush()` may fail.
    /// - `Some(false)`: open as read-only. `flush()` will always fail.
    /// - `Some(true)`: open as read-write. `open()` fails if read-write is not
    ///   possible. `flush()` will not fail due to permission issues.
    ///
    /// Note:  The index is always mutable in-memory. Only `flush()` may fail.
    pub fn write(&mut self, value: Option<bool>) -> &mut Self {
        self.write = value;
        self
    }

    /// Specify the logical length of the file.
    ///
    /// If `len` is `None`, use the actual file length. Otherwise, use the
    /// length specified by `len`. Reading the file length requires locking.
    ///
    /// This is useful for lock-free reads, or accessing to multiple versions of
    /// the index at the same time.
    ///
    /// To get a valid logical length, check the return value of [Index::flush].
    pub fn logical_len(&mut self, len: Option<u64>) -> &mut Self {
        self.len = len;
        self
    }

    /// Specify the external key buffer.
    ///
    /// With an external key buffer, keys could be stored as references using
    /// `index.insert_advanced` to save space.
    pub fn key_buf(&mut self, buf: Option<Arc<AsRef<[u8]> + Send + Sync>>) -> &mut Self {
        self.key_buf = buf;
        self
    }

    /// Open the index file with given options.
    ///
    /// Driven by the "immutable by default" idea, together with append-only
    /// properties, [OpenOptions::open] returns a "snapshotted" view of the
    /// index. Changes to the filesystem won't change instantiated [Index]es.
    pub fn open<P: AsRef<Path>>(&self, path: P) -> io::Result<Index> {
        let open_result = if self.write == Some(false) {
            fs::OpenOptions::new().read(true).open(path.as_ref())
        } else {
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .append(true)
                .open(path.as_ref())
        };
        let (read_only, mut file) = match self.write {
            Some(true) => (false, open_result?),
            Some(false) => (true, open_result?),
            None => {
                // Fall back to open the file as read-only, automatically.
                if open_result.is_err() {
                    (true, fs::OpenOptions::new().read(true).open(path.as_ref())?)
                } else {
                    (false, open_result.unwrap())
                }
            }
        };

        let (mmap, len) = {
            match self.len {
                None => {
                    // Take the lock to read file length, since that decides root entry location.
                    let mut lock = ScopedFileLock::new(&mut file, false)?;
                    mmap_readonly(lock.as_ref(), None)?
                }
                Some(len) => {
                    // No need to lock for getting file length.
                    mmap_readonly(&file, Some(len))?
                }
            }
        };

        let checksum_chunk_size = self.checksum_chunk_size;
        let mut checksum = if checksum_chunk_size > 0 {
            Some(ChecksumTable::new(&path)?)
        } else {
            None
        };

        let (dirty_radixes, root) = if len == 0 {
            // Empty file. Create root radix entry as an dirty entry, and
            // rebuild checksum table (in case it's corrupted).
            let radix_offset = RadixOffset::from_dirty_index(0);
            if let Some(ref mut table) = checksum {
                table.clear();
            }
            let meta = Default::default();
            (vec![MemRadix::default()], MemRoot { radix_offset, meta })
        } else {
            // Verify the header byte.
            check_type(&mmap, 0, TYPE_HEAD)?;
            // Load root entry from the end of the file (truncated at the logical length).
            (vec![], MemRoot::read_from_end(&mmap, len, &checksum)?)
        };

        let key_buf = self.key_buf.clone();

        Ok(Index {
            file,
            buf: mmap,
            read_only,
            root,
            dirty_radixes,
            dirty_links: vec![],
            dirty_leafs: vec![],
            dirty_keys: vec![],
            dirty_ext_keys: vec![],
            checksum,
            checksum_chunk_size,
            key_buf: key_buf.unwrap_or(Arc::new(b"")),
            len,
        })
    }
}

impl Index {
    /// Return a cloned [Index].
    pub fn clone(&self) -> io::Result<Index> {
        let file = self.file.duplicate()?;
        let mmap = mmap_readonly(&file, Some(self.len))?.0;
        let checksum = match self.checksum {
            Some(ref table) => Some(table.clone()?),
            None => None,
        };
        Ok(Index {
            file,
            buf: mmap,
            read_only: self.read_only,
            root: self.root.clone(),
            dirty_keys: self.dirty_keys.clone(),
            dirty_ext_keys: self.dirty_ext_keys.clone(),
            dirty_leafs: self.dirty_leafs.clone(),
            dirty_links: self.dirty_links.clone(),
            dirty_radixes: self.dirty_radixes.clone(),
            checksum,
            checksum_chunk_size: self.checksum_chunk_size,
            key_buf: self.key_buf.clone(),
            len: self.len,
        })
    }

    /// Get metadata attached to the root node. This is what previously set by
    /// [Index::set_meta].
    pub fn get_meta(&self) -> &[u8] {
        &self.root.meta
    }

    /// Set metadata attached to the root node. Will be written at
    /// [Index::flush] time.
    pub fn set_meta<B: AsRef<[u8]>>(&mut self, meta: B) {
        self.root.meta = meta.as_ref().to_vec().into_boxed_slice()
    }

    /// Flush changes to disk.
    ///
    /// Take the file lock when writing.
    ///
    /// Return 0 if nothing needs to be written. Otherwise return the new file
    /// length on success. Return [io::ErrorKind::PermissionDenied] if the file
    /// was marked read-only at open time.
    ///
    /// The new file length can be used to obtain the exact same view of the
    /// index as it currently is. That means, other changes to the indexes won't
    /// be "combined" during flush. For example, given the following events
    /// happened in order:
    /// - Open. Get Index X.
    /// - Open using the same arguments. Get Index Y.
    /// - Write key "p" to X.
    /// - Write key "q" to Y.
    /// - Flush X. Get new length LX.
    /// - Flush Y. Get new length LY.
    /// - Open using LY as `logical_len`. Get Index Z.
    ///
    /// Then key "p" does not exist in Z. This allows some advanced usecases.
    /// On the other hand, if "merging changes" is the desired behavior, the
    /// caller needs to take another lock, re-instantiate [Index] and re-insert
    /// keys.
    pub fn flush(&mut self) -> io::Result<u64> {
        if self.read_only {
            return Err(io::ErrorKind::PermissionDenied.into());
        }

        let mut new_len = self.len;
        if !self.root.radix_offset.is_dirty() {
            // Nothing changed
            return Ok(new_len);
        }

        // Critical section: need write lock
        {
            let mut offset_map = OffsetMap::empty_for_index(self);
            let estimated_dirty_bytes = self.dirty_links.len() * 50;
            let mut lock = ScopedFileLock::new(&mut self.file, true)?;
            let len = lock.as_mut().seek(SeekFrom::End(0))?;
            let mut buf = Vec::with_capacity(estimated_dirty_bytes);

            // Write in the following order:
            // header, keys, links, leafs, radixes, root.
            // Latter entries depend on former entries.

            if len == 0 {
                buf.write_all(&[TYPE_HEAD])?;
            }

            for (i, entry) in self.dirty_keys.iter().enumerate() {
                if !entry.is_unused() {
                    let offset = buf.len() as u64 + len;
                    offset_map.key_map[i] = offset;
                    entry.write_to(&mut buf, &offset_map)?;
                };
            }

            // Inlined leafs. They might affect ExtKeys and Links. Need to write first.
            for i in 0..self.dirty_leafs.len() {
                let mut entry = self.dirty_leafs.get_mut(i).unwrap();
                let offset = buf.len() as u64 + len;
                if !entry.is_unused()
                    && entry.maybe_write_inline_to(
                        &mut buf,
                        &self.buf,
                        len,
                        &mut self.dirty_ext_keys,
                        &mut self.dirty_links,
                        &mut offset_map,
                    )? {
                    offset_map.leaf_map[i] = offset;
                    entry.mark_unused();
                }
            }

            for (i, entry) in self.dirty_ext_keys.iter().enumerate() {
                if !entry.is_unused() {
                    let offset = buf.len() as u64 + len;
                    offset_map.ext_key_map[i] = offset;
                    entry.write_to(&mut buf, &offset_map)?;
                }
            }

            for (i, entry) in self.dirty_links.iter().enumerate() {
                if !entry.is_unused() {
                    let offset = buf.len() as u64 + len;
                    offset_map.link_map[i] = offset;
                    entry.write_to(&mut buf, &offset_map)?;
                }
            }

            // Non-inlined leafs.
            for (i, entry) in self.dirty_leafs.iter().enumerate() {
                if !entry.is_unused() {
                    let offset = buf.len() as u64 + len;
                    offset_map.leaf_map[i] = offset;
                    entry.write_noninline_to(&mut buf, &offset_map)?;
                }
            }

            // Write Radix entries in reversed order since former ones might refer to latter ones.
            for (i, entry) in self.dirty_radixes.iter().rev().enumerate() {
                let offset = buf.len() as u64 + len;
                entry.write_to(&mut buf, &offset_map)?;
                offset_map.radix_map[i] = offset;
            }

            self.root.write_to(&mut buf, &offset_map)?;
            new_len = buf.len() as u64 + len;
            lock.as_mut().write_all(&buf)?;

            // Remap and update root since length has changed
            let (mmap, mmap_len) = mmap_readonly(lock.as_ref(), None)?;
            self.buf = mmap;

            // Sanity check - the length should be expected. Otherwise, the lock
            // is somehow ineffective.
            if mmap_len != new_len {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }

            if let Some(ref mut table) = self.checksum {
                debug_assert!(self.checksum_chunk_size > 0);
                let chunk_size_log = 63 - (self.checksum_chunk_size as u64).leading_zeros();
                table.update(chunk_size_log.into())?;
            }
            self.root = MemRoot::read_from_end(&self.buf, new_len, &self.checksum)?;
        }

        // Outside critical section
        self.dirty_radixes.clear();
        self.dirty_leafs.clear();
        self.dirty_links.clear();
        self.dirty_keys.clear();
        self.dirty_ext_keys.clear();
        self.len = new_len;

        Ok(new_len)
    }

    /// Lookup by `key`. Return [LinkOffset].
    ///
    /// To test if the key exists or not, use [Offset::is_null].
    /// To obtain all values, use [LinkOffset::values].
    pub fn get<K: AsRef<[u8]>>(&self, key: &K) -> io::Result<LinkOffset> {
        let mut offset: Offset = self.root.radix_offset.into();
        let mut iter = Base16Iter::from_base256(key);

        while !offset.is_null() {
            // Read the entry at "offset"
            match offset.to_typed(&self.buf, &self.checksum)? {
                TypedOffset::Radix(radix) => {
                    match iter.next() {
                        None => {
                            // The key ends at this Radix entry.
                            return radix.link_offset(self);
                        }
                        Some(x) => {
                            // Follow the `x`-th child in the Radix entry.
                            offset = radix.child(self, x)?;
                        }
                    }
                }
                TypedOffset::Leaf(leaf) => {
                    // Meet a leaf. If key matches, return the link offset.
                    let (stored_key, link_offset) = leaf.key_and_link_offset(self)?;
                    if stored_key == key.as_ref() {
                        return Ok(link_offset);
                    } else {
                        return Ok(LinkOffset::default());
                    }
                }
                _ => return Err(InvalidData.into()),
            }
        }

        // Not found
        Ok(LinkOffset::default())
    }

    /// Insert a key-value pair. The value will be the head of the linked list.
    /// That is, `get(key).values().first()` will return the newly inserted
    /// value.
    pub fn insert<K: AsRef<[u8]>>(&mut self, key: &K, value: u64) -> io::Result<()> {
        self.insert_advanced(InsertKey::Embed(key.as_ref()), value.into(), None)
    }

    /// Update the linked list for a given key.
    ///
    /// If `link` is None, behave like `insert`. Otherwise, ignore the existing
    /// values `key` mapped to, create a new link entry that chains to the given
    /// [LinkOffset].
    ///
    /// `key` could be a reference, or an embedded value. See [InsertKey] for
    /// details.
    ///
    /// This is a low-level API.
    pub fn insert_advanced(
        &mut self,
        key: InsertKey,
        value: u64,
        link: Option<LinkOffset>,
    ) -> io::Result<()> {
        let mut offset: Offset = self.root.radix_offset.into();
        let mut step = 0;
        let (key, key_buf_offset) = match key {
            InsertKey::Embed(k) => (k, None),
            InsertKey::Reference((start, len)) => {
                let key = &self.key_buf.as_ref().as_ref()[start as usize..(start + len) as usize];
                // UNSAFE NOTICE: `key` is valid as long as `self.key_buf` is valid. `self.key_buf`
                // won't be changed. So `self` can still be mutable without a read-only
                // relationship with `key`.
                let detached_key = unsafe { &*(key as (*const [u8])) };
                (detached_key, Some((start, len)))
            }
        };
        let mut iter = Base16Iter::from_base256(&key);

        let mut last_radix = RadixOffset::default();
        let mut last_child = 0u8;

        loop {
            match offset.to_typed(&self.buf, &self.checksum)? {
                TypedOffset::Radix(radix) => {
                    // Copy radix entry since we must modify it.
                    let radix = radix.copy(self)?;
                    offset = radix.into();

                    if step == 0 {
                        self.root.radix_offset = radix;
                    } else {
                        last_radix.set_child(self, last_child, offset);
                    }

                    last_radix = radix;

                    match iter.next() {
                        None => {
                            let old_link_offset = radix.link_offset(self)?;
                            let new_link_offset =
                                link.unwrap_or(old_link_offset).create(self, value);
                            radix.set_link(self, new_link_offset);
                            return Ok(());
                        }
                        Some(x) => {
                            let next_offset = radix.child(self, x)?;
                            if next_offset.is_null() {
                                // "key" is longer than existing ones. Create key and leaf entries.
                                let link_offset =
                                    link.unwrap_or(LinkOffset::default()).create(self, value);
                                let key_offset = self.create_key(key, key_buf_offset);
                                let leaf_offset =
                                    LeafOffset::create(self, link_offset, key_offset.into());
                                radix.set_child(self, x, leaf_offset.into());
                                return Ok(());
                            } else {
                                offset = next_offset;
                                last_child = x;
                            }
                        }
                    }
                }
                TypedOffset::Leaf(leaf) => {
                    let (old_key, link_offset) = {
                        let (old_key, link_offset) = leaf.key_and_link_offset(self)?;
                        // Detach "old_key" from "self".
                        // About safety: This is to avoid a memory copy / allocation.
                        // `old_key` are only valid before `dirty_*keys` being resized.
                        // `old_iter` (used by `split_leaf`) and `old_key` are not used
                        // after creating a key. So it's safe to not copy it.
                        let detached_key = unsafe { &*(old_key as (*const [u8])) };
                        (detached_key, link_offset)
                    };
                    if old_key == key.as_ref() {
                        // Key matched. Need to copy leaf entry.
                        let new_link_offset = link.unwrap_or(link_offset).create(self, value);
                        let new_leaf_offset = leaf.set_link(self, new_link_offset)?;
                        last_radix.set_child(self, last_child, new_leaf_offset.into());
                    } else {
                        // Key mismatch. Do a leaf split.
                        let new_link_offset =
                            link.unwrap_or(LinkOffset::default()).create(self, value);
                        self.split_leaf(
                            leaf,
                            old_key,
                            key.as_ref(),
                            key_buf_offset,
                            step,
                            last_radix,
                            last_child,
                            link_offset,
                            new_link_offset,
                        )?;
                    }
                    return Ok(());
                }
                _ => return Err(InvalidData.into()),
            }

            step += 1;
        }
    }

    /// Split a leaf entry. Separated from `insert_advanced` to make `insert_advanced`
    /// shorter.  The parameters are internal states inside `insert_advanced`. Calling this
    /// from other functions makes less sense.
    #[inline]
    fn split_leaf(
        &mut self,
        old_leaf_offset: LeafOffset,
        old_key: &[u8],
        new_key: &[u8],
        key_buf_offset: Option<(u64, u64)>,
        step: usize,
        radix_offset: RadixOffset,
        child: u8,
        old_link_offset: LinkOffset,
        new_link_offset: LinkOffset,
    ) -> io::Result<()> {
        // This is probably the most complex part. Here are some explanation about input parameters
        // and what this function is supposed to do for some cases:
        //
        // Input parameters are marked using `*`:
        //
        //      Offset            | Content
        //      root_radix        | Radix(child1: radix1, ...)         \
        //      radix1            | Radix(child2: radix2, ...)         |> steps
        //      ...               | ...                                | (for skipping check
        //      *radix_offset*    | Radix(*child*: *leaf_offset*, ...) /  of prefix in keys)
        //      *old_leaf_offset* | Leaf(link_offset: *old_link_offset*, ...)
        //      *new_link_offset* | Link(...)
        //
        //      old_* are redundant, but they are pre-calculated by the caller. So just reuse them.
        //
        // Here are 3 kinds of examples (Keys are embed in Leaf for simplicity):
        //
        // Example 1. old_key = "1234"; new_key = "1278".
        //
        //      Offset | Before                | After
        //           A | Radix(1: B)           | Radix(1: C)
        //           B | Leaf("1234", Link: X) | Leaf("1234", Link: X)
        //           C |                       | Radix(2: E)
        //           D |                       | Leaf("1278")
        //           E |                       | Radix(3: B, 7: D)
        //
        // Example 2. old_key = "1234", new_key = "12". No need for a new leaf entry:
        //
        //      Offset | Before                | After
        //           A | Radix(1: B)           | Radix(1: C)
        //           B | Leaf("1234", Link: X) | Leaf("1234", Link: X)
        //           C |                       | Radix(2: B, Link: Y)
        //
        // Example 3. old_key = "12", new_key = "1234". Need new leaf. Old leaf is not needed.
        //
        //      Offset | Before              | After
        //           A | Radix(1: B)         | Radix(1: C)
        //           B | Leaf("12", Link: X) | Leaf("12", Link: X) # not used
        //           C |                     | Radix(2: E, Link: X)
        //           D |                     | Leaf("1234", Link: Y)
        //           E |                     | Radix(3: D)

        // UNSAFE NOTICE: Read the "UNSAFE NOTICE" inside `insert_advanced` to learn more.
        // Basically, `old_iter` is only guaranteed available if there is no insertion to
        // `self.dirty_keys` or `self.dirty_ext_keys`. That's true here since we won't read
        // `old_iter` after creating new keys. But be aware of the constraint when modifying the
        // code.
        let mut old_iter = Base16Iter::from_base256(&old_key).skip(step);
        let mut new_iter = Base16Iter::from_base256(&new_key).skip(step);

        let mut last_radix_offset = radix_offset;
        let mut last_radix_child = child;

        let mut completed = false;

        loop {
            let b1 = old_iter.next();
            let b2 = new_iter.next();

            let mut radix = MemRadix::default();

            if let Some(b1) = b1 {
                // Initial value for the b1-th child. Could be rewritten by
                // "set_radix_entry_child" in the next loop iteration.
                radix.offsets[b1 as usize] = old_leaf_offset.into();
            } else {
                // Example 3. old_key is a prefix of new_key. A leaf is still needed.
                // The new leaf will be created by the next "if" block.
                old_leaf_offset.mark_unused(self);
                radix.link_offset = old_link_offset;
            }

            if b2.is_none() {
                // Example 2. new_key is a prefix of old_key. A new leaf is not needed.
                radix.link_offset = new_link_offset;
                completed = true;
            } else if b1 != b2 {
                // Example 1 and Example 3. A new leaf is needed.
                let new_key_offset = self.create_key(new_key, key_buf_offset);
                let new_leaf_offset = LeafOffset::create(self, new_link_offset, new_key_offset);
                radix.offsets[b2.unwrap() as usize] = new_leaf_offset.into();
                completed = true;
            }

            // Create the Radix entry, and connect it to the parent entry.
            let offset = RadixOffset::create(self, radix);
            last_radix_offset.set_child(self, last_radix_child, offset.into());

            if completed {
                break;
            }

            debug_assert!(b1 == b2);
            last_radix_offset = offset;
            last_radix_child = b2.unwrap();
        }

        Ok(())
    }

    /// Create a key (if key_buf_offset is None) or ext key (if key_buf_offset is set) entry.
    #[inline]
    fn create_key(&mut self, key: &[u8], key_buf_offset: Option<(u64, u64)>) -> Offset {
        match key_buf_offset {
            None => KeyOffset::create(self, key).into(),
            Some((start, len)) => ExtKeyOffset::create(self, start, len).into(),
        }
    }

    /// Verify checksum for the given range. Internal API used by `*Offset` structs.
    #[inline]
    fn verify_checksum(&self, start: u64, length: u64) -> io::Result<()> {
        verify_checksum(&self.checksum, start, length)
    }
}

//// Debug Formatter

impl Debug for Offset {
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        if self.is_null() {
            write!(f, "None")
        } else if self.is_dirty() {
            match self.to_typed(&b""[..], &None).unwrap() {
                TypedOffset::Radix(x) => x.fmt(f),
                TypedOffset::Leaf(x) => x.fmt(f),
                TypedOffset::Link(x) => x.fmt(f),
                TypedOffset::Key(x) => x.fmt(f),
                TypedOffset::ExtKey(x) => x.fmt(f),
            }
        } else {
            write!(f, "Disk[{}]", self.0)
        }
    }
}

impl Debug for MemRadix {
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        write!(f, "Radix {{ link: {:?}", self.link_offset)?;
        for (i, v) in self.offsets.iter().cloned().enumerate() {
            if !v.is_null() {
                write!(f, ", {}: {:?}", i, v)?;
            }
        }
        write!(f, " }}")
    }
}

impl Debug for MemLeaf {
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        if self.is_unused() {
            write!(f, "Leaf (unused)")
        } else {
            write!(
                f,
                "Leaf {{ key: {:?}, link: {:?} }}",
                self.key_offset, self.link_offset
            )
        }
    }
}

impl Debug for MemLink {
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "Link {{ value: {}, next: {:?} }}",
            self.value, self.next_link_offset
        )
    }
}

impl Debug for MemKey {
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        if self.is_unused() {
            write!(f, "Key (unused)")
        } else {
            write!(f, "Key {{ key:")?;
            for byte in self.key.iter() {
                write!(f, " {:X}", byte)?;
            }
            write!(f, " }}")
        }
    }
}

impl Debug for MemExtKey {
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        if self.is_unused() {
            write!(f, "ExtKey (unused)")
        } else {
            write!(f, "ExtKey {{ start: {}, len: {} }}", self.start, self.len)
        }
    }
}

impl Debug for MemRoot {
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        if self.meta.is_empty() {
            write!(f, "Root {{ radix: {:?} }}", self.radix_offset)
        } else {
            write!(
                f,
                "Root {{ radix: {:?}, meta: {:?} }}",
                self.radix_offset, self.meta
            )
        }
    }
}

impl Debug for Index {
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "Index {{ len: {}, root: {:?} }}\n",
            self.buf.len(),
            self.root.radix_offset
        )?;

        // On-disk entries
        let offset_map = OffsetMap::default();
        let mut buf = Vec::with_capacity(self.buf.len());
        buf.push(TYPE_HEAD);
        loop {
            let i = buf.len();
            if i >= self.buf.len() {
                break;
            }
            write!(f, "Disk[{}]: ", i)?;
            let type_int = self.buf[i];
            let i = i as u64;
            match type_int {
                TYPE_RADIX => {
                    let e = MemRadix::read_from(&self.buf, i, &None).expect("read");
                    e.write_to(&mut buf, &offset_map).expect("write");
                    write!(f, "{:?}\n", e)?;
                }
                TYPE_LEAF => {
                    let e = MemLeaf::read_from(&self.buf, i, &None).expect("read");
                    e.write_noninline_to(&mut buf, &offset_map).expect("write");
                    write!(f, "{:?}\n", e)?;
                }
                TYPE_INLINE_LEAF => {
                    let e = MemLeaf::read_from(&self.buf, i, &None).expect("read");
                    write!(f, "Inline{:?}\n", e)?;
                    // Just skip the type int byte so we can parse inlined structures.
                    buf.push(TYPE_INLINE_LEAF);
                }
                TYPE_LINK => {
                    let e = MemLink::read_from(&self.buf, i, &None).unwrap();
                    e.write_to(&mut buf, &offset_map).expect("write");
                    write!(f, "{:?}\n", e)?;
                }
                TYPE_KEY => {
                    let e = MemKey::read_from(&self.buf, i, &None).expect("read");
                    e.write_to(&mut buf, &offset_map).expect("write");
                    write!(f, "{:?}\n", e)?;
                }
                TYPE_EXT_KEY => {
                    let e = MemExtKey::read_from(&self.buf, i, &None).expect("read");
                    e.write_to(&mut buf, &offset_map).expect("write");
                    write!(f, "{:?}\n", e)?;
                }
                TYPE_ROOT => {
                    let e = MemRoot::read_from(&self.buf, i, &None).expect("read");
                    e.write_to(&mut buf, &offset_map).expect("write");
                    write!(f, "{:?}\n", e)?;
                }
                _ => {
                    write!(f, "Broken Data!\n")?;
                    break;
                }
            }
        }

        if buf.len() > 1 && self.buf[..] != buf[..] {
            return write!(f, "Inconsistent Data!\n");
        }

        // In-memory entries
        for (i, e) in self.dirty_radixes.iter().enumerate() {
            write!(f, "Radix[{}]: ", i)?;
            write!(f, "{:?}\n", e)?;
        }

        for (i, e) in self.dirty_leafs.iter().enumerate() {
            write!(f, "Leaf[{}]: ", i)?;
            write!(f, "{:?}\n", e)?;
        }

        for (i, e) in self.dirty_links.iter().enumerate() {
            write!(f, "Link[{}]: ", i)?;
            write!(f, "{:?}\n", e)?;
        }

        for (i, e) in self.dirty_keys.iter().enumerate() {
            write!(f, "Key[{}]: ", i)?;
            write!(f, "{:?}\n", e)?;
        }

        for (i, e) in self.dirty_ext_keys.iter().enumerate() {
            write!(f, "ExtKey[{}]: {:?}\n", i, e)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::prelude::*;
    use tempdir::TempDir;

    fn open_opts() -> OpenOptions {
        let mut opts = OpenOptions::new();
        // Use 1 as checksum chunk size to make sure checksum check covers necessary bytes.
        opts.checksum_chunk_size(1);
        opts
    }

    #[test]
    fn test_distinct_one_byte_keys() {
        let dir = TempDir::new("index").expect("tempdir");
        let mut index = open_opts().open(dir.path().join("a")).expect("open");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 1, root: Radix[0] }\n\
             Radix[0]: Radix { link: None }\n"
        );

        index.insert(&[], 55).expect("update");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 1, root: Radix[0] }\n\
             Radix[0]: Radix { link: Link[0] }\n\
             Link[0]: Link { value: 55, next: None }\n"
        );

        index.insert(&[0x12], 77).expect("update");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 1, root: Radix[0] }\n\
             Radix[0]: Radix { link: Link[0], 1: Leaf[0] }\n\
             Leaf[0]: Leaf { key: Key[0], link: Link[1] }\n\
             Link[0]: Link { value: 55, next: None }\n\
             Link[1]: Link { value: 77, next: None }\n\
             Key[0]: Key { key: 12 }\n"
        );

        let link = index.get(&[0x12]).expect("get");
        index
            .insert_advanced(InsertKey::Embed(&[0x34]), 99, link.into())
            .expect("update");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 1, root: Radix[0] }\n\
             Radix[0]: Radix { link: Link[0], 1: Leaf[0], 3: Leaf[1] }\n\
             Leaf[0]: Leaf { key: Key[0], link: Link[1] }\n\
             Leaf[1]: Leaf { key: Key[1], link: Link[2] }\n\
             Link[0]: Link { value: 55, next: None }\n\
             Link[1]: Link { value: 77, next: None }\n\
             Link[2]: Link { value: 99, next: Link[1] }\n\
             Key[0]: Key { key: 12 }\n\
             Key[1]: Key { key: 34 }\n"
        );
    }

    #[test]
    fn test_distinct_one_byte_keys_flush() {
        let dir = TempDir::new("index").expect("tempdir");
        let mut index = open_opts().open(dir.path().join("a")).expect("open");

        // 1st flush.
        assert_eq!(index.flush().expect("flush"), 9);
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 9, root: Disk[1] }\n\
             Disk[1]: Radix { link: None }\n\
             Disk[5]: Root { radix: Disk[1] }\n"
        );

        // Mixed on-disk and in-memory state.
        index.insert(&[], 55).expect("update");
        index.insert(&[0x12], 77).expect("update");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 9, root: Radix[0] }\n\
             Disk[1]: Radix { link: None }\n\
             Disk[5]: Root { radix: Disk[1] }\n\
             Radix[0]: Radix { link: Link[0], 1: Leaf[0] }\n\
             Leaf[0]: Leaf { key: Key[0], link: Link[1] }\n\
             Link[0]: Link { value: 55, next: None }\n\
             Link[1]: Link { value: 77, next: None }\n\
             Key[0]: Key { key: 12 }\n"
        );

        // After 2nd flush. There are 2 roots.
        let link = index.get(&[0x12]).expect("get");
        index
            .insert_advanced(InsertKey::Embed(&[0x34]), 99, link.into())
            .expect("update");
        index.flush().expect("flush");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 50, root: Disk[30] }\n\
             Disk[1]: Radix { link: None }\n\
             Disk[5]: Root { radix: Disk[1] }\n\
             Disk[9]: Key { key: 12 }\n\
             Disk[12]: Key { key: 34 }\n\
             Disk[15]: Link { value: 55, next: None }\n\
             Disk[18]: Link { value: 77, next: None }\n\
             Disk[21]: Link { value: 99, next: Disk[18] }\n\
             Disk[24]: Leaf { key: Disk[9], link: Disk[18] }\n\
             Disk[27]: Leaf { key: Disk[12], link: Disk[21] }\n\
             Disk[30]: Radix { link: Disk[15], 1: Disk[24], 3: Disk[27] }\n\
             Disk[46]: Root { radix: Disk[30] }\n"
        );
    }

    #[test]
    fn test_leaf_split() {
        let dir = TempDir::new("index").expect("tempdir");
        let mut index = open_opts().open(dir.path().join("a")).expect("open");

        // Example 1: two keys are not prefixes of each other
        index.insert(&[0x12, 0x34], 5).expect("insert");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 1, root: Radix[0] }\n\
             Radix[0]: Radix { link: None, 1: Leaf[0] }\n\
             Leaf[0]: Leaf { key: Key[0], link: Link[0] }\n\
             Link[0]: Link { value: 5, next: None }\n\
             Key[0]: Key { key: 12 34 }\n"
        );
        index.insert(&[0x12, 0x78], 7).expect("insert");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 1, root: Radix[0] }\n\
             Radix[0]: Radix { link: None, 1: Radix[1] }\n\
             Radix[1]: Radix { link: None, 2: Radix[2] }\n\
             Radix[2]: Radix { link: None, 3: Leaf[0], 7: Leaf[1] }\n\
             Leaf[0]: Leaf { key: Key[0], link: Link[0] }\n\
             Leaf[1]: Leaf { key: Key[1], link: Link[1] }\n\
             Link[0]: Link { value: 5, next: None }\n\
             Link[1]: Link { value: 7, next: None }\n\
             Key[0]: Key { key: 12 34 }\n\
             Key[1]: Key { key: 12 78 }\n"
        );

        // Example 2: new key is a prefix of the old key
        let mut index = open_opts().open(dir.path().join("a")).expect("open");
        index.insert(&[0x12, 0x34], 5).expect("insert");
        index.insert(&[0x12], 7).expect("insert");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 1, root: Radix[0] }\n\
             Radix[0]: Radix { link: None, 1: Radix[1] }\n\
             Radix[1]: Radix { link: None, 2: Radix[2] }\n\
             Radix[2]: Radix { link: Link[1], 3: Leaf[0] }\n\
             Leaf[0]: Leaf { key: Key[0], link: Link[0] }\n\
             Link[0]: Link { value: 5, next: None }\n\
             Link[1]: Link { value: 7, next: None }\n\
             Key[0]: Key { key: 12 34 }\n"
        );

        // Example 3: old key is a prefix of the new key
        let mut index = open_opts().open(dir.path().join("a")).expect("open");
        index.insert(&[0x12], 5).expect("insert");
        index.insert(&[0x12, 0x78], 7).expect("insert");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 1, root: Radix[0] }\n\
             Radix[0]: Radix { link: None, 1: Radix[1] }\n\
             Radix[1]: Radix { link: None, 2: Radix[2] }\n\
             Radix[2]: Radix { link: Link[0], 7: Leaf[1] }\n\
             Leaf[0]: Leaf (unused)\n\
             Leaf[1]: Leaf { key: Key[1], link: Link[1] }\n\
             Link[0]: Link { value: 5, next: None }\n\
             Link[1]: Link { value: 7, next: None }\n\
             Key[0]: Key (unused)\n\
             Key[1]: Key { key: 12 78 }\n"
        );

        // Same key. Multiple values.
        let mut index = open_opts().open(dir.path().join("a")).expect("open");
        index.insert(&[0x12], 5).expect("insert");
        index.insert(&[0x12], 7).expect("insert");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 1, root: Radix[0] }\n\
             Radix[0]: Radix { link: None, 1: Leaf[0] }\n\
             Leaf[0]: Leaf { key: Key[0], link: Link[1] }\n\
             Link[0]: Link { value: 5, next: None }\n\
             Link[1]: Link { value: 7, next: Link[0] }\n\
             Key[0]: Key { key: 12 }\n"
        );
    }

    #[test]
    fn test_leaf_split_flush() {
        // Similar with test_leaf_split, but flush the first key before inserting the second.
        // This triggers some new code paths.
        let dir = TempDir::new("index").expect("tempdir");
        let mut index = open_opts().open(dir.path().join("1")).expect("open");

        // Example 1: two keys are not prefixes of each other
        index.insert(&[0x12, 0x34], 5).expect("insert");
        index.flush().expect("flush");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 23, root: Disk[11] }\n\
             Disk[1]: Key { key: 12 34 }\n\
             Disk[5]: Link { value: 5, next: None }\n\
             Disk[8]: Leaf { key: Disk[1], link: Disk[5] }\n\
             Disk[11]: Radix { link: None, 1: Disk[8] }\n\
             Disk[19]: Root { radix: Disk[11] }\n"
        );
        index.insert(&[0x12, 0x78], 7).expect("insert");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 23, root: Radix[0] }\n\
             Disk[1]: Key { key: 12 34 }\n\
             Disk[5]: Link { value: 5, next: None }\n\
             Disk[8]: Leaf { key: Disk[1], link: Disk[5] }\n\
             Disk[11]: Radix { link: None, 1: Disk[8] }\n\
             Disk[19]: Root { radix: Disk[11] }\n\
             Radix[0]: Radix { link: None, 1: Radix[1] }\n\
             Radix[1]: Radix { link: None, 2: Radix[2] }\n\
             Radix[2]: Radix { link: None, 3: Disk[8], 7: Leaf[0] }\n\
             Leaf[0]: Leaf { key: Key[0], link: Link[0] }\n\
             Link[0]: Link { value: 7, next: None }\n\
             Key[0]: Key { key: 12 78 }\n"
        );

        // Example 2: new key is a prefix of the old key
        let mut index = open_opts().open(dir.path().join("2")).expect("open");
        index.insert(&[0x12, 0x34], 5).expect("insert");
        index.flush().expect("flush");
        index.insert(&[0x12], 7).expect("insert");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 23, root: Radix[0] }\n\
             Disk[1]: Key { key: 12 34 }\n\
             Disk[5]: Link { value: 5, next: None }\n\
             Disk[8]: Leaf { key: Disk[1], link: Disk[5] }\n\
             Disk[11]: Radix { link: None, 1: Disk[8] }\n\
             Disk[19]: Root { radix: Disk[11] }\n\
             Radix[0]: Radix { link: None, 1: Radix[1] }\n\
             Radix[1]: Radix { link: None, 2: Radix[2] }\n\
             Radix[2]: Radix { link: Link[0], 3: Disk[8] }\n\
             Link[0]: Link { value: 7, next: None }\n"
        );

        // Example 3: old key is a prefix of the new key
        // Only one flush - only one key is written.
        let mut index = open_opts().open(dir.path().join("3a")).expect("open");
        index.insert(&[0x12], 5).expect("insert");
        index.insert(&[0x12, 0x78], 7).expect("insert");
        index.flush().expect("flush");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 46, root: Disk[34] }\n\
             Disk[1]: Key { key: 12 78 }\n\
             Disk[5]: Link { value: 5, next: None }\n\
             Disk[8]: Link { value: 7, next: None }\n\
             Disk[11]: Leaf { key: Disk[1], link: Disk[8] }\n\
             Disk[14]: Radix { link: Disk[5], 7: Disk[11] }\n\
             Disk[26]: Radix { link: None, 2: Disk[14] }\n\
             Disk[34]: Radix { link: None, 1: Disk[26] }\n\
             Disk[42]: Root { radix: Disk[34] }\n"
        );

        // With two flushes - the old key cannot be removed since it was written.
        let mut index = open_opts().open(dir.path().join("3b")).expect("open");
        index.insert(&[0x12], 5).expect("insert");
        index.flush().expect("flush");
        index.insert(&[0x12, 0x78], 7).expect("insert");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 22, root: Radix[0] }\n\
             Disk[1]: Key { key: 12 }\n\
             Disk[4]: Link { value: 5, next: None }\n\
             Disk[7]: Leaf { key: Disk[1], link: Disk[4] }\n\
             Disk[10]: Radix { link: None, 1: Disk[7] }\n\
             Disk[18]: Root { radix: Disk[10] }\n\
             Radix[0]: Radix { link: None, 1: Radix[1] }\n\
             Radix[1]: Radix { link: None, 2: Radix[2] }\n\
             Radix[2]: Radix { link: Disk[4], 7: Leaf[0] }\n\
             Leaf[0]: Leaf { key: Key[0], link: Link[0] }\n\
             Link[0]: Link { value: 7, next: None }\n\
             Key[0]: Key { key: 12 78 }\n"
        );

        // Same key. Multiple values.
        let mut index = open_opts().open(dir.path().join("4")).expect("open");
        index.insert(&[0x12], 5).expect("insert");
        index.flush().expect("flush");
        index.insert(&[0x12], 7).expect("insert");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 22, root: Radix[0] }\n\
             Disk[1]: Key { key: 12 }\n\
             Disk[4]: Link { value: 5, next: None }\n\
             Disk[7]: Leaf { key: Disk[1], link: Disk[4] }\n\
             Disk[10]: Radix { link: None, 1: Disk[7] }\n\
             Disk[18]: Root { radix: Disk[10] }\n\
             Radix[0]: Radix { link: None, 1: Leaf[0] }\n\
             Leaf[0]: Leaf { key: Disk[1], link: Link[0] }\n\
             Link[0]: Link { value: 7, next: Disk[4] }\n"
        );
    }

    #[test]
    fn test_external_keys() {
        let buf = Arc::new(vec![0x12u8, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
        let dir = TempDir::new("index").expect("tempdir");
        let mut index = open_opts()
            .key_buf(Some(buf.clone()))
            .open(dir.path().join("a"))
            .expect("open");
        index
            .insert_advanced(InsertKey::Reference((1, 2)), 55, None)
            .expect("insert");
        index.flush().expect("flush");
        index
            .insert_advanced(InsertKey::Reference((1, 3)), 77, None)
            .expect("insert");
        assert_eq!(
            format!("{:?}", index),
            "Index { len: 20, root: Radix[0] }\n\
             Disk[1]: InlineLeaf { key: Disk[2], link: Disk[5] }\n\
             Disk[2]: ExtKey { start: 1, len: 2 }\n\
             Disk[5]: Link { value: 55, next: None }\n\
             Disk[8]: Radix { link: None, 3: Disk[1] }\n\
             Disk[16]: Root { radix: Disk[8] }\n\
             Radix[0]: Radix { link: None, 3: Radix[1] }\n\
             Radix[1]: Radix { link: None, 4: Radix[2] }\n\
             Radix[2]: Radix { link: None, 5: Radix[3] }\n\
             Radix[3]: Radix { link: None, 6: Radix[4] }\n\
             Radix[4]: Radix { link: Disk[5], 7: Leaf[0] }\n\
             Leaf[0]: Leaf { key: ExtKey[0], link: Link[0] }\n\
             Link[0]: Link { value: 77, next: None }\n\
             ExtKey[0]: ExtKey { start: 1, len: 3 }\n"
        );
    }

    #[test]
    fn test_inline_leafs() {
        let buf = Arc::new(vec![0x12u8, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
        let dir = TempDir::new("index").expect("tempdir");
        let mut index = open_opts()
            .key_buf(Some(buf.clone()))
            .open(dir.path().join("a"))
            .expect("open");

        // New entry. Should be inlined.
        index
            .insert_advanced(InsertKey::Reference((1, 1)), 55, None)
            .unwrap();
        index.flush().expect("flush");

        // Independent leaf. Should also be inlined.
        index
            .insert_advanced(InsertKey::Reference((2, 1)), 77, None)
            .unwrap();
        index.flush().expect("flush");

        // The link with 88 should refer to the inlined leaf 77.
        index
            .insert_advanced(InsertKey::Reference((2, 1)), 88, None)
            .unwrap();
        index.flush().expect("flush");

        // Not inlined because dependent link was not written first.
        // (could be optimized in the future)
        index
            .insert_advanced(InsertKey::Reference((3, 1)), 99, None)
            .unwrap();
        index
            .insert_advanced(InsertKey::Reference((3, 1)), 100, None)
            .unwrap();
        index.flush().expect("flush");

        assert_eq!(
            format!("{:?}", index),
            "Index { len: 97, root: Disk[77] }\n\
             Disk[1]: InlineLeaf { key: Disk[2], link: Disk[5] }\n\
             Disk[2]: ExtKey { start: 1, len: 1 }\n\
             Disk[5]: Link { value: 55, next: None }\n\
             Disk[8]: Radix { link: None, 3: Disk[1] }\n\
             Disk[16]: Root { radix: Disk[8] }\n\
             Disk[20]: InlineLeaf { key: Disk[21], link: Disk[24] }\n\
             Disk[21]: ExtKey { start: 2, len: 1 }\n\
             Disk[24]: Link { value: 77, next: None }\n\
             Disk[27]: Radix { link: None, 3: Disk[1], 5: Disk[20] }\n\
             Disk[39]: Root { radix: Disk[27] }\n\
             Disk[43]: Link { value: 88, next: Disk[24] }\n\
             Disk[46]: Leaf { key: Disk[21], link: Disk[43] }\n\
             Disk[49]: Radix { link: None, 3: Disk[1], 5: Disk[46] }\n\
             Disk[61]: Root { radix: Disk[49] }\n\
             Disk[65]: ExtKey { start: 3, len: 1 }\n\
             Disk[68]: Link { value: 99, next: None }\n\
             Disk[71]: Link { value: 100, next: Disk[68] }\n\
             Disk[74]: Leaf { key: Disk[65], link: Disk[71] }\n\
             Disk[77]: Radix { link: None, 3: Disk[1], 5: Disk[46], 7: Disk[74] }\n\
             Disk[93]: Root { radix: Disk[77] }\n"
        )
    }

    #[test]
    fn test_clone() {
        let dir = TempDir::new("index").expect("tempdir");
        let mut index = open_opts().open(dir.path().join("a")).expect("open");

        index.insert(&[], 55).expect("insert");
        index.insert(&[0x12], 77).expect("insert");
        index.flush().expect("flush");
        index.insert(&[0x15], 99).expect("insert");

        let index2 = index.clone().expect("clone");
        assert_eq!(format!("{:?}", index), format!("{:?}", index2));
    }

    #[test]
    fn test_open_options_write() {
        let dir = TempDir::new("index").expect("tempdir");
        let mut index = OpenOptions::new().open(dir.path().join("a")).expect("open");
        index.insert(&[0x12], 77).expect("insert");
        index.flush().expect("flush");

        OpenOptions::new()
            .write(Some(false))
            .open(dir.path().join("b"))
            .expect_err("open"); // file does not exist

        let mut index = OpenOptions::new()
            .write(Some(false))
            .open(dir.path().join("a"))
            .expect("open");
        index.flush().expect_err("cannot flush read-only index");
    }

    #[test]
    fn test_linked_list_values() {
        let dir = TempDir::new("index").expect("tempdir");
        let mut index = OpenOptions::new().open(dir.path().join("a")).expect("open");
        let list = vec![11u64, 17, 19, 31];
        for i in list.iter().rev() {
            index.insert(&[], *i).expect("insert");
        }

        let list1: Vec<u64> = index
            .get(&[])
            .unwrap()
            .values(&index)
            .map(|v| v.unwrap())
            .collect();
        assert_eq!(list, list1);

        index.flush().expect("flush");
        let list2: Vec<u64> = index
            .get(&[])
            .unwrap()
            .values(&index)
            .map(|v| v.unwrap())
            .collect();
        assert_eq!(list, list2);

        // Empty linked list
        assert_eq!(index.get(&[1]).unwrap().values(&index).count(), 0);

        // In case error happens, the iteration still stops.
        index.insert(&[], 5).expect("insert");
        index.dirty_links[0].next_link_offset = LinkOffset(Offset(1000));
        // Note: `collect` can return `io::Result<Vec<u64>>`. But that does not exercises the
        // infinite loop avoidance logic since `collect` stops iteration at the first error.
        let list_errored: Vec<io::Result<u64>> = index.get(&[]).unwrap().values(&index).collect();
        assert!(list_errored[list_errored.len() - 1].is_err());
    }

    #[test]
    fn test_checksum_bitflip() {
        let dir = TempDir::new("index").expect("tempdir");
        let mut index = open_opts().open(dir.path().join("a")).expect("open");

        // Debug build is much slower than release build. Limit the key length to 1-byte.
        #[cfg(debug_assertions)]
        let keys = vec![vec![0x13], vec![0x17], vec![]];

        // Release build can afford 2-byte key test.
        #[cfg(not(debug_assertions))]
        let keys = vec![
            vec![0x12, 0x34],
            vec![0x12, 0x78],
            vec![0x34, 0x56],
            vec![0x34],
            vec![0x78],
            vec![0x78, 0x9a],
        ];

        for (i, key) in keys.iter().enumerate() {
            index.insert(key, i as u64).expect("insert");
            index.insert(key, (i as u64) << 50).expect("insert");
        }
        index.flush().expect("flush");

        // Read the raw bytes of the index content
        let bytes = {
            let mut f = File::open(dir.path().join("a")).expect("open");
            let mut buf = vec![];
            f.read_to_end(&mut buf).expect("read");
            buf
        };

        fn is_corrupted(index: &Index, key: &[u8]) -> bool {
            let link = index.get(&key);
            match link {
                Err(_) => true,
                Ok(link) => link.values(&index).any(|v| v.is_err()),
            }
        }

        // Every bit change should trigger errors when reading all contents
        for i in 0..(bytes.len() * 8) {
            let mut bytes = bytes.clone();
            bytes[i / 8] ^= 1u8 << (i % 8);
            let mut f = File::create(dir.path().join("a")).expect("create");
            f.write_all(&bytes).expect("write");

            let index = open_opts().open(dir.path().join("a"));
            let detected = match index {
                Err(_) => true,
                Ok(index) => {
                    #[cfg(debug_assertions)]
                    let range = 0;
                    #[cfg(not(debug_assertions))]
                    let range = 0x10000;

                    (0..range).any(|key_int| {
                        let key = [(key_int >> 8) as u8, (key_int & 0xff) as u8];
                        is_corrupted(&index, &key)
                    }) || (0..0x100).any(|key_int| {
                        let key = [key_int as u8];
                        is_corrupted(&index, &key)
                    }) || is_corrupted(&index, &[])
                }
            };
            assert!(detected, "bit flip at {} is not detected", i);
        }
    }

    fn test_root_meta() {
        let dir = TempDir::new("rootmeta").expect("tempdir");
        let mut index = open_opts().open(dir.path().join("a")).expect("open");
        assert!(index.get_meta().is_empty());
        let meta = vec![200; 4000];
        index.set_meta(&meta);
        assert_eq!(index.get_meta(), &meta[..]);
        index.flush().expect("flush");
        let index = open_opts().open(dir.path().join("a")).expect("open");
        assert_eq!(index.get_meta(), &meta[..]);
    }

    quickcheck! {
        fn test_single_value(map: HashMap<Vec<u8>, u64>, flush: bool) -> bool {
            let dir = TempDir::new("index").expect("tempdir");
            let mut index = open_opts().open(dir.path().join("a")).expect("open");

            for (key, value) in &map {
                index.insert(key, *value).expect("insert");
            }

            if flush {
                let len = index.flush().expect("flush");
                index = open_opts().logical_len(len.into()).open(dir.path().join("a")).unwrap();
            }

            map.iter().all(|(key, value)| {
                let link_offset = index.get(key).expect("lookup");
                assert!(!link_offset.is_null());
                link_offset.value_and_next(&index).unwrap().0 == *value
            })
        }

        fn test_multiple_values(map: HashMap<Vec<u8>, Vec<u64>>) -> bool {
            let dir = TempDir::new("index").expect("tempdir");
            let mut index = open_opts().open(dir.path().join("a")).expect("open");

            for (key, values) in &map {
                for value in values.iter().rev() {
                    index.insert(key, *value).expect("insert");
                }
                if values.len() == 0 {
                    // Flush sometimes.
                    index.flush().expect("flush");
                }
            }

            map.iter().all(|(key, values)| {
                let v: Vec<u64> =
                    index.get(key).unwrap().values(&index).map(|v| v.unwrap()).collect();
                v == *values
            })
        }
    }
}
