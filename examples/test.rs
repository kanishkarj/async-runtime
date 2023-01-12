// use async_executor::reactor::Reactor;
// pub fn main() {
//     let reactor = Reactor::new(1024, 6, 10);
//     println!("{:?}", reactor.run());
// }

use async_executor::executor::*;
use async_executor::futures::*;
use async_executor::reactor::*;
use futures::executor::block_on;
use log::{debug, error, info, log_enabled, Level};
const HTTP_RESP: &[u8] = b"HTTP/1.1 200 OK
content-type: text/html
content-length: 5

HELLO";

fn main() {
    env_logger::init();
    RUNTIME.run();

    let (mut executor, spawner) = new_executor_and_spawner();
    info!("the answer was");
    // Run the executor until the task queue is empty.
    // This will print "howdy!", pause, and then print "done!".
    // Spawn a task to print before and after waiting on a timer.
    spawner.clone().spawn((|| async move {
        let listener = MyTcpListener::bind("127.0.0.1:8000");
        loop {
            match listener.maccept().await {
                Ok(val) => {
                    // println!("got tcp stream");
                    // println!(
                    //     "{}",
                    //     std::str::from_utf8(&val.read().await.unwrap()).unwrap()
                    // );
                    spawner.spawn(async move {
                        loop {
                            if val.read().await.unwrap().len() != 0 {
                                // We are not hanlding failed writes as it is possible for the client to just disconnect off.
                                val.write(HTTP_RESP).await;
                            } else {
                                break;
                            }
                        }
                    });
                }
                val => println!("err: {:?}", val),
            }
        }
    })());

    // Drop the spawner so that our executor knows it is finished and won't
    // receive more incoming tasks to run.
    // drop(spawner.clone());
    executor.run();
    // loop {}
}
