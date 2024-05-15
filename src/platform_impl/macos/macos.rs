use std::{
    ffi::CString,
    io::{self, ErrorKind},
    mem,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    os::unix::prelude::FromRawFd,
};

use async_stream::stream;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::broadcast,
    task::JoinHandle,
};

use crate::platform_impl::macos::bind::*;
use crate::{Route, RouteChange};

// see https://opensource.apple.com/source/network_cmds/network_cmds-606.40.2/netstat.tproj/route.c.auto.html
// for example C code of how the MacOS route API works.

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

        let fd = unsafe { socket(PF_ROUTE as i32, SOCK_RAW as i32, AF_UNSPEC as i32) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let route_fd = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
        route_fd.set_nonblocking(true)?;
        let tokio_fd: UnixStream = route_fd.try_into()?;

        let listen_handle = tokio::spawn(Self::listen(tx.clone(), tokio_fd));

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

    async fn listen(tx: broadcast::Sender<RouteChange>, mut sock: UnixStream) {
        let mut buf = [0u8; 2048];
        loop {
            let read = sock.read(&mut buf).await.expect("sock read err");
            assert!(read > 0);
            // NOTE: we don't know it's safe to read past type yet!
            // https://man.freebsd.org/cgi/man.cgi?query=route&apropos=0&sektion=4&manpath=FreeBSD+7.2-RELEASE&format=html
            let hdr: &rt_msghdr = unsafe { mem::transmute(buf.as_mut_ptr()) };
            if !matches!(hdr.rtm_type as u32, RTM_ADD | RTM_DELETE | RTM_CHANGE) {
                continue;
            }
            const HDR_SIZE: usize = mem::size_of::<rt_msghdr>();
            assert!(read >= HDR_SIZE);
            let route = message_to_route(hdr, &buf[HDR_SIZE..read]);

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

fn message_to_route(hdr: &rt_msghdr, msg: &[u8]) -> Option<Route> {
    let mut gateway = None;

    // check if message has no destination
    if hdr.rtm_addrs & (1 << RTAX_DST) == 0 {
        return None;
    }

    // The body of the route message (msg) is a list of `struct sockaddr`. However, thanks to v6,
    // the size

    // See https://opensource.apple.com/source/network_cmds/network_cmds-606.40.2/netstat.tproj/route.c.auto.html,
    // function `get_rtaddrs()`
    let mut route_addresses = [None; RTAX_MAX as usize];
    let mut cur_pos = 0;
    for idx in 0..RTAX_MAX as usize {
        if hdr.rtm_addrs & (1 << idx) != 0 {
            let buf = &msg[cur_pos..];
            if buf.len() < mem::size_of::<sockaddr>() {
                continue;
            }
            assert!(buf.len() >= std::mem::size_of::<sockaddr>());
            let sa: &sockaddr = unsafe { &*(buf.as_ptr() as *const sockaddr) };
            assert!(buf.len() >= sa.sa_len as usize);
            route_addresses[idx] = Some(sa);

            // see ROUNDUP() macro in the route.c file linked above.
            // The len needs to be a multiple of 4bytes
            let aligned_len = if sa.sa_len == 0 {
                4
            } else {
                ((sa.sa_len - 1) | 0x3) + 1
            };
            cur_pos += aligned_len as usize;
        }
    }

    let destination = sa_to_ip(route_addresses[RTAX_DST as usize].unwrap())?;

    let mut prefix = match destination {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    };

    // check if message has a gateway
    if hdr.rtm_addrs & (1 << RTAX_GATEWAY) != 0 {
        let gw_sa = route_addresses[RTAX_GATEWAY as usize].unwrap();
        gateway = sa_to_ip(gw_sa);
        if let Some(IpAddr::V6(v6gw)) = gateway {
            // unicast link local start with FE80::
            let is_unicast_ll = v6gw.segments()[0] == 0xfe80;
            // v6 multicast starts with FF
            let is_multicast = v6gw.octets()[0] == 0xff;
            // lower 4 bit of byte1 encode the multicast scope
            let multicast_scope = v6gw.octets()[1] & 0x0f;
            // scope 1 is interface/node-local. scope 2 is link-local
            // RFC4291, Sec. 2.7 for the gory details
            if is_unicast_ll || (is_multicast && (multicast_scope == 1 || multicast_scope == 2)) {
                // how fun. So it looks like some kernels encode the scope_id of the v6 address in
                // byte 2 & 3 of the gateway IP, if it's unicast link_local, or multicast with interface-local
                // or link-local scope. So we need to set these two bytes to 0 to turn it into the
                // real gateway address
                // Logic again taken from route.c (see link above), function `p_sockaddr()`
                let segs = v6gw.segments();
                gateway = Some(IpAddr::V6(Ipv6Addr::new(
                    segs[0], 0, segs[2], segs[3], segs[4], segs[5], segs[6], segs[7],
                )))
            }
        }
    }

    // check if message has netmask
    if hdr.rtm_addrs & (1 << RTAX_NETMASK) != 0 {
        match route_addresses[RTAX_NETMASK as usize] {
            None => prefix = 0,
            // Yes, apparently a 0 prefixlen is encoded as having an sa_len of 0
            // (at least in some cases).
            Some(sa) if sa.sa_len == 0 => prefix = 0,
            Some(sa) => match destination {
                IpAddr::V4(_) => {
                    let mask_sa: &sockaddr_in = unsafe { std::mem::transmute(sa) };
                    prefix = u32::from_be(mask_sa.sin_addr.s_addr).leading_ones() as u8;
                }
                IpAddr::V6(_) => {
                    let mask_sa: &sockaddr_in6 = unsafe { std::mem::transmute(sa) };
                    // sin6_addr.__u6_addr is a union that represents the 16 v6 bytes either as
                    // 16 u8's or 16 u16's or 4 u32's. So we need the unsafe here because of the union
                    prefix = u128::from_be_bytes(unsafe { mask_sa.sin6_addr.__u6_addr.__u6_addr8 })
                        .leading_ones() as u8;
                }
            },
        }
    }

    Some(Route {
        destination,
        prefix,
        gateway,
        ifindex: Some(hdr.rtm_index as u32),
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
        unsafe { mem::zeroed() }
    }
}

fn sa_to_ip(sa: &sockaddr) -> Option<IpAddr> {
    match sa.sa_family as u32 {
        AF_INET => {
            assert!(sa.sa_len as usize >= std::mem::size_of::<sockaddr_in>());
            let inet: &sockaddr_in = unsafe { std::mem::transmute(sa) };
            let octets: [u8; 4] = inet.sin_addr.s_addr.to_ne_bytes();
            Some(IpAddr::from(octets))
        }
        AF_INET6 => {
            assert!(sa.sa_len as usize >= std::mem::size_of::<sockaddr_in6>());
            let inet6: &sockaddr_in6 = unsafe { std::mem::transmute(sa) };
            let octets: [u8; 16] = unsafe { inet6.sin6_addr.__u6_addr.__u6_addr8 };
            Some(IpAddr::from(octets))
        }
        AF_LINK => None,
        _ => None,
    }
}

#[allow(dead_code)] // currently unused but lets leave it in since it might come in handy
fn sa_to_link(sa: &sockaddr) -> Option<(Option<[u8; 6]>, u16)> {
    match sa.sa_family as u32 {
        AF_LINK => {
            assert!(sa.sa_len as usize >= std::mem::size_of::<sockaddr_dl>());
            let sa_dl: &sockaddr_dl = unsafe { std::mem::transmute(sa) };
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

fn try_get_msg_buf() -> io::Result<(Vec<u8>, usize)> {
    const MAX_RETRYS: usize = 3;

    for _ in 0..MAX_RETRYS {
        let mut mib: [u32; 6] = [0; 6];
        let mut len = 0;

        mib[0] = CTL_NET;
        mib[1] = AF_ROUTE;
        mib[2] = 0;
        mib[3] = 0; // family: ipv4 & ipv6
        mib[4] = NET_RT_DUMP;
        // mib[5] flags: 0

        // see: https://github.com/golang/net/blob/ec05fdcd71141c885f3fb84c41d1c692f094ccbe/route/route.go#L126
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
            // will retry return error if
            continue;
        } else {
            return Ok((msgs_buf, len));
        }
    }

    Err(io::Error::new(
        io::ErrorKind::Other,
        "Failed to get routing table",
    ))
}

async fn list_routes() -> io::Result<Vec<Route>> {
    let (mut msgs_buf, len) = try_get_msg_buf()?;

    let mut routes = vec![];
    let mut offset = 0;

    // Note: we need to check against the `len` that `sysctl` returned which might
    // be smaller than the size of `msgs_buf`
    while offset + std::mem::size_of::<rt_msghdr>() <= len {
        let buf = &mut msgs_buf[offset..];

        assert!(buf.len() >= std::mem::size_of::<rt_msghdr>());

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

        if let Some(route) = message_to_route(rt_hdr, rt_msg) {
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

    let fd = unsafe { socket(PF_ROUTE as i32, SOCK_RAW as i32, AF_UNSPEC as i32) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }

    let slice = {
        let ptr = &rtmsg as *const m_rtmsg as *const u8;
        let len = rtmsg.hdr.rtm_msglen as usize;
        unsafe { std::slice::from_raw_parts(ptr, len) }
    };
    let route_fd = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
    let mut f: UnixStream = route_fd.try_into()?;

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
