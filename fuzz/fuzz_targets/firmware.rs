//! What the firmware hands beize-tpm in the initrd, before anything in it
//! is checked: the TPM event log and the EFI variables and signature lists
//! inside it. beize-tpm is a program, so its two parser modules are taken
//! here as they are.
#![no_main]

#[allow(dead_code)]
#[path = "../../os/beize-tpm/src/efi.rs"]
mod efi;
#[allow(dead_code)]
#[path = "../../os/beize-tpm/src/eventlog.rs"]
mod eventlog;

libfuzzer_sys::fuzz_target!(|data: &[u8]| efi::fuzz_firmware(data));
