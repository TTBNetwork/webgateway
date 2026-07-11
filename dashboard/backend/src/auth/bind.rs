use std::{
    sync::{LazyLock, RwLock},
    time::Duration,
};

use shared::objectid::ObjectId;
use ttl_cache::TtlCache;

use crate::{auth::generate_random_secret, models::auth::AuthTempSecret};

pub static BIND_CACHE: LazyLock<RwLock<ttl_cache::TtlCache<ObjectId, String>>> =
    LazyLock::new(|| RwLock::new(TtlCache::new(60)));
pub static TIMEOUT: LazyLock<Duration> = LazyLock::new(|| Duration::from_mins(10)); // 1 minute

pub fn generate_random_to_key() -> AuthTempSecret {
    let id = ObjectId::new();
    let secret = generate_random_secret();
    BIND_CACHE
        .write()
        .unwrap()
        .insert(id, secret.clone(), *TIMEOUT);
    AuthTempSecret { id, secret }
}

pub fn refresh(key: ObjectId) -> anyhow::Result<AuthTempSecret> {
    let contains = { BIND_CACHE.read().unwrap().contains_key(&key) };
    if contains {
        let secret = { BIND_CACHE.write().unwrap().remove(&key).unwrap() };
        {
            BIND_CACHE
                .write()
                .unwrap()
                .insert(key, secret.clone(), *TIMEOUT);
        }
        Ok(AuthTempSecret { id: key, secret })
    } else {
        Err(anyhow::anyhow!("TOTP Secret is expired"))
    }
}

pub fn get_secret_from_key(key: ObjectId) -> Option<String> {
    BIND_CACHE.write().unwrap().get(&key).cloned()
}

pub fn remove_key(key: ObjectId) {
    BIND_CACHE.write().unwrap().remove(&key);
}
