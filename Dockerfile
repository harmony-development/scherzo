# Alpine build image to build Scherzo's statically compiled binary
FROM alpine:3.12 as builder

# Specifies which revision/commit is build. Defaults to HEAD
ARG GIT_REF=origin/master

RUN sed -i \
	-e 's|v3\.12|edge|' \
	/etc/apk/repositories

RUN apk add --no-cache cargo protoc

COPY src src
COPY Cargo.toml Cargo.toml
COPY Cargo.lock Cargo.lock
COPY .cargo .cargo

RUN cargo install --path .

FROM alpine:3.12

EXPOSE 2289

RUN mkdir -p /srv/scherzo
COPY --from=builder /root/.cargo/bin/scherzo /srv/scherzo/

RUN echo "listen_on_localhost = false" > /srv/scherzo/config.toml

RUN set -x ; \
    addgroup -Sg 82 www-data 2>/dev/null ; \
    adduser -S -D -H -h /srv/scherzo -G www-data -g www-data www-data 2>/dev/null ; \
    addgroup www-data www-data 2>/dev/null && exit 0 ; exit 1

RUN chown -cR www-data:www-data /srv/scherzo

RUN apk add --no-cache \
        curl \
        ca-certificates \
        libgcc

VOLUME ["/srv/scherzo/db", "/srv/scherzo/media", "/srv/scherzo/logs"]

HEALTHCHECK --start-period=2s CMD curl --fail -s http://localhost:2289/_harmony/version || curl -k --fail -s https://localhost:2289/_harmony/version || exit 1

USER www-data
WORKDIR /srv/scherzo
ENTRYPOINT [ "/srv/scherzo/scherzo" ]