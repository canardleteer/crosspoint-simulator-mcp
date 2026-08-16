//! Hand-written module tree for connectrpc `file_per_package=true` output.

#![allow(
    non_camel_case_types,
    dead_code,
    unused_imports,
    unused_qualifications,
    clippy::derivable_impls,
    clippy::match_single_binding,
    clippy::uninlined_format_args,
    clippy::doc_lazy_continuation,
    clippy::module_inception
)]

pub mod crosspoint {
    pub mod sim {
        pub mod control {
            pub mod v1alpha1 {
                include!("crosspoint.sim.control.v1alpha1.rs");
            }
        }
    }
}
