# Architecture

This file describes the high-level architecture of scherzo.

## `src/main.rs`

This is the entrypoint for the `scherzo` server binary, and where everything is
combined to work in harmony (pun intended). It mostly handles setup work like
parsing config (`parse_config`); tracing, opentelemetry, tokio-console
(all in `setup_tracing`); database (`setup_db`); hRPC server transport
(HTTP, done in `setup_transport`) and then hands these over to `setup_server`.

All of these are done in the `run` function, which combines all those together
and serves the server. It also handles cleanup work (DB flush, tracing flush).

## `src/impls`

This is where API implementations are put in. Each protocol package has it's own
module, REST APIs have their own module. Other more miscellanous code (mainly
`admin_action.rs` and `against.rs`) are also stored here.

- Each endpoint has their own file in their respective protocol package module.
- Protocol packages that utilize the database have structs named `InsertServerNameTree`
which abstract over DB operations used in that package. It is recommended to put
all DB logic for endpoint handlers in these structs as methods, with `endpoint_name_logic` name.

## `src/db`

This module contains database implementations, keys used for storing certain
type of values in various protocol package database trees, serialize / deserialize
functions for data types and database migrations (`migration` module).

- A macro for defining deserialization functions is provided here (`impl_deser`).
- See the `mod.rs` file of the module for implementation details.

## `src/bin`

Contains various binary utilities used to work with scherzo, mainly for database
operations (database migration from one implementation to another, CLI for common
database operations etc.).

## `src/config.rs`

This module defines the config structure, and implements functions for utilizing
the config values. See the file to see how to add new config options.

## `src/error.rs`

This module defines the error type used by scherzo in various places. See the file
itself on how to add new error variants.

## `src/key.rs`

This module implements the key handling code for federation between servers.

## `src/utils`

This module contains various utility code used by different places in code.

## `scherzo_derive`

This crate is a proc macro crate that implements various macros used by `scherzo`
to implement stuff more conveniently.