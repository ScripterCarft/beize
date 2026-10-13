//! What an unprivileged caller sends beize-accountsd and beize-updated
//! before they look at who it is: Varlink messages, their JSON (sd-json),
//! method and parameters.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| beize_varlink::fuzz_request(data));
