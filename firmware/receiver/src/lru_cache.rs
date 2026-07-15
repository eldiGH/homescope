struct LruCacheItem<V> {
    last_tick: u64,
    value: V,
}

pub struct LruCache<K, V, const N: usize> {
    /// Occupied slots stay packed at the front: `cache[..len]` is `Some`, the rest `None`.
    /// A future `remove` must swap the last item into the hole — leaving a gap makes the
    /// entry at `len - 1` invisible to the scans, and the next insert silently overwrites it.
    cache: [Option<(K, LruCacheItem<V>)>; N],
    len: usize,
    tick: u64,
}

impl<K, V, const N: usize> Default for LruCache<K, V, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V, const N: usize> LruCache<K, V, N> {
    pub fn new() -> Self {
        Self {
            cache: [const { None }; _],
            len: 0,
            tick: 0,
        }
    }
}

impl<K: Eq, V, const N: usize> LruCache<K, V, N> {
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.tick += 1;
        let tick = self.tick;

        if let Some(item) = self.find_item_mut(&key) {
            item.last_tick = tick;

            return Some(core::mem::replace(&mut item.value, value));
        }

        let index = if self.len >= N {
            self.cache
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s.as_ref().map(|(_, item)| (i, item.last_tick)))
                .min_by_key(|&(_, tick)| tick)
                .map(|(i, _)| i)
                .unwrap_or(0)
        } else {
            let len = self.len;
            self.len += 1;
            len
        };

        self.cache[index] = Some((
            key,
            LruCacheItem {
                last_tick: tick,
                value,
            },
        ));

        None
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        self.tick += 1;
        let tick = self.tick;

        let item = self.find_item_mut(key)?;
        item.last_tick = tick;

        Some(&item.value)
    }

    fn find_item_mut(&mut self, key: &K) -> Option<&mut LruCacheItem<V>> {
        self.cache[..self.len]
            .iter_mut()
            .flatten()
            .find_map(|(k, item)| if k == key { Some(item) } else { None })
    }
}
