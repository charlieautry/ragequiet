//! Embeds the brand `.ico` into the Windows executable's resources.
//!
//! Guarded by `CARGO_CFG_TARGET_OS` (not `cfg!(windows)`) so cross-compiling
//! *to* Windows from another host still embeds the icon, and building on
//! Windows *for* another target doesn't try to invoke the Windows-only
//! resource compiler.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/brand/ragequiet.ico")
            .compile()
            .expect("embed the exe icon resource");
    }
}
