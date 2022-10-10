#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]

use std::{
    io::{self, Error, ErrorKind},
    net::IpAddr,
    ops::Add,
    os::unix::prelude::FromRawFd,
};

use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncWriteExt},
};

use crate::Route;

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

pub(crate) struct Handle;

impl Handle {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self)
    }

    pub(crate) async fn default_gateway(&self) -> io::Result<IpAddr> {
        default_gateway()
    }

    pub(crate) async fn add(&self, route: &Route) -> io::Result<()> {
        add_route(route.destination, route.mask(), route.gateway, route.ifindex).await
    }
}

#[inline(always)]
unsafe fn sa_size(sa: *const sockaddr) -> usize {
    if sa.is_null() || (*sa).sa_len == 0 {
        std::mem::size_of::<usize>()
    } else {
        (((*sa).sa_len - 1) as usize | (std::mem::size_of::<usize>() - 1)).add(1)
    }
}

fn default_gateway() -> Result<IpAddr, Error> {
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

#[repr(C)]
#[derive(Clone, Copy)]
struct m_rtmsg {
    hdr: rt_msghdr,
    attrs: [u8; 128],
}

impl Default for sockaddr_dl {
    fn default() -> Self {
        Self {
            sdl_len: std::mem::size_of::<Self>() as u8,
            sdl_family: AF_LINK as u8,
            sdl_index: 0,
            sdl_type: 0,
            sdl_nlen: 0,
            sdl_alen: 0,
            sdl_slen: 0,
            sdl_data: [0i8; 12],
        }
    }
}

impl Default for rt_metrics {
    fn default() -> Self {
        Self {
            rmx_locks: 0,
            rmx_mtu: 0,
            rmx_hopcount: 0,
            rmx_expire: 0,
            rmx_recvpipe: 0,
            rmx_sendpipe: 0,
            rmx_ssthresh: 0,
            rmx_rtt: 0,
            rmx_rttvar: 0,
            rmx_pksent: 0,
            rmx_state: 0,
            rmx_filler: [0u32; 3],
        }
    }
}

async fn add_route(dst: IpAddr, dst_mask: IpAddr, gateway: Option<IpAddr>, ifindex: Option<u32>) -> io::Result<()> {
    let mut rtm_flags = (RTF_STATIC | RTF_UP) as i32;
    if gateway.is_some() {
        rtm_flags |= RTF_GATEWAY as i32;
    }

    let mut rtmsg = m_rtmsg {
        hdr: rt_msghdr {
            rtm_msglen: 128,
            rtm_version: RTM_VERSION as u8,
            rtm_type: RTM_ADD as u8,
            rtm_index: 0,
            rtm_flags,
            rtm_addrs: (RTA_DST | RTA_NETMASK | RTA_GATEWAY) as i32, // 7
            rtm_pid: 0,
            rtm_seq: 1,
            rtm_errno: 0,
            rtm_use: 0,
            rtm_inits: 0,
            rtm_rmx: rt_metrics::default(),
        },
        attrs: [0u8; 128],
    };

    let mut attr_offset = 0;

    match dst {
        IpAddr::V4(addr) => {
            let sa_len = std::mem::size_of::<sockaddr_in>();
            let sa_in = sockaddr_in {
                sin_len: sa_len as u8,
                sin_family: AF_INET as u8,
                sin_port: 0,
                sin_addr: unsafe { std::mem::transmute(addr.octets()) },
                sin_zero: [0i8; 8],
            };

            let sa_ptr = &sa_in as *const sockaddr_in as *const u8;
            let sa_bytes = unsafe { std::slice::from_raw_parts(sa_ptr, sa_len) };
            (&mut rtmsg.attrs[..sa_len]).copy_from_slice(sa_bytes);

            attr_offset += sa_len;
        }
        IpAddr::V6(addr) => {
            let sa_len = std::mem::size_of::<sockaddr_in6>();
            let sa_in = sockaddr_in6 {
                sin6_len: sa_len as u8,
                sin6_family: AF_INET6 as u8,
                sin6_port: 0,
                sin6_flowinfo: 0,
                sin6_addr: in6_addr {
                    __u6_addr: unsafe { std::mem::transmute(addr.octets()) },
                },
                sin6_scope_id: 0,
            };

            let sa_ptr = &sa_in as *const sockaddr_in6 as *const u8;
            let sa_bytes = unsafe { std::slice::from_raw_parts(sa_ptr, sa_len) };
            (&mut rtmsg.attrs[..sa_len]).copy_from_slice(sa_bytes);

            attr_offset += sa_len;
        }
    }

    if let Some(gateway) = gateway {
        match gateway {
            IpAddr::V4(addr) => {
                let sa_len = std::mem::size_of::<sockaddr_in>();
                let sa_in = sockaddr_in {
                    sin_len: sa_len as u8,
                    sin_family: AF_INET as u8,
                    sin_port: 0,
                    sin_addr: in_addr {
                        s_addr: unsafe { std::mem::transmute(addr.octets()) },
                    },
                    sin_zero: [0i8; 8],
                };

                let sa_ptr = &sa_in as *const sockaddr_in as *const u8;
                let sa_bytes = unsafe { std::slice::from_raw_parts(sa_ptr, sa_len) };
                (&mut rtmsg.attrs[attr_offset..attr_offset + sa_len]).copy_from_slice(sa_bytes);

                attr_offset += sa_len;
            }
            IpAddr::V6(addr) => {
                let sa_len = std::mem::size_of::<sockaddr_in6>();
                let sa_in = sockaddr_in6 {
                    sin6_len: sa_len as u8,
                    sin6_family: AF_INET6 as u8,
                    sin6_port: 0,
                    sin6_flowinfo: 0,
                    sin6_addr: in6_addr {
                        __u6_addr: unsafe { std::mem::transmute(addr.octets()) },
                    },
                    sin6_scope_id: 0,
                };

                let sa_ptr = &sa_in as *const sockaddr_in6 as *const u8;
                let sa_bytes = unsafe { std::slice::from_raw_parts(sa_ptr, sa_len) };
                (&mut rtmsg.attrs[attr_offset..attr_offset + sa_len]).copy_from_slice(sa_bytes);

                attr_offset += sa_len;
            }
        }
    }

    if let Some(ifindex) = ifindex {
        let sdl_len = std::mem::size_of::<sockaddr_dl>();
        let sa_dl = sockaddr_dl {
            sdl_len: sdl_len as u8,
            sdl_family: AF_LINK as u8,
            sdl_index: ifindex as u16,
            ..Default::default()
        };

        let sa_ptr = &sa_dl as *const sockaddr_dl as *const u8;
        let sa_bytes = unsafe { std::slice::from_raw_parts(sa_ptr, sdl_len) };
        (&mut rtmsg.attrs[attr_offset..attr_offset + sdl_len]).copy_from_slice(sa_bytes);

        attr_offset += sdl_len;
    }

    match dst_mask {
        IpAddr::V4(addr) => {
            let sa_len = std::mem::size_of::<sockaddr_in>();
            let sa_in = sockaddr_in {
                sin_len: sa_len as u8,
                sin_family: AF_INET as u8,
                sin_port: 0,
                sin_addr: in_addr {
                    s_addr: unsafe { std::mem::transmute(addr.octets()) },
                },
                sin_zero: [0i8; 8],
            };

            let sa_ptr = &sa_in as *const sockaddr_in as *const u8;
            let sa_bytes = unsafe { std::slice::from_raw_parts(sa_ptr, sa_len) };
            (&mut rtmsg.attrs[attr_offset..attr_offset + sa_len]).copy_from_slice(sa_bytes);

            attr_offset += sa_len;
        }
        IpAddr::V6(addr) => {
            let sa_len = std::mem::size_of::<sockaddr_in6>();
            let sa_in = sockaddr_in6 {
                sin6_len: sa_len as u8,
                sin6_family: AF_INET6 as u8,
                sin6_port: 0,
                sin6_flowinfo: 0,
                sin6_addr: in6_addr {
                    __u6_addr: unsafe { std::mem::transmute(addr.octets()) },
                },
                sin6_scope_id: 0,
            };

            let sa_ptr = &sa_in as *const sockaddr_in6 as *const u8;
            let sa_bytes = unsafe { std::slice::from_raw_parts(sa_ptr, sa_len) };
            (&mut rtmsg.attrs[attr_offset..attr_offset + sa_len]).copy_from_slice(sa_bytes);

            attr_offset += sa_len;
        }
    }

    let msg_len = std::mem::size_of::<rt_msghdr>() + attr_offset;
    rtmsg.hdr.rtm_msglen = msg_len as u16;

    let fd = unsafe { socket(PF_ROUTE as i32, SOCK_RAW as i32, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }

    let ptr = &rtmsg as *const m_rtmsg as *const u8;
    let len = rtmsg.hdr.rtm_msglen as usize;
    let mut f = unsafe { File::from_raw_fd(fd) };
    f.write_all(unsafe { std::slice::from_raw_parts(ptr, len) })
        .await?;

    let mut buf = [0u8; std::mem::size_of::<m_rtmsg>()];

    let amt = f.read(&mut buf).await?;

    let _ = &mut rtmsg.attrs[..amt as usize - std::mem::size_of::<rt_msghdr>()];

    Ok(())
}
