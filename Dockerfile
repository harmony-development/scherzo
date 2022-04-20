# syntax=docker/dockerfile:1
FROM alpine:edge AS builder
WORKDIR /usr/src/scherzo

# Install required packages to build scherzo and it's dependencies
RUN apk add --no-cache cargo rust protobuf cmake

ENV RUSTC_BOOTSTRAP=1 \
    RUSTFLAGS="--cfg tokio_unstable"

# == Build dependencies without our own code separately for caching ==
#
# Need a fake main.rs since Cargo refuses to build anything otherwise.
#
# See https://github.com/rust-lang/cargo/issues/2644 for a Cargo feature
# request that would allow just dependencies to be compiled, presumably
# regardless of whether source files are available.
RUN mkdir -p src/bin \
    && touch src/lib.rs \
    && echo 'fn main() {}' > src/main.rs \
    && echo 'fn main() {}' > src/bin/cmd.rs \
    && echo 'fn main() {}' > src/bin/migrate.rs
COPY scherzo_derive scherzo_derive
COPY Cargo.toml Cargo.lock ./
RUN cargo build --release && rm -r src

# Copy over actual scherzo sources
COPY src src
COPY resources resources
COPY protocols protocols
COPY example_config.toml build.rs ./

# main.rs and lib.rs need their timestamp updated for this to work correctly since
# otherwise the build with the fake main.rs from above is newer than the
# source files (COPY preserves timestamps).
#
# Builds scherzo and places the binary at /usr/src/conduit/scherzo/release/scherzo
RUN touch src/main.rs \
    && touch src/lib.rs \
    && touch src/bin/cmd.rs \
    && touch src/bin/migrate.rs \
    && cargo build --release

# ---------------------------------------------------------------------------------------------------------------
# Stuff below this line actually ends up in the resulting docker image
# ---------------------------------------------------------------------------------------------------------------
FROM alpine:latest AS runner

# Standard port on which scherzo launches.
# You still need to map the port when using the docker command or docker-compose.
EXPOSE 2289

# scherzo needs:
#   ca-certificates: for https
#   curl: for the healthcheck script
#   shadow: needed for useradd groupadd
RUN apk add --no-cache ca-certificates curl shadow

# Created directory for the database and media files
RUN mkdir -p /srv/scherzo

# Test if scherzo is still alive
HEALTHCHECK --start-period=5s --interval=5s CMD curl --fail -s http://localhost:2289/_harmony/about || curl -k --fail -s https://localhost:2289/_harmony/about || exit 1

# Copy over the actual scherzo binary from the builder stage
COPY --from=builder /usr/src/scherzo/target/release/scherzo /src/scherzo/scherzo

# Improve security: Don't run stuff as root, that does not need to run as root
# Most distros also use 1000:1000 for the first real user, so this should resolve volume mounting problems.
ARG USER_ID=1000
ARG GROUP_ID=1000
RUN set -x ; \
    groupadd -r -g ${GROUP_ID} scherzo ; \
    useradd -l -r -M -d /srv/scherzo -o -u ${USER_ID} -g scherzo scherzo && exit 0 ; exit 1

# Change ownership of scherzo files to scherzo user and group and make the healthcheck executable:
RUN chown -cR scherzo:scherzo /srv/scherzo

# Change user to scherzo, no root permissions afterwards:
USER scherzo
# Set container home directory
WORKDIR /srv/scherzo

# Run scherzo and print backtraces on panics
ENV RUST_BACKTRACE=1
ENTRYPOINT [ "/srv/scherzo/scherzo" ]
