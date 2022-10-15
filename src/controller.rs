use tokio::net::TcpListener;
use tokio::io::AsyncWriteExt;
use std::io;
use crossbeam_channel::*;

pub struct Controller { }
impl Controller {
    pub fn new() -> Controller {
	Controller {  }
    }

    pub async fn start (self, rx: Receiver<u32>) -> Result<(), io::Error> {
	let listener = TcpListener::bind("127.0.0.1:6142").await?;

	let (mut socket, _) = listener.accept().await?;

        tokio::spawn(async move {
	    loop {
		socket.write_u32(rx.recv().unwrap()).await;
	    }
        });
	Ok(())
    }
}
