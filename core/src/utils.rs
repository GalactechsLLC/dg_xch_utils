use hkdf::hmac::digest::Output;
use sha2::{Digest, Sha256, Sha256VarCore};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Error;
use std::mem::swap;
use tokio::select;
#[cfg(not(target_os = "windows"))]
use tokio::signal::unix::{SignalKind, signal};
#[cfg(target_os = "windows")]
use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};

pub fn hash_256(input: impl AsRef<[u8]>) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(input);
    let mut buf = [0u8; 32];
    hasher.finalize_into(<&mut Output<Sha256VarCore>>::from(&mut buf));
    buf
}

/// Fixed-capacity insertion-order cache used by farmer runtime statistics.
///
/// This preserves the small cache API that previously forced the core crate to
/// depend on Portfu 1.x even though it is unrelated to HTTP serving.
pub struct CircularCache<K: Eq + Hash, V, const N: usize> {
    keys: [Option<K>; N],
    hashes: [Option<u64>; N],
    values: [Option<V>; N],
    index: Option<usize>,
}

impl<K: Eq + Hash, V, const N: usize> Default for CircularCache<K, V, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Eq + Hash, V, const N: usize> CircularCache<K, V, N> {
    pub fn new() -> Self {
        Self {
            keys: [const { None }; N],
            hashes: [const { None }; N],
            values: [const { None }; N],
            index: None,
        }
    }

    pub fn keys(&self) -> &[Option<K>] {
        &self.keys
    }

    pub fn values(&self) -> &[Option<V>] {
        &self.values
    }

    pub fn first(&self, key: &K) -> Option<&V> {
        let search_hash = hash_key(key);
        self.hashes
            .iter()
            .position(|hash| *hash == Some(search_hash))
            .and_then(|index| self.values[index].as_ref())
    }

    pub fn get(&self, key: &K) -> Vec<&Option<V>> {
        let search_hash = hash_key(key);
        self.hashes
            .iter()
            .enumerate()
            .filter_map(|(index, hash)| (*hash == Some(search_hash)).then_some(&self.values[index]))
            .collect()
    }

    pub fn get_all(&self, key: &K) -> Vec<&Option<V>> {
        self.get(key)
    }

    pub fn contains(&self, key: &K) -> bool {
        self.hashes.contains(&Some(hash_key(key)))
    }

    pub fn insert(&mut self, key: K, value: V) -> (Option<K>, Option<V>) {
        let index = match &mut self.index {
            Some(index) => {
                *index = index.wrapping_add(1);
                *index
            }
            None => {
                self.index = Some(0);
                0
            }
        };
        let slot = index % N;
        let mut key = Some(key);
        let mut hash = Some(hash_key(key.as_ref().expect("cache key is present")));
        let mut value = Some(value);
        swap(&mut self.keys[slot], &mut key);
        swap(&mut self.hashes[slot], &mut hash);
        swap(&mut self.values[slot], &mut value);
        (key, value)
    }

    pub fn replace(&mut self, key: K, value: V) -> Option<V> {
        let index = self
            .keys
            .iter()
            .position(|candidate| candidate.as_ref() == Some(&key))?;
        self.values[index].replace(value)
    }

    pub fn slice(&self) -> &[Option<V>] {
        match self.index {
            None => &[],
            Some(index) if index < N => &self.values[..index],
            Some(_) => &self.values,
        }
    }
}

fn hash_key<K: Hash>(key: &K) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

#[cfg(not(target_os = "windows"))]
pub async fn await_termination() -> Result<(), Error> {
    let mut term_signal = signal(SignalKind::terminate())?;
    let mut int_signal = signal(SignalKind::interrupt())?;
    let mut quit_signal = signal(SignalKind::quit())?;
    let mut alarm_signal = signal(SignalKind::alarm())?;
    let mut hup_signal = signal(SignalKind::hangup())?;
    select! {
        _ = term_signal.recv() => (),
        _ = int_signal.recv() => (),
        _ = quit_signal.recv() => (),
        _ = alarm_signal.recv() => (),
        _ = hup_signal.recv() => ()
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub async fn await_termination() -> Result<(), Error> {
    let mut ctrl_break_signal = ctrl_break()?;
    let mut ctrl_c_signal = ctrl_c()?;
    let mut ctrl_close_signal = ctrl_close()?;
    let mut ctrl_logoff_signal = ctrl_logoff()?;
    let mut ctrl_shutdown_signal = ctrl_shutdown()?;
    select! {
        _ = ctrl_break_signal.recv() => (),
        _ = ctrl_c_signal.recv() => (),
        _ = ctrl_close_signal.recv() => (),
        _ = ctrl_logoff_signal.recv() => (),
        _ = ctrl_shutdown_signal.recv() => ()
    }
    Ok(())
}
