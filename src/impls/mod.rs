pub mod auth;
pub mod chat;
pub mod mediaproxy;
pub mod rest;
pub mod sync;

use std::{
    convert::TryInto,
    mem::size_of,
    time::{Duration, UNIX_EPOCH},
};

use harmony_rust_sdk::api::exports::{
    hrpc::{
        http,
        server::filters::{rate::Rate, rate_limit},
        warp::{self, filters::BoxedFilter, Filter, Reply},
    },
    prost::bytes::Bytes,
};
use rand::Rng;
use reqwest::Response;
use smol_str::SmolStr;

use crate::ServerError;

fn get_time_secs() -> u64 {
    UNIX_EPOCH
        .elapsed()
        .expect("time is before unix epoch")
        .as_secs()
}

fn gen_rand_inline_str() -> SmolStr {
    // Safety: arrays generated by gen_rand_arr are alphanumeric, so they are valid ASCII chars as well as UTF-8 chars [tag:inlined_smol_str_gen] [ref:alphanumeric_array_gen]
    let arr = gen_rand_arr::<22>();
    let str = unsafe { std::str::from_utf8_unchecked(&arr) };
    // Safety: generated array is exactly 22 u8s long
    SmolStr::new_inline(str)
}

#[allow(dead_code)]
fn gen_rand_str<const LEN: usize>() -> SmolStr {
    let arr = gen_rand_arr::<LEN>();
    // Safety: arrays generated by gen_rand_arr are alphanumeric, so they are valid ASCII chars as well as UTF-8 chars [ref:alphanumeric_array_gen]
    let str = unsafe { std::str::from_utf8_unchecked(&arr) };
    SmolStr::new(str)
}

fn gen_rand_arr<const LEN: usize>() -> [u8; LEN] {
    let mut res = [0_u8; LEN];

    for (index, ch) in rand::thread_rng()
        .sample_iter(rand::distributions::Alphanumeric) // [tag:alphanumeric_array_gen]
        .take(LEN)
        .enumerate()
    {
        // Safety: we only take `LEN` long u8s, so this can never panic
        res[index] = ch;
    }

    res
}

fn gen_rand_u64() -> u64 {
    rand::thread_rng().gen_range(1..u64::MAX)
}

fn rate(num: u64, dur: u64) -> BoxedFilter<()> {
    rate_limit(
        Rate::new(num, Duration::from_secs(dur)),
        ServerError::TooFast,
    )
    .boxed()
}

fn get_mimetype(response: &Response) -> &str {
    response
        .headers()
        .get(&http::header::CONTENT_TYPE)
        .map(|val| val.to_str().ok())
        .flatten()
        .map(|s| s.split(';').next())
        .flatten()
        .unwrap_or("application/octet-stream")
}

fn get_content_length(response: &Response) -> http::HeaderValue {
    response
        .headers()
        .get(&http::header::CONTENT_LENGTH)
        .cloned()
        .unwrap_or_else(|| unsafe {
            http::HeaderValue::from_maybe_shared_unchecked(Bytes::from_static(b"0"))
        })
}

#[inline(always)]
fn make_u64_iter_logic(raw: &[u8]) -> impl Iterator<Item = u64> + '_ {
    raw.chunks_exact(size_of::<u64>())
        .map(|raw| u64::from_be_bytes(raw.try_into().unwrap()))
}

const SCHERZO_VERSION: &str = git_version::git_version!(
    prefix = "git:",
    cargo_prefix = "cargo:",
    fallback = "unknown"
);

pub fn version() -> BoxedFilter<(impl Reply,)> {
    warp::get()
        .and(warp::path!("_harmony" / "version"))
        .map(|| format!("scherzo {}\n", SCHERZO_VERSION))
        .boxed()
}
