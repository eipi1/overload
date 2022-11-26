# Run on Docker

Application can be run on docker in standalone/single instance mode. It requires container images 
tagged as *standalone-{version}*.

For example to run the latest snapshot

```shell
docker run -p 3030:3030 mdsarowar/overload:latest-standalone-snapshot
```

# Build
To build from source and run locally you'll be needing rust [installed](https://doc.rust-lang.org/book/ch01-01-installation.html) on the system.
 ```shell
 cargo run
```
This will start overload server on port 3030
