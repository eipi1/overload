FROM ubuntu:18.04
ADD target/release/overload /usr/bin/overload
RUN chmod +x /usr/bin/overload
EXPOSE 3030
ENTRYPOINT /usr/bin/overload