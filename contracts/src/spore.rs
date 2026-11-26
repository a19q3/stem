//! Compare the signed transfer receipt with the actual indexed Cell.

use ckb_std::{ckb_constants::Source, error::SysError, high_level::load_cell_data_hash};
use stem_bridge_wire::spore::TransferContent;

use crate::metatype::{bounded_data, cell_data_length};

/// Full byte equality for immutable Spore data, including its original encoding.
pub fn same_content(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    // Every bit pattern is a valid u64. align_to returns only complete,
    // aligned words in the middle and preserves the prefix and trailing bytes.
    let (left_prefix, left_words, left_tail) = unsafe { left.align_to::<u64>() };
    let (right_prefix, right_words, right_tail) = unsafe { right.align_to::<u64>() };
    if left_prefix.len() != right_prefix.len() {
        return left == right;
    }
    if left_prefix != right_prefix || left_tail != right_tail {
        return false;
    }
    left_words
        .iter()
        .zip(right_words)
        .all(|(left, right)| left == right)
}

pub fn matches_content(
    index: usize,
    source: Source,
    content: TransferContent<'_>,
) -> Result<bool, SysError> {
    match content {
        TransferContent::Inline(raw) => Ok(bounded_data(index, source, raw.len())? == raw),
        TransferContent::Committed { length, digest } => Ok(cell_data_length(index, source)?
            == length
            && &load_cell_data_hash(index, source)? == digest),
    }
}

#[cfg(test)]
mod tests {
    use super::same_content;

    #[test]
    fn exact_equality_for_all_alignments_and_partial_words() {
        let original = [0x5a; 80];
        let mut other = original;
        for length in 0..=64 {
            for left_offset in 0..8 {
                let left = &original[left_offset..left_offset + length];
                for right_offset in 0..8 {
                    let right = &mut other[right_offset..right_offset + length];
                    assert!(same_content(left, right));
                    for index in 0..length {
                        right[index] ^= 1;
                        assert!(!same_content(left, right));
                        right[index] ^= 1;
                    }
                    assert!(!same_content(left, &other[..length + 1]));
                }
            }
        }
    }
}
