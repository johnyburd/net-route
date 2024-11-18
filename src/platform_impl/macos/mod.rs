mod bind;

#[allow(clippy::module_inception)]
pub(crate) mod macos;

pub(crate) use macos::Handle;

pub use macos::ifname_to_index;
