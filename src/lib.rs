use std::{io, net::IpAddr};

mod platform_impl;


/// Fetch next-hop gateway for one of the default routes on the system
pub fn default_gateway() -> Result<IpAddr, io::Error> {
    platform_impl::default_gateway()
}

#[cfg(test)]
mod tests {
    use crate::default_gateway;

    #[test]
    fn it_gets_default_gateway() {
        let route = default_gateway().unwrap();
        println!("default route: {}", route);
    }
}
