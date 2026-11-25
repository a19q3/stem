//! Deterministic identities for objects first issued on Stem.

use crate::{hash, intercell::Identity};

pub fn creation_id(network: &Identity, input: &[u8; 36], output_index: u32) -> Identity {
    hash(
        b"Stem object identity\0",
        &[network, input, &output_index.to_le_bytes()],
    )
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
}
