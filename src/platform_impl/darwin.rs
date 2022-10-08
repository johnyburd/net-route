#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]

use std::{
    io::{Error, ErrorKind},
    net::IpAddr,
    ops::Add,
};
include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

#[inline(always)]
unsafe fn sa_size(sa: *const sockaddr) -> usize {
    if sa.is_null() || (*sa).sa_len == 0 {
        std::mem::size_of::<usize>()
    } else {
        (((*sa).sa_len - 1) as usize | (std::mem::size_of::<usize>() - 1)).add(1)
    }
}

pub fn default_gateway() -> Result<IpAddr, Error> {
    type LineBuf = [u8; MAXHOSTNAMELEN as usize];
    let mut needed: u64 = 0;
    let mut mib: [i32; 6] = [CTL_NET as i32, PF_ROUTE as i32, 0, 0, NET_RT_DUMP as i32, 0];
    let mut line: LineBuf = [0; MAXHOSTNAMELEN as usize];

    if unsafe {
        sysctl(
            mib.as_mut_ptr(),
            6,
            std::ptr::null_mut(),
            &mut needed,
            std::ptr::null_mut(),
            0,
        )
    } < 0
    {
        return Err(Error::new(ErrorKind::Other, "route dump estimate"));
    }

    let mut buf: Vec<u8> = Vec::with_capacity(needed as usize);

    if unsafe {
        sysctl(
            mib.as_mut_ptr(),
            6,
            buf.as_mut_ptr() as _,
            &mut needed,
            std::ptr::null_mut(),
            0,
        )
    } < 0
    {
        return Err(Error::new(ErrorKind::Other, "route dump"));
    }

    if buf.capacity() != needed as usize {
        return Err(Error::new(ErrorKind::Other, "unexpected size!"));
    }

    let rtm: *const rt_msghdr = buf.as_ptr() as *const _;
    let sa: *const sockaddr = unsafe { rtm.add(1) } as *const _;

    let sockin: *const sockaddr_in = unsafe { (sa as *const u8).add(sa_size(sa)) } as *const _;

    unsafe {
        inet_ntop(
            AF_INET as i32,
            &(*sockin).sin_addr.s_addr as *const _ as *const _,
            line.as_mut_ptr() as *mut _,
            std::mem::size_of::<LineBuf>() as u32 - 1,
        )
    };

    let router = String::from_utf8_lossy(&line);
    let router = router.trim_matches(char::from(0));

    Ok(router.parse().unwrap())
}
