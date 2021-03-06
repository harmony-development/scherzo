# scherzo

Harmony server implemented in Rust.

## Deploy

With docker (or podman):
```
docker pull yusdacra/scherzo:latest
docker run -d -p 2289:2289 -v db:/srv/scherzo/db -v media:/srv/scherzo/media yusdacra/scherzo:latest
```

One liner to start the latest master CI artifact:
```
mkdir scherzo && cd scherzo && curl -L https://git.io/Jn9Lo > scherzo && chmod +x scherzo && ./scherzo
```

## Roadmap

- Auth service: (implemented)
    - dynamic auth is implemented (login and register by email)
- Chat service: (almost implemented)
    - everything except emote APIs are implemented
- Mediaproxy service: (implemented)
- Voice service: (not started)
- Rest APIs: (implemented)
- Federation: (implemented)

## Build

- Clone this repo
- Make sure you have the toolchain described in `rust-toolchain.toml` installed
    - This will be installed automatically for you if you have rustup setup!
- Make sure you have `lld` installed
- Run `cargo build`

If you have Nix, you can just do:
- Flakes: `nix build` to build, `nix develop` for development shell
- Non-flakes: `nix-build` to build, `nix-shell shell.nix` for development shell

You can also get an executable binary from the latest `Continous build` release.