use std::ops::Bound::{self, *};

use crate::hummock::iterator::{HummockIterator, ReverseSortedIterator};
use crate::hummock::key::{get_epoch, key_with_epoch, user_key as to_user_key, Epoch};
use crate::hummock::value::HummockValue;
use crate::hummock::HummockResult;

/// [`ReverseUserKeyIterator`] can be used by user directly.
pub struct ReverseUserKeyIterator {
    /// Inner table iterator.
    iterator: ReverseSortedIterator,

    /// We just met a new key
    just_met_new_key: bool,

    /// Last user key
    last_key: Vec<u8>,

    /// Last user value
    last_val: Vec<u8>,

    /// Last user key value is deleted
    last_delete: bool,

    /// Flag for whether the iterator reach over the right end of the range.
    out_of_range: bool,

    /// Start and end bounds of user key.
    key_range: (Bound<Vec<u8>>, Bound<Vec<u8>>),

    /// Only read values if `epoch <= self.read_epoch`.
    read_epoch: Epoch,
}

impl ReverseUserKeyIterator {
    /// Create [`UserKeyIterator`] with maximum epoch.
    pub(crate) fn new(
        iterator: ReverseSortedIterator,
        key_range: (Bound<Vec<u8>>, Bound<Vec<u8>>),
    ) -> Self {
        Self::new_with_epoch(iterator, key_range, Epoch::MAX)
    }

    /// Create [`UserKeyIterator`] with given `read_epoch`.
    pub(crate) fn new_with_epoch(
        iterator: ReverseSortedIterator,
        key_range: (Bound<Vec<u8>>, Bound<Vec<u8>>),
        read_epoch: u64,
    ) -> Self {
        Self {
            iterator,
            out_of_range: false,
            key_range,
            just_met_new_key: false,
            last_key: Vec::new(),
            last_val: Vec::new(),
            last_delete: true,
            read_epoch,
        }
    }

    fn out_of_range(&self, key: &[u8]) -> bool {
        match &self.key_range.0 {
            Included(begin_key) => key < begin_key.as_slice(),
            Excluded(begin_key) => key <= begin_key.as_slice(),
            Unbounded => false,
        }
    }

    fn reset(&mut self) {
        self.last_key.clear();
        self.just_met_new_key = false;
        self.last_delete = true;
        self.out_of_range = false;
    }

    /// Get the iterator move to the next step.
    ///
    /// Returned result:
    /// - if `Ok(())` is returned, it means that the iterator successfully move to the next position
    ///   (may reach to the end and thus not valid)
    /// - if `Err(_) ` is returned, it means that some error happended.
    pub async fn next(&mut self) -> HummockResult<()> {
        // We need to deal with three cases:
        // 1. current key == last key.
        //    Since current key must have an epoch newer than the one of the last key,
        //    we assign current kv as the new last kv and also inherit its status of deletion, and
        // continue.
        //
        // 2. current key != last key.
        //    We have to make a decision for the last key.
        //    a. If it not deleted, we stop.
        //    b. Otherwise, we continue to find the next new key.
        //
        // 3. `self.iterator` invalid. The case is the same as 2. However, option b is invalid now.
        // We just stop. Without further `next`, `ReverseUserKeyIterator` is still valid.

        // We remark that whether `self.iterator` is valid and `ReverseUserKeyIterator` is valid can
        // be different even if we leave `out_of_range` out of consideration. This diffs
        // from `UserKeyIterator` because we always make a decision about the past key only
        // when we enter a new state, such as encountering a new key, or `self.iterator`
        // turning invalid.

        if !self.iterator.is_valid() {
            // We abuse `last_delete` to indicate that we are indeed invalid now, i.e. run out of kv
            // pairs.
            self.last_delete = true;
            return Ok(());
        }

        while self.iterator.is_valid() {
            let full_key = self.iterator.key();
            let epoch = get_epoch(full_key);
            let key = to_user_key(full_key);

            if epoch <= self.read_epoch {
                if self.just_met_new_key {
                    self.last_key.clear();
                    self.last_key.extend_from_slice(key);
                    self.just_met_new_key = false;
                    // If we encounter an out-of-range key, stop early.
                    if self.out_of_range(&self.last_key) {
                        self.out_of_range = true;
                        break;
                    }
                } else if self.last_key != key {
                    if !self.last_delete {
                        // We remark that we don't check `out_of_range` here as the other two cases
                        // covered all situation. 2(a)
                        self.just_met_new_key = true;
                        return Ok(());
                    } else {
                        // 2(b)
                        self.last_key.clear();
                        self.last_key.extend_from_slice(key);
                        // If we encounter an out-of-range key, stop early.
                        if self.out_of_range(&self.last_key) {
                            self.out_of_range = true;
                            break;
                        }
                    }
                }
                // TODO: Since the real world workload may follow power law or 20/80 rule, or
                // whatever name. We may directly seek to the next key if we have
                // been seeing the same key for too many times.

                // 1 and 2(a)
                match self.iterator.value() {
                    HummockValue::Put(val) => {
                        self.last_val.clear();
                        self.last_val.extend_from_slice(val);
                        self.last_delete = false;
                    }
                    HummockValue::Delete => {
                        self.last_delete = true;
                    }
                }
            }
            self.iterator.next().await?;
        }
        Ok(()) // not valid, EOF
    }

    /// Return the key with the newest version. Thus no version in it, and only the `user_key` will
    /// be returned.
    ///
    /// The returned key is de-duplicated and thus it will not output the same key, unless the
    /// `rewind` or `seek` methods are called.
    ///
    /// Note: before call the function you need to ensure that the iterator is valid.
    pub fn key(&self) -> &[u8] {
        assert!(self.is_valid());
        self.last_key.as_slice()
    }

    /// The returned value is in the form of user value.
    ///
    /// Note: before call the function you need to ensure that the iterator is valid.
    pub fn value(&self) -> &[u8] {
        assert!(self.is_valid());
        self.last_val.as_slice()
    }

    /// Reset the iterating position to the beginning.
    pub async fn rewind(&mut self) -> HummockResult<()> {
        // handle range scan
        match &self.key_range.1 {
            Included(end_key) => {
                let full_key = &key_with_epoch(end_key.clone(), 0);
                self.iterator.seek(full_key).await?;
            }
            Excluded(_) => unimplemented!("excluded begin key is not supported"),
            Unbounded => self.iterator.rewind().await?,
        };

        // handle multi-version
        self.reset();
        // handle range scan when key < begin_key
        self.next().await
    }

    /// Reset the iterating position to the first position where the key >= provided key.
    pub async fn seek(&mut self, user_key: &[u8]) -> HummockResult<()> {
        // handle range scan when key > end_key
        let user_key = match &self.key_range.1 {
            Included(end_key) => {
                if end_key.as_slice() < user_key {
                    end_key.clone()
                } else {
                    Vec::from(user_key)
                }
            }
            Excluded(_) => unimplemented!("excluded begin key is not supported"),
            Unbounded => Vec::from(user_key),
        };
        let full_key = &key_with_epoch(user_key, 0);
        self.iterator.seek(full_key).await?;

        // handle multi-version
        self.reset();
        // handle range scan when key < begin_key
        self.next().await
    }

    /// Indicate whether the iterator can be used.
    pub fn is_valid(&self) -> bool {
        // handle range scan
        // key <= end_key is guaranteed by seek/rewind function
        // We remark that there are only three cases out of four combinations:
        // (iterator valid && last_delete false) is impossible
        let has_enough_input = self.iterator.is_valid() || !self.last_delete;
        has_enough_input && (!self.out_of_range)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ops::Bound::*;
    use std::sync::Arc;

    use itertools::Itertools;
    use rand::distributions::Alphanumeric;
    use rand::{thread_rng, Rng};

    use super::*;
    use crate::hummock::cloud::gen_remote_table;
    use crate::hummock::iterator::test_utils::{
        default_builder_opt_for_test, iterator_test_key_of, iterator_test_key_of_epoch, test_key,
        test_value_of, TestIteratorBuilder, TEST_KEYS_COUNT,
    };
    use crate::hummock::iterator::variants::BACKWARD;
    use crate::hummock::iterator::BoxedHummockIterator;
    use crate::hummock::key::{prev_key, user_key};
    use crate::hummock::table::{Table, TableIterator};
    use crate::hummock::value::HummockValue;
    use crate::hummock::{ReverseTableIterator, TableBuilder};
    use crate::object::{InMemObjectStore, ObjectStore};

    #[tokio::test]
    async fn test_reverse_user_key_basic() {
        let base_key_value = usize::MAX - 100;
        let (iters, validators): (Vec<_>, Vec<_>) = (0..3)
            .map(|iter_id| {
                TestIteratorBuilder::<BACKWARD>::default()
                    .id(0)
                    .map_key(move |id, x| {
                        iterator_test_key_of(id, base_key_value - x * 3 + (3 - iter_id as usize))
                    })
                    .map_value(move |id, x| {
                        test_value_of(id, base_key_value - x * 3 + (3 - iter_id as usize) + 1)
                    })
                    .finish()
            })
            .unzip();

        let iters: Vec<BoxedHummockIterator> = iters
            .into_iter()
            .map(|x| Box::new(x) as BoxedHummockIterator)
            .collect_vec();

        let si = ReverseSortedIterator::new(iters);
        let mut uki = ReverseUserKeyIterator::new(si, (Unbounded, Unbounded));
        uki.rewind().await.unwrap();

        let mut i = 0;
        while uki.is_valid() {
            let k = uki.key();
            let v = uki.value();
            validators[i % 3].assert_value(i / 3, v);
            validators[i % 3].assert_user_key(i / 3, k);
            i += 1;

            uki.next().await.unwrap();
            if i == TEST_KEYS_COUNT * 3 {
                assert!(!uki.is_valid());
                break;
            }
        }
        assert!(
            i >= TEST_KEYS_COUNT * 3,
            "We expect to see {} keys. But we actually have seen {} keys",
            TEST_KEYS_COUNT * 3,
            i
        );
    }

    #[tokio::test]
    async fn test_reverse_user_key_seek() {
        let base_key_value = usize::MAX - 100;
        let (iters, validators): (Vec<_>, Vec<_>) = (0..3)
            .map(|iter_id| {
                TestIteratorBuilder::<BACKWARD>::default()
                    .id(0)
                    .total(20)
                    .map_key(move |id, x| {
                        iterator_test_key_of(id, base_key_value - x * 3 + (3 - iter_id as usize))
                    })
                    .map_value(move |id, x| {
                        test_value_of(id, base_key_value - x * 3 + (3 - iter_id as usize) + 1)
                    })
                    .finish()
            })
            .unzip();

        let iters: Vec<BoxedHummockIterator> = iters
            .into_iter()
            .map(|x| Box::new(x) as BoxedHummockIterator)
            .collect_vec();

        let si = ReverseSortedIterator::new(iters);
        let mut uki = ReverseUserKeyIterator::new(si, (Unbounded, Unbounded));
        let test_validator = &validators[2];

        // right edge case
        uki.seek(user_key(test_key!(test_validator, 3 * TEST_KEYS_COUNT)))
            .await
            .unwrap();
        assert!(!uki.is_valid());

        // normal case
        uki.seek(user_key(test_key!(test_validator, 4)))
            .await
            .unwrap();
        let k = uki.key();
        let v = uki.value();
        test_validator.assert_value(4, v);
        test_validator.assert_user_key(4, k);

        uki.seek(user_key(test_key!(test_validator, 17)))
            .await
            .unwrap();
        let k = uki.key();
        let v = uki.value();
        test_validator.assert_value(17, v);
        test_validator.assert_user_key(17, k);

        // left edge case
        uki.seek(user_key(test_key!(test_validator, 0)))
            .await
            .unwrap();
        let k = uki.key();
        let v = uki.value();

        test_validator.assert_user_key(0, k);
        test_validator.assert_value(0, v);
    }

    #[tokio::test]
    async fn test_reverse_user_key_delete() {
        // key=[table, idx, epoch], value
        let kv_pairs = vec![
            (0, 2, 300, HummockValue::Delete),
            (0, 1, 100, HummockValue::Put(test_value_of(0, 1))),
        ];
        let table0 = add_kv_pair(kv_pairs).await;

        let kv_pairs = vec![
            (0, 2, 400, HummockValue::Put(test_value_of(0, 2))),
            (0, 1, 200, HummockValue::Delete),
        ];
        let table1 = add_kv_pair(kv_pairs).await;

        let iters: Vec<BoxedHummockIterator> = vec![
            Box::new(TableIterator::new(Arc::new(table0))),
            Box::new(TableIterator::new(Arc::new(table1))),
        ];
        let si = ReverseSortedIterator::new(iters);
        let mut uki = ReverseUserKeyIterator::new(si, (Unbounded, Unbounded));
        uki.rewind().await.unwrap();

        // verify
        let k = uki.key();
        let v = uki.value();
        assert_eq!(k, user_key(iterator_test_key_of(0, 2).as_slice()));
        assert_eq!(v, test_value_of(0, 2));

        // only one valid kv pair
        uki.next().await.unwrap();
        assert!(!uki.is_valid());
    }

    // left..=end
    #[tokio::test]
    async fn test_reverse_user_key_range_inclusive() {
        // key=[table, idx, epoch], value
        let kv_pairs = vec![
            (0, 0, 200, HummockValue::Delete),
            (0, 0, 100, HummockValue::Put(test_value_of(0, 0))),
            (0, 1, 200, HummockValue::Put(test_value_of(0, 1))),
            (0, 1, 100, HummockValue::Delete),
            (0, 2, 400, HummockValue::Delete),
            (0, 2, 300, HummockValue::Put(test_value_of(0, 2))),
            (0, 2, 200, HummockValue::Delete),
            (0, 2, 100, HummockValue::Put(test_value_of(0, 2))),
            (0, 3, 100, HummockValue::Put(test_value_of(0, 3))),
            (0, 5, 200, HummockValue::Delete),
            (0, 5, 100, HummockValue::Put(test_value_of(0, 5))),
            (0, 6, 100, HummockValue::Put(test_value_of(0, 6))),
            (0, 7, 300, HummockValue::Put(test_value_of(0, 7))),
            (0, 7, 200, HummockValue::Delete),
            (0, 7, 100, HummockValue::Put(test_value_of(0, 7))),
            (0, 8, 100, HummockValue::Put(test_value_of(0, 8))),
        ];
        let table = add_kv_pair(kv_pairs).await;
        let iters: Vec<BoxedHummockIterator> =
            vec![Box::new(ReverseTableIterator::new(Arc::new(table)))];
        let si = ReverseSortedIterator::new(iters);

        let begin_key = Included(user_key(key_range_test_key(0, 2, 0).as_slice()).to_vec());
        let end_key = Included(user_key(key_range_test_key(0, 7, 0).as_slice()).to_vec());

        let mut uki = ReverseUserKeyIterator::new(si, (begin_key, end_key));

        // ----- basic iterate -----
        uki.rewind().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 7).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 6).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 3).as_slice()));
        uki.next().await.unwrap();
        assert!(!uki.is_valid());

        // ----- after-end-range iterate -----
        uki.seek(user_key(iterator_test_key_of(0, 8).as_slice()))
            .await
            .unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 7).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 6).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 3).as_slice()));
        uki.next().await.unwrap();
        assert!(!uki.is_valid());

        // ----- end-range iterate -----
        uki.seek(user_key(iterator_test_key_of(0, 7).as_slice()))
            .await
            .unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 7).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 6).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 3).as_slice()));
        uki.next().await.unwrap();
        assert!(!uki.is_valid());

        // ----- begin-range iterate -----
        uki.seek(user_key(iterator_test_key_of(0, 2).as_slice()))
            .await
            .unwrap();
        assert!(!uki.is_valid());

        // ----- before-begin-range iterate -----
        uki.seek(user_key(iterator_test_key_of(0, 1).as_slice()))
            .await
            .unwrap();
        assert!(!uki.is_valid());
    }

    // left..end
    #[tokio::test]
    async fn test_reverse_user_key_range() {
        // key=[table, idx, epoch], value
        let kv_pairs = vec![
            (0, 0, 200, HummockValue::Delete),
            (0, 0, 100, HummockValue::Put(test_value_of(0, 0))),
            (0, 1, 200, HummockValue::Put(test_value_of(0, 1))),
            (0, 1, 100, HummockValue::Delete),
            (0, 2, 300, HummockValue::Put(test_value_of(0, 2))),
            (0, 2, 200, HummockValue::Delete),
            (0, 2, 100, HummockValue::Delete),
            (0, 3, 100, HummockValue::Put(test_value_of(0, 3))),
            (0, 5, 200, HummockValue::Delete),
            (0, 5, 100, HummockValue::Put(test_value_of(0, 5))),
            (0, 6, 100, HummockValue::Put(test_value_of(0, 6))),
            (0, 7, 100, HummockValue::Put(test_value_of(0, 7))),
            (0, 8, 100, HummockValue::Put(test_value_of(0, 8))),
        ];
        let table = add_kv_pair(kv_pairs).await;
        let iters: Vec<BoxedHummockIterator> =
            vec![Box::new(ReverseTableIterator::new(Arc::new(table)))];
        let si = ReverseSortedIterator::new(iters);

        let begin_key = Excluded(user_key(key_range_test_key(0, 2, 0).as_slice()).to_vec());
        let end_key = Included(user_key(key_range_test_key(0, 7, 0).as_slice()).to_vec());

        let mut uki = ReverseUserKeyIterator::new(si, (begin_key, end_key));

        // ----- basic iterate -----
        uki.rewind().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 7).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 6).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 3).as_slice()));
        uki.next().await.unwrap();
        assert!(!uki.is_valid());

        // ----- after-bend-range iterate -----
        uki.seek(user_key(iterator_test_key_of(0, 8).as_slice()))
            .await
            .unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 7).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 6).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 3).as_slice()));
        uki.next().await.unwrap();
        assert!(!uki.is_valid());

        // ----- end-range iterate -----
        uki.seek(user_key(iterator_test_key_of(0, 7).as_slice()))
            .await
            .unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 7).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 6).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 3).as_slice()));
        uki.next().await.unwrap();
        assert!(!uki.is_valid());

        // ----- begin-range iterate -----
        uki.seek(user_key(iterator_test_key_of(0, 2).as_slice()))
            .await
            .unwrap();
        assert!(!uki.is_valid());

        // ----- begin-begin-range iterate -----
        uki.seek(user_key(iterator_test_key_of(0, 1).as_slice()))
            .await
            .unwrap();
        assert!(!uki.is_valid());
    }

    // ..=right
    #[tokio::test]
    async fn test_reverse_user_key_range_to_inclusive() {
        // key=[table, idx, epoch], value
        let kv_pairs = vec![
            (0, 0, 200, HummockValue::Delete),
            (0, 0, 100, HummockValue::Put(test_value_of(0, 0))),
            (0, 1, 200, HummockValue::Put(test_value_of(0, 1))),
            (0, 1, 100, HummockValue::Delete),
            (0, 2, 300, HummockValue::Put(test_value_of(0, 2))),
            (0, 2, 200, HummockValue::Delete),
            (0, 2, 100, HummockValue::Delete),
            (0, 3, 100, HummockValue::Put(test_value_of(0, 3))),
            (0, 5, 200, HummockValue::Delete),
            (0, 5, 100, HummockValue::Put(test_value_of(0, 5))),
            (0, 6, 100, HummockValue::Put(test_value_of(0, 6))),
            (0, 7, 200, HummockValue::Delete),
            (0, 7, 100, HummockValue::Put(test_value_of(0, 7))),
            (0, 8, 100, HummockValue::Put(test_value_of(0, 8))),
        ];
        let table = add_kv_pair(kv_pairs).await;
        let iters: Vec<BoxedHummockIterator> =
            vec![Box::new(ReverseTableIterator::new(Arc::new(table)))];
        let si = ReverseSortedIterator::new(iters);
        let end_key = Included(user_key(key_range_test_key(0, 7, 0).as_slice()).to_vec());

        let mut uki = ReverseUserKeyIterator::new(si, (Unbounded, end_key));

        // ----- basic iterate -----
        uki.rewind().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 6).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 3).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 2).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 1).as_slice()));
        uki.next().await.unwrap();
        assert!(!uki.is_valid());

        // ----- end-range iterate -----
        uki.seek(user_key(iterator_test_key_of(0, 7).as_slice()))
            .await
            .unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 6).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 3).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 2).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 1).as_slice()));
        uki.next().await.unwrap();
        assert!(!uki.is_valid());

        // ----- in-range iterate -----
        uki.seek(user_key(iterator_test_key_of(0, 6).as_slice()))
            .await
            .unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 6).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 3).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 2).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 1).as_slice()));
        uki.next().await.unwrap();
        assert!(!uki.is_valid());

        // ----- begin-range iterate -----
        uki.seek(user_key(iterator_test_key_of(0, 0).as_slice()))
            .await
            .unwrap();
        assert!(!uki.is_valid());
    }

    // left..
    #[tokio::test]
    async fn test_reverse_user_key_range_from() {
        // key=[table, idx, epoch], value
        let kv_pairs = vec![
            (0, 0, 200, HummockValue::Delete),
            (0, 0, 100, HummockValue::Put(test_value_of(0, 0))),
            (0, 1, 200, HummockValue::Put(test_value_of(0, 1))),
            (0, 1, 100, HummockValue::Delete),
            (0, 2, 300, HummockValue::Put(test_value_of(0, 2))),
            (0, 2, 200, HummockValue::Delete),
            (0, 2, 100, HummockValue::Delete),
            (0, 3, 100, HummockValue::Put(test_value_of(0, 3))),
            (0, 5, 200, HummockValue::Delete),
            (0, 5, 100, HummockValue::Put(test_value_of(0, 5))),
            (0, 6, 100, HummockValue::Put(test_value_of(0, 6))),
            (0, 7, 200, HummockValue::Delete),
            (0, 7, 100, HummockValue::Put(test_value_of(0, 7))),
            (0, 8, 100, HummockValue::Put(test_value_of(0, 8))),
        ];
        let table = add_kv_pair(kv_pairs).await;
        let iters: Vec<BoxedHummockIterator> =
            vec![Box::new(ReverseTableIterator::new(Arc::new(table)))];
        let si = ReverseSortedIterator::new(iters);
        let begin_key = Included(user_key(key_range_test_key(0, 2, 0).as_slice()).to_vec());

        let mut uki = ReverseUserKeyIterator::new(si, (begin_key, Unbounded));

        // ----- basic iterate -----
        uki.rewind().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 8).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 6).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 3).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 2).as_slice()));
        uki.next().await.unwrap();
        assert!(!uki.is_valid());

        // ----- begin-range iterate -----
        uki.seek(user_key(iterator_test_key_of(0, 2).as_slice()))
            .await
            .unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 2).as_slice()));
        uki.next().await.unwrap();
        assert!(!uki.is_valid());

        // ----- in-range iterate -----
        uki.seek(user_key(iterator_test_key_of(0, 5).as_slice()))
            .await
            .unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 3).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 2).as_slice()));
        uki.next().await.unwrap();
        assert!(!uki.is_valid());

        // ----- end-range iterate -----
        uki.seek(user_key(iterator_test_key_of(0, 8).as_slice()))
            .await
            .unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 8).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 6).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 3).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 2).as_slice()));
        uki.next().await.unwrap();
        assert!(!uki.is_valid());

        // ----- after-end-range iterate -----
        uki.seek(user_key(iterator_test_key_of(0, 9).as_slice()))
            .await
            .unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 8).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 6).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 3).as_slice()));
        uki.next().await.unwrap();
        assert_eq!(uki.key(), user_key(iterator_test_key_of(0, 2).as_slice()));
        uki.next().await.unwrap();
        assert!(!uki.is_valid());
    }

    fn clone_table(table: &Table) -> Table {
        Table {
            id: table.id,
            meta: table.meta.clone(),
            obj_client: table.obj_client.clone(),
            data_path: table.data_path.clone(),
        }
    }

    fn key_from_num(num: usize) -> Vec<u8> {
        let width = 20;
        format!("{:0width$}", num, width = width)
            .as_bytes()
            .to_vec()
    }

    async fn chaos_test_case(
        table: Table,
        start_bound: Bound<Vec<u8>>,
        end_bound: Bound<Vec<u8>>,
        truth: &BTreeMap<Vec<u8>, BTreeMap<Epoch, HummockValue<Vec<u8>>>>,
    ) {
        let start_key = match &start_bound {
            Bound::Included(b) => prev_key(&b.clone()),
            Bound::Excluded(b) => b.clone(),
            Unbounded => key_from_num(0),
        };
        let end_key = match &end_bound {
            Bound::Included(b) => b.clone(),
            Unbounded => key_from_num(999999999999),
            _ => unimplemented!(),
        };
        let iters: Vec<BoxedHummockIterator> = vec![Box::new(ReverseTableIterator::new(Arc::new(
            clone_table(&table),
        )))];
        let rsi = ReverseSortedIterator::new(iters);
        let mut ruki = ReverseUserKeyIterator::new(rsi, (start_bound, end_bound));
        let num_puts: usize = truth
            .iter()
            .map(|(key, inserts)| {
                if *key > end_key || *key <= start_key {
                    return 0;
                }
                match inserts.first_key_value().unwrap().1 {
                    HummockValue::Put(_) => 1,
                    HummockValue::Delete => 0,
                }
            })
            .reduce(|accum, item| accum + item)
            .unwrap();
        println!("The total number of valid puts:{}", num_puts);
        let mut num_kvs = 0;
        ruki.rewind().await.unwrap();
        for (key, value) in truth.iter().rev() {
            if *key > end_key || *key <= start_key {
                continue;
            }
            let (time, value) = value.first_key_value().unwrap();
            if let HummockValue::Delete = value {
                continue;
            }
            assert!(ruki.is_valid(), "num_kvs:{}", num_kvs);
            let full_key = key_with_epoch(key.clone(), *time);
            assert_eq!(ruki.key(), user_key(&full_key), "num_kvs:{}", num_kvs);
            if let HummockValue::Put(bytes) = &value {
                assert_eq!(ruki.value(), &bytes[..], "num_kvs:{}", num_kvs);
            }
            ruki.next().await.unwrap();
            num_kvs += 1;
        }
        assert!(!ruki.is_valid());
        assert_eq!(num_kvs, num_puts);
    }

    #[tokio::test]
    async fn test_reverse_user_key_chaos() {
        // We first generate the key value pairs.
        let mut rng = thread_rng();
        let mut truth: BTreeMap<Vec<u8>, BTreeMap<Epoch, HummockValue<Vec<u8>>>> = BTreeMap::new();
        let mut prev_key_number: usize = 1;
        let number_of_keys = 5000;
        for _ in 0..number_of_keys {
            let key: usize = rng.gen_range(prev_key_number..=(prev_key_number + 10));
            prev_key_number = key + 1;
            let key_bytes = key_from_num(key);
            let mut prev_time = 500;
            let num_updates = rng.gen_range(1..10usize);
            for _ in 0..num_updates {
                let time: Epoch = rng.gen_range(prev_time..=(prev_time + 1000));
                let is_delete = rng.gen_range(0..=1usize) < 1usize;
                match is_delete {
                    true => {
                        truth
                            .entry(key_bytes.clone())
                            .or_default()
                            .insert(time, HummockValue::Delete);
                    }
                    false => {
                        let value_size = rng.gen_range(100..=200);
                        let value: String = thread_rng()
                            .sample_iter(&Alphanumeric)
                            .take(value_size)
                            .map(char::from)
                            .collect();
                        truth
                            .entry(key_bytes.clone())
                            .or_default()
                            .insert(time, HummockValue::Put(value.into_bytes()));
                    }
                }
                prev_time = time + 1;
            }
        }
        // We inject the key value pairs into the table.
        let mut b = TableBuilder::new(default_builder_opt_for_test());
        for (key, inserts) in &truth {
            for (time, value) in inserts {
                let full_key = key_with_epoch(key.clone(), *time);
                b.add(&full_key, value.clone());
            }
        }
        let (data, meta) = b.finish();
        // get remote table
        let obj_client = Arc::new(InMemObjectStore::new()) as Arc<dyn ObjectStore>;
        let table = gen_remote_table(obj_client, 0, data, meta, None)
            .await
            .unwrap();

        let repeat = 20;
        for _ in 0..repeat {
            let mut rng = thread_rng();
            let end_key: usize = rng.gen_range(2..=prev_key_number);
            let end_key_bytes = key_from_num(end_key);
            let begin_key: usize = rng.gen_range(1..=end_key);
            let begin_key_bytes = key_from_num(begin_key);
            println!(
                "begin_key:{:?},end_key:{:?}",
                begin_key_bytes, end_key_bytes
            );
            chaos_test_case(clone_table(&table), Unbounded, Unbounded, &truth).await;
            chaos_test_case(
                clone_table(&table),
                Unbounded,
                Included(end_key_bytes.clone()),
                &truth,
            )
            .await;
            chaos_test_case(
                clone_table(&table),
                Included(begin_key_bytes.clone()),
                Unbounded,
                &truth,
            )
            .await;
            chaos_test_case(
                clone_table(&table),
                Excluded(begin_key_bytes.clone()),
                Unbounded,
                &truth,
            )
            .await;
            chaos_test_case(
                clone_table(&table),
                Included(begin_key_bytes.clone()),
                Included(end_key_bytes.clone()),
                &truth,
            )
            .await;
            chaos_test_case(
                clone_table(&table),
                Excluded(begin_key_bytes),
                Included(end_key_bytes),
                &truth,
            )
            .await;
        }
    }

    // key=[table, idx, epoch], value
    async fn add_kv_pair(kv_pairs: Vec<(u64, usize, u64, HummockValue<Vec<u8>>)>) -> Table {
        let mut b = TableBuilder::new(default_builder_opt_for_test());
        for kv in kv_pairs {
            b.add(key_range_test_key(kv.0, kv.1, kv.2).as_slice(), kv.3);
        }
        let (data, meta) = b.finish();
        // get remote table
        let obj_client = Arc::new(InMemObjectStore::new()) as Arc<dyn ObjectStore>;
        gen_remote_table(obj_client, 0, data, meta, None)
            .await
            .unwrap()
    }

    fn key_range_test_key(table: u64, idx: usize, epoch: u64) -> Vec<u8> {
        iterator_test_key_of_epoch(table, idx, epoch)
    }
}
