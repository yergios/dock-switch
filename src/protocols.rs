//! Bindings for COSMIC's private Wayland protocols, generated from the XML in
//! `protocols/` (copied unchanged from pop-os/cosmic-protocols, `unstable/`).
//! The `cosmic-protocols` crate is avoided because it is GPL-3.0-only, while
//! the XML files themselves carry permissive notices.

#![allow(
    dead_code,
    non_camel_case_types,
    non_upper_case_globals,
    non_snake_case
)]
#![allow(unused_imports, unused_unsafe, unused_variables, clippy::all)]

// Only needed because the deprecated members of toplevel-info reference it.
pub mod workspace {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::backend as wayland_backend;
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/cosmic-workspace-unstable-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/cosmic-workspace-unstable-v1.xml");
}

pub mod toplevel_info {
    use super::workspace::*;
    use wayland_client;
    use wayland_client::protocol::*;
    use wayland_protocols::ext::foreign_toplevel_list::v1::client::*;
    use wayland_protocols::ext::workspace::v1::client::*;

    pub mod __interfaces {
        use super::super::workspace::__interfaces::*;
        use wayland_client::backend as wayland_backend;
        use wayland_client::protocol::__interfaces::*;
        use wayland_protocols::ext::foreign_toplevel_list::v1::client::__interfaces::*;
        use wayland_protocols::ext::workspace::v1::client::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/cosmic-toplevel-info-unstable-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/cosmic-toplevel-info-unstable-v1.xml");
}

pub mod toplevel_management {
    use super::toplevel_info::*;
    use super::workspace::*;
    use wayland_client;
    use wayland_client::protocol::*;
    use wayland_protocols::ext::workspace::v1::client::*;

    pub mod __interfaces {
        use super::super::toplevel_info::__interfaces::*;
        use super::super::workspace::__interfaces::*;
        use wayland_client::backend as wayland_backend;
        use wayland_client::protocol::__interfaces::*;
        use wayland_protocols::ext::workspace::v1::client::__interfaces::*;
        wayland_scanner::generate_interfaces!(
            "protocols/cosmic-toplevel-management-unstable-v1.xml"
        );
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/cosmic-toplevel-management-unstable-v1.xml");
}
