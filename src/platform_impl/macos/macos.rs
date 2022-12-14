use std::{
    ffi::CString,
    io::{self, ErrorKind},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    os::unix::prelude::FromRawFd,
};

use async_stream::stream;
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncWriteExt},
    sync::broadcast,
    task::JoinHandle,
};

use crate::platform_impl::macos::bind::*;
use crate::{Route, RouteChange};

pub fn ifname_to_index(name: &str) -> Option<u32> {
    let name = CString::new(name).ok()?;
    let idx = unsafe { if_nametoindex(name.as_ptr()) };
    if idx != 0 {
        Some(idx)
    } else {
        None
    }
}

pub(crate) struct Handle {
    tx: broadcast::Sender<RouteChange>,
    listen_handle: JoinHandle<()>,
}

impl Handle {
    pub(crate) fn new() -> io::Result<Self> {
        // TODO wait until user registers a listener to open the socket
        let (tx, _) = broadcast::channel::<RouteChange>(16);

        let fd = unsafe { socket(PF_ROUTE as i32, SOCK_RAW as i32, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let route_fd = unsafe { File::from_raw_fd(fd) };

        let listen_handle = tokio::spawn(Self::listen(tx.clone(), route_fd));

        Ok(Self { tx, listen_handle })
    }

    pub(crate) async fn default_route(&self) -> io::Result<Option<Route>> {
        for route in self.list().await? {
            if (route.destination == Ipv4Addr::UNSPECIFIED
                || route.destination == Ipv6Addr::UNSPECIFIED)
                && route.prefix == 0
                && route.gateway != Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
                && route.gateway != Some(IpAddr::V6(Ipv6Addr::UNSPECIFIED))
            {
                return Ok(Some(route));
            }
        }
        Ok(None)
    }

    pub(crate) fn route_listen_stream(&self) -> impl futures::Stream<Item = RouteChange> {
        let mut rx = self.tx.subscribe();
        stream! {
            loop {
                match rx.recv().await {
                    Ok(ev) => yield ev,
                    Err(e) => match e {
                        broadcast::error::RecvError::Closed => break,
                        broadcast::error::RecvError::Lagged(_) => continue,
                    }
                }
            }
        }
    }

    pub(crate) async fn delete(&self, route: &Route) -> io::Result<()> {
        add_or_del_route(route.destination, route.mask(), None, None, false).await
    }

    pub(crate) async fn add(&self, route: &Route) -> io::Result<()> {
        add_or_del_route(
            route.destination,
            route.mask(),
            route.gateway,
            route.ifindex,
            true,
        )
        .await
    }

    pub(crate) async fn list(&self) -> io::Result<Vec<Route>> {
        list_routes().await
    }

    async fn listen(tx: broadcast::Sender<RouteChange>, mut sock: File) {
        let mut buf = [0u8; 2048];
        loop {
            // TODO: should probably use this
            let _read = sock.read(&mut buf).await.expect("sock read err");
            let hdr: &rt_msghdr;
            let route = unsafe {
                hdr = std::mem::transmute(buf.as_mut_ptr());
                let msg = (hdr as *const rt_msghdr).add(1) as *mut u8;
                message_to_route(hdr, msg)
            };

            if let Some(route) = route {
                _ = tx.send(match hdr.rtm_type as u32 {
                    RTM_ADD => RouteChange::Add(route),
                    RTM_DELETE => RouteChange::Delete(route),
                    RTM_CHANGE => RouteChange::Change(route),
                    _ => continue,
                });
            }
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.listen_handle.abort();
    }
}

fn message_to_route(hdr: &rt_msghdr, msg: *mut u8) -> Option<Route> {
    let destination;
    let mut gateway = None;
    let mut ifindex = None;

    // check if message has no destination
    if hdr.rtm_addrs & (1 << RTAX_DST) == 0 {
        return None;
    }

    unsafe {
        let dst_sa: &sockaddr = std::mem::transmute((msg as *mut sockaddr).add(RTAX_DST as usize));
        destination = sa_to_ip(dst_sa)?;
    }

    let mut prefix = match destination {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    };

    // check if message has a gateway
    if hdr.rtm_addrs & (1 << RTAX_GATEWAY) != 0 {
        unsafe {
            //let gw_sa: &sockaddr = std::mem::transmute(msg);
            let gw_sa: &sockaddr =
                std::mem::transmute((msg as *mut sockaddr).add(RTAX_GATEWAY as usize));

            // try to convert sockaddr to ip
            gateway = sa_to_ip(gw_sa);
            // if that fails try to convert it to a link
            if gateway.is_none() {
                if let Some((_mac, ifidx)) = sa_to_link(gw_sa) {
                    // TODO do something with mac?
                    ifindex = Some(ifidx as u32);
                }
            }
        }
    }

    // check if message has netmask
    if hdr.rtm_addrs & (1 << RTAX_NETMASK) != 0 {
        unsafe {
            match destination {
                IpAddr::V4(_) => {
                    let mask_sa: &sockaddr_in =
                        std::mem::transmute((msg as *mut sockaddr).add(RTAX_NETMASK as usize));
                    let octets: [u8; 4] = mask_sa.sin_addr.s_addr.to_ne_bytes();
                    prefix = u32::from_be_bytes(octets).leading_ones() as u8;
                }
                IpAddr::V6(_) => {
                    let mask_sa: &sockaddr_in6 =
                        std::mem::transmute((msg as *mut sockaddr).add(RTAX_NETMASK as usize));
                    let octets: [u8; 16] = mask_sa.sin6_addr.__u6_addr.__u6_addr8;
                    prefix = u128::from_be_bytes(octets).leading_ones() as u8;
                }
            }
        }
    }

    Some(Route {
        destination,
        prefix,
        gateway,
        ifindex,
    })
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(non_camel_case_types)]
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
        AF_LINK => None,
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

    let mut routes = vec![];
    let mut offset = 0;

    loop {
        let buf = &mut msgs_buf[offset..];

        if buf.len() < std::mem::size_of::<rt_msghdr>() {
            break;
        }

        let rt_hdr = unsafe { std::mem::transmute::<_, &rt_msghdr>(buf.as_ptr()) };
        assert_eq!(rt_hdr.rtm_version as u32, RTM_VERSION);
        if rt_hdr.rtm_errno != 0 {
            return Err(code_to_error(rt_hdr.rtm_errno));
        }

        let msg_len = rt_hdr.rtm_msglen as usize;
        offset += msg_len;

        if rt_hdr.rtm_flags as u32 & RTF_WASCLONED != 0 {
            continue;
        }
        let rt_msg = &mut buf[std::mem::size_of::<rt_msghdr>()..msg_len];

        if let Some(route) = message_to_route(rt_hdr, rt_msg.as_mut_ptr()) {
            routes.push(route);
        }
    }

    Ok(routes)
}

fn code_to_error(err: i32) -> io::Error {
    let kind = match err {
        17 => io::ErrorKind::AlreadyExists, // EEXIST
        3 => io::ErrorKind::NotFound,       // ESRCH
        3436 => io::ErrorKind::OutOfMemory, // ENOBUFS
        _ => io::ErrorKind::Other,
    };

    io::Error::new(kind, format!("rtm_errno {}", err))
}

async fn add_or_del_route(
    dst: IpAddr,
    dst_mask: IpAddr,
    gateway: Option<IpAddr>,
    ifindex: Option<u32>,
    add: bool,
) -> io::Result<()> {
    let mut rtm_flags = (RTF_STATIC | RTF_UP) as i32;
    // TODO not sure about this !add
    if gateway.is_some() || !add {
        rtm_flags |= RTF_GATEWAY as i32;
    }

    let mut rtm_addrs = RTA_DST | RTA_NETMASK;
    if add {
        rtm_addrs |= RTA_GATEWAY;
    }

    let rtm_type = if add { RTM_ADD } else { RTM_DELETE } as u8;

    let mut rtmsg = m_rtmsg {
        hdr: rt_msghdr {
            rtm_msglen: 128,
            rtm_version: RTM_VERSION as u8,
            rtm_type,
            rtm_index: 0,
            rtm_flags,
            rtm_addrs: rtm_addrs as i32,
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

    let slice = {
        let ptr = &rtmsg as *const m_rtmsg as *const u8;
        let len = rtmsg.hdr.rtm_msglen as usize;
        unsafe { std::slice::from_raw_parts(ptr, len) }
    };
    let mut f = unsafe { File::from_raw_fd(fd) };

    f.write_all(slice).await?;

    let mut buf = [0u8; std::mem::size_of::<m_rtmsg>()];

    let read = f.read(&mut buf).await?;

    if read < std::mem::size_of::<rt_msghdr>() {
        return Err(io::Error::new(ErrorKind::Other, "Unexpected message len"));
    }

    let rt_hdr: &rt_msghdr = unsafe { std::mem::transmute(buf.as_ptr()) };
    assert_eq!(rt_hdr.rtm_version as u32, RTM_VERSION);
    if rt_hdr.rtm_errno != 0 {
        return Err(code_to_error(rt_hdr.rtm_errno));
    }

    Ok(())
}
