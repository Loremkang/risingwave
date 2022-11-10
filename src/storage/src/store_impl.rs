// Copyright 2022 Singularity Data
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::Bound;
use std::fmt::Debug;
use std::future::Future;
use std::ops::Deref;
use std::sync::Arc;

use bytes::Bytes;
use enum_as_inner::EnumAsInner;
use tracing::warn;
use risingwave_common::catalog::TableId;
use risingwave_common::config::StorageConfig;
use risingwave_common_service::observer_manager::RpcNotificationClient;
use risingwave_hummock_sdk::HummockReadEpoch;
use risingwave_object_store::object::{
    parse_local_object_store, parse_remote_object_store, ObjectStoreImpl,
};

use crate::error::StorageResult;
use crate::hummock::hummock_meta_client::MonitoredHummockMetaClient;
use crate::hummock::{
    HummockStorage, HummockStorageV1, SstableStore, TieredCache, TieredCacheMetricsBuilder,
};
use crate::memory::MemoryStateStore;
use crate::monitor::{MonitoredStateStore as Monitored, ObjectStoreMetrics, StateStoreMetrics};
use crate::storage_value::StorageValue;
use crate::store::{LocalStateStore, ReadOptions, StateStoreRead, StateStoreWrite, WriteOptions};
use crate::{StateStore, StateStoreIter};

/// The type erased [`StateStore`].
#[derive(Clone, EnumAsInner)]
pub enum StateStoreImpl {
    /// The Hummock state store, which operates on an S3-like service. URLs beginning with
    /// `hummock` will be automatically recognized as Hummock state store.
    ///
    /// Example URLs:
    ///
    /// * `hummock+s3://bucket`
    /// * `hummock+minio://KEY:SECRET@minio-ip:port`
    /// * `hummock+memory` (should only be used in 1 compute node mode)
    HummockStateStore(Monitored<VerifyStateStore<HummockStorage, MemoryStateStore>>),
    HummockStateStoreV1(Monitored<VerifyStateStore<HummockStorageV1, MemoryStateStore>>),
    /// In-memory B-Tree state store. Should only be used in unit and integration tests. If you
    /// want speed up e2e test, you should use Hummock in-memory mode instead. Also, this state
    /// store misses some critical implementation to ensure the correctness of persisting streaming
    /// state. (e.g., no read_epoch support, no async checkpoint)
    MemoryStateStore(Monitored<MemoryStateStore>),
}

impl StateStoreImpl {
    pub fn shared_in_memory_store(state_store_metrics: Arc<StateStoreMetrics>) -> Self {
        Self::MemoryStateStore(MemoryStateStore::shared().monitored(state_store_metrics))
    }

    pub fn for_test() -> Self {
        StateStoreImpl::MemoryStateStore(
            MemoryStateStore::new().monitored(Arc::new(StateStoreMetrics::unused())),
        )
    }
}

impl Debug for StateStoreImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateStoreImpl::HummockStateStore(_) => write!(f, "HummockStateStore"),
            StateStoreImpl::HummockStateStoreV1(_) => write!(f, "HummockStateStoreV1"),
            StateStoreImpl::MemoryStateStore(_) => write!(f, "MemoryStateStore"),
        }
    }
}

#[macro_export]
macro_rules! dispatch_state_store {
    ($impl:expr, $store:ident, $body:tt) => {{
        use $crate::store_impl::StateStoreImpl;

        match $impl {
            StateStoreImpl::MemoryStateStore($store) => {
                // WARNING: don't change this. Enabling memory backend will cause monomorphization
                // explosion and thus slow compile time in release mode.
                #[cfg(debug_assertions)]
                {
                    $body
                }
                #[cfg(not(debug_assertions))]
                {
                    let _store = $store;
                    unimplemented!("memory state store should never be used in release mode");
                }
            }

            StateStoreImpl::HummockStateStore($store) => $body,

            StateStoreImpl::HummockStateStoreV1($store) => $body,
        }
    }};
}

use crate::store::{
    EmptyFutureTrait, GetFutureTrait, IngestBatchFutureTrait, IterFutureTrait, NextFutureTrait,
    SyncFutureTrait,
};

fn assert_result_eq<Item: PartialEq + Debug, E>(
    first: &std::result::Result<Item, E>,
    second: &std::result::Result<Item, E>,
) {
    match (first, second) {
        (Ok(first), Ok(second)) => {
            if first != second {
                warn!("result different: {:?} {:?}", first, second);
            }
            assert_eq!(first, second);
        }
        (Err(_), Err(_)) => {}
        _ => {
            warn!("one success and one failed");
            panic!("result not equal");
        },
    }
}

pub struct VerifyStateStore<A, E> {
    pub actual: A,
    pub expected: E,
}

impl<A: StateStoreIter<Item: PartialEq + Debug>, E: StateStoreIter<Item = A::Item>> StateStoreIter
    for VerifyStateStore<A, E>
{
    type Item = A::Item;

    type NextFuture<'a> = impl NextFutureTrait<'a, A::Item>;

    fn next(&mut self) -> Self::NextFuture<'_> {
        async {
            let actual = self.actual.next().await;
            let expected = self.expected.next().await;
            assert_result_eq(&actual, &expected);
            actual
        }
    }
}

impl<A: StateStoreRead, E: StateStoreRead> StateStoreRead for VerifyStateStore<A, E> {
    type Iter = VerifyStateStore<A::Iter, E::Iter>;

    define_state_store_read_associated_type!();

    fn get<'a>(
        &'a self,
        key: &'a [u8],
        epoch: u64,
        read_options: ReadOptions,
    ) -> Self::GetFuture<'_> {
        async move {
            let actual = self.actual.get(key, epoch, read_options.clone()).await;
            let expected = self.expected.get(key, epoch, read_options).await;
            assert_result_eq(&actual, &expected);
            actual
        }
    }

    fn iter(
        &self,
        key_range: (Bound<Vec<u8>>, Bound<Vec<u8>>),
        epoch: u64,
        read_options: ReadOptions,
    ) -> Self::IterFuture<'_> {
        async move {
            let actual = self
                .actual
                .iter(key_range.clone(), epoch, read_options.clone())
                .await?;
            let expected = self.expected.iter(key_range, epoch, read_options).await?;
            Ok(VerifyStateStore { actual, expected })
        }
    }
}

impl<A: StateStoreWrite, E: StateStoreWrite> StateStoreWrite for VerifyStateStore<A, E> {
    define_state_store_write_associated_type!();

    fn ingest_batch(
        &self,
        kv_pairs: Vec<(Bytes, StorageValue)>,
        delete_ranges: Vec<(Bytes, Bytes)>,
        write_options: WriteOptions,
    ) -> Self::IngestBatchFuture<'_> {
        async move {
            let actual = self
                .actual
                .ingest_batch(
                    kv_pairs.clone(),
                    delete_ranges.clone(),
                    write_options.clone(),
                )
                .await;
            let expected = self
                .expected
                .ingest_batch(kv_pairs, delete_ranges, write_options)
                .await;
            assert_eq!(actual.is_err(), expected.is_err());
            actual
        }
    }
}

impl<A: Clone, E: Clone> Clone for VerifyStateStore<A, E> {
    fn clone(&self) -> Self {
        Self {
            actual: self.actual.clone(),
            expected: self.expected.clone(),
        }
    }
}

impl<A: LocalStateStore, E: LocalStateStore> LocalStateStore for VerifyStateStore<A, E> {}

impl<A: StateStore, E: StateStore> StateStore for VerifyStateStore<A, E> {
    type Local = VerifyStateStore<A::Local, E::Local>;

    type NewLocalFuture<'a> = impl Future<Output = Self::Local> + Send;

    define_state_store_associated_type!();

    fn try_wait_epoch(&self, epoch: HummockReadEpoch) -> Self::WaitEpochFuture<'_> {
        self.actual.try_wait_epoch(epoch)
    }

    fn sync(&self, epoch: u64) -> Self::SyncFuture<'_> {
        self.actual.sync(epoch)
    }

    fn seal_epoch(&self, epoch: u64, is_checkpoint: bool) {
        self.actual.seal_epoch(epoch, is_checkpoint)
    }

    fn clear_shared_buffer(&self) -> Self::ClearSharedBufferFuture<'_> {
        async move { self.actual.clear_shared_buffer().await }
    }

    fn new_local(&self, table_id: TableId) -> Self::NewLocalFuture<'_> {
        async move {
            VerifyStateStore {
                actual: self.actual.new_local(table_id).await,
                expected: self.expected.new_local(table_id).await,
            }
        }
    }
}

impl<A, E> Deref for VerifyStateStore<A, E> {
    type Target = A;

    fn deref(&self) -> &Self::Target {
        &self.actual
    }
}

impl StateStoreImpl {
    #[cfg_attr(not(target_os = "linux"), expect(unused_variables))]
    pub async fn new(
        s: &str,
        file_cache_dir: &str,
        config: Arc<StorageConfig>,
        hummock_meta_client: Arc<MonitoredHummockMetaClient>,
        state_store_stats: Arc<StateStoreMetrics>,
        object_store_metrics: Arc<ObjectStoreMetrics>,
        tiered_cache_metrics_builder: TieredCacheMetricsBuilder,
    ) -> StorageResult<Self> {
        #[cfg(not(target_os = "linux"))]
        let tiered_cache = TieredCache::none();

        #[cfg(target_os = "linux")]
        let tiered_cache = if file_cache_dir.is_empty() {
            TieredCache::none()
        } else {
            use crate::hummock::file_cache::cache::FileCacheOptions;
            use crate::hummock::HummockError;

            let options = FileCacheOptions {
                dir: file_cache_dir.to_string(),
                capacity: config.file_cache.capacity_mb * 1024 * 1024,
                total_buffer_capacity: config.file_cache.total_buffer_capacity_mb * 1024 * 1024,
                cache_file_fallocate_unit: config.file_cache.cache_file_fallocate_unit_mb
                    * 1024
                    * 1024,
                cache_meta_fallocate_unit: config.file_cache.cache_meta_fallocate_unit_mb
                    * 1024
                    * 1024,
                cache_file_max_write_size: config.file_cache.cache_file_max_write_size_mb
                    * 1024
                    * 1024,
                flush_buffer_hooks: vec![],
            };
            let metrics = Arc::new(tiered_cache_metrics_builder.file());
            TieredCache::file(options, metrics)
                .await
                .map_err(HummockError::tiered_cache)?
        };

        let store = match s {
            hummock if hummock.starts_with("hummock+") => {
                let remote_object_store = parse_remote_object_store(
                    hummock.strip_prefix("hummock+").unwrap(),
                    object_store_metrics.clone(),
                    config.object_store_use_batch_delete,
                )
                .await;
                let object_store = if config.enable_local_spill {
                    let local_object_store = parse_local_object_store(
                        config.local_object_store.as_str(),
                        object_store_metrics.clone(),
                    );
                    ObjectStoreImpl::hybrid(local_object_store, remote_object_store)
                } else {
                    remote_object_store
                };

                let sstable_store = Arc::new(SstableStore::new(
                    Arc::new(object_store),
                    config.data_directory.to_string(),
                    config.block_cache_capacity_mb * (1 << 20),
                    config.meta_cache_capacity_mb * (1 << 20),
                    tiered_cache,
                ));
                let notification_client =
                    RpcNotificationClient::new(hummock_meta_client.get_inner().clone());

                if !config.enable_state_store_v1 {
                    let inner = HummockStorage::new(
                        config.clone(),
                        sstable_store,
                        hummock_meta_client.clone(),
                        notification_client,
                        state_store_stats.clone(),
                    )
                    .await?;

                    let inner = VerifyStateStore {
                        actual: inner,
                        expected: MemoryStateStore::new(),
                    };

                    StateStoreImpl::HummockStateStore(inner.monitored(state_store_stats))
                } else {
                    let inner = HummockStorageV1::new(
                        config.clone(),
                        sstable_store,
                        hummock_meta_client.clone(),
                        notification_client,
                        state_store_stats.clone(),
                    )
                    .await?;

                    let inner = VerifyStateStore {
                        actual: inner,
                        expected: MemoryStateStore::new(),
                    };

                    StateStoreImpl::HummockStateStoreV1(inner.monitored(state_store_stats))
                }
            }

            "in_memory" | "in-memory" => {
                tracing::warn!("In-memory state store should never be used in end-to-end benchmarks or production environment. Scaling and recovery are not supported.");
                StateStoreImpl::shared_in_memory_store(state_store_stats.clone())
            }

            other => unimplemented!("{} state store is not supported", other),
        };

        Ok(store)
    }
}
