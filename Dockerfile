FROM ubuntu:18.04
RUN apt update && \
    apt install -y openssl
ADD target/release/overload /usr/bin/overload
RUN chmod +x /usr/bin/overload
EXPOSE 3030
ENTRYPOINT /usr/bin/overload