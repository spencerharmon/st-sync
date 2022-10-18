use tokio::net::TcpListener;
use tokio::io::AsyncWriteExt;
use std::io;
use crossbeam_channel::*;

struct ControllerChannel {
    rx: Receiver<u64>
}
impl ControllerChannel {
    pub fn new(rx: Receiver<u64>) -> ControllerChannel {
	ControllerChannel { rx }
    }

    async fn start (self) -> Result<(), io::Error> {
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

pub struct Controller {
    tx: Sender<u64>
}
impl Controller {
    pub fn new() -> Controller {
	let (tx, rx) = bounded(1);
	let cc = ControllerChannel::new(rx);
	tokio::task::spawn(cc.start());
	Controller { tx }
    }
    pub fn send_next_beat_frame(&self, next_beat_frame: u64){
	self.tx.send(next_beat_frame);	
    }
}
