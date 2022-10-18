use net_route::{Route, Handle};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let handle = Handle::new()?;
    // warning: this may break network connecitivity
    let route = Route::new("192.168.2.0".parse().unwrap(), 26);
        // windows options
        //.with_luid(19985273102270464)
        //.with_metric(5)
        //.with_ifindex(6);
    println!("route delete {:?} {}", route, route.mask());
    handle.delete(&route).await
}