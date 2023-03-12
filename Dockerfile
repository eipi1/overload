FROM ubuntu:20.04
RUN apt update && \
    apt install -y openssl liblua5.3-0 && \
    ln -s /usr/lib/x86_64-linux-gnu/libssl.so.1.1 /usr/lib/x86_64-linux-gnu/libssl.so.3
ADD target/release/overload /usr/bin/overload
RUN chmod +x /usr/bin/overload
EXPOSE 3030
ENTRYPOINT /usr/bin/overload