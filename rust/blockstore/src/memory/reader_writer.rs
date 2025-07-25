use std::ops::RangeBounds;

use super::{
    super::{BlockfileError, Key, Value},
    storage::{Readable, Storage, StorageBuilder, StorageManager, Writeable},
};
use crate::key::{InvalidKeyConversion, KeyWrapper};
use chroma_error::ChromaError;

#[derive(Clone)]
pub struct MemoryBlockfileWriter {
    builder: StorageBuilder,
    storage_manager: StorageManager,
    id: uuid::Uuid,
}

pub struct MemoryBlockfileFlusher {
    id: uuid::Uuid,
}

impl MemoryBlockfileFlusher {
    pub(crate) fn id(&self) -> uuid::Uuid {
        self.id
    }

    pub(crate) fn prefix_path(&self) -> &str {
        ""
    }
}

impl MemoryBlockfileWriter {
    pub(super) fn new(storage_manager: StorageManager) -> Self {
        let builder = storage_manager.create();
        let id = builder.id;
        Self {
            builder,
            storage_manager,
            id,
        }
    }

    pub(crate) fn commit(&self) -> Result<MemoryBlockfileFlusher, Box<dyn ChromaError>> {
        self.storage_manager.commit(self.builder.id);
        Ok(MemoryBlockfileFlusher { id: self.id })
    }

    pub(crate) fn set<K: Key + Into<KeyWrapper>, V: Value + Writeable>(
        &self,
        prefix: &str,
        key: K,
        value: V,
    ) -> Result<(), Box<dyn ChromaError>> {
        let key = key.clone().into();
        V::write_to_storage(prefix, key, value, &self.builder);
        Ok(())
    }

    pub(crate) fn delete<K: Key + Into<KeyWrapper>, V: Value + Writeable>(
        &self,
        prefix: &str,
        key: K,
    ) -> Result<(), Box<dyn ChromaError>> {
        let key = key.into();
        V::remove_from_storage(prefix, key, &self.builder);
        Ok(())
    }

    pub(crate) fn id(&self) -> uuid::Uuid {
        self.id
    }
}

#[derive(Clone)]
pub struct MemoryBlockfileReader<K: Key, V: Value> {
    _storage_manager: StorageManager,
    storage: Storage,
    marker: std::marker::PhantomData<(K, V)>,
}

impl<
        'storage,
        K: Key + Into<KeyWrapper> + TryFrom<&'storage KeyWrapper, Error = InvalidKeyConversion>,
        V: Value + Readable<'storage>,
    > MemoryBlockfileReader<K, V>
{
    pub(crate) fn open(id: uuid::Uuid, storage_manager: StorageManager) -> Self {
        let storage = storage_manager.get(id).unwrap();
        Self {
            _storage_manager: storage_manager,
            storage,
            marker: std::marker::PhantomData,
        }
    }

    pub(crate) fn get(
        &'storage self,
        prefix: &str,
        key: K,
    ) -> Result<Option<V>, Box<dyn ChromaError>> {
        let key = key.into();
        Ok(V::read_from_storage(prefix, key, &self.storage))
    }

    pub(crate) fn get_range_iter<'prefix, PrefixRange, KeyRange>(
        &'storage self,
        prefix_range: PrefixRange,
        key_range: KeyRange,
    ) -> Result<impl Iterator<Item = (&'storage str, K, V)> + 'storage, Box<dyn ChromaError>>
    where
        PrefixRange: RangeBounds<&'prefix str>,
        KeyRange: RangeBounds<K>,
    {
        let values = V::read_range_from_storage(
            prefix_range,
            (
                key_range.start_bound().map(|k| k.clone().into()),
                key_range.end_bound().map(|k| k.clone().into()),
            ),
            &self.storage,
        );
        if values.is_empty() {
            return Err(Box::new(BlockfileError::NotFoundError));
        }

        Ok(values
            .into_iter()
            .map(|(key, value)| (key.prefix.as_str(), K::try_from(&key.key).unwrap(), value)))
    }

    pub(crate) fn count(&self) -> Result<usize, Box<dyn ChromaError>> {
        V::count(&self.storage)
    }

    pub(crate) fn contains(&'storage self, prefix: &str, key: K) -> bool {
        V::contains(prefix, key.into(), &self.storage)
    }

    pub(crate) fn id(&self) -> uuid::Uuid {
        self.storage.id
    }

    pub(crate) fn rank(&'storage self, prefix: &'storage str, key: K) -> usize {
        V::rank(prefix, key.into(), &self.storage)
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Bound;

    use super::*;
    use chroma_types::{Chunk, DataRecord, LogRecord, Operation, OperationRecord};

    #[test]
    fn test_blockfile_string() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", "key1", "value1".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<&str, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let value = reader.get("prefix", "key1").unwrap().unwrap();
        assert_eq!(value, "value1");
    }

    #[test]
    fn test_string_key_rbm_value() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let mut bitmap = roaring::RoaringBitmap::new();
        bitmap.insert(1);
        bitmap.insert(2);
        bitmap.insert(3);
        let _ = writer.set("prefix", "bitmap1", bitmap);
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<&str, roaring::RoaringBitmap> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let value = reader.get("prefix", "bitmap1").unwrap().unwrap();
        assert!(value.contains(1));
        assert!(value.contains(2));
        assert!(value.contains(3));
    }

    #[test]
    fn test_string_key_data_record_value() {
        // TODO: cleanup this test
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let id = uuid::Uuid::new_v4().to_string();
        let embedding = vec![1.0, 2.0, 3.0];
        let record = DataRecord {
            id: &id,
            embedding: &embedding,
            metadata: None,
            document: None,
        };

        let data = vec![
            LogRecord {
                log_offset: 1,
                record: OperationRecord {
                    id: "embedding_id_1".to_string(),
                    embedding: Some(vec![1.0, 2.0, 3.0]),
                    encoding: None,
                    metadata: None,
                    document: None,
                    operation: Operation::Add,
                },
            },
            LogRecord {
                log_offset: 2,
                record: OperationRecord {
                    id: "embedding_id_2".to_string(),
                    embedding: Some(vec![4.0, 5.0, 6.0]),
                    encoding: None,
                    metadata: None,
                    document: None,
                    operation: Operation::Add,
                },
            },
            LogRecord {
                log_offset: 3,
                record: OperationRecord {
                    id: "embedding_id_3".to_string(),
                    embedding: Some(vec![7.0, 8.0, 9.0]),
                    encoding: None,
                    metadata: None,
                    document: None,
                    operation: Operation::Add,
                },
            },
        ];
        let data: Chunk<LogRecord> = Chunk::new(data.into());
        let data_records = data
            .iter()
            .map(|record| DataRecord {
                id: &record.0.record.id,
                embedding: record.0.record.embedding.as_ref().unwrap(),
                document: None,
                metadata: None,
            })
            .collect::<Vec<_>>();
        let id = writer.id();
        let _ = writer.set("prefix", "key1", &record);
        for record in data_records {
            let _ = writer.set("prefix", record.id, &record);
        }

        writer.commit().unwrap();

        let reader: MemoryBlockfileReader<&str, DataRecord> =
            MemoryBlockfileReader::open(id, storage_manager);
        let record = reader.get("prefix", "embedding_id_1").unwrap().unwrap();
        assert_eq!(record.id, "embedding_id_1");
        assert_eq!(record.embedding, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn test_bool_key() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", true, "value1".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<bool, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let value = reader.get("prefix", true).unwrap();
        assert_eq!(value, Some("value1"));
    }

    #[test]
    fn test_u32_key() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1, "value1".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<u32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let value = reader.get("prefix", 1).unwrap();
        assert_eq!(value, Some("value1"));
    }

    #[test]
    fn test_float32_key() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1.0, "value1".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<f32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let value = reader.get("prefix", 1.0).unwrap();
        assert_eq!(value, Some("value1"));
    }

    #[test]
    fn test_get_by_prefix() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", "key1", "value1".to_string());
        let _ = writer.set("prefix", "key2", "value2".to_string());
        let _ = writer.set("different_prefix", "key3", "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<&str, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let range_iter = reader.get_range_iter("prefix"..="prefix", ..).unwrap();
        let values = range_iter.collect::<Vec<_>>();
        assert_eq!(values.len(), 2);
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == "key1" && *value == "value1"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == "key2" && *value == "value2"));
    }

    #[test]
    fn test_get_gt_int_none_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1, "value1".to_string());
        let _ = writer.set("prefix", 2, "value2".to_string());
        let _ = writer.set("prefix", 3, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<u32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let values =
            reader.get_range_iter("prefix"..="prefix", (Bound::Excluded(3), Bound::Unbounded));
        assert!(values.is_err());
    }

    #[test]
    fn test_get_gt_int_all_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1, "value1".to_string());
        let _ = writer.set("prefix", 2, "value2".to_string());
        let _ = writer.set("prefix", 3, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<u32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let range_iter = reader
            .get_range_iter("prefix"..="prefix", (Bound::Excluded(0), Bound::Unbounded))
            .unwrap();
        let values = range_iter.collect::<Vec<_>>();
        assert_eq!(values.len(), 3);
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 1 && *value == "value1"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 2 && *value == "value2"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 3 && *value == "value3"));
    }

    #[test]
    fn test_get_gt_int_some_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1, "value1".to_string());
        let _ = writer.set("prefix", 2, "value2".to_string());
        let _ = writer.set("prefix", 3, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<u32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let range_iter = reader
            .get_range_iter("prefix"..="prefix", (Bound::Excluded(1), Bound::Unbounded))
            .unwrap();
        let values = range_iter.collect::<Vec<_>>();
        assert_eq!(values.len(), 2);
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 2 && *value == "value2"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 3 && *value == "value3"));
    }

    #[test]
    fn test_get_gt_float_none_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1.0, "value1".to_string());
        let _ = writer.set("prefix", 2.0, "value2".to_string());
        let _ = writer.set("prefix", 3.0, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<f32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let values = reader.get_range_iter(
            "prefix"..="prefix",
            (Bound::Excluded(3.0), Bound::Unbounded),
        );
        assert!(values.is_err());
    }

    #[test]
    fn test_get_gt_float_all_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1.0, "value1".to_string());
        let _ = writer.set("prefix", 2.0, "value2".to_string());
        let _ = writer.set("prefix", 3.0, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<f32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let range_iter = reader
            .get_range_iter(
                "prefix"..="prefix",
                (Bound::Excluded(0.0), Bound::Unbounded),
            )
            .unwrap();
        let values = range_iter.collect::<Vec<_>>();
        assert_eq!(values.len(), 3);
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 1.0 && *value == "value1"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 2.0 && *value == "value2"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 3.0 && *value == "value3"));
    }

    #[test]
    fn test_get_gt_float_some_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1.0, "value1".to_string());
        let _ = writer.set("prefix", 2.0, "value2".to_string());
        let _ = writer.set("prefix", 3.0, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<f32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let range_iter = reader
            .get_range_iter(
                "prefix"..="prefix",
                (Bound::Excluded(1.0), Bound::Unbounded),
            )
            .unwrap();
        let values = range_iter.collect::<Vec<_>>();
        assert_eq!(values.len(), 2);
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 2.0 && *value == "value2"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 3.0 && *value == "value3"));
    }

    #[test]
    fn test_get_gte_int_none_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1, "value1".to_string());
        let _ = writer.set("prefix", 2, "value2".to_string());
        let _ = writer.set("prefix", 3, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<u32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let values = reader.get_range_iter("prefix"..="prefix", 4..);
        assert!(values.is_err());
    }

    #[test]
    fn test_get_gte_int_all_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1, "value1".to_string());
        let _ = writer.set("prefix", 2, "value2".to_string());
        let _ = writer.set("prefix", 3, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<u32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let range_iter = reader.get_range_iter("prefix"..="prefix", 1..).unwrap();
        let values = range_iter.collect::<Vec<_>>();
        assert_eq!(values.len(), 3);
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 1 && *value == "value1"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 2 && *value == "value2"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 3 && *value == "value3"));
    }

    #[test]
    fn test_get_gte_int_some_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1, "value1".to_string());
        let _ = writer.set("prefix", 2, "value2".to_string());
        let _ = writer.set("prefix", 3, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<u32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let range_iter = reader.get_range_iter("prefix"..="prefix", 2..).unwrap();
        let values = range_iter.collect::<Vec<_>>();
        assert_eq!(values.len(), 2);
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 2 && *value == "value2"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 3 && *value == "value3"));
    }

    #[test]
    fn test_get_gte_float_none_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1.0, "value1".to_string());
        let _ = writer.set("prefix", 2.0, "value2".to_string());
        let _ = writer.set("prefix", 3.0, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<f32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let values = reader.get_range_iter("prefix"..="prefix", 3.5..);
        assert!(values.is_err());
    }

    #[test]
    fn test_get_gte_float_all_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1.0, "value1".to_string());
        let _ = writer.set("prefix", 2.0, "value2".to_string());
        let _ = writer.set("prefix", 3.0, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<f32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let range_iter = reader.get_range_iter("prefix"..="prefix", 0.5..).unwrap();
        let values = range_iter.collect::<Vec<_>>();
        assert_eq!(values.len(), 3);
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 1.0 && *value == "value1"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 2.0 && *value == "value2"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 3.0 && *value == "value3"));
    }

    #[test]
    fn test_get_gte_float_some_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1.0, "value1".to_string());
        let _ = writer.set("prefix", 2.0, "value2".to_string());
        let _ = writer.set("prefix", 3.0, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<f32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let range_iter = reader.get_range_iter("prefix"..="prefix", 1.5..).unwrap();
        let values = range_iter.collect::<Vec<_>>();
        assert_eq!(values.len(), 2);
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 2.0 && *value == "value2"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 3.0 && *value == "value3"));
    }

    #[test]
    fn test_get_lt_int_none_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1, "value1".to_string());
        let _ = writer.set("prefix", 2, "value2".to_string());
        let _ = writer.set("prefix", 3, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<u32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let values = reader.get_range_iter("prefix"..="prefix", ..1);
        assert!(values.is_err());
    }

    #[test]
    fn test_get_lt_int_all_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1, "value1".to_string());
        let _ = writer.set("prefix", 2, "value2".to_string());
        let _ = writer.set("prefix", 3, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<u32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let range_iter = reader.get_range_iter("prefix"..="prefix", ..4).unwrap();
        let values = range_iter.collect::<Vec<_>>();
        assert_eq!(values.len(), 3);
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 1 && *value == "value1"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 2 && *value == "value2"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 3 && *value == "value3"));
    }

    #[test]
    fn test_get_lt_int_some_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1, "value1".to_string());
        let _ = writer.set("prefix", 2, "value2".to_string());
        let _ = writer.set("prefix", 3, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<u32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let range_iter = reader.get_range_iter("prefix"..="prefix", ..3).unwrap();
        let values = range_iter.collect::<Vec<_>>();
        assert_eq!(values.len(), 2);
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 1 && *value == "value1"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 2 && *value == "value2"));
    }

    #[test]
    fn test_get_lt_float_none_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1.0, "value1".to_string());
        let _ = writer.set("prefix", 2.0, "value2".to_string());
        let _ = writer.set("prefix", 3.0, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<f32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let values = reader.get_range_iter("prefix"..="prefix", ..0.5);
        assert!(values.is_err());
    }

    #[test]
    fn test_get_lt_float_all_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1.0, "value1".to_string());
        let _ = writer.set("prefix", 2.0, "value2".to_string());
        let _ = writer.set("prefix", 3.0, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<f32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let range_iter = reader.get_range_iter("prefix"..="prefix", ..3.5).unwrap();
        let values = range_iter.collect::<Vec<_>>();
        assert_eq!(values.len(), 3);
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 1.0 && *value == "value1"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 2.0 && *value == "value2"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 3.0 && *value == "value3"));
    }

    #[test]
    fn test_get_lt_float_some_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1.0, "value1".to_string());
        let _ = writer.set("prefix", 2.0, "value2".to_string());
        let _ = writer.set("prefix", 3.0, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<f32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let range_iter = reader.get_range_iter("prefix"..="prefix", ..2.5).unwrap();
        let values = range_iter.collect::<Vec<_>>();
        assert_eq!(values.len(), 2);
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 1.0 && *value == "value1"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 2.0 && *value == "value2"));
    }

    #[test]
    fn test_get_lte_int_none_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1, "value1".to_string());
        let _ = writer.set("prefix", 2, "value2".to_string());
        let _ = writer.set("prefix", 3, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<u32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let values = reader.get_range_iter("prefix"..="prefix", ..=0);
        assert!(values.is_err());
    }

    #[test]
    fn test_get_lte_int_all_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1, "value1".to_string());
        let _ = writer.set("prefix", 2, "value2".to_string());
        let _ = writer.set("prefix", 3, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<u32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let range_iter = reader.get_range_iter("prefix"..="prefix", ..=3).unwrap();
        let values = range_iter.collect::<Vec<_>>();
        assert_eq!(values.len(), 3);
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 1 && *value == "value1"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 2 && *value == "value2"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 3 && *value == "value3"));
    }

    #[test]
    fn test_get_lte_int_some_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1, "value1".to_string());
        let _ = writer.set("prefix", 2, "value2".to_string());
        let _ = writer.set("prefix", 3, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<u32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let range_iter = reader.get_range_iter("prefix"..="prefix", ..=2).unwrap();
        let values = range_iter.collect::<Vec<_>>();
        assert_eq!(values.len(), 2);
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 1 && *value == "value1"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 2 && *value == "value2"));
    }

    #[test]
    fn test_get_lte_float_none_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1.0, "value1".to_string());
        let _ = writer.set("prefix", 2.0, "value2".to_string());
        let _ = writer.set("prefix", 3.0, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<f32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let values = reader.get_range_iter("prefix"..="prefix", ..=0.5);
        assert!(values.is_err());
    }

    #[test]
    fn test_get_lte_float_all_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1.0, "value1".to_string());
        let _ = writer.set("prefix", 2.0, "value2".to_string());
        let _ = writer.set("prefix", 3.0, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<f32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let range_iter = reader.get_range_iter("prefix"..="prefix", ..=3.0).unwrap();
        let values = range_iter.collect::<Vec<_>>();
        assert_eq!(values.len(), 3);
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 1.0 && *value == "value1"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 2.0 && *value == "value2"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 3.0 && *value == "value3"));
    }

    #[test]
    fn test_get_lte_float_some_returned() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let _ = writer.set("prefix", 1.0, "value1".to_string());
        let _ = writer.set("prefix", 2.0, "value2".to_string());
        let _ = writer.set("prefix", 3.0, "value3".to_string());
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<f32, &str> =
            MemoryBlockfileReader::open(writer.id, storage_manager);
        let range_iter = reader.get_range_iter("prefix"..="prefix", ..=2.0).unwrap();
        let values = range_iter.collect::<Vec<_>>();
        assert_eq!(values.len(), 2);
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 1.0 && *value == "value1"));
        assert!(values
            .iter()
            .any(|(_, key, value)| *key == 2.0 && *value == "value2"));
    }

    #[test]
    fn test_delete() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let id = writer.id;
        let _ = writer.set("prefix", "key1", "value1".to_string());
        let _ = writer.set("prefix", "key2", "value2".to_string());
        let _ = writer.set("different_prefix", "key3", "value3".to_string());
        // delete
        let _ = writer.delete::<&str, String>("prefix", "key1");
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<&str, &str> =
            MemoryBlockfileReader::open(id, storage_manager.clone());
        let key_2 = reader.get("prefix", "key2").unwrap();
        assert_eq!(key_2, Some("value2"));
        let key_3 = reader.get("different_prefix", "key3").unwrap();
        assert_eq!(key_3, Some("value3"));

        let key_1 = reader.get("prefix", "key1");
        assert!(matches!(key_1, Ok(None)));
    }

    #[tokio::test]
    async fn test_rank() {
        let storage_manager = StorageManager::new();
        let writer = MemoryBlockfileWriter::new(storage_manager.clone());
        let id = writer.id;

        let n = 2000;
        for i in 0..n {
            let key = format!("key{:04}", i);
            let value = format!("value{:04}", i);
            let _ = writer.set("prefix", key.as_str(), value.to_string());
        }
        let _ = writer.commit();

        let reader: MemoryBlockfileReader<&str, &str> =
            MemoryBlockfileReader::open(id, storage_manager.clone());
        for i in 0..n {
            let rank_key = format!("key{:04}", i);
            let rank = MemoryBlockfileReader::<&str, &str>::rank(&reader, "prefix", &rank_key);
            assert_eq!(rank, i);
        }
    }
}
