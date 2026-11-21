//! A small bounded cache: when it grows past its capacity, the least
//! recently used quarter is evicted.

use std::collections::HashMap;
use std::hash::Hash;

pub struct Lru<K, V> {
    map: HashMap<K, (V, u64)>,
    tick: u64,
    cap: usize,
}

impl<K: Hash + Eq + Clone, V: Clone> Lru<K, V> {
    pub fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            tick: 0,
            cap: cap.max(4),
        }
    }

    pub fn get(&mut self, k: &K) -> Option<V> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(k).map(|e| {
            e.1 = tick;
            e.0.clone()
        })
    }

    pub fn insert(&mut self, k: K, v: V) {
        self.tick += 1;
        self.map.insert(k, (v, self.tick));
        if self.map.len() > self.cap {
            let mut ages: Vec<u64> = self.map.values().map(|e| e.1).collect();
            ages.sort_unstable();
            let cutoff = ages[ages.len() / 4];
            self.map.retain(|_, e| e.1 > cutoff);
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_least_recently_used() {
        let mut c = Lru::new(8);
        for i in 0..8 {
            c.insert(i, i);
        }
        c.get(&0); // keep 0 hot
        c.insert(100, 100);
        assert!(c.len() <= 8);
        assert_eq!(c.get(&0), Some(0));
        assert_eq!(c.get(&1), None);
    }
}
