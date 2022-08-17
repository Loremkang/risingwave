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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::monitor::StateStoreMetrics;
#[derive(Default)]
pub struct StoreLocalStatistic {
    pub cache_data_block_miss: u64,
    pub cache_data_block_total: u64,
    pub cache_meta_block_miss: u64,
    pub cache_meta_block_total: u64,

    pub tiered_cache_total: u64,
    pub tiered_cache_miss: Arc<AtomicU64>,

    // include multiple versions of one key.
    pub scan_key_count: u64,
    pub processed_key_count: u64,
    pub bloom_filter_true_negative_count: u64,
    pub bloom_filter_might_positive_count: u64,
    pub remote_io_time: Arc<AtomicU64>,
}

impl StoreLocalStatistic {
    pub fn add(&mut self, other: &StoreLocalStatistic) {
        self.cache_meta_block_miss += other.cache_data_block_miss;
        self.cache_meta_block_total += other.cache_meta_block_total;

        self.cache_data_block_miss += other.cache_data_block_miss;
        self.cache_data_block_total += other.cache_data_block_total;

        self.tiered_cache_total += other.tiered_cache_total;
        self.tiered_cache_miss.fetch_add(
            other.tiered_cache_miss.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );

        self.scan_key_count += other.scan_key_count;
        self.processed_key_count += other.processed_key_count;
        self.bloom_filter_true_negative_count += other.bloom_filter_true_negative_count;
        self.bloom_filter_might_positive_count += other.bloom_filter_might_positive_count;
        self.remote_io_time.fetch_add(
            other.remote_io_time.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
    }

    pub fn report(&self, metrics: &StateStoreMetrics) {
        if self.cache_data_block_total > 0 {
            metrics
                .sst_store_block_request_counts
                .with_label_values(&["data_total"])
                .inc_by(self.cache_data_block_total);
        }

        if self.cache_data_block_miss > 0 {
            metrics
                .sst_store_block_request_counts
                .with_label_values(&["data_miss"])
                .inc_by(self.cache_data_block_miss);
        }

        if self.cache_meta_block_total > 0 {
            metrics
                .sst_store_block_request_counts
                .with_label_values(&["meta_total"])
                .inc_by(self.cache_meta_block_total);
        }

        if self.cache_meta_block_miss > 0 {
            metrics
                .sst_store_block_request_counts
                .with_label_values(&["meta_miss"])
                .inc_by(self.cache_meta_block_miss);
        }

        if self.tiered_cache_total > 0 {
            metrics
                .sst_store_block_request_counts
                .with_label_values(&["tiered_cache_total"])
                .inc_by(self.tiered_cache_total)
        }

        let tiered_cache_miss = self.tiered_cache_miss.load(Ordering::Relaxed);
        if tiered_cache_miss > 0 {
            metrics
                .sst_store_block_request_counts
                .with_label_values(&["tiered_cache_miss"])
                .inc_by(tiered_cache_miss)
        }

        if self.bloom_filter_true_negative_count > 0 {
            metrics
                .bloom_filter_true_negative_counts
                .inc_by(self.bloom_filter_true_negative_count);
        }

        if self.bloom_filter_might_positive_count > 0 {
            metrics
                .bloom_filter_might_positive_counts
                .inc_by(self.bloom_filter_might_positive_count);
        }
        let remote_io_time = self.remote_io_time.load(Ordering::Relaxed) as f64;
        if remote_io_time > 0.0 {
            metrics.remote_read_time.observe(remote_io_time / 1000.0);
        }
    }
}
