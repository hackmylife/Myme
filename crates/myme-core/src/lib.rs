//! # myme-core
//!
//! Platform-agnostic IME engine for Japanese input.
//!
//! ## Module layout
//!
//! | Module        | Responsibility                                              |
//! |---------------|-------------------------------------------------------------|
//! | [`romaji`]    | Romaji → kana conversion tables and state machine          |
//! | [`dictionary`]| Binary dictionary loading, lookup, and prefix search        |
//! | [`candidate`] | Candidate list construction, scoring, and user-history bias |
//! | [`session`]   | Per-input-session state: buffer, preedit string, segments   |
//! | [`ffi`]       | C-ABI surface exported to the macOS InputMethodKit plug-in  |

pub mod candidate;
pub mod dictionary;
pub mod ffi;
pub mod learning;
pub mod romaji;
pub mod segmenter;
pub mod session;
pub mod user_dict;
