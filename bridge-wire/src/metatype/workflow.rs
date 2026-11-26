//! Portable state and signature transcript for a three-step object workflow.

use crate::{hash, intercell::Identity, object::revision_fields};

pub const STEPS: usize = 3;
pub const SIGNATURE_SIZE: usize = 64;
pub const MAX_PROOF: usize = STEPS * SIGNATURE_SIZE;
pub const ACTION: u16 = 1;

pub struct Workflow<'a> {
    pub network: &'a Identity,
    pub object: &'a Identity,
    pub escrow_code: &'a Identity,
    pub authorities: &'a [Identity; STEPS],
}

impl<'a> Workflow<'a> {
    pub fn decode(raw: &'a [u8]) -> Option<Self> {
        let (fields, rest) = raw.as_chunks::<32>();
        let [network, object, escrow_code, authorities @ ..] = fields else {
            return None;
        };
        let authorities: &[Identity; STEPS] = authorities.try_into().ok()?;
        if !rest.is_empty()
            || fields.contains(&[0; 32])
            || authorities[0] == authorities[1]
            || authorities[0] == authorities[2]
            || authorities[1] == authorities[2]
        {
            return None;
        }
        Some(Self {
            network,
            object,
            escrow_code,
            authorities,
        })
    }

    pub fn verify_successor(
        &self,
        script: &Identity,
        before: State,
        after: State,
        proof: &[u8],
        max_steps: usize,
        mut verify: impl FnMut(&Identity, &Identity, &[u8; SIGNATURE_SIZE]) -> bool,
    ) -> bool {
        if before == after {
            return proof.is_empty();
        }
        let Some(count) = after.revision.checked_sub(before.revision) else {
            return false;
        };
        let (signatures, remaining) = proof.as_chunks::<SIGNATURE_SIZE>();
        if !(1..=max_steps).contains(&count)
            || signatures.len() != count
            || !remaining.is_empty()
            || (before.revision != 0 && before.subject != after.subject)
        {
            return false;
        }
        let mut head = before.head;
        for (i, signature) in signatures.iter().enumerate() {
            let index = before.revision + i;
            head = step_digest(self.network, script, &head, &after.subject, index + 1);
            if !verify(&self.authorities[index], &head, signature) {
                return false;
            }
        }
        head == after.head
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct State {
    revision: usize,
    subject: Identity,
    head: Identity,
}

impl State {
    pub fn decode(data: &[u8], script: &Identity) -> Option<Self> {
        let (revision, mime, content) = revision_fields(data)?;
        let revision = usize::try_from(revision).ok()?;
        let ([subject, head], []) = content.as_chunks::<32>() else {
            return None;
        };
        if mime != b"application/octet-stream" || revision > STEPS {
            return None;
        }
        if revision == 0 {
            if *subject != [0; 32] || *head != initial_head(script) {
                return None;
            }
        } else if *subject == [0; 32] || *head == [0; 32] {
            return None;
        }
        Some(Self {
            revision,
            subject: *subject,
            head: *head,
        })
    }

    pub fn is_initial(self) -> bool {
        self.revision == 0
    }
}

pub fn initial_head(script: &Identity) -> Identity {
    hash(b"Stem Metatype workflow start\0", &[script])
}

pub fn step_digest(
    network: &Identity,
    script: &Identity,
    previous: &Identity,
    subject: &Identity,
    step: usize,
) -> Identity {
    hash(
        b"Stem Metatype workflow step\0",
        &[
            network,
            script,
            previous,
            subject,
            &(step as u64).to_le_bytes(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successor_checks_every_step_in_order() {
        let script = [4; 32];
        let workflow = Workflow {
            network: &[1; 32],
            object: &[2; 32],
            escrow_code: &[3; 32],
            authorities: &[[5; 32], [6; 32], [7; 32]],
        };
        let start = State {
            revision: 0,
            subject: [0; 32],
            head: initial_head(&script),
        };
        let subject = [8; 32];
        let first = step_digest(workflow.network, &script, &start.head, &subject, 1);
        let second = step_digest(workflow.network, &script, &first, &subject, 2);
        let done = State {
            revision: 2,
            subject,
            head: second,
        };
        let mut proof = [0; 128];
        proof[..32].copy_from_slice(&first);
        proof[32] = 5;
        proof[64..96].copy_from_slice(&second);
        proof[96] = 6;
        let check = |key: &Identity, digest: &Identity, seal: &[u8; 64]| {
            &seal[..32] == digest && seal[32] == key[0]
        };
        assert!(workflow.verify_successor(&script, start, done, &proof, STEPS, check));
        assert!(!workflow.verify_successor(&script, start, done, &proof, 1, check));
        assert!(!workflow.verify_successor(&script, start, done, &proof[..64], STEPS, check));
        proof.rotate_left(64);
        assert!(!workflow.verify_successor(&script, start, done, &proof, STEPS, check));
        assert!(!workflow.verify_successor(&script, done, done, &proof, STEPS, check));
        assert!(workflow.verify_successor(&script, done, done, &[], STEPS, check));
    }
}
