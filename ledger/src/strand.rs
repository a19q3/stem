//! Borrowed validation primitives for the native Strand schema.
extern crate alloc;
use alloc::vec::Vec;
pub use ckb_types::bytes;
pub type Bytes = bytes::Bytes;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerificationError;
impl core::fmt::Display for VerificationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("invalid canonical Strand object")
    }
}
#[cfg(not(target_os = "none"))]
impl std::error::Error for VerificationError {}
pub type Verify = Result<(), VerificationError>;
pub const EMPTY_VECTOR: [u8; 4] = [0, 0, 0, 0];
pub fn unpack_number(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().expect("verified Strand number"))
}
pub fn number(b: &[u8]) -> Result<usize, VerificationError> {
    Ok(u32::from_le_bytes(
        b.get(..4)
            .ok_or(VerificationError)?
            .try_into()
            .map_err(|_| VerificationError)?,
    ) as usize)
}
pub fn fixed_vector_check(b: &[u8], width: usize) -> Result<usize, VerificationError> {
    let n = number(b)?;
    if n.checked_mul(width).and_then(|x| x.checked_add(4)) != Some(b.len()) {
        return Err(VerificationError);
    }
    Ok(n)
}
/// Minimal unsigned LEB128 with a schema-defined numeric width. The final
/// group may contain only the remaining value bits; overlong zero groups,
/// truncation and continuation beyond the width are rejected before shifting.
pub fn uleb_span(b: &[u8], bits: u32) -> Result<usize, VerificationError> {
    if bits == 0 || bits > 128 {
        return Err(VerificationError);
    }
    let groups = bits.div_ceil(7) as usize;
    for index in 0..groups {
        let byte = *b.get(index).ok_or(VerificationError)?;
        let value = byte & 0x7f;
        let remaining = (bits - index as u32 * 7).min(7);
        if value as u32 >= (1u32 << remaining) {
            return Err(VerificationError);
        }
        if byte & 0x80 == 0 {
            if index != 0 && value == 0 {
                return Err(VerificationError);
            }
            return Ok(index + 1);
        }
    }
    Err(VerificationError)
}
/// Only for a slice already checked by its generated Reader.
pub fn uleb_value(b: &[u8]) -> u128 {
    b.iter().enumerate().fold(0, |value, (i, byte)| {
        value | (((byte & 0x7f) as u128) << (i * 7))
    })
}
pub fn uleb_len(value: u128) -> usize {
    (128 - value.leading_zeros()).max(1).div_ceil(7) as usize
}
pub fn uleb_encode(mut value: u128) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(uleb_len(value));
    while value >= 128 {
        bytes.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    bytes.push(value as u8);
    bytes
}
pub trait Reader<'r>: Copy {
    fn new_unchecked(b: &'r [u8]) -> Self;
    fn as_slice(&self) -> &'r [u8];
    fn verify(b: &[u8]) -> Verify;
    fn span(b: &[u8]) -> Result<usize, VerificationError>;
    fn from_slice(b: &'r [u8]) -> Result<Self, VerificationError> {
        Self::verify(b)?;
        Ok(Self::new_unchecked(b))
    }
}
/// Optional bounded local index for repeated random reads of a verified dense
/// vector. The wire retains no offset table. Construction scans once; `get`
/// borrows the original bytes in constant time without a heap allocation.
#[derive(Debug)]
pub struct DenseIndex<'r, R, const MAX: usize> {
    bytes: &'r [u8],
    spans: [(u32, u32); MAX],
    count: usize,
    reader: core::marker::PhantomData<R>,
}
impl<'r, R: Reader<'r>, const MAX: usize> DenseIndex<'r, R, MAX> {
    pub(crate) fn new(bytes: &'r [u8]) -> Result<Self, VerificationError> {
        let count = number(bytes)?;
        if count > MAX {
            return Err(VerificationError);
        }
        let mut spans = [(0, 0); MAX];
        let mut position = 4usize;
        for span in &mut spans[..count] {
            let size = R::span(bytes.get(position..).ok_or(VerificationError)?)?;
            let end = position.checked_add(size).ok_or(VerificationError)?;
            if end > bytes.len() {
                return Err(VerificationError);
            }
            *span = (
                u32::try_from(position).map_err(|_| VerificationError)?,
                u32::try_from(end).map_err(|_| VerificationError)?,
            );
            position = end;
        }
        if position != bytes.len() {
            return Err(VerificationError);
        }
        Ok(Self {
            bytes,
            spans,
            count,
            reader: core::marker::PhantomData,
        })
    }
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn get(&self, index: usize) -> Option<R> {
        if index >= self.count {
            return None;
        }
        let (start, end) = self.spans[index];
        Some(R::new_unchecked(&self.bytes[start as usize..end as usize]))
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = R> + '_ {
        (0..self.count).map(|index| self.get(index).expect("indexed dense item"))
    }
}
pub trait Entity: Clone {
    type Builder: Builder<Entity = Self>;
    type Reader<'r>: Reader<'r>
    where
        Self: 'r;
    fn new_unchecked(b: Bytes) -> Self;
    fn as_slice(&self) -> &[u8];
    fn as_bytes(&self) -> Bytes;
    fn as_builder(&self) -> Self::Builder;
    fn new_builder() -> Self::Builder;
    fn as_reader(&self) -> Self::Reader<'_> {
        Self::Reader::new_unchecked(self.as_slice())
    }
}

pub(crate) fn owned_field(bytes: &Bytes, field: &[u8]) -> Bytes {
    let (base, start) = (bytes.as_ptr() as usize, field.as_ptr() as usize);
    if start
        .checked_sub(base)
        .is_some_and(|offset| offset <= bytes.len() && field.len() <= bytes.len() - offset)
    {
        bytes.slice_ref(field)
    } else {
        // Absent fields borrow their schema default from static storage.
        Bytes::copy_from_slice(field)
    }
}

pub struct DenseIter<T> {
    bytes: Bytes,
    pos: usize,
    remaining: usize,
    item: core::marker::PhantomData<T>,
}

impl<T> DenseIter<T> {
    pub(crate) fn new(bytes: Bytes) -> Self {
        let remaining = unpack_number(&bytes) as usize;
        Self {
            bytes,
            pos: 4,
            remaining,
            item: core::marker::PhantomData,
        }
    }
}

impl<T: Entity> Iterator for DenseIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        if self.remaining == 0 {
            return None;
        }
        let end = self.pos + T::Reader::span(&self.bytes[self.pos..]).expect("verified item");
        let item = T::new_unchecked(self.bytes.slice(self.pos..end));
        self.pos = end;
        self.remaining -= 1;
        Some(item)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<T: Entity> ExactSizeIterator for DenseIter<T> {}
pub trait Builder {
    type Entity: Entity;
    fn build(self) -> Self::Entity;
    #[cfg(not(target_os = "none"))]
    fn write<W: std::io::Write>(&self, w: &mut W) -> std::io::Result<()>;
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Byte([u8; 1]);
impl From<u8> for Byte {
    fn from(v: u8) -> Self {
        Self([v])
    }
}
impl From<Byte> for u8 {
    fn from(value: Byte) -> Self {
        value.0[0]
    }
}
impl Byte {
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}
pub mod prelude {
    pub use super::{Builder, Byte, ByteReader, Entity, Reader};
}
#[derive(Clone, Copy)]
pub struct ByteReader<'r>(&'r [u8]);
impl<'r> Reader<'r> for ByteReader<'r> {
    fn new_unchecked(b: &'r [u8]) -> Self {
        Self(b)
    }
    fn as_slice(&self) -> &'r [u8] {
        self.0
    }
    fn verify(b: &[u8]) -> Verify {
        if b.len() == 1 {
            Ok(())
        } else {
            Err(VerificationError)
        }
    }
    fn span(b: &[u8]) -> Result<usize, VerificationError> {
        if b.is_empty() {
            Err(VerificationError)
        } else {
            Ok(1)
        }
    }
}
