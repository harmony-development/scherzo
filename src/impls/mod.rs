pub mod auth;
pub mod chat;
pub mod mediaproxy;
pub mod rest;

use std::time::Duration;

use harmony_rust_sdk::api::exports::hrpc::{
    server::filters::{rate::Rate, rate_limit},
    warp::{filters::BoxedFilter, Filter},
};
use rand::Rng;

use crate::ServerError;

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

fn rate(num: u64, dur: u64) -> BoxedFilter<()> {
    rate_limit(
        Rate::new(num, Duration::from_secs(dur)),
        ServerError::TooFast,
    )
    .boxed()
}

#[macro_export]
macro_rules! concat_static {
    ( $len:expr, $first_arr:expr, $( $array:expr ),+ ) => {
        {
            let mut new = [0; $len];
            for (to, from) in new.iter_mut().zip(
                $first_arr
                    .iter()
                    $(
                        .chain($array.iter())
                    )+
            ) {
                *to = *from;
            }
            new
        }
    };
}
