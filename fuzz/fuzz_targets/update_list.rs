//! A release's list as beize-update-fetch downloads it (read after its
//! signature is checked, but parsed strictly all the same).
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| beize_update::list::fuzz_list(data));
