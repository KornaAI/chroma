use crate::key::CompositeKey;
use chroma_error::ChromaError;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Debug;
use std::ops::{Bound, RangeBounds};
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

// ============
// Sparse Index Delimeter
// ============

/// A sentinel blockfilekey wrapper to represent the start blocks range
/// # Note
/// The start key is used to represent the first block in the sparse index, this makes
/// it easier to handle the case where the first block is split into two and also makes
/// determining the target block for a given key easier
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) enum SparseIndexDelimiter {
    Start,
    Key(CompositeKey),
}

impl PartialEq for SparseIndexDelimiter {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (SparseIndexDelimiter::Start, SparseIndexDelimiter::Start) => true,
            (SparseIndexDelimiter::Key(k1), SparseIndexDelimiter::Key(k2)) => k1 == k2,
            _ => false,
        }
    }
}

impl Eq for SparseIndexDelimiter {}

impl PartialOrd for SparseIndexDelimiter {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SparseIndexDelimiter {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (SparseIndexDelimiter::Start, SparseIndexDelimiter::Start) => std::cmp::Ordering::Equal,
            (SparseIndexDelimiter::Start, SparseIndexDelimiter::Key(_)) => std::cmp::Ordering::Less,
            (SparseIndexDelimiter::Key(_), SparseIndexDelimiter::Start) => {
                std::cmp::Ordering::Greater
            }
            (SparseIndexDelimiter::Key(k1), SparseIndexDelimiter::Key(k2)) => k1.cmp(k2),
        }
    }
}

// ============
// Sparse Index Writer
// ============

#[derive(Clone)]
pub struct SparseIndexWriter {
    pub(super) data: Arc<Mutex<SparseIndexWriterData>>,
}

pub(super) struct SparseIndexWriterData {
    pub(super) forward: BTreeMap<SparseIndexDelimiter, Uuid>,
    reverse: HashMap<Uuid, SparseIndexDelimiter>,
    // The number of keys in each block in the sparse index.
    // This is not intended updated incrementally, and is only populated
    // at commit time of the blockfile.
    pub(super) counts: BTreeMap<SparseIndexDelimiter, u32>,
}

impl SparseIndexWriterData {
    pub(super) fn len(&self) -> usize {
        self.forward.len()
    }
}

#[derive(Error, Debug)]
pub enum AddError {
    #[error("Block id already exists in the sparse index")]
    BlockIdExists,
}

impl ChromaError for AddError {
    fn code(&self) -> chroma_error::ErrorCodes {
        match self {
            AddError::BlockIdExists => chroma_error::ErrorCodes::InvalidArgument,
        }
    }
}

#[derive(Error, Debug)]
pub enum SetCountError {
    #[error("Block id does not exist in the sparse index")]
    BlockIdDoesNotExist,
}

impl ChromaError for SetCountError {
    fn code(&self) -> chroma_error::ErrorCodes {
        match self {
            SetCountError::BlockIdDoesNotExist => chroma_error::ErrorCodes::InvalidArgument,
        }
    }
}

impl SparseIndexWriter {
    pub(crate) fn new(initial_block_id: Uuid) -> Self {
        let mut forward = BTreeMap::new();
        let mut reverse = HashMap::new();
        let counts = BTreeMap::new();

        forward.insert(SparseIndexDelimiter::Start, initial_block_id);
        reverse.insert(initial_block_id, SparseIndexDelimiter::Start);

        let data = SparseIndexWriterData {
            forward,
            reverse,
            counts,
        };

        Self {
            data: Arc::new(Mutex::new(data)),
        }
    }

    pub(super) fn apply_updates(
        &self,
        blocks_to_replace: Vec<(Uuid, Uuid)>,
        blocks_to_add: Vec<(CompositeKey, Uuid)>,
    ) -> Result<(), AddError> {
        let mut lock_guard = self.data.lock();
        for (old_block_id, new_block_id) in blocks_to_replace {
            if let Some(old_start_key) = lock_guard.reverse.remove(&old_block_id) {
                lock_guard.forward.remove(&old_start_key);
                lock_guard
                    .forward
                    .insert(old_start_key.clone(), new_block_id);
                lock_guard
                    .reverse
                    .insert(new_block_id, old_start_key.clone());
                let old_count = lock_guard
                    .counts
                    .remove(&old_start_key)
                    .expect("Invariant Violation, these maps are always in sync");
                lock_guard.counts.insert(old_start_key, old_count);
            }
        }

        for (start_key, block_id) in blocks_to_add {
            if lock_guard.reverse.contains_key(&block_id) {
                return Err(AddError::BlockIdExists);
            }
            lock_guard
                .forward
                .insert(SparseIndexDelimiter::Key(start_key.clone()), block_id);
            lock_guard
                .reverse
                .insert(block_id, SparseIndexDelimiter::Key(start_key));
        }
        Ok(())
    }

    pub(crate) fn add_block(
        &self,
        start_key: CompositeKey,
        block_id: Uuid,
    ) -> Result<(), AddError> {
        let mut data = self.data.lock();

        if data.reverse.contains_key(&block_id) {
            return Err(AddError::BlockIdExists);
        }

        data.forward
            .insert(SparseIndexDelimiter::Key(start_key.clone()), block_id);
        data.reverse
            .insert(block_id, SparseIndexDelimiter::Key(start_key));

        Ok(())
    }

    pub(super) fn replace_block(&self, old_block_id: Uuid, new_block_id: Uuid) {
        let mut data = self.data.lock();
        if let Some(old_start_key) = data.reverse.remove(&old_block_id) {
            data.forward.remove(&old_start_key);
            data.forward.insert(old_start_key.clone(), new_block_id);
            data.reverse.insert(new_block_id, old_start_key.clone());
            let old_count = data
                .counts
                .remove(&old_start_key)
                .expect("Invariant Violation, these maps are always in sync");
            data.counts.insert(old_start_key, old_count);
        }
    }

    /// Set the number of keys in a block in the sparse index.
    /// This is not intended to be updated incrementally, and is only populated
    /// at commit time of the blockfile.
    /// # Arguments
    /// * `block_id` - The block id to set the count for
    /// * `count` - The number of keys in the block
    pub(crate) fn set_count(&self, block_id: Uuid, count: u32) -> Result<(), SetCountError> {
        let mut data = self.data.lock();
        let start_key = data.reverse.get(&block_id);
        match start_key.cloned() {
            Some(start_key) => {
                data.counts.insert(start_key, count);
                Ok(())
            }
            None => Err(SetCountError::BlockIdDoesNotExist),
        }
    }

    pub(super) fn get_target_block_id(&self, search_key: &CompositeKey) -> Uuid {
        let data = self.data.lock();
        let forward = &data.forward;
        *get_target_block(search_key, forward)
    }

    pub(super) fn len(&self) -> usize {
        let data = self.data.lock();
        data.forward.len()
    }

    pub(super) fn remove_block(&self, block_id: &Uuid) -> bool {
        // We commit and flush an empty dummy block if the blockfile is empty.
        // It can happen that other indexes of the segment are not empty. In this case,
        // our segment open() logic breaks down since we only handle either
        // all indexes initialized or none at all but not other combinations.
        // We could argue that we should fix the readers to handle these cases
        // but this is simpler, easier and less error prone to do.
        let mut data = self.data.lock();
        let mut removed = false;
        if data.len() > 1 {
            if let Some(start_key) = data.reverse.remove(block_id) {
                data.forward.remove(&start_key);
                // data.counts is not guaranteed to be in sync with forward, so ignore the result if the key doesn't exist
                let _ = data.counts.remove(&start_key);
            }
            removed = true;
        }
        // It can happen that the sparse index does not contain
        // the start key after this sequence of operations,
        // for e.g. consider the following:
        // sparse_index: {start_key: block_id1, some_key: block_id2, some_other_key: block_id3}
        // If we delete block_id1 from the sparse index then it becomes
        // {some_key: block_id2, some_other_key: block_id3}
        // This should be changed to {start_key: block_id2, some_other_key: block_id3}
        self.correct_start_key(&mut data);
        removed
    }

    fn correct_start_key(&self, data: &mut SparseIndexWriterData) {
        if data.len() == 0 {
            return;
        }
        let key_copy;
        {
            let mut curr_iter = data.forward.iter();
            let (key, _) = curr_iter.nth(0).unwrap();
            if key == &SparseIndexDelimiter::Start {
                return;
            }
            key_copy = key.clone();
        }
        tracing::info!("Correcting start key of sparse index");
        if let Some(id) = data.forward.remove(&key_copy) {
            data.reverse.remove(&id);
            data.forward.insert(SparseIndexDelimiter::Start, id);
            data.reverse.insert(id, SparseIndexDelimiter::Start);
            // data.counts is not guaranteed to be in sync with forward
            if let Some(old_count) = data.counts.remove(&key_copy) {
                data.counts.insert(SparseIndexDelimiter::Start, old_count);
            }
        }
    }

    #[cfg(test)]
    fn to_reader(&self) -> Result<SparseIndexReader, ToReaderError> {
        let data = self.data.lock();
        if data.forward.len() != data.counts.len() {
            return Err(ToReaderError::CountsNotSet);
        }

        let zipped = data.forward.iter().zip(data.counts.iter());
        let new_forward = zipped.map(|((key, block_id), (_, count))| {
            (key.clone(), SparseIndexValue::new(*block_id, *count))
        });
        let new_forward = BTreeMap::from_iter(new_forward);
        Ok(SparseIndexReader::new(new_forward))
    }
}

impl Debug for SparseIndexWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SparseIndexWriter").finish()
    }
}

#[cfg(test)]
#[derive(Error, Debug)]
enum ToReaderError {
    #[error("Counts not set to be the same length as forward")]
    CountsNotSet,
}

// ============
// Sparse Index Reader
// ============

/// A sparse index is used by a Blockfile to map a range of keys to a block id
#[derive(Clone, Serialize, Deserialize)]
pub struct SparseIndexReader {
    pub(super) data: Arc<SparseIndexReaderData>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct SparseIndexReaderData {
    pub(super) forward: BTreeMap<SparseIndexDelimiter, SparseIndexValue>,
}

/// A value in the sparse index.
/// # Fields
/// * `id` - The block id that contains the keys in the range
/// * `count` - The number of keys in the block
#[derive(Serialize, Deserialize)]
pub(super) struct SparseIndexValue {
    pub(super) id: Uuid,
    pub(super) count: u32,
}

impl SparseIndexValue {
    pub(super) fn new(id: Uuid, count: u32) -> Self {
        Self { id, count }
    }
}

impl SparseIndexReader {
    pub(super) fn new(data: BTreeMap<SparseIndexDelimiter, SparseIndexValue>) -> Self {
        Self {
            data: Arc::new(SparseIndexReaderData { forward: data }),
        }
    }

    /// Get the number of keys in the sparse index
    /// Used in unit test
    #[allow(dead_code)]
    pub(super) fn len(&self) -> usize {
        self.data.forward.len()
    }

    /// Get the block id for a given key
    pub(super) fn get_target_block_id(&self, search_key: &CompositeKey) -> Uuid {
        let forward = &self.data.forward;
        get_target_block(search_key, forward).id
    }

    /// Get all the block ids that contain keys in the given input search keys
    pub(super) fn get_all_target_block_ids(&self, mut search_keys: Vec<CompositeKey>) -> Vec<Uuid> {
        // Sort so that we can search in one iteration.
        let data = &self.data;
        let forward = &data.forward;
        search_keys.sort();
        let mut result_uuids = Vec::new();
        let curr_iter = forward.iter();
        let mut next_iter = forward.iter().skip(1);
        let mut search_iter = search_keys.iter().peekable();
        for (curr_key, curr_block_value) in curr_iter {
            let search_key = match search_iter.peek() {
                Some(key) => SparseIndexDelimiter::Key((**key).clone()),
                None => {
                    break;
                }
            };
            if let Some((next_key, _)) = next_iter.next() {
                if search_key >= *curr_key && search_key < *next_key {
                    result_uuids.push(curr_block_value.id);
                    // Move forward all search keys that match this block.
                    search_iter.next();
                    while let Some(key) = search_iter.peek() {
                        let search_key = SparseIndexDelimiter::Key((**key).clone());
                        if search_key >= *curr_key && search_key < *next_key {
                            search_iter.next();
                        } else {
                            break;
                        }
                    }
                }
            } else {
                // last block. All the remaining keys should be satisfied by this.
                result_uuids.push(curr_block_value.id);
                break;
            }
        }
        result_uuids
    }

    pub(super) fn get_block_ids_for_prefixes(&self, mut prefixes: Vec<&str>) -> Vec<Uuid> {
        prefixes.sort();
        let mut result_uuids = Vec::new();
        let block_start = self.data.forward.iter();
        let block_end = block_start
            .clone()
            .skip(1)
            .map(|(delim, _)| match delim {
                SparseIndexDelimiter::Start => {
                    unreachable!("The start delimiter should only appear in the first block")
                }
                SparseIndexDelimiter::Key(composite_key) => Some(composite_key.prefix.as_str()),
            })
            .chain([None]);
        let mut prefix_iter = prefixes.into_iter().peekable();
        for ((start_delim, block), end_prefix) in block_start.zip(block_end) {
            if let SparseIndexDelimiter::Key(CompositeKey {
                prefix: start_prefix,
                key: _,
            }) = start_delim
            {
                while let Some(&prefix) = prefix_iter.peek() {
                    if start_prefix.as_str() <= prefix {
                        break;
                    }
                    prefix_iter.next();
                }
            }
            if let Some(&prefix) = prefix_iter.peek() {
                if end_prefix.is_none() || end_prefix.is_some_and(|end_prefix| prefix <= end_prefix)
                {
                    result_uuids.push(block.id);
                }
            } else {
                break;
            }
        }
        result_uuids
    }

    pub(super) fn get_block_ids_range<'prefix, PrefixRange>(
        &self,
        prefix_range: PrefixRange,
    ) -> Vec<Uuid>
    where
        PrefixRange: RangeBounds<&'prefix str>,
    {
        let forward = &self.data.forward;

        // We do not materialize the last key of each block, so we must check the next block's start key to determine if the current block's end key is within the query range.
        let start_keys_offset_by_1_iter = forward
            .iter()
            .skip(1)
            .map(|(k, _)| match k {
                SparseIndexDelimiter::Start => {
                    panic!("Invariant violation. Sparse index is not valid.");
                }
                SparseIndexDelimiter::Key(k) => Some(k),
            })
            .chain(std::iter::once(None));

        forward
            .iter()
            .zip(start_keys_offset_by_1_iter)
            .map(|((start_key, block_uuid), end_key)| (block_uuid, start_key, end_key))
            .filter(|(_, block_start_delimiter, block_end_delimiter)| {
                // The block should be retained if and only if its prefix range overlaps with the given prefix range
                // The necessary and sufficient condition for range R1, R2 to overlap is MAX(R1.START, R2.START) <= MIN(R1.END, R2.END)
                let max_start_prefix = match block_start_delimiter {
                    SparseIndexDelimiter::Start => prefix_range.start_bound().cloned(),
                    SparseIndexDelimiter::Key(block_start_key) => {
                        let start_prefix = block_start_key.prefix.as_str();
                        match prefix_range.start_bound() {
                            Bound::Included(given_start_prefix)
                            | Bound::Excluded(given_start_prefix)
                                if given_start_prefix < &start_prefix =>
                            {
                                Bound::Included(start_prefix)
                            }
                            Bound::Unbounded => Bound::Included(start_prefix),
                            given_bound => given_bound.cloned(),
                        }
                    }
                };

                let min_end_prefix = match block_end_delimiter {
                    Some(block_end_key) => {
                        let end_prefix = block_end_key.prefix.as_str();
                        match prefix_range.end_bound() {
                            Bound::Included(given_end_prefix)
                            | Bound::Excluded(given_end_prefix)
                                if &end_prefix < given_end_prefix =>
                            {
                                Bound::Included(end_prefix)
                            }
                            Bound::Unbounded => Bound::Included(end_prefix),
                            given_bound => given_bound.cloned(),
                        }
                    }
                    None => prefix_range.end_bound().cloned(),
                };

                // Check whether max_start_prefix <= min_end_prefix
                match (max_start_prefix, min_end_prefix) {
                    (Bound::Included(start), Bound::Included(end)) => start <= end,
                    (Bound::Included(start), Bound::Excluded(end))
                    | (Bound::Excluded(start), Bound::Included(end))
                    | (Bound::Excluded(start), Bound::Excluded(end)) => start < end,
                    // At least one of these is unbounded.
                    _ => true,
                }
            })
            .map(|(sparse_index_value, _, _)| sparse_index_value.id)
            .collect()
    }

    /// Fork the sparse index to create a new sparse index
    /// with the same data as the current sparse index
    pub(super) fn fork(&self) -> SparseIndexWriter {
        let mut new_forward = BTreeMap::new();
        let mut new_reverse = HashMap::new();
        let mut new_counts = BTreeMap::new();
        let old_data = &self.data;
        let old_forward = &old_data.forward;
        for (key, curr_block_value) in old_forward.iter() {
            new_forward.insert(key.clone(), curr_block_value.id);
            new_reverse.insert(curr_block_value.id, key.clone());
            new_counts.insert(key.clone(), curr_block_value.count);
        }

        SparseIndexWriter {
            data: Arc::new(Mutex::new(SparseIndexWriterData {
                forward: new_forward,
                reverse: new_reverse,
                counts: new_counts,
            })),
        }
    }

    /// Check if the sparse index is valid by ensuring that the keys are in order
    pub(super) fn is_valid(&self) -> bool {
        let data = &self.data;
        let mut first = true;
        // Two pointer traversal to check if the keys are in order and that the start key is first
        let iter_slow = data.forward.iter();
        let mut iter_fast = data.forward.iter().skip(1);
        for (curr_key, _) in iter_slow {
            if first {
                if curr_key != &SparseIndexDelimiter::Start {
                    return false;
                }
                first = false;
            }
            if let Some((next_key, _)) = iter_fast.next() {
                if curr_key >= next_key {
                    return false;
                }
            }
        }
        true
    }
}

impl Debug for SparseIndexReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SparseIndexReader").finish()
    }
}

// Helper function to get the target block id for a given key
fn get_target_block<'data, T>(
    search_key: &CompositeKey,
    forward: &'data BTreeMap<SparseIndexDelimiter, T>,
) -> &'data T {
    match forward
        .range(..=SparseIndexDelimiter::Key(search_key.clone()))
        .next_back()
    {
        Some((_, data)) => data,
        None => {
            panic!("No blocks in the sparse index");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sparse_index() {
        let block_id_1 = uuid::Uuid::new_v4();
        let sparse_index = SparseIndexWriter::new(block_id_1);
        let mut blockfile_key = CompositeKey::new("prefix".to_string(), "a");
        sparse_index
            .set_count(block_id_1, 10)
            .expect("Set count should succeed");
        assert_eq!(sparse_index.get_target_block_id(&blockfile_key), block_id_1);

        blockfile_key = CompositeKey::new("prefix".to_string(), "b");
        assert_eq!(sparse_index.get_target_block_id(&blockfile_key), block_id_1);

        // Split the range into two blocks (start, c), and (c, end)
        let block_id_2 = uuid::Uuid::new_v4();
        blockfile_key = CompositeKey::new("prefix".to_string(), "c");
        sparse_index
            .add_block(blockfile_key.clone(), block_id_2)
            .expect("No error");
        sparse_index
            .set_count(block_id_2, 20)
            .expect("Set count should succeed");
        assert_eq!(sparse_index.get_target_block_id(&blockfile_key), block_id_2);

        // d should fall into the second block
        blockfile_key = CompositeKey::new("prefix".to_string(), "d");
        assert_eq!(sparse_index.get_target_block_id(&blockfile_key), block_id_2);

        // Split the second block into (c, f) and (f, end)
        let block_id_3 = uuid::Uuid::new_v4();
        blockfile_key = CompositeKey::new("prefix".to_string(), "f");
        sparse_index
            .add_block(blockfile_key.clone(), block_id_3)
            .expect("No error");
        sparse_index
            .set_count(block_id_3, 30)
            .expect("Set count should succeed");
        assert_eq!(sparse_index.get_target_block_id(&blockfile_key), block_id_3);

        // g should fall into the third block
        blockfile_key = CompositeKey::new("prefix".to_string(), "g");
        assert_eq!(sparse_index.get_target_block_id(&blockfile_key), block_id_3);

        // b should fall into the first block
        blockfile_key = CompositeKey::new("prefix".to_string(), "b");
        assert_eq!(sparse_index.get_target_block_id(&blockfile_key), block_id_1);
    }

    #[test]
    fn test_count() {
        let ids = [
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        ];
        let keys = [
            CompositeKey::new("prefix".to_string(), "a"),
            CompositeKey::new("prefix".to_string(), "c"),
            CompositeKey::new("prefix".to_string(), "e"),
        ];
        let counts = [10, 20, 30];

        let sparse_index = SparseIndexWriter::new(ids[0]);
        sparse_index
            .set_count(ids[0], counts[0])
            .expect("Set count should succeed");
        sparse_index
            .add_block(keys[1].clone(), ids[1])
            .expect("No error");
        sparse_index
            .set_count(ids[1], counts[1])
            .expect("Set count should succeed");
        sparse_index
            .add_block(keys[2].clone(), ids[2])
            .expect("No error");
        sparse_index
            .set_count(ids[2], counts[2])
            .expect("Set count should succeed");

        // Check that the counts are set correctly
        assert_eq!(sparse_index.data.lock().counts.len(), 3);
        for i in 0..ids.len() {
            let target_key = match i {
                0 => SparseIndexDelimiter::Start,
                _ => SparseIndexDelimiter::Key(keys[i].clone()),
            };
            assert_eq!(
                sparse_index.data.lock().counts.get(&target_key).unwrap(),
                &counts[i]
            );
        }

        // Check that we can't insert count for block not in map
        let non_existent_id = uuid::Uuid::new_v4();
        let result = sparse_index.set_count(non_existent_id, 10);
        assert!(matches!(result, Err(SetCountError::BlockIdDoesNotExist)));
    }

    #[test]
    fn test_to_reader() {
        let block_id_0 = uuid::Uuid::new_v4();

        // Add an initial block to the sparse index
        let sparse_index = SparseIndexWriter::new(block_id_0);
        sparse_index
            .set_count(block_id_0, 5)
            .expect("Set count should succeed");

        // Add some more blocks
        let blockfile_key = CompositeKey::new("prefix".to_string(), "a");
        let block_id_1 = uuid::Uuid::new_v4();
        sparse_index
            .add_block(blockfile_key.clone(), block_id_1)
            .expect("No error");
        sparse_index
            .set_count(block_id_1, 10)
            .expect("Set count should succeed");

        let blockfile_key = CompositeKey::new("prefix".to_string(), "c");
        let block_id_2 = uuid::Uuid::new_v4();
        sparse_index
            .add_block(blockfile_key.clone(), block_id_2)
            .expect("No error");
        sparse_index
            .set_count(block_id_2, 20)
            .expect("Set count should succeed");

        let new_sparse_index = sparse_index.to_reader().expect("Conversion should succeed");
        let old_data = sparse_index.data.lock();

        assert_eq!(old_data.forward.len(), old_data.reverse.len());
        for (old_key, old_block_id) in old_data.forward.iter() {
            let new_block_id = old_data.forward.get(old_key).unwrap();
            assert_eq!(old_block_id, new_block_id);
        }

        // Test fork for reverse map
        let forked_sparse_index = new_sparse_index.fork();
        let forked_data = forked_sparse_index.data.lock();
        assert_eq!(old_data.reverse.len(), forked_data.reverse.len());
        for (old_block_id, old_key) in old_data.reverse.iter() {
            let new_key = forked_data.reverse.get(old_block_id).unwrap();
            assert_eq!(old_key, new_key);
        }
    }

    #[test]
    fn test_get_all_block_ids() {
        let block_id_0 = uuid::Uuid::new_v4();
        let writer = SparseIndexWriter::new(block_id_0);
        writer
            .set_count(block_id_0, 5)
            .expect("Set count should succeed");
        let mut blockfile_key = CompositeKey::new("prefix".to_string(), "a");
        let block_id_1 = uuid::Uuid::new_v4();
        writer
            .add_block(blockfile_key.clone(), block_id_1)
            .expect("No error");
        writer
            .set_count(block_id_1, 10)
            .expect("Set count should succeed");

        // Split the range into two blocks (start, c), and (c, end)
        let block_id_2 = uuid::Uuid::new_v4();
        blockfile_key = CompositeKey::new("prefix".to_string(), "d");
        writer
            .add_block(blockfile_key.clone(), block_id_2)
            .expect("No error");
        writer
            .set_count(block_id_2, 10)
            .expect("Set count should succeed");

        //

        // Split the second block into (c, f) and (f, end)
        let block_id_3 = uuid::Uuid::new_v4();
        blockfile_key = CompositeKey::new("prefix".to_string(), "f");
        writer
            .add_block(blockfile_key.clone(), block_id_3)
            .expect("No error");
        writer
            .set_count(block_id_3, 10)
            .expect("Set count should succeed");
        let composite_keys = vec![
            CompositeKey::new("prefix".to_string(), "b"),
            CompositeKey::new("prefix".to_string(), "c"),
            CompositeKey::new("prefix".to_string(), "d"),
            CompositeKey::new("prefix".to_string(), "e"),
        ];

        let reader = writer.to_reader().expect("Conversion should succeed");
        let blocks = reader.get_all_target_block_ids(composite_keys);
        assert_eq!(blocks.len(), 2);
        assert!(blocks.contains(&block_id_1));
        assert!(blocks.contains(&block_id_2));
        let composite_keys = vec![
            CompositeKey::new("prefix".to_string(), "f"),
            CompositeKey::new("prefix".to_string(), "g"),
            CompositeKey::new("prefix".to_string(), "h"),
            CompositeKey::new("prefix".to_string(), "i"),
        ];
        let blocks = reader.get_all_target_block_ids(composite_keys);
        assert_eq!(blocks.len(), 1);
        assert!(blocks.contains(&block_id_3));
    }

    #[test]
    fn test_get_block_ids_range() {
        let block_id_0 = uuid::Uuid::new_v4();
        let writer = SparseIndexWriter::new(block_id_0);
        writer
            .set_count(block_id_0, 5)
            .expect("Set count should succeed");
        let mut blockfile_key = CompositeKey::new("a".to_string(), "a");
        let block_id_1 = uuid::Uuid::new_v4();
        writer
            .add_block(blockfile_key.clone(), block_id_1)
            .expect("No error");
        writer
            .set_count(block_id_1, 10)
            .expect("Set count should succeed");

        let block_id_2 = uuid::Uuid::new_v4();
        blockfile_key = CompositeKey::new("a".to_string(), "c");
        writer
            .add_block(blockfile_key.clone(), block_id_2)
            .expect("No error");
        writer
            .set_count(block_id_2, 10)
            .expect("Set count should succeed");

        let block_id_3 = uuid::Uuid::new_v4();
        blockfile_key = CompositeKey::new("b".to_string(), "a");
        writer
            .add_block(blockfile_key.clone(), block_id_3)
            .expect("No error");
        writer
            .set_count(block_id_3, 10)
            .expect("Set count should succeed");

        let block_id_4 = uuid::Uuid::new_v4();
        blockfile_key = CompositeKey::new("b".to_string(), "f");
        writer
            .add_block(blockfile_key.clone(), block_id_4)
            .expect("No error");
        writer
            .set_count(block_id_4, 10)
            .expect("Set count should succeed");

        let block_id_5 = uuid::Uuid::new_v4();
        blockfile_key = CompositeKey::new("c".to_string(), "n");
        writer
            .add_block(blockfile_key.clone(), block_id_5)
            .expect("No error");
        writer
            .set_count(block_id_5, 10)
            .expect("Set count should succeed");

        let block_id_6 = uuid::Uuid::new_v4();
        blockfile_key = CompositeKey::new("d".to_string(), "x");
        writer
            .add_block(blockfile_key.clone(), block_id_6)
            .expect("No error");
        writer
            .set_count(block_id_6, 10)
            .expect("Set count should succeed");

        let reader = writer.to_reader().expect("Conversion should succeed");
        let blocks = reader.get_block_ids_range(..);
        assert_eq!(
            blocks,
            vec![
                block_id_0, block_id_1, block_id_2, block_id_3, block_id_4, block_id_5, block_id_6
            ]
        );

        let blocks = reader.get_block_ids_range(.."a");
        assert_eq!(blocks, vec![block_id_0]);

        let blocks = reader.get_block_ids_range(..="a");
        assert_eq!(blocks, vec![block_id_0, block_id_1, block_id_2]);

        let blocks = reader.get_block_ids_range("b"..="c");
        assert_eq!(blocks, vec![block_id_2, block_id_3, block_id_4, block_id_5]);

        let blocks = reader.get_block_ids_range("c"..);
        assert_eq!(blocks, vec![block_id_4, block_id_5, block_id_6]);
    }

    #[test]
    fn test_serde() {
        let ids = [uuid::Uuid::new_v4(), uuid::Uuid::new_v4()];
        let counts = [10, 20];
        let keys = [
            CompositeKey::new("prefix".to_string(), "a"),
            CompositeKey::new("prefix".to_string(), "c"),
        ];

        let sparse_index = SparseIndexWriter::new(ids[0]);
        sparse_index
            .set_count(ids[0], counts[0])
            .expect("Set count should succeed");

        // Split the range into two blocks (start, c), and (c, end)
        sparse_index
            .add_block(keys[1].clone(), ids[1])
            .expect("No error");
        sparse_index
            .set_count(ids[1], counts[1])
            .expect("Set count should succeed");

        let reader = sparse_index.to_reader().expect("Conversion should succeed");

        let serialized = bincode::serialize(&reader).unwrap();
        let deserialized: SparseIndexReader = bincode::deserialize(&serialized).unwrap();

        let old_data = sparse_index.data.lock();
        let new_data = deserialized.data;
        for (key, block_id) in old_data.forward.iter() {
            assert_eq!(new_data.forward.get(key).unwrap().id, *block_id);
        }

        for i in 0..ids.len() {
            let target_key = match i {
                0 => SparseIndexDelimiter::Start,
                _ => SparseIndexDelimiter::Key(keys[i].clone()),
            };
            assert_eq!(new_data.forward.get(&target_key).unwrap().count, counts[i]);
            assert_eq!(new_data.forward.get(&target_key).unwrap().id, ids[i]);
        }
    }
}
