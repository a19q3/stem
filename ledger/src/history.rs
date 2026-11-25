//! The consensus MMR retains only its leaf count and peaks.

use anyhow::Result;
use ckb_types::{packed::Byte32, prelude::*};
use fern_stake::history::Frontier;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct History {
    network: Byte32,
    frontier: Frontier,
}

impl History {
    pub fn empty(network: Byte32) -> Self {
        let frontier = Frontier::empty(network.unpack());
        Self { network, frontier }
    }

    pub fn decode(network: Byte32, encoded: &[u8]) -> Result<Self> {
        let frontier = Frontier::decode(network.unpack(), encoded)?;
        Ok(Self { network, frontier })
    }

    pub fn encode(&self) -> Vec<u8> {
        self.frontier.encode()
    }

    pub fn network(&self) -> &Byte32 {
        &self.network
    }

    pub fn count(&self) -> u64 {
        self.frontier.count()
    }

    pub fn root(&self) -> Byte32 {
        self.frontier.root().pack()
    }

    pub fn append(&mut self, height: u64, id: &Byte32) -> Result<()> {
        Ok(self.frontier.append(height, &id.unpack())?)
    }

    pub fn verify_extension(&self, count: u64, root: &Byte32, suffix: &[Byte32]) -> bool {
        if suffix.len() > 128 {
            return false;
        }
        let suffix = suffix
            .iter()
            .map(Unpack::<[u8; 32]>::unpack)
            .collect::<Vec<_>>();
        self.frontier
            .verify_extension(count, &root.unpack(), &suffix)
    }

    pub fn verify_inclusion(&self, height: u64, id: &Byte32, siblings: &[Byte32]) -> bool {
        if siblings.len() > 63 {
            return false;
        }
        let siblings = siblings
            .iter()
            .map(Unpack::<[u8; 32]>::unpack)
            .collect::<Vec<_>>();
        self.frontier
            .verify_inclusion(height, &id.unpack(), &siblings)
    }
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
        let mut indexed = Frontier::empty(network.unpack());
        let mut nodes = std::collections::BTreeMap::new();
        for (height, id) in ids.iter().enumerate() {
            indexed.append_indexed(height as u64, &id.unpack(), |start, level, hash| {
                nodes.insert((start, level), hash);
            })?;
        }
        let get = |start, level| -> Result<_> { Ok(nodes[&(start, level)]) };
        let suffix = fern_stake::history::extension(3, 5, get)?
            .into_iter()
            .map(|hash| hash.pack())
            .collect::<Vec<_>>();
        assert!(earlier.verify_extension(5, &history.root(), &suffix));
        assert!(!earlier.verify_extension(5, &history.root(), &suffix[..1]));
        let rebuilt = Frontier::from_index(network.unpack(), 5, get)?;
        assert_eq!(rebuilt.root().pack(), history.root());
        for (height, hash) in ids.iter().enumerate() {
            let path = fern_stake::history::inclusion(height as u64, 5, get)?
                .into_iter()
                .map(|hash| hash.pack())
                .collect::<Vec<_>>();
            assert!(history.verify_inclusion(height as u64, hash, &path));
            assert!(!history.verify_inclusion(
                height as u64,
                &ids[(height + 1) % ids.len()],
                &path
            ));
        }
        Ok(())
    }
}
