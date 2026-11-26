//! Fixed Capacity royalty for an independently identified object.

use super::Hash;
use crate::hash;

pub struct Royalty<'a> {
    pub object: &'a Hash,
    pub stem_payee: &'a Hash,
    pub ckb_payee: &'a Hash,
    pub amount: u64,
}

impl<'a> Royalty<'a> {
    pub fn decode(args: &'a [u8]) -> Option<Self> {
        let (object, args) = args.split_first_chunk::<32>()?;
        let (stem_payee, args) = args.split_first_chunk::<32>()?;
        let (ckb_payee, args) = args.split_first_chunk::<32>()?;
        let amount = u64::from_le_bytes(args.try_into().ok()?);
        (![object, stem_payee, ckb_payee].contains(&&[0; 32]) && amount != 0).then_some(Self {
            object,
            stem_payee,
            ckb_payee,
            amount,
        })
    }
}

pub fn payment_tag(deployment: Option<&Hash>, royalty_script: &Hash) -> Hash {
    hash(
        b"Stem Metatype royalty payment\0",
        &[deployment.unwrap_or(&[0; 32]), royalty_script],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn royalty_arguments_and_payment_scope_are_canonical() {
        let mut bytes = [1; 104];
        bytes[96..].copy_from_slice(&12u64.to_le_bytes());
        let rule = Royalty::decode(&bytes).unwrap();
        assert_eq!(rule.amount, 12);
        assert!(Royalty::decode(&bytes[..103]).is_none());
        bytes[32..64].fill(0);
        assert!(Royalty::decode(&bytes).is_none());
        assert_ne!(payment_tag(None, &[2; 32]), payment_tag(None, &[3; 32]));
        assert_ne!(
            payment_tag(Some(&[4; 32]), &[2; 32]),
            payment_tag(Some(&[5; 32]), &[2; 32])
        );
        assert_ne!(
            payment_tag(None, &[2; 32]),
            payment_tag(Some(&[4; 32]), &[2; 32])
        );
    }
}
