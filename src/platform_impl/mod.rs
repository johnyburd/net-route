#[cfg(all(target_os = "macos", not(doc)))]
mod darwin;
#[cfg(all(target_os = "macos", not(doc)))]
pub(crate) use darwin::Handle as PlatformHandle;


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
