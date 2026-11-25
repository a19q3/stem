//! The shared native Header accumulator used by Stem and L1 checkpoints.

use alloc::vec::Vec;

use crate::{Error, Hash32, Result, digest};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frontier {
    network: Hash32,
    count: u64,
    peaks: Vec<Hash32>,
}

impl Frontier {
    pub fn empty(network: Hash32) -> Self {
        Self {
            network,
            count: 0,
            peaks: Vec::new(),
        }
    }

    pub fn decode(network: Hash32, bytes: &[u8]) -> Result<Self> {
        let (count, peaks) = bytes.split_at_checked(8).ok_or(Error::Encoding)?;
        let count = u64::from_le_bytes(count.try_into().map_err(|_| Error::Encoding)?);
        if peaks.len() != 32 * count.count_ones() as usize {
            return Err(Error::Encoding);
        }
        let peaks = peaks
            .chunks_exact(32)
            .map(|raw| raw.try_into().expect("32 byte chunk"))
            .collect();
        Ok(Self {
            network,
            count,
            peaks,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = self.count.to_le_bytes().to_vec();
        for peak in &self.peaks {
            bytes.extend_from_slice(peak);
        }
        bytes
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn root(&self) -> Hash32 {
        digest(b"Stem history root\0", &[&self.network, &self.encode()])
    }

    pub fn append(&mut self, height: u64, header: &Hash32) -> Result<()> {
        self.append_indexed(height, header, |_, _, _| {})
    }

    pub fn append_indexed(
        &mut self,
        height: u64,
        header: &Hash32,
        record: impl FnMut(u64, u32, Hash32),
    ) -> Result<()> {
        if height != self.count {
            return Err(Error::Sequence);
        }
        self.append_subtree(0, leaf(&self.network, height, header), record)
    }

    pub fn from_index<E>(
        network: Hash32,
        count: u64,
        mut get: impl FnMut(u64, u32) -> core::result::Result<Hash32, E>,
    ) -> core::result::Result<Self, E> {
        let mut peaks = Vec::with_capacity(count.count_ones() as usize);
        let mut start = 0;
        for level in (0..64).rev() {
            let width = 1u64 << level;
            if count & width != 0 {
                peaks.push(get(start, level)?);
                start += width;
            }
        }
        Ok(Self {
            network,
            count,
            peaks,
        })
    }

    fn append_subtree(
        &mut self,
        mut level: u32,
        mut node: Hash32,
        mut record: impl FnMut(u64, u32, Hash32),
    ) -> Result<()> {
        let width = 1u64.checked_shl(level).ok_or(Error::Overflow)?;
        if !self.count.is_multiple_of(width) {
            return Err(Error::Sequence);
        }
        let count = self.count.checked_add(width).ok_or(Error::Overflow)?;
        record(count - width, level, node);
        while self.count & (1u64 << level) != 0 {
            node = merge(
                &self.network,
                &self.peaks.pop().ok_or(Error::Encoding)?,
                &node,
            );
            level += 1;
            record(count - (1u64 << level), level, node);
        }
        self.peaks.push(node);
        self.count = count;
        Ok(())
    }

    pub fn verify_extension(
        &self,
        later_count: u64,
        later_root: &Hash32,
        suffix: &[Hash32],
    ) -> bool {
        if later_count < self.count || suffix.len() > 128 {
            return false;
        }
        let mut frontier = self.clone();
        for node in suffix {
            let remaining = later_count - frontier.count;
            if remaining == 0 {
                return false;
            }
            let level = level(frontier.count, later_count);
            if frontier.append_subtree(level, *node, |_, _, _| {}).is_err() {
                return false;
            }
        }
        frontier.count == later_count && frontier.root() == *later_root
    }

    pub fn verify_inclusion(&self, height: u64, header: &Hash32, siblings: &[Hash32]) -> bool {
        let Some((position, first, level)) = peak(self.count, height) else {
            return false;
        };
        if siblings.len() != level as usize {
            return false;
        }
        let offset = height - first;
        let mut node = leaf(&self.network, height, header);
        for (bit, sibling) in siblings.iter().enumerate() {
            node = if offset & (1u64 << bit) == 0 {
                merge(&self.network, &node, sibling)
            } else {
                merge(&self.network, sibling, &node)
            };
        }
        node == self.peaks[position]
    }
}

fn level(from: u64, to: u64) -> u32 {
    (63 - (to - from).leading_zeros()).min(from.trailing_zeros())
}

fn peak(count: u64, height: u64) -> Option<(usize, u64, u32)> {
    if height >= count {
        return None;
    }
    let mut start = 0;
    let mut position = 0;
    for level in (0..64).rev() {
        let width = 1u64 << level;
        if count & width != 0 {
            if height - start < width {
                return Some((position, start, level));
            }
            start += width;
            position += 1;
        }
    }
    None
}

pub fn leaf(network: &Hash32, height: u64, header: &Hash32) -> Hash32 {
    digest(
        b"Stem history Header\0",
        &[network, &height.to_le_bytes(), header],
    )
}

pub fn merge(network: &Hash32, left: &Hash32, right: &Hash32) -> Hash32 {
    digest(b"Stem history node\0", &[network, left, right])
}

/// Read the minimal sibling path from an archive index.
pub fn inclusion<E: From<Error>>(
    height: u64,
    count: u64,
    mut get: impl FnMut(u64, u32) -> core::result::Result<Hash32, E>,
) -> core::result::Result<Vec<Hash32>, E> {
    let (_, start, level) = peak(count, height).ok_or(Error::Sequence)?;
    let offset = height - start;
    (0..level)
        .map(|bit| get(start + ((offset >> bit) ^ 1) * (1u64 << bit), bit))
        .collect()
}

/// Read one subtree for each maximal aligned interval of the suffix.
pub fn extension<E: From<Error>>(
    from: u64,
    to: u64,
    mut get: impl FnMut(u64, u32) -> core::result::Result<Hash32, E>,
) -> core::result::Result<Vec<Hash32>, E> {
    if from > to {
        return Err(Error::Sequence.into());
    }
    let mut start = from;
    let mut result = Vec::new();
    while start < to {
        let level = level(start, to);
        result.push(get(start, level)?);
        start += 1u64 << level;
    }
    Ok(result)
}
