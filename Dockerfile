FROM ubuntu:20.04 as builder

# Disable Prompt During Packages Installation
ARG DEBIAN_FRONTEND=noninteractive

# Update Ubuntu Software repository
RUN apt update
RUN apt install curl -y

RUN cd /root && curl -L https://github.com/harmony-development/scherzo/releases/download/continuous/scherzo > scherzo && chmod +x scherzo

FROM ubuntu:20.04

EXPOSE 2289

RUN mkdir -p /srv/scherzo
COPY --from=builder /root/scherzo /srv/scherzo/

RUN echo "listen_on_localhost = false" > /srv/scherzo/config.toml

RUN set -x ; \
    addgroup -Sg 82 www-data 2>/dev/null ; \
    adduser -S -D -H -h /srv/scherzo -G www-data -g www-data www-data 2>/dev/null ; \
    addgroup www-data www-data 2>/dev/null && exit 0 ; exit 1

RUN chown -cR www-data:www-data /srv/scherzo

RUN apt update

RUN apt install -y \
        curl \
        ca-certificates \
        gcc

VOLUME ["/srv/scherzo/db", "/srv/scherzo/media", "/srv/scherzo/logs"]

HEALTHCHECK --start-period=2s CMD curl --fail -s http://localhost:2289/_harmony/about || curl -k --fail -s https://localhost:2289/_harmony/about || exit 1

USER www-data
WORKDIR /srv/scherzo
ENTRYPOINT [ "/srv/scherzo/scherzo" ]
