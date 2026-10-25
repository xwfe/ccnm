//! The `ccnm` main thread gets an 8 MiB stack on Windows, as it has on
//! macOS and Linux.
//!
//! Windows gives a main thread 1 MiB. Debug builds have large frames, and a
//! clap command line the size of ccnm's is the kind of thing that overflows
//! it: gld, with a smaller one, died that way the first time its Windows CI
//! ran it for real (exit 0xC00000FD, `thread 'main' has overflowed its
//! stack`, 7 of 9 end-to-end tests; gld `crates/cli/build.rs`). A release
//! build has smaller frames but the same eighth of the headroom, so both
//! get it. Only the main thread: the MCP server's tokio threads and the
//! ones ccnm spawns set their own.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    const STACK: u32 = 8 * 1024 * 1024;
    match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("msvc") => println!("cargo:rustc-link-arg-bins=/STACK:{STACK}"),
        Ok("gnu") => println!("cargo:rustc-link-arg-bins=-Wl,--stack,{STACK}"),
        _ => {}
    }
}
