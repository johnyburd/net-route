#[cfg(all(target_os = "macos", not(doc)))]
mod darwin;
#[cfg(all(target_os = "macos", not(doc)))]
pub(crate) use darwin::default_gateway;


#[cfg(all(target_os = "linux", not(doc)))]
mod linux;
#[cfg(all(target_os = "linux", not(doc)))]
pub(crate) use linux::default_gateway;
