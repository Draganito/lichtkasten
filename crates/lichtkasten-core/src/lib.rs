//! Lichtkasten core: everything that reads image files and nothing that
//! draws them. Shared by the desktop app (egui) and the Android app
//! (UniFFI). No UI dependency, no paths required: every reader works on
//! `Read + Seek`, so a file descriptor handed over from Android is enough.
//!
//! Hard rule for callers over an FFI: nothing in here may panic its way out.
//! Decoders run under configurable limits ([`decode::DecodeLimits`]) and
//! third-party parsers that are known to panic on hostile input run under
//! `catch_unwind`.

pub mod decode;
pub mod ext;
#[cfg(feature = "lossless-jpeg")]
pub mod jpeg_lossless;
pub mod meta;
pub mod orient;
pub mod raw;

pub use decode::{DecodeError, DecodeLimits};
pub use meta::Metadata;
