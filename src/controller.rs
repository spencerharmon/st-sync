use tokio::net::TcpListener;
use tokio::io::AsyncWriteExt;
use std::io;
use crossbeam_channel::*;

pub struct Controller {
    rx: Receiver<u64>
}
impl Controller {
    pub fn new(rx: Receiver<u64>) -> Controller {
	Controller { rx }
    }

    pub async fn start (self) -> Result<(), io::Error> {
	let listener = TcpListener::bind("127.0.0.1:6142").await?;

	let (mut socket, _) = listener.accept().await?;

        tokio::spawn(async move {
	    loop {
		let val = self.rx.recv().unwrap().to_le_bytes();
		println!("ctlr rx val {:?}", val);

		socket.write(&val).await;
	    }
        });
	Ok(())
    }
}
