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
curl -L https://github.com/harmony-development/scherzo/releases/download/continuous/scherzo > scherzo && chmod +x scherzo && ./scherzo
```

## Roadmap

- Auth service: (partially done)
    - dynamic auth is implemented (login and register by email)
- Chat service: (almost done)
    - everything except emote APIs are implemented
- Mediaproxy service: (implemented)
- Voice service: (not started)
- Rest APIs: (implemented)
- Federation: (not implemented)

## Build

- Make sure you have the latest stable Rust toolchain installed (not required if you have Nix)
- Clone this repo
- Run `cargo build`

If you have Nix, you can do:
- Flakes: `nix build`
- Non-flakes: `nix-build`

You can also get an executable binary from the latest `Continous build` release.