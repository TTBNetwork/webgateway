use sha2::{Digest, Sha256};
pub fn hash_domains<S: AsRef<str>>(domains: &[S]) -> String {
    let mut hasher = Sha256::new();
    for d in domains {
        hasher.update(d.as_ref().as_bytes());
    }
    // let slice = hasher.finalize().as_slice();
    hex::encode(hasher.finalize())
}
