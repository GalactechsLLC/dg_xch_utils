use serde::Serialize;
use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

#[derive(Default, Clone)]
pub struct RecentErrors<T: Clone + Serialize> {
    depth: usize,
    cache_duration: Duration,
    errors: VecDeque<(T, SystemTime)>,
}
impl<T: Clone + Serialize> RecentErrors<T> {
    pub fn new(depth: usize, cache_duration: Duration) -> Self {
        Self {
            depth,
            cache_duration,
            errors: Default::default(),
        }
    }
    pub fn add(&mut self, t: T) {
        self.errors.push_front((t, SystemTime::now()));
        self.trim();
    }
    pub fn get(&mut self) -> Vec<(T, SystemTime)> {
        self.trim();
        self.errors.iter().cloned().collect()
    }
    pub fn trim(&mut self) {
        self.errors.truncate(self.depth);
        self.errors
            .retain(|(_, d)| match SystemTime::now().duration_since(*d) {
                Ok(dur) => dur < self.cache_duration,
                Err(_) => false,
            });
    }
}
