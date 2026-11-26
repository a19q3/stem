//! Shared identity and state rules for objects first issued on Stem.

use crate::{hash, intercell::Identity, metatype::MAX_STATE_BYTES, spore::plain_spore};

pub fn creation_id(network: &Identity, input: &[u8; 36], output_index: u32) -> Identity {
    hash(
        b"Stem object identity\0",
        &[network, input, &output_index.to_le_bytes()],
    )
}

pub struct ObjectArgs<'a> {
    pub id: &'a Identity,
    pub native_authority: &'a Identity,
    pub ckb_authority: &'a Identity,
    pub policy_code: &'a Identity,
    pub revisable: bool,
}

impl<'a> ObjectArgs<'a> {
    pub fn decode(raw: &'a [u8]) -> Option<Self> {
        let (base, mode) = raw.split_at_checked(4 * 32)?;
        let [id, native_authority, ckb_authority, policy_code] = base.as_chunks::<32>().0 else {
            return None;
        };
        let revisable = match mode {
            [] => false,
            [1] => true,
            _ => return None,
        };
        (!base.as_chunks::<32>().0.contains(&[0; 32])).then_some(Self {
            id,
            native_authority,
            ckb_authority,
            policy_code,
            revisable,
        })
    }

    pub fn valid_state(&self, raw: &[u8]) -> bool {
        if self.revisable {
            revision(raw).is_some()
        } else {
            plain_spore(raw)
        }
    }

    pub fn permits(&self, before: &[u8], after: &[u8]) -> bool {
        if self.revisable {
            valid_revision_transition(before, after)
        } else {
            before == after && plain_spore(before)
        }
    }
}

/// A revision is followed by canonical plain object content. The length
/// bound also fits the same state into one portable Manifest entry.
pub fn revision(raw: &[u8]) -> Option<u64> {
    let (number, content) = raw.split_first_chunk()?;
    (raw.len() <= MAX_STATE_BYTES && plain_spore(content)).then(|| u64::from_le_bytes(*number))
}

pub fn valid_revision_transition(before: &[u8], after: &[u8]) -> bool {
    let (Some(previous), Some(next)) = (revision(before), revision(after)) else {
        return false;
    };
    next > previous || (next == previous && before == after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_binds_network_input_and_output_position() {
        let id = creation_id(&[1; 32], &[2; 36], 0);
        assert_ne!(id, creation_id(&[9; 32], &[2; 36], 0));
        assert_ne!(id, creation_id(&[1; 32], &[3; 36], 0));
        assert_ne!(id, creation_id(&[1; 32], &[2; 36], 1));
    }

    #[test]
    fn revision_changes_require_a_higher_version() {
        let state = |version: u64, content| {
            let mut raw = version.to_le_bytes().to_vec();
            raw.extend_from_slice(&35u32.to_le_bytes());
            for offset in [16u32, 30, 35] {
                raw.extend_from_slice(&offset.to_le_bytes());
            }
            raw.extend_from_slice(&[10, 0, 0, 0]);
            raw.extend_from_slice(b"text/plain");
            raw.extend_from_slice(&[1, 0, 0, 0, content]);
            raw
        };
        let original = state(0, b'x');
        assert_eq!(revision(&original), Some(0));
        assert!(valid_revision_transition(&original, &original));
        assert!(valid_revision_transition(&original, &state(2, b'y')));
        assert!(!valid_revision_transition(&original, &state(0, b'y')));
        assert!(!valid_revision_transition(&state(2, b'y'), &original));
        assert!(!valid_revision_transition(&original, &state(1, b'z')[..12]));
        assert!(!valid_revision_transition(
            &state(u64::MAX, b'x'),
            &original
        ));
        assert!(valid_revision_transition(&original, &state(u64::MAX, b'x')));
    }
}
