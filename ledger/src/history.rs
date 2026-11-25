//! The consensus MMR retains only its leaf count and peaks.

use anyhow::{Context, Result, ensure};
use ckb_types::{packed::Byte32, prelude::*};

use crate::hash;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct History {
    network: Byte32,
    count: u64,
    peaks: Vec<Byte32>,
}

impl History {
    pub fn empty(network: Byte32) -> Self {
        Self {
            network,
            count: 0,
            peaks: Vec::new(),
        }
    }

    pub fn decode(network: Byte32, encoded: &[u8]) -> Result<Self> {
        let (count, peaks) = encoded
            .split_at_checked(8)
            .context("truncated MMR frontier")?;
        let count = u64::from_le_bytes(count.try_into()?);
        ensure!(
            peaks.len() == 32 * count.count_ones() as usize,
            "noncanonical MMR peaks"
        );
        let peaks = peaks
            .chunks_exact(32)
            .map(Byte32::from_slice)
            .collect::<Result<_, _>>()?;
        Ok(Self {
            network,
            count,
            peaks,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(8 + self.peaks.len() * 32);
        bytes.extend_from_slice(&self.count.to_le_bytes());
        for peak in &self.peaks {
            bytes.extend_from_slice(peak.as_slice());
        }
        bytes
    }

    pub fn network(&self) -> &Byte32 {
        &self.network
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn root(&self) -> Byte32 {
        hash(b"Fern history root\0", &self.network, &[&self.encode()])
    }

    pub fn append(&mut self, height: u64, id: &Byte32) -> Result<()> {
        ensure!(height == self.count, "history must append the next Header");
        self.append_subtree(0, leaf(&self.network, height, id))
    }

    fn append_subtree(&mut self, mut level: u32, mut root: Byte32) -> Result<()> {
        let width = 1u64
            .checked_shl(level)
            .context("invalid MMR subtree level")?;
        ensure!(self.count.is_multiple_of(width), "unaligned MMR subtree");
        let new_count = self
            .count
            .checked_add(width)
            .context("MMR leaf count overflow")?;
        while self.count & (1u64 << level) != 0 {
            let left = self.peaks.pop().expect("canonical MMR frontier");
            root = node(&self.network, &left, &root);
            level += 1;
        }
        self.peaks.push(root);
        self.count = new_count;
        Ok(())
    }

    /// A consistency witness covers the remaining interval with maximal aligned
    /// subtrees in order. The verifier computes their sizes from the two counts.
    pub fn verify_extension(
        &self,
        later_count: u64,
        later_root: &Byte32,
        suffix: &[Byte32],
    ) -> bool {
        if later_count < self.count || suffix.len() > 128 {
            return false;
        }
        let mut frontier = self.clone();
        for subtree in suffix {
            let remaining = later_count - frontier.count;
            if remaining == 0 {
                return false;
            }
            let level = (63 - remaining.leading_zeros()).min(frontier.count.trailing_zeros());
            if frontier.append_subtree(level, subtree.clone()).is_err() {
                return false;
            }
        }
        frontier.count == later_count && frontier.root() == *later_root
    }

    pub fn verify_inclusion(&self, height: u64, id: &Byte32, siblings: &[Byte32]) -> bool {
        let Some((position, start, level)) = peak(self.count, height) else {
            return false;
        };
        if siblings.len() != level as usize {
            return false;
        }
        let offset = height - start;
        let mut current = leaf(&self.network, height, id);
        for (depth, sibling) in siblings.iter().enumerate() {
            current = if offset & (1u64 << depth) == 0 {
                node(&self.network, &current, sibling)
            } else {
                node(&self.network, sibling, &current)
            };
        }
        current == self.peaks[position]
    }
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

fn leaf(network: &Byte32, height: u64, header: &Byte32) -> Byte32 {
    hash(
        b"Fern history Header\0",
        network,
        &[&height.to_le_bytes(), header.as_slice()],
    )
}

fn node(network: &Byte32, left: &Byte32, right: &Byte32) -> Byte32 {
    hash(
        b"Fern history node\0",
        network,
        &[left.as_slice(), right.as_slice()],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaks_bind_height_and_verify_append_only_history() -> Result<()> {
        let network = [3u8; 32].pack();
        let ids = [
            [1u8; 32].pack(),
            [2u8; 32].pack(),
            [3u8; 32].pack(),
            [4u8; 32].pack(),
            [5u8; 32].pack(),
        ];
        let mut history = History::empty(network.clone());
        assert!(history.append(1, &ids[0]).is_err());
        for (height, id) in ids.iter().take(3).enumerate() {
            history.append(height as u64, id)?;
        }
        let earlier = history.clone();
        history.append(3, &ids[3])?;
        history.append(4, &ids[4])?;
        assert_eq!(
            history,
            History::decode(network.clone(), &history.encode())?
        );
        let leaf3 = leaf(&network, 3, &ids[3]);
        let leaf4 = leaf(&network, 4, &ids[4]);
        assert!(earlier.verify_extension(5, &history.root(), &[leaf3.clone(), leaf4]));
        assert!(!earlier.verify_extension(5, &history.root(), &[leaf3]));
        let leaf1 = leaf(&network, 1, &ids[1]);
        let subtree = node(
            &network,
            &leaf(&network, 2, &ids[2]),
            &leaf(&network, 3, &ids[3]),
        );
        assert!(history.verify_inclusion(0, &ids[0], &[leaf1.clone(), subtree.clone()]));
        assert!(!history.verify_inclusion(1, &ids[0], &[leaf1, subtree]));
        assert!(history.verify_inclusion(4, &ids[4], &[]));
        assert!(!history.verify_inclusion(5, &ids[4], &[]));
        Ok(())
    }
}
