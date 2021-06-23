# Alpine build image to build Scherzo's statically compiled binary
FROM alpine:3.12 as builder

# Specifies which revision/commit is build. Defaults to HEAD
ARG GIT_REF=origin/master

RUN sed -i \
	-e 's|v3\.12|edge|' \
	/etc/apk/repositories

RUN apk add --no-cache cargo protoc

RUN cargo install --git "https://gitlab.com/harmony-development/scherzo.git" --rev ${GIT_REF}

FROM alpine:3.12

EXPOSE 2289

RUN mkdir -p /srv/scherzo
COPY --from=builder /root/.cargo/bin/scherzo /srv/scherzo/

RUN apk add --no-cache \
        ca-certificates \
        libgcc

RUN set -x ; \
    addgroup -Sg 82 www-data 2>/dev/null ; \
    adduser -S -D -H -h /srv/scherzo -G www-data -g www-data www-data 2>/dev/null ; \
    addgroup www-data www-data 2>/dev/null && exit 0 ; exit 1

RUN chown -cR www-data:www-data /srv/scherzo

VOLUME ["/srv/scherzo/db"]

USER www-data
WORKDIR /srv/scherzo
ENTRYPOINT [ "/srv/scherzo/scherzo" ]