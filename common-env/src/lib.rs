//! Common environment variables uses by various components

use once_cell::sync::OnceCell;
use std::env;
use std::str::FromStr;

static REQUEST_BUNDLE_SIZE: OnceCell<usize> = OnceCell::new();
pub const ENV_NAME_BUNDLE_SIZE: &str = "REQUEST_BUNDLE_SIZE";
pub const DEFAULT_REQUEST_BUNDLE_SIZE: usize = 50;
pub fn request_bundle_size() -> usize {
    *REQUEST_BUNDLE_SIZE.get_or_init(|| {
        env::var(ENV_NAME_BUNDLE_SIZE)
            .map_err(|_| ())
            .and_then(|port| usize::from_str(&port).map_err(|_| ()))
            .unwrap_or(DEFAULT_REQUEST_BUNDLE_SIZE)
    })
}

const ENV_NAME_WAIT_ON_NO_CONNECTION: &str = "WAIT_ON_NO_CONNECTION";
const DEFAULT_WAIT_ON_NO_CONNECTION: u64 = 100;
pub fn wait_on_no_connection() -> u64 {
    env::var(ENV_NAME_WAIT_ON_NO_CONNECTION)
        .map_err(|_| ())
        .and_then(|val| u64::from_str(&val).map_err(|_| ()))
        .unwrap_or(DEFAULT_WAIT_ON_NO_CONNECTION)
}
