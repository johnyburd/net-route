
use futures::StreamExt;
use net_route::Handle;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let handle = Handle::new()?;
    let stream = handle.route_listen_stream();

    futures::pin_mut!(stream);

    println!("Listening for route events, press Ctrl+C to cancel...");
    while let Some(value) = stream.next().await {
        println!("{:?}", value);
    }
    Ok(())
}
