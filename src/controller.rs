//use tokio::net::TcpListener;
use tokio::net::TcpListener;
use tokio::io::AsyncWriteExt;
use std::io;
use crossbeam_channel::*;
use std::time::Duration;
use async_std::task;


struct ControllerChannel {
}
impl ControllerChannel {
    pub fn new() -> ControllerChannel {
	ControllerChannel { }
    }

    async fn start (self, rx: Receiver<u64>) {
	let (meta_channel_tx, meta_channel_rx) = bounded::<Sender<u64>>(1);
	//dispatcher
	tokio::spawn(async move {
    	    let mut clients = Vec::new();

	    loop {
   		while let Ok(chan) = meta_channel_rx.try_recv() {
		    clients.push(chan);
		}
		task::sleep(Duration::from_millis(10)).await;
		if let Ok(frame) = rx.try_recv() {
		    for i in 0..clients.len() {
			if let Some(c) = clients.get_mut(i) {
			    println!("len: {}", c.len());
			    if let Err(_) = c.try_send(frame) {
				println!("client disconnected");
				clients.swap_remove(i);
			    }
			}
		    }
		}
	    }
	});

	//new client loop
	if let Ok(listener) = TcpListener::bind("127.0.0.1:6142").await {
	    loop {
		if let Ok((mut socket, _)) = listener.accept().await {
  		    let (new_chan_tx, new_chan_rx) = bounded::<u64>(1);
		    tokio::spawn(async move {
			println!("New client connected");
			loop {
			    if let Ok(_) = &socket.writable().await {
				if let Ok(val) = new_chan_rx.try_recv() {
				    if let Err(_) = &socket.try_write(&val.to_le_bytes()) {
					&socket.shutdown();
					break
				    }
				    println!("ctlr rx val {:?}", val);
				}
			    }
//			    tokio::task::yield_now().await;
 		            task::sleep(Duration::from_millis(10)).await;
			}
			println!("Client disconnected");
		    });
		    meta_channel_tx.try_send(new_chan_tx);
		}
	    }
	}
	panic!("control reached unknown condition");
    }
}

pub struct Controller {
    tx: Sender<u64>
}
impl Controller {
    pub fn new() -> Controller {
	let (tx, rx) = bounded(1);
	let cc = ControllerChannel::new();
	tokio::task::spawn(cc.start(rx));
	Controller { tx }
    }
    pub fn send_next_beat_frame(&self, next_beat_frame: u64){
	self.tx.send(next_beat_frame);	
    }
}
