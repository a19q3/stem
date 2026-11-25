//! Live Cell membership at an empty checkpoint, using CometBFT's RFC 6962 Merkle rules.

use std::{
    fs::File,
    io::{Cursor, Read, Write},
    os::unix::fs::FileExt,
};

use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ckb_types::{packed::OutPoint, prelude::*};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub type Hash = [u8; 32];
pub const MAX_IMAGE: u64 = 8 * 1024 * 1024 * 1024;
const MAX_RECORD: usize = 16 * 1024 * 1024;
const GROUP_RECORDS: usize = 128;
const GROUP_BYTES: u32 = 1024 * 1024;
const POINT: usize = OutPoint::TOTAL_SIZE;
const GROUP_SIZE: u64 = (POINT + 8 + 4 + 32) as u64;
const RECORD_PREFIX: u32 = (8 + 1 + POINT) as u32;
const MAX_GROUPS: u64 = MAX_IMAGE / (GROUP_RECORDS as u64 * RECORD_PREFIX as u64)
    + MAX_IMAGE / ((GROUP_BYTES / 2) as u64)
    + 1;

fn leaf(parts: &[&[u8]]) -> Hash {
    let mut hash = Sha256::new();
    hash.update([0]);
    for part in parts {
        hash.update(part);
    }
    hash.finalize().into()
}

fn combine(left: &Hash, right: &Hash) -> Hash {
    let mut hash = Sha256::new();
    hash.update([1]);
    hash.update(left);
    hash.update(right);
    hash.finalize().into()
}

fn split(count: u64) -> u64 {
    1 << (u64::BITS - (count - 1).leading_zeros() - 1)
}

// The preorder layout of a tree of n leaves occupies 2n-1 hashes. Each left
// child starts immediately after its parent and its sibling follows its 2k-1 hashes.
fn tree(nodes: &mut Vec<Hash>, leaves: &[Hash]) -> Hash {
    if leaves.len() == 1 {
        nodes.push(leaves[0]);
        return leaves[0];
    }
    let position = nodes.len();
    nodes.push([0; 32]);
    let (left, right) = leaves.split_at(split(leaves.len() as u64) as usize);
    let root = combine(&tree(nodes, left), &tree(nodes, right));
    nodes[position] = root;
    root
}

fn path(
    index: u64,
    total: u64,
    node: u64,
    load: &impl Fn(u64) -> Result<Hash>,
    aunts: &mut Vec<Hash>,
) -> Result<()> {
    if total == 1 {
        return Ok(());
    }
    let left = split(total);
    let (first, right) = (node + 1, node + left * 2);
    let sibling = if index < left {
        path(index, left, first, load, aunts)?;
        right
    } else {
        path(index - left, total - left, right, load, aunts)?;
        first
    };
    aunts.push(load(sibling)?);
    Ok(())
}

fn proof(index: u64, total: u64, load: impl Fn(u64) -> Result<Hash>) -> Result<Value> {
    let mut aunts = Vec::new();
    path(index, total, 0, &load, &mut aunts)?;
    Ok(
        json!({"index": index, "total": total, "aunts": aunts.into_iter().map(hex::encode).collect::<Vec<_>>()}),
    )
}

#[derive(Default)]
struct Frontier(Vec<Option<Hash>>);

impl Frontier {
    fn push(&mut self, mut hash: Hash) {
        for height in &mut self.0 {
            match height.take() {
                Some(left) => hash = combine(&left, &hash),
                None => {
                    *height = Some(hash);
                    return;
                }
            }
        }
        self.0.push(Some(hash));
    }

    fn root(&self) -> Hash {
        self.0
            .iter()
            .flatten()
            .copied()
            .reduce(|right, left| combine(&left, &right))
            .unwrap_or_else(|| Sha256::digest([]).into())
    }
}

struct Group {
    first: [u8; POINT],
    offset: u64,
    size: u32,
    root: Hash,
}

impl Default for Group {
    fn default() -> Self {
        Self {
            first: [0; POINT],
            offset: 0,
            size: 0,
            root: [0; 32],
        }
    }
}

impl Group {
    fn read(file: &File, index: u64) -> Result<Self> {
        let mut record = [0; GROUP_SIZE as usize];
        file.read_exact_at(&mut record, 8 + GROUP_SIZE * index)?;
        Ok(Self {
            first: record[..POINT].try_into()?,
            offset: u64::from_le_bytes(record[POINT..POINT + 8].try_into()?),
            size: u32::from_le_bytes(record[POINT + 8..POINT + 12].try_into()?),
            root: record[POINT + 12..].try_into()?,
        })
    }

    fn write(&self, writer: &mut impl Write) -> Result<()> {
        writer.write_all(&self.first)?;
        writer.write_all(&self.offset.to_le_bytes())?;
        writer.write_all(&self.size.to_le_bytes())?;
        writer.write_all(&self.root)?;
        Ok(())
    }
}

#[derive(Default)]
pub struct Builder {
    group: Group,
    current_leaves: usize,
    current: Frontier,
    outer: Frontier,
    groups: Vec<Group>,
}

impl Builder {
    pub fn add(&mut self, offset: u64, key: &[u8], value: &[u8]) -> Result<()> {
        let point = &key[1..];
        ensure!(
            value.len() <= MAX_RECORD,
            "live Cell record exceeds archive bounds"
        );
        let size = RECORD_PREFIX + u32::try_from(value.len())?;
        if self.current_leaves == GROUP_RECORDS
            || (self.current_leaves > 0 && self.group.size.saturating_add(size) > GROUP_BYTES)
        {
            self.finish_group();
        }
        if self.current_leaves == 0 {
            self.group.first = point.try_into()?;
            self.group.offset = offset;
        }
        self.group.size = self
            .group
            .size
            .checked_add(size)
            .context("Cell group size overflow")?;
        self.current.push(leaf(&[point, value]));
        self.current_leaves += 1;
        Ok(())
    }

    fn finish_group(&mut self) {
        self.group.root = self.current.root();
        self.outer.push(leaf(&[&self.group.root]));
        let group = std::mem::take(&mut self.group);
        if group.offset + u64::from(group.size) <= MAX_IMAGE {
            self.groups.push(group);
        }
        self.current.0.clear();
        self.current_leaves = 0;
    }

    pub fn finish(mut self) -> (Hash, Index) {
        if self.current_leaves > 0 {
            self.finish_group();
        }
        (self.outer.root(), Index(self.groups))
    }
}

pub struct Index(Vec<Group>);

impl Index {
    pub fn write(self, root: &Hash, writer: &mut impl Write) -> Result<()> {
        let count = self.0.len() as u64;
        ensure!(count <= MAX_GROUPS, "too many Cell proof groups");
        writer.write_all(&count.to_le_bytes())?;
        let mut leaves = Vec::with_capacity(self.0.len());
        for group in self.0 {
            group.write(writer)?;
            leaves.push(leaf(&[&group.root]));
        }
        if !leaves.is_empty() {
            let mut nodes = Vec::with_capacity(leaves.len() * 2 - 1);
            ensure!(
                &tree(&mut nodes, &leaves) == root,
                "Cell proof index root differs from checkpoint"
            );
            for hash in nodes {
                writer.write_all(&hash)?;
            }
        }
        Ok(())
    }
}

pub fn prove(
    index: &File,
    image: &File,
    size: u64,
    root: &Hash,
    point: &OutPoint,
) -> Result<Option<Value>> {
    let mut count = [0; 8];
    index.read_exact_at(&mut count, 0)?;
    let count = u64::from_le_bytes(count);
    ensure!(count <= MAX_GROUPS, "invalid Cell proof group count");
    if count == 0 {
        ensure!(root == &Sha256::digest([])[..], "Cell proof index is empty");
        return Ok(None);
    }
    let tree_offset = 8 + GROUP_SIZE * count;
    ensure!(
        index.metadata()?.len() == tree_offset + (2 * count - 1) * 32,
        "invalid Cell proof index size"
    );
    let load = |node| {
        let mut hash = [0; 32];
        index.read_exact_at(&mut hash, tree_offset + 32 * node)?;
        Ok(hash)
    };
    ensure!(
        &load(0)? == root,
        "Cell proof index belongs to a different checkpoint"
    );
    let mut lower = 0;
    let mut upper = count;
    while lower < upper {
        let middle = lower + (upper - lower) / 2;
        if Group::read(index, middle)?.first.as_slice() <= point.as_slice() {
            lower = middle + 1;
        } else {
            upper = middle;
        }
    }
    let Some(group_index) = lower.checked_sub(1) else {
        return Ok(None);
    };
    let group = Group::read(index, group_index)?;
    ensure!(
        group.size > 0
            && u64::from(group.size) <= (MAX_RECORD + RECORD_PREFIX as usize) as u64
            && group
                .offset
                .checked_add(u64::from(group.size))
                .is_some_and(|end| end <= size),
        "invalid Cell proof group range"
    );
    let mut bytes = vec![0; group.size as usize];
    image.read_exact_at(&mut bytes, group.offset)?;
    let mut reader = Cursor::new(bytes.as_slice());
    let mut leaves = Vec::new();
    let mut record = None;
    while (reader.position() as usize) < bytes.len() {
        let mut prefix = [0; RECORD_PREFIX as usize];
        reader.read_exact(&mut prefix)?;
        let key_size = u32::from_le_bytes(prefix[..4].try_into()?);
        let value_size = u32::from_le_bytes(prefix[4..8].try_into()?) as usize;
        ensure!(
            key_size as usize == 1 + POINT && prefix[8] == b'c' && leaves.len() < GROUP_RECORDS,
            "invalid Cell record in checkpoint group"
        );
        let start = reader.position() as usize;
        let value = bytes
            .get(start..start + value_size)
            .context("truncated Cell record")?;
        if &prefix[9..] == point.as_slice() {
            record = Some((leaves.len() as u64, value));
        }
        leaves.push(leaf(&[&prefix[9..], value]));
        reader.set_position((start + value_size) as u64);
    }
    let mut nodes = Vec::with_capacity(leaves.len() * 2 - 1);
    ensure!(
        tree(&mut nodes, &leaves) == group.root,
        "Cell proof group differs from its index"
    );
    let Some((leaf_index, value)) = record else {
        return Ok(None);
    };
    Ok(Some(json!({
        "record": STANDARD.encode(value),
        "out_point": hex::encode(point.as_slice()),
        "cell_path": proof(leaf_index, leaves.len() as u64, |node| Ok(nodes[node as usize]))?,
        "group_path": proof(group_index, count, load)?,
        "group_root": hex::encode(group.root),
    })))
}
