use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

/// A rendered page: document, page, longest side and the sha256 of the file it came from, so
/// an edited document never serves a stale image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageKey {
	pub id: Uuid,
	pub page: u32,
	pub width: u32,
	pub sha256: String,
}

/// Least-recently-used page PNGs in memory, bounded by total bytes.
pub struct PageCache {
	capacity: usize,
	inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
	bytes: usize,
	entries: VecDeque<(PageKey, Arc<Vec<u8>>)>,
}

impl PageCache {
	pub fn new(capacity: usize) -> Self {
		Self {
			capacity,
			inner: Mutex::default(),
		}
	}

	pub fn get(&self, key: &PageKey) -> Option<Arc<Vec<u8>>> {
		let mut inner = self.lock();
		let position = inner.entries.iter().position(|(known, _)| known == key)?;
		let entry = inner.entries.remove(position)?;
		let png = entry.1.clone();
		inner.entries.push_back(entry);
		Some(png)
	}

	pub fn put(&self, key: PageKey, png: Arc<Vec<u8>>) {
		if png.len() > self.capacity {
			return;
		}
		let mut inner = self.lock();
		if let Some(position) = inner.entries.iter().position(|(known, _)| *known == key)
			&& let Some((_, old)) = inner.entries.remove(position)
		{
			inner.bytes -= old.len();
		}
		inner.bytes += png.len();
		inner.entries.push_back((key, png));
		while inner.bytes > self.capacity {
			match inner.entries.pop_front() {
				Some((_, evicted)) => inner.bytes -= evicted.len(),
				None => break,
			}
		}
	}

	pub fn bytes(&self) -> usize {
		self.lock().bytes
	}

	fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
		self.inner
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn key(page: u32) -> PageKey {
		PageKey {
			id: Uuid::nil(),
			page,
			width: 100,
			sha256: "abc".into(),
		}
	}

	#[test]
	fn evicts_the_least_recently_used_beyond_capacity() {
		let cache = PageCache::new(10);
		cache.put(key(1), Arc::new(vec![0; 4]));
		cache.put(key(2), Arc::new(vec![0; 4]));
		assert!(cache.get(&key(1)).is_some());
		cache.put(key(3), Arc::new(vec![0; 4]));
		assert!(cache.get(&key(2)).is_none());
		assert!(cache.get(&key(1)).is_some());
		assert!(cache.get(&key(3)).is_some());
		assert_eq!(cache.bytes(), 8);
	}

	#[test]
	fn replaces_an_entry_and_skips_oversized_ones() {
		let cache = PageCache::new(10);
		cache.put(key(1), Arc::new(vec![0; 4]));
		cache.put(key(1), Arc::new(vec![1; 6]));
		assert_eq!(cache.bytes(), 6);
		assert_eq!(cache.get(&key(1)).unwrap()[0], 1);
		cache.put(key(2), Arc::new(vec![0; 11]));
		assert!(cache.get(&key(2)).is_none());
		assert_eq!(cache.bytes(), 6);
	}

	#[test]
	fn a_changed_file_is_a_different_key() {
		let cache = PageCache::new(10);
		cache.put(key(1), Arc::new(vec![0; 4]));
		let edited = PageKey {
			sha256: "def".into(),
			..key(1)
		};
		assert!(cache.get(&edited).is_none());
	}
}
