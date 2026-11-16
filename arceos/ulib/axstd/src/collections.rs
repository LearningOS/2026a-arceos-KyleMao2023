//! Collection types for `axstd`.

use alloc::vec::Vec;
use core::borrow::Borrow;
use core::fmt;
use core::hash::{BuildHasher, Hash, Hasher};
use core::mem;
use core::ops::Index;

pub use alloc::collections::*;

/// Default [`BuildHasher`] seeded by `axhal::misc::random()`.
#[derive(Clone, Debug)]
pub struct RandomState {
    seed: u64,
}

impl RandomState {
    /// Creates a new `RandomState` with a random seed from the HAL.
    pub fn new() -> Self {
        let r = crate::os::arceos::modules::axhal::misc::random();
        Self {
            seed: (r as u64) ^ ((r >> 64) as u64),
        }
    }
}

impl Default for RandomState {
    fn default() -> Self {
        Self::new()
    }
}

/// Default FNV-1a + SplitMix64 hasher used by [`RandomState`].
#[derive(Clone)]
pub struct DefaultHasher {
    state: u64,
}

impl Hasher for DefaultHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.state ^= b as u64;
            self.state = self.state.wrapping_mul(0x1000_0000_01b3);
        }
    }

    fn finish(&self) -> u64 {
        let mut x = self.state;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^ (x >> 31)
    }
}

impl BuildHasher for RandomState {
    type Hasher = DefaultHasher;

    fn build_hasher(&self) -> Self::Hasher {
        DefaultHasher {
            state: 0xcbf2_9ce4_8422_2325 ^ self.seed,
        }
    }
}

#[derive(Clone)]
enum Slot<K, V> {
    Empty,
    Tombstone,
    Occupied(u64, K, V),
}

/// A hash map implemented with open addressing and linear probing.
#[derive(Clone)]
pub struct HashMap<K, V, S = RandomState> {
    slots: Vec<Slot<K, V>>,
    len: usize,
    tombstones: usize,
    hash_builder: S,
}

impl<K, V> HashMap<K, V, RandomState> {
    /// Creates an empty `HashMap`.
    pub fn new() -> Self {
        Self::with_hasher(RandomState::new())
    }

    /// Creates an empty `HashMap` with at least the specified capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self::with_capacity_and_hasher(capacity, RandomState::new())
    }
}

impl<K, V, S> HashMap<K, V, S> {
    /// Creates an empty `HashMap` which uses the given hash builder.
    pub const fn with_hasher(hash_builder: S) -> Self {
        Self {
            slots: Vec::new(),
            len: 0,
            tombstones: 0,
            hash_builder,
        }
    }

    /// Creates an empty `HashMap` with at least the specified capacity and hash builder.
    pub fn with_capacity_and_hasher(capacity: usize, hash_builder: S) -> Self {
        let mut map = Self::with_hasher(hash_builder);
        if capacity > 0 {
            let cap = ((capacity * 4) / 3 + 1).next_power_of_two().max(8);
            map.slots.resize_with(cap, || Slot::Empty);
        }
        map
    }

    /// Returns the number of elements in the map.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the map contains no elements.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the number of elements the map can hold without reallocating.
    pub fn capacity(&self) -> usize {
        (self.slots.len() * 3) / 4
    }

    /// Clears the map, removing all key-value pairs.
    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            *slot = Slot::Empty;
        }
        self.len = 0;
        self.tombstones = 0;
    }

    /// An iterator visiting all key-value pairs in arbitrary order.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.slots.iter().filter_map(|slot| match slot {
            Slot::Occupied(_, k, v) => Some((k, v)),
            _ => None,
        })
    }

    /// An iterator visiting all key-value pairs in arbitrary order, with mutable values.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&K, &mut V)> {
        self.slots.iter_mut().filter_map(|slot| match slot {
            Slot::Occupied(_, k, v) => Some((&*k, v)),
            _ => None,
        })
    }

    /// An iterator visiting all keys in arbitrary order.
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.iter().map(|(k, _)| k)
    }

    /// An iterator visiting all values in arbitrary order.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, v)| v)
    }
}

impl<K, V, S> HashMap<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher,
{
    fn make_hash<Q: Hash + ?Sized>(&self, key: &Q) -> u64 {
        let mut hasher = self.hash_builder.build_hasher();
        key.hash(&mut hasher);
        hasher.finish()
    }

    fn ensure_capacity(&mut self) {
        let cap = self.slots.len();
        if cap == 0 {
            self.rehash(8);
        } else if (self.len + self.tombstones + 1) * 4 >= cap * 3 {
            let new_cap = if self.len * 2 >= cap { cap * 2 } else { cap };
            self.rehash(new_cap);
        }
    }

    fn rehash(&mut self, new_cap: usize) {
        let mask = new_cap - 1;
        let mut new_slots = Vec::with_capacity(new_cap);
        new_slots.resize_with(new_cap, || Slot::Empty);

        for slot in mem::take(&mut self.slots) {
            if let Slot::Occupied(hash, key, value) = slot {
                let mut idx = (hash as usize) & mask;
                while !matches!(new_slots[idx], Slot::Empty) {
                    idx = (idx + 1) & mask;
                }
                new_slots[idx] = Slot::Occupied(hash, key, value);
            }
        }
        self.slots = new_slots;
        self.tombstones = 0;
    }

    fn find_index<Q>(&self, key: &Q) -> Option<usize>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        if self.slots.is_empty() {
            return None;
        }
        let hash = self.make_hash(key);
        let mask = self.slots.len() - 1;
        let mut idx = (hash as usize) & mask;
        loop {
            match &self.slots[idx] {
                Slot::Empty => return None,
                Slot::Tombstone => {}
                Slot::Occupied(h, k, _) if *h == hash && k.borrow() == key => return Some(idx),
                Slot::Occupied(_, _, _) => {}
            }
            idx = (idx + 1) & mask;
        }
    }

    /// Inserts a key-value pair into the map, returning the old value if present.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.ensure_capacity();
        let hash = self.make_hash(&key);
        let mask = self.slots.len() - 1;
        let mut idx = (hash as usize) & mask;
        let mut first_tombstone = None;

        loop {
            match &mut self.slots[idx] {
                Slot::Empty => {
                    let target = first_tombstone.unwrap_or(idx);
                    if first_tombstone.is_some() {
                        self.tombstones -= 1;
                    }
                    self.slots[target] = Slot::Occupied(hash, key, value);
                    self.len += 1;
                    return None;
                }
                Slot::Tombstone => {
                    if first_tombstone.is_none() {
                        first_tombstone = Some(idx);
                    }
                }
                Slot::Occupied(h, k, v) if *h == hash && *k == key => {
                    return Some(mem::replace(v, value));
                }
                Slot::Occupied(_, _, _) => {}
            }
            idx = (idx + 1) & mask;
        }
    }

    /// Returns a reference to the value corresponding to the key.
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let idx = self.find_index(key)?;
        match &self.slots[idx] {
            Slot::Occupied(_, _, v) => Some(v),
            _ => None,
        }
    }

    /// Returns a mutable reference to the value corresponding to the key.
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let idx = self.find_index(key)?;
        match &mut self.slots[idx] {
            Slot::Occupied(_, _, v) => Some(v),
            _ => None,
        }
    }

    /// Returns `true` if the map contains a value for the specified key.
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.find_index(key).is_some()
    }

    /// Removes a key from the map, returning the value if it was present.
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let idx = self.find_index(key)?;
        match mem::replace(&mut self.slots[idx], Slot::Tombstone) {
            Slot::Occupied(_, _, v) => {
                self.len -= 1;
                self.tombstones += 1;
                Some(v)
            }
            _ => None,
        }
    }
}

impl<K, V, S> Default for HashMap<K, V, S>
where
    S: Default,
{
    fn default() -> Self {
        Self::with_hasher(S::default())
    }
}

impl<K, V, Q, S> Index<&Q> for HashMap<K, V, S>
where
    K: Eq + Hash + Borrow<Q>,
    Q: Eq + Hash + ?Sized,
    S: BuildHasher,
{
    type Output = V;

    fn index(&self, key: &Q) -> &Self::Output {
        self.get(key).expect("no entry found for key")
    }
}

impl<K, V, S> fmt::Debug for HashMap<K, V, S>
where
    K: fmt::Debug,
    V: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K, V, S> FromIterator<(K, V)> for HashMap<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Default,
{
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let iter = iter.into_iter();
        let (lower, _) = iter.size_hint();
        let mut map = Self::with_capacity_and_hasher(lower, S::default());
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}
