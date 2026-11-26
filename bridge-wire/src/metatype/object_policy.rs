use crate::intercell::Identity;

use super::MAX_STATE_BYTES;

/// The extra Type Script binds a complete object Script and its rule source.
pub struct ObjectPolicy<'a> {
    pub object: &'a Identity,
    pub rule: RuleSource<'a>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum RuleSource<'a> {
    DataHash(&'a Identity),
    LiveCkbType(&'a Identity),
}

impl<'a> ObjectPolicy<'a> {
    pub fn decode(raw: &'a [u8]) -> Option<Self> {
        let (object, raw) = raw.split_first_chunk::<32>()?;
        let (&kind, raw) = raw.split_first()?;
        let id: &Identity = raw.try_into().ok()?;
        if *object == [0; 32] || *id == [0; 32] {
            return None;
        }
        let rule = match kind {
            0 => RuleSource::DataHash(id),
            1 => RuleSource::LiveCkbType(id),
            _ => return None,
        };
        Some(Self { object, rule })
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

    pub fn increases(&self, previous: &Self) -> bool {
        self.0 > previous.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_a_nonzero_object_and_rule_and_rejects_noncanonical_limits() {
        let mut args = [&[1; 32][..], &[0], &[2; 32]].concat();
        let policy = ObjectPolicy::decode(&args).unwrap();
        assert_eq!(policy.object, &[1; 32]);
        assert_eq!(policy.rule, RuleSource::DataHash(&[2; 32]));
        args[32] = 1;
        assert_eq!(
            ObjectPolicy::decode(&args).unwrap().rule,
            RuleSource::LiveCkbType(&[2; 32])
        );
        args[32] = 2;
        assert!(ObjectPolicy::decode(&args).is_none());
        args[32] = 1;
        args.push(0);
        assert!(ObjectPolicy::decode(&args).is_none());
        assert!(ObjectPolicy::decode(&[0; 65]).is_none());

        let rule = StateLimit::decode(&3u16.to_le_bytes()).unwrap();
        assert!(rule.permits(b"abc"));
        assert!(!rule.permits(b"abcd"));
        assert!(!rule.permits(b""));
        assert!(StateLimit::decode(&[0, 0]).is_none());
        assert!(StateLimit::decode(&[3, 0, 0]).is_none());
        assert!(StateLimit::decode(&((MAX_STATE_BYTES + 1) as u16).to_le_bytes()).is_none());
        let larger = StateLimit::decode(&4u16.to_le_bytes()).unwrap();
        assert!(larger.increases(&rule));
        assert!(!rule.increases(&larger));
        assert!(!rule.increases(&rule));
    }
}
