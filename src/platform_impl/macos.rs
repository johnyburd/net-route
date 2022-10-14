#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]

use std::{
    cmp::min,
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
        add_route(
            route.destination,
            route.mask(),
            route.gateway,
            route.ifindex,
        )
        .await
    }

    pub(crate) async fn list(&self) -> io::Result<Vec<Route>> {
        list_routes().await
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

unsafe fn sa_to_ip(sa: &sockaddr) -> Option<IpAddr> {
    match sa.sa_family as u32 {
        AF_INET => {
            let inet: &sockaddr_in = std::mem::transmute(sa);
            let octets: [u8; 4] = inet.sin_addr.s_addr.to_ne_bytes();
            Some(IpAddr::from(octets))
        }
        AF_INET6 => {
            let inet6: &sockaddr_in6 = std::mem::transmute(sa);
            let octets: [u8; 16] = inet6.sin6_addr.__u6_addr.__u6_addr8;
            Some(IpAddr::from(octets))
        }
        AF_LINK => {
            // TODO
            None
        }
        _ => None,
    }
}

unsafe fn sa_to_link(sa: &sockaddr) -> Option<(Option<[u8; 6]>, u16)> {
    match sa.sa_family as u32 {
        AF_LINK => {
            let sa_dl = sa as *const _ as *const sockaddr_dl;
            let ifindex = (*sa_dl).sdl_index;
            let mac;
            if (*sa_dl).sdl_alen == 6 {
                let i = (*sa_dl).sdl_nlen as usize;

                let a = (*sa_dl).sdl_data[i + 0] as u8;
                let b = (*sa_dl).sdl_data[i + 1] as u8;
                let c = (*sa_dl).sdl_data[i + 2] as u8;
                let d = (*sa_dl).sdl_data[i + 3] as u8;
                let e = (*sa_dl).sdl_data[i + 4] as u8;
                let f = (*sa_dl).sdl_data[i + 5] as u8;
                mac = Some([a, b, c, d, e, f]);
            } else {
                mac = None;
            }
            Some((mac, ifindex))
        }
        _ => None,
    }
}

#[inline]
const fn align(len: usize) -> usize {
    (len + 3) & !3
}

async fn list_routes() -> io::Result<Vec<Route>> {
    let mut mib: [u32; 6] = [0; 6];
    let mut len = 0;

    mib[0] = CTL_NET;
    mib[1] = AF_ROUTE;
    mib[2] = 0;
    mib[3] = 0; // family: ipv4 & ipv6
    mib[4] = NET_RT_DUMP;
    // mib[5] flags: 0

    if unsafe {
        sysctl(
            &mut mib as *mut _ as *mut _,
            6,
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }

    let mut msgs_buf: Vec<u8> = vec![0; len as usize];

    if unsafe {
        sysctl(
            &mut mib as *mut _ as *mut _,
            6,
            msgs_buf.as_mut_ptr() as _,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }

    // TODO we know capacity here
    let mut routes = vec![];

    let mut offset = 0;
    loop {
        let buf = &mut msgs_buf[offset..];

        println!("remaining {}", buf.len());
        if buf.len() < std::mem::size_of::<rt_msghdr>() {
            break;
        }

        let rt_hdr = unsafe { std::mem::transmute::<_, &rt_msghdr>(buf.as_ptr()) };
        //assert!(rt_hdr.rtm_addrs < RTAX_MAX as i32);
        assert_eq!(rt_hdr.rtm_version as u32, RTM_VERSION);
        if rt_hdr.rtm_errno != 0 {
            return Err(io::Error::new(
                ErrorKind::Other,
                format!("Rtm err {}", rt_hdr.rtm_errno),
            ));
        }

        let msg_len = rt_hdr.rtm_msglen as usize;
        println!("msg len {}", msg_len);
        offset += msg_len;

        //let rtm_pkt = &mut buf[..msg_len];
        //assert!(rtm_pkt.len() >= msg_len);
        let mut rt_msg = &mut buf[std::mem::size_of::<rt_msghdr>()..msg_len];

        // check if route has a destination
        if rt_hdr.rtm_addrs & (1 << RTAX_DST) == 0 {
            continue;
        }

        let sa = unsafe { std::mem::transmute::<_, &sockaddr>(rt_msg.as_ptr()) };
        let sa_len = sa.sa_len as usize;
        let dst = match unsafe { sa_to_ip(sa) } {
            None => continue,
            Some(dst) => dst,
        };

        rt_msg = &mut rt_msg[align(sa_len)..];

        let mut gw = None;
        let mut ifindex = None;

        if rt_hdr.rtm_addrs & (1 << RTAX_GATEWAY) != 0 {
            println!("some gateway");
            unsafe {
                let gw_sa = std::mem::transmute::<_, &sockaddr>(rt_msg.as_ptr());
                gw = sa_to_ip(gw_sa);
                if let Some((maybe_mac, ifidx)) = sa_to_link(gw_sa) {
                    // TODO
                    ifindex = Some(ifidx as u32);
                }
                rt_msg = &mut rt_msg[align(gw_sa.sa_len.into())..];
            }
        } else {
            println!("No gateway")
        }

        let mut mask = None;
        if rt_hdr.rtm_addrs & (1 << RTAX_NETMASK) != 0 {
            let mask_len = rt_msg[0];
            mask = match dst {
                IpAddr::V4(_) if mask_len > 0 => {
                    let mut octets = [0u8; 4];
                    println!("mask len {}", mask_len);
                    let mask_len = min(mask_len - 1, 4) as usize;
                    (&mut octets[..mask_len]).copy_from_slice(&rt_msg[1..mask_len + 1]);
                    Some(IpAddr::from(octets))
                }
                //IpAddr::V4(_) if mask_len == 0 => Some(IpAddr::from([0u8; 4])),
                IpAddr::V6(_) if mask_len > 0 => {
                    let mut octets = [0u8; 16];
                    let mask_len = min(mask_len - 1, 16) as usize;
                    println!("mask len {}", mask_len);
                    (&mut octets[..mask_len]).copy_from_slice(&rt_msg[1..mask_len + 1]);
                    Some(IpAddr::from(octets))
                }
                //IpAddr::V6(_) if mask_len == 0 => Some(IpAddr::from([0u8; 16])),
                //_ => unreachable!()
                _ => None,
            }
        }

        let prefix_len = mask
            .map(|addr| match addr {
                IpAddr::V4(addr) => {
                    let mask = u32::from(addr);
                    println!("mask {:032b}", !mask);
                    (!mask).leading_zeros() as u8
                }
                IpAddr::V6(addr) => {
                    let mask = u128::from(addr);
                    println!("mask {:0128b}", !mask);
                    (!mask).leading_zeros() as u8
                }
            })
            .unwrap_or_else(|| match dst {
                // TODO not sure if this is right
                IpAddr::V4(addr) => 32 - u32::from(addr).trailing_zeros(),
                IpAddr::V6(addr) => 128 - u128::from(addr).trailing_zeros(),
            } as u8);

        let prefix_len = match dst {
            // TODO not sure if this is right
            IpAddr::V4(addr) => 32 - u32::from(addr).trailing_zeros(),
            IpAddr::V6(addr) => 128 - u128::from(addr).trailing_zeros(),
        } as u8;

        let mut route = Route::new(dst, prefix_len);
        route.gateway = gw;
        route.ifindex = ifindex;

        if let Some(mask) = mask {
            println!("len {}", prefix_len);
            //assert_eq!(route.mask(), mask);
        }
        routes.push(route);

        //let prefix_len = let mask = u32::from(mask);

        //let prefix = (!mask).leading_zeros() as u8;
    }

    Ok(routes)
}

async fn add_route(
    dst: IpAddr,
    dst_mask: IpAddr,
    gateway: Option<IpAddr>,
    ifindex: Option<u32>,
) -> io::Result<()> {
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
