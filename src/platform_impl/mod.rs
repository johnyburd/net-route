#[cfg(all(target_os = "macos", not(doc)))]
mod macos;
#[cfg(all(target_os = "macos", not(doc)))]
pub use macos::ifname_to_index;
#[cfg(all(target_os = "macos", not(doc)))]
pub(crate) use macos::Handle as PlatformHandle;

#[cfg(all(target_os = "linux", not(doc)))]
mod linux;
#[cfg(all(target_os = "linux", not(doc)))]
pub(crate) use linux::Handle as PlatformHandle;

#[cfg(all(target_os = "windows", not(doc)))]
mod windows;
#[cfg(all(target_os = "windows", not(doc)))]
pub(crate) use self::windows::Handle as PlatformHandle;

#[cfg(doc)]
pub(crate) struct PlatformHandle;
