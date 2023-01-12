# Asynchronous Futures Runtime

A multi-threaded asynchronous runtime that executes Rust futures.

- Uses atomics and concurrent data-structures to avoid performance degradation due to lock contention. 
- The runtime uses a work-stealing cooperative scheduler to ensure even load distribution between the multiple worker threads.
- The reactor uses edge-triggered Epoll (with support for multiple threads) to notify the scheduler of the occurrence of an event.
- Apart from socket read scaling, it also supports socket accept scaling.

This was mainly a learning exercise for me to understand how epoll works and the inner workings of tokio.

`examples/test.rs` contains a TCP server I wrote using this runtime, when configured to use 5 threads it was able to handle 360K requests per second. This can be started by running `cargo run --example test`.
