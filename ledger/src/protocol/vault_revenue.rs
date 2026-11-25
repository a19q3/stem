use anyhow::{Context, Result, ensure};
use ckb_types::{bytes::Bytes, packed::Byte32};

use crate::{
    protocol::{byte32, compact64, hash32},
    strand::prelude::{Builder as WireBuilder, Entity as WireEntity},
    wire,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultRevenueEntry {
    pub lane: Byte32,
    /// Cumulative gross fees attributed to the vault. Actual disbursements
    /// are authenticated separately by the fund policy and payout records.
    pub attributed: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultRevenue(Vec<VaultRevenueEntry>);

impl VaultRevenue {
    pub fn new(entries: Vec<VaultRevenueEntry>) -> Result<Self> {
        ensure!(
            !entries.is_empty()
                && entries.iter().all(|entry| entry.lane != Byte32::default())
                && entries.windows(2).all(|pair| pair[0].lane < pair[1].lane),
            "vault revenue lanes must be sorted and unique"
        );
        let revenue = Self(entries);
        revenue.total()?;
        Ok(revenue)
    }

    pub fn entries(&self) -> &[VaultRevenueEntry] {
        &self.0
    }

    pub fn total(&self) -> Result<u64> {
        self.0.iter().try_fold(0u64, |sum, entry| {
            sum.checked_add(entry.attributed)
                .context("vault revenue total overflow")
        })
    }

    /// Divide the block's fees by recognized vault Capacity, in lane order.
    /// Differencing cumulative quotients assigns the exact total and avoids
    /// awarding roundoff to an empty last lane.
    pub fn allocate(mut self, fees: u64, weights: &[u64]) -> Result<Self> {
        ensure!(
            weights.len() == self.0.len(),
            "wrong vault revenue weight count"
        );
        let total = weights.iter().try_fold(0u64, |sum, &amount| {
            sum.checked_add(amount).context("vault weight overflow")
        })?;
        ensure!(total != 0, "fees have no recognized vault backing");
        let mut prefix = 0u64;
        let mut allocated = 0u64;
        for (entry, &weight) in self.0.iter_mut().zip(weights) {
            prefix += weight;
            let share = (u128::from(fees) * u128::from(prefix) / u128::from(total)) as u64;
            entry.attributed = entry
                .attributed
                .checked_add(share - allocated)
                .context("vault revenue overflow")?;
            allocated = share;
        }
        self.total()?;
        Ok(self)
    }

    pub fn encode(&self) -> Bytes {
        wire::VaultRevenue::new_builder()
            .set(
                self.0
                    .iter()
                    .map(|entry| {
                        wire::VaultRevenueEntry::new_builder()
                            .lane(byte32(&entry.lane))
                            .attributed(compact64(entry.attributed))
                            .build()
                    })
                    .collect(),
            )
            .build()
            .as_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let data = wire::VaultRevenue::from_slice(bytes)?;
        let entries = data
            .into_iter()
            .map(|item| {
                Ok(VaultRevenueEntry {
                    lane: hash32(item.lane()),
                    attributed: item.attributed().as_reader().value().try_into()?,
                })
            })
            .collect::<Result<_>>()?;
        Self::new(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ckb_types::prelude::*;

    #[test]
    fn vault_allocation_conserves_fees_and_awards_nothing_to_zero_weight() -> Result<()> {
        let entries = (1..=4)
            .map(|i| VaultRevenueEntry {
                lane: [i; 32].pack(),
                attributed: 0,
            })
            .collect();
        let revenue = VaultRevenue::new(entries)?;
        let attributed = revenue
            .clone()
            .allocate(17, &[1, 0, 2, 0])?
            .allocate(2, &[0, 2, 0, 2])?;
        let amounts: Vec<_> = attributed
            .entries()
            .iter()
            .map(|entry| entry.attributed)
            .collect();
        assert_eq!(amounts, [5, 1, 12, 1]);
        assert_eq!(attributed.total()?, 19);
        assert_eq!(VaultRevenue::decode(&attributed.encode())?, attributed);
        assert!(revenue.clone().allocate(1, &[0; 4]).is_err());
        assert!(revenue.allocate(1, &[u64::MAX, 1, 0, 0]).is_err());
        Ok(())
    }
}
