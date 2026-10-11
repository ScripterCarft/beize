//! Booting the disk image in QEMU: interactively (`bz run`) and as the
//! automated end-to-end test (`bz smoke`).

mod checks;
mod console;
mod debug;
mod machine;
mod report;
mod run;
mod serve;
mod smoke;

pub use debug::debug;
pub use run::run;
pub use smoke::smoke;
