//! Shared identity and state rules for objects first issued on Stem.

use crate::{
    hash,
    intercell::Identity,
    metatype::MAX_STATE_BYTES,
    spore::{plain_spore, plain_spore_fields},
};

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
    let (revision, _, _) = revision_fields(raw)?;
    Some(revision)
}

pub(crate) fn revision_fields(raw: &[u8]) -> Option<(u64, &[u8], &[u8])> {
    if raw.len() > MAX_STATE_BYTES {
        return None;
    }
    let (number, content) = raw.split_first_chunk()?;
    let (mime, content) = plain_spore_fields(content)?;
    Some((u64::from_le_bytes(*number), mime, content))
}

pub fn valid_revision_transition(before: &[u8], after: &[u8]) -> bool {
    let (Some(previous), Some(next)) = (revision(before), revision(after)) else {
        return false;
    };
    next > previous || (next == previous && before == after)
}

pub fn valid_append_only_transition(before: &[u8], after: &[u8]) -> bool {
    let Some((previous, previous_mime, prefix)) = revision_fields(before) else {
        return false;
    };
    if before == after {
        return true;
    }
    let Some((next, mime, content)) = revision_fields(after) else {
        return false;
    };
    next > previous
        && mime == previous_mime
        && content.len() > prefix.len()
        && content.starts_with(prefix)
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

    #[test]
    fn append_only_revisions_compose_across_skipped_revisions() {
        let state = |version: u64, mime: &[u8], content: &[u8]| {
            let mut raw = version.to_le_bytes().to_vec();
            let content_offset = 20 + mime.len() as u32;
            let total = content_offset + 4 + content.len() as u32;
            for field in [total, 16, content_offset, total, mime.len() as u32] {
                raw.extend_from_slice(&field.to_le_bytes());
            }
            raw.extend_from_slice(mime);
            raw.extend_from_slice(&(content.len() as u32).to_le_bytes());
            raw.extend_from_slice(content);
            raw
        };
        let plain = |version, content| state(version, b"text/plain", content);
        let original = plain(3, b"one");
        let intermediate = plain(4, b"one two");
        let current = plain(18, b"one two three");
        assert!(valid_append_only_transition(&original, &original));
        assert!(valid_append_only_transition(&original, &intermediate));
        assert!(valid_append_only_transition(&intermediate, &current));
        assert!(valid_append_only_transition(&original, &current));
        assert!(!valid_append_only_transition(&current, &intermediate));
        for invalid in [
            plain(18, b"twone"),
            plain(18, b"on"),
            plain(18, b"one"),
            plain(3, b"one two three"),
            state(18, b"application/octet-stream", b"one two three"),
        ] {
            assert!(!valid_append_only_transition(&original, &invalid));
        }
        assert!(!valid_append_only_transition(b"malformed", b"malformed"));
    }
}
