//! Authorization for one native object creation or successor, issued by a CKB Type.
use crate::intercell::Identity;

pub const SIZE: usize = 137;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Create = 0,
    Transfer = 1,
}

#[derive(Debug, Eq, PartialEq)]
pub struct Permit {
    pub action: Action,
    pub network: Identity,
    pub object: Identity,
    pub source: [u8; 36],
    pub output_hash: Identity,
}

impl Permit {
    pub fn encode(&self) -> [u8; SIZE] {
        let mut bytes = [0; SIZE];
        bytes[..4].copy_from_slice(b"SOP1");
        bytes[4] = self.action as u8;
        bytes[5..37].copy_from_slice(&self.network);
        bytes[37..69].copy_from_slice(&self.object);
        bytes[69..105].copy_from_slice(&self.source);
        bytes[105..].copy_from_slice(&self.output_hash);
        bytes
    }

    pub fn decode(raw: &[u8]) -> Option<Self> {
        if raw.len() != SIZE || &raw[..4] != b"SOP1" {
            return None;
        }
        let action = match raw[4] {
            0 => Action::Create,
            1 => Action::Transfer,
            _ => return None,
        };
        let permit = Self {
            action,
            network: raw[5..37].try_into().ok()?,
            object: raw[37..69].try_into().ok()?,
            source: raw[69..105].try_into().ok()?,
            output_hash: raw[105..].try_into().ok()?,
        };
        (permit.network != [0; 32]
            && permit.object != [0; 32]
            && permit.source[..32] != [0; 32]
            && permit.output_hash != [0; 32])
            .then_some(permit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_actions_use_one_exact_record() {
        for action in [Action::Create, Action::Transfer] {
            let expected = Permit {
                action,
                network: [1; 32],
                object: [2; 32],
                source: [3; 36],
                output_hash: [4; 32],
            };
            let raw = expected.encode();
            assert_eq!(Permit::decode(&raw), Some(expected));
            assert!(Permit::decode(&raw[1..]).is_none());
            let mut extended = [0; SIZE + 1];
            extended[..SIZE].copy_from_slice(&raw);
            assert!(Permit::decode(&extended).is_none());
            for i in [0, 4] {
                let mut invalid = raw;
                invalid[i] = 0xff;
                assert!(Permit::decode(&invalid).is_none());
            }
            for start in [5, 37, 69, 105] {
                let mut invalid = raw;
                invalid[start..start + 32].fill(0);
                assert!(Permit::decode(&invalid).is_none());
            }
        }
    }
}
