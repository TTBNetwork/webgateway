use std::{sync::{LazyLock, RwLock}, time::Duration};

use shared::objectid::ObjectId;
use ttl_cache::TtlCache;

use crate::{auth::generate_random_secret, models::auth::AuthTempSecret};

pub static BIND_CACHE: LazyLock<RwLock<ttl_cache::TtlCache<ObjectId, String>>> = LazyLock::new(|| {
    RwLock::new(TtlCache::new(60))
});
pub static TIMEOUT: LazyLock<Duration> = LazyLock::new(|| Duration::from_mins(5)); // 1 minute

pub fn generate_random_to_key() -> AuthTempSecret {
    let id = ObjectId::new();
    let secret = generate_random_secret();
    BIND_CACHE.write().unwrap().insert(id, secret.clone(), *TIMEOUT);
    AuthTempSecret {
        id,
        secret
    }
}

pub fn get_secret_from_key(key: ObjectId) -> Option<String> {
    BIND_CACHE.write().unwrap().get(&key).cloned()
}

pub fn remove_key(key: ObjectId) {
    BIND_CACHE.write().unwrap().remove(&key);
}