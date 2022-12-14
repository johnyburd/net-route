mod bind;
pub(crate) mod macos;

pub(crate) use macos::Handle;

pub use macos::ifname_to_index;
