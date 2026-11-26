use crate::intercell::Identity;

use super::MAX_STATE_BYTES;

/// The extra Type Script binds a complete object Script and a Cell Dep's raw data.
pub struct ObjectPolicy<'a> {
    pub object: &'a Identity,
    pub rule_hash: &'a Identity,
}

impl<'a> ObjectPolicy<'a> {
    pub fn decode(raw: &'a [u8]) -> Option<Self> {
        let (fields, tail) = raw.as_chunks::<32>();
        let [object, rule_hash] = fields else {
            return None;
        };
        (tail.is_empty() && !fields.contains(&[0; 32])).then_some(Self { object, rule_hash })
    }
}

pub struct StateLimit(usize);

impl StateLimit {
    pub const ENCODED_BYTES: usize = 2;

    pub fn decode(raw: &[u8]) -> Option<Self> {
        let limit = usize::from(u16::from_le_bytes(raw.try_into().ok()?));
        (1..=MAX_STATE_BYTES)
            .contains(&limit)
            .then_some(Self(limit))
    }

    pub fn permits(&self, state: &[u8]) -> bool {
        !state.is_empty() && state.len() <= self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_a_nonzero_object_and_rule_and_rejects_noncanonical_limits() {
        let mut args = [[1; 32], [2; 32]].as_flattened().to_vec();
        let policy = ObjectPolicy::decode(&args).unwrap();
        assert_eq!(policy.object, &[1; 32]);
        assert_eq!(policy.rule_hash, &[2; 32]);
        args.push(0);
        assert!(ObjectPolicy::decode(&args).is_none());
        assert!(ObjectPolicy::decode(&[0; 64]).is_none());

        let rule = StateLimit::decode(&3u16.to_le_bytes()).unwrap();
        assert!(rule.permits(b"abc"));
        assert!(!rule.permits(b"abcd"));
        assert!(!rule.permits(b""));
        assert!(StateLimit::decode(&[0, 0]).is_none());
        assert!(StateLimit::decode(&[3, 0, 0]).is_none());
        assert!(StateLimit::decode(&((MAX_STATE_BYTES + 1) as u16).to_le_bytes()).is_none());
    }
}
