//! Beize's own Wayland protocols, generated from their XML at compile time
//! as wayland-protocols generates the upstream ones: `client` for the
//! shell, `server` for the compositor.
//!
//! - `beize-shell-v1.xml`: the windows as the shell sees them (ADR 0028 §1).

#![allow(missing_docs, clippy::all)]

macro_rules! protocol {
    ($path:literal) => {
        // The scanner reads the XML while compiling but does not tell
        // Cargo: read here too, a changed protocol rebuilds the crate.
        const _: &str = include_str!(concat!("../", $path));

        #[cfg(feature = "client")]
        pub mod client {
            #![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables)]
            #![allow(non_upper_case_globals, non_snake_case, unused_imports)]
            use wayland_client;
            use wayland_client::protocol::*;

            pub mod __interfaces {
                use wayland_client::protocol::__interfaces::*;
                wayland_scanner::generate_interfaces!($path);
            }
            use self::__interfaces::*;

            wayland_scanner::generate_client_code!($path);
        }

        #[cfg(feature = "server")]
        pub mod server {
            #![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables)]
            #![allow(non_upper_case_globals, non_snake_case, unused_imports)]
            use wayland_server;
            use wayland_server::protocol::*;

            pub mod __interfaces {
                use wayland_server::protocol::__interfaces::*;
                wayland_scanner::generate_interfaces!($path);
            }
            use self::__interfaces::*;

            wayland_scanner::generate_server_code!($path);
        }
    };
}

pub mod shell {
    protocol!("beize-shell-v1.xml");
}
