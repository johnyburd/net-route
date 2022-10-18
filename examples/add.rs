use net_route::{Route, Handle};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let handle = Handle::new()?;
    let route = Route::new("192.168.2.0".parse().unwrap(), 26)
        // windows options
        //.with_luid(19985273102270464)
        //.with_metric(5)
        //.with_ifindex(6)
        .with_gateway("192.168.2.1".parse().unwrap());
    println!("route add {:?}", route);
    handle.add(&route).await
}