# scherzo

Harmony server implemented in Rust.

## TODO

- Auth service: (partially done)
    - dynamic auth is implemented (login and register by email)
- Chat service: (almost done)
    - everything except emote APIs are implemented
- Mediaproxy service: (not started)
- Voice service: (not started)
- Rest APIs: implemented

## Build

- Clone this repo
- Run `cargo build`

If you have Nix, you can do:
- Flakes: `nix build`
- Non-flake: `nix-build`