pub mod auth;
pub mod chat;

use rand::Rng;

fn gen_rand_str(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(rand::distributions::Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

fn gen_rand_u64() -> u64 {
    rand::thread_rng().gen_range(1..u64::MAX)
}
