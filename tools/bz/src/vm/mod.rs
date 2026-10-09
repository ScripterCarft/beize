//! Booting the disk image in QEMU: interactively (`bz run`) and as the
//! automated end-to-end test (`bz smoke`).

mod checks;
mod console;
mod machine;
mod report;
mod run;
mod smoke;

pub use run::run;
pub use smoke::smoke;
