# scherzo

Harmony server implemented in Rust.

It uses [hyper] for serving HTTP via [hrpc-rs], and currently supports [sled]
and `sqlite` as a database backend.

## Deploy

Note: the CI builds are currently compiled with `sled` DB backend.

With docker (or podman):
```
docker pull yusdacra/scherzo:latest
docker run -d -p 2289:2289 -v db:/srv/scherzo/db -v media:/srv/scherzo/media yusdacra/scherzo:latest
```

One liner to start the latest master CI artifact:
```
mkdir scherzo && cd scherzo && curl -L https://github.com/harmony-development/scherzo/releases/download/continuous/scherzo > scherzo && chmod +x scherzo && ./scherzo
```

## Configuration

See the [example config](./example_config.toml) for a commented config file
with all options available.

## Roadmap

- Auth service: (implemented)
    - dynamic auth is implemented (login and register by email)
- Chat service: (implemented)
- Mediaproxy service: (implemented)
- Voice service: (implemented)
- Rest APIs: (implemented)
- Federation: (implemented)

## Build

- Clone this repo
- Make sure you have the toolchain described in `rust-toolchain.toml` installed
    - This will be installed automatically for you if you have rustup setup!
- Make sure you have `mold`, `clang` and `protoc` installed
- Run `cargo build`

If you have Nix, you can just do:
- Flakes: `nix build` to build, `nix develop` for development shell
- Non-flakes: `nix-build` to build, `nix-shell shell.nix` for development shell

You can also get an executable binary from the latest `Continous build` release.

[hyper]: https://github.com/tokio-rs/hyper
[hrpc-rs]: https://github.com/harmony-development/hrpc-rs
[sled]: https://github.com/spacejam/sled