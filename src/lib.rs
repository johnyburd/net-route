use std::{io, net::IpAddr};

mod platform_impl;

pub fn default_route() -> Result<IpAddr, io::Error> {
    platform_impl::default_route()
}

#[cfg(test)]
mod tests {
    use crate::default_route;

    #[test]
    fn it_gets_default_route() {
        let route = default_route().unwrap();
        println!("default route: {}", route);
    }
}
