use crate::intercell::Identity;

use super::{
    MAX_STATE_BYTES,
    custody::{Custody, Profile},
};

/// The extra Type Script binds a complete object Script and its rule source.
pub struct ObjectPolicy<'a> {
    pub object: &'a Identity,
    pub rule: RuleSource<'a>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum RuleSource<'a> {
    DataHash(&'a Identity),
    LiveCkbType {
        registry: &'a Identity,
        route: &'a Identity,
    },
}

impl<'a> ObjectPolicy<'a> {
    pub fn decode(raw: &'a [u8]) -> Option<Self> {
        let (object, raw) = raw.split_first_chunk::<32>()?;
        let (&kind, raw) = raw.split_first()?;
        let (id, raw) = raw.split_first_chunk::<32>()?;
        if *object == [0; 32] || *id == [0; 32] {
            return None;
        }
        let rule = match kind {
            0 if raw.is_empty() => RuleSource::DataHash(id),
            1 => {
                let route: &Identity = raw.try_into().ok()?;
                if *route == [0; 32] {
                    return None;
                }
                RuleSource::LiveCkbType {
                    registry: id,
                    route,
                }
            }
            _ => return None,
        };
        Some(Self { object, rule })
    }
}

#[derive(PartialEq, Eq)]
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

    pub fn permits_successor(&self, previous: Option<&[u8]>, state: &[u8]) -> bool {
        self.permits(state) || (valid_state(state) && previous == Some(state))
    }
}

pub fn valid_state(state: &[u8]) -> bool {
    (1..=MAX_STATE_BYTES).contains(&state.len())
}

#[derive(Clone, Copy)]
pub struct RouteCodes<'a> {
    pub escrow: &'a Identity,
    pub dispatcher: &'a Identity,
    pub gate: &'a Identity,
    pub mailbox: &'a Identity,
    pub adapter: &'a Identity,
}

impl<'a> RouteCodes<'a> {
    pub const SIZE: usize = 5 * 32;
    pub const WRAPPER_SIZE: usize = 53 + 3 * 32;

    pub fn authenticate(raw: &'a [u8], commitment: &Identity) -> Option<Self> {
        let (hashes, tail) = raw.as_chunks::<32>();
        let [escrow, dispatcher, gate, mailbox, adapter] = hashes else {
            return None;
        };
        (tail.is_empty()
            && !hashes.contains(&[0; 32])
            && &crate::hash(b"Stem Metatype ceiling route\0", &[raw]) == commitment)
            .then_some(Self {
                escrow,
                dispatcher,
                gate,
                mailbox,
                adapter,
            })
    }

    pub fn matches_custody(&self, custody: &Custody, object: &Identity) -> bool {
        custody.profile == Profile::Revisable
            && &custody.data_script == object
            && &custody.guard_code == self.gate
            && &custody.mailbox_code == self.mailbox
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
        assert!(ObjectPolicy::decode(&args).is_none());
        args.extend_from_slice(&[3; 32]);
        assert_eq!(
            ObjectPolicy::decode(&args).unwrap().rule,
            RuleSource::LiveCkbType {
                registry: &[2; 32],
                route: &[3; 32]
            }
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
        assert!(rule.permits_successor(Some(b"above"), b"above"));
        assert!(!rule.permits_successor(Some(b"above"), b"abovE"));
        assert!(!rule.permits_successor(None, b"above"));
        assert!(rule.permits_successor(Some(b"above"), b"end"));
        let route = [4; RouteCodes::SIZE];
        let commitment = crate::hash(b"Stem Metatype ceiling route\0", &[&route]);
        assert!(RouteCodes::authenticate(&route, &commitment).is_some());
        assert!(RouteCodes::authenticate(&route, &[9; 32]).is_none());
    }
}
