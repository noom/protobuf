// Protocol Buffers - Google's data interchange format
// Copyright 2023 Google LLC.  All rights reserved.
//
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file or at
// https://developers.google.com/open-source/licenses/bsd

//! Items specific to `bytes` and `string` fields.
#![allow(dead_code)]
#![allow(unused)]

use crate::__internal::Private;
use crate::__runtime::{PtrAndLen, RawMessage};
use crate::{Mut, MutProxied, MutProxy, Optional, Proxied, View, ViewProxy};
use std::borrow::Cow;
use std::cmp::{Eq, Ord, Ordering, PartialEq, PartialOrd};
use std::convert::{AsMut, AsRef};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::iter;
use std::ops::{Deref, DerefMut};
use utf8::Utf8Chunks;

impl Proxied for [u8] {
    type View<'msg> = &'msg [u8];
}

impl<'msg> ViewProxy<'msg> for &'msg [u8] {
    type Proxied = [u8];

    fn as_view(&self) -> &[u8] {
        self
    }

    fn into_view<'shorter>(self) -> &'shorter [u8]
    where
        'msg: 'shorter,
    {
        self
    }
}

/// The bytes were not valid UTF-8.
#[derive(Debug, PartialEq)]
pub struct Utf8Error(pub(crate) ());

impl From<std::str::Utf8Error> for Utf8Error {
    fn from(_: std::str::Utf8Error) -> Utf8Error {
        Utf8Error(())
    }
}

/// A shared immutable view of a protobuf `string` field's contents.
///
/// Like a `str`, it can be cheaply accessed as bytes and
/// is dynamically sized, requiring it be accessed through a pointer.
///
/// # UTF-8 and `&str` access
///
/// Protobuf [docs] state that a `string` field contains UTF-8 encoded text.
/// However, not every runtime enforces this, and the Rust runtime is designed
/// to integrate with other runtimes with FFI, like C++.
///
/// Because of this, in order to access the contents as a `&str`, users must
/// call [`ProtoStr::to_str`] to perform a (possibly runtime-elided) UTF-8
/// validation check. However, the Rust API only allows `set()`ting a `string`
/// field with data should be valid UTF-8 like a `&str` or a
/// `&ProtoStr`. This means that this check should rarely fail, but is necessary
/// to prevent UB when interacting with C++, which has looser restrictions.
///
/// Most of the time, users should not perform direct `&str` access to the
/// contents - this type implements `Display` and comparison with `str`,
/// so it's best to avoid a UTF-8 check by working directly with `&ProtoStr`
/// or converting to `&[u8]`.
///
/// # `Display` and `ToString`
/// `ProtoStr` is ordinarily UTF-8 and so implements `Display`. If there are
/// any invalid UTF-8 sequences, they are replaced with [`U+FFFD REPLACEMENT
/// CHARACTER`]. Because anything implementing `Display` also implements
/// `ToString`, `proto_str.to_string()` is equivalent to
/// `String::from_utf8_lossy(proto_str.as_bytes()).into_owned()`.
///
/// [docs]: https://protobuf.dev/programming-guides/proto2/#scalar
/// [dst]: https://doc.rust-lang.org/reference/dynamically-sized-types.html
/// [`U+FFFD REPLACEMENT CHARACTER`]: std::char::REPLACEMENT_CHARACTER
#[repr(transparent)]
pub struct ProtoStr([u8]);

impl ProtoStr {
    /// Converts `self` to a byte slice.
    ///
    /// Note: this type does not implement `Deref`; you must call `as_bytes()`
    /// or `AsRef<[u8]>` to get access to bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Yields a `&str` slice if `self` contains valid UTF-8.
    ///
    /// This may perform a runtime check, dependent on runtime.
    ///
    /// `String::from_utf8_lossy(proto_str.as_bytes())` can be used to
    /// infallibly construct a string, replacing invalid UTF-8 with
    /// [`U+FFFD REPLACEMENT CHARACTER`].
    ///
    /// [`U+FFFD REPLACEMENT CHARACTER`]: std::char::REPLACEMENT_CHARACTER
    // This is not `try_to_str` since `to_str` is shorter, with `CStr` as precedent.
    pub fn to_str(&self) -> Result<&str, Utf8Error> {
        Ok(std::str::from_utf8(&self.0)?)
    }

    /// Converts `self` to a string, including invalid characters.
    ///
    /// Invalid UTF-8 sequences are replaced with
    /// [`U+FFFD REPLACEMENT CHARACTER`].
    ///
    /// Users should be prefer this to `.to_string()` provided by `Display`.
    /// `.to_cow_lossy()` is the same operation, but it may avoid an
    /// allocation if the string is already UTF-8.
    ///
    /// [`U+FFFD REPLACEMENT CHARACTER`]: std::char::REPLACEMENT_CHARACTER
    //
    // This method is named `to_string_lossy` in `CStr`, but since `to_string`
    // also exists on this type, this name was chosen to avoid confusion.
    pub fn to_cow_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.0)
    }

    /// Returns `true` if `self` has a length of zero bytes.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the length of `self`.
    ///
    /// Like `&str`, this is a length in bytes, not `char`s or graphemes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Iterates over the `char`s in this protobuf `string`.
    ///
    /// Invalid UTF-8 sequences are replaced with
    /// [`U+FFFD REPLACEMENT CHARACTER`].
    ///
    /// [`U+FFFD REPLACEMENT CHARACTER`]: std::char::REPLACEMENT_CHARACTER
    pub fn chars(&self) -> impl Iterator<Item = char> + '_ + fmt::Debug {
        Utf8Chunks::new(self.as_bytes()).flat_map(|chunk| {
            let mut yield_replacement_char = !chunk.invalid().is_empty();
            chunk.valid().chars().chain(iter::from_fn(move || {
                // Yield a single replacement character for every
                // non-empty invalid sequence.
                yield_replacement_char.then(|| {
                    yield_replacement_char = false;
                    char::REPLACEMENT_CHARACTER
                })
            }))
        })
    }

    /// Returns an iterator over chunks of UTF-8 data in the string.
    ///
    /// An `Ok(&str)` is yielded for every valid UTF-8 chunk, and an
    /// `Err(&[u8])` for each non-UTF-8 chunk. An `Err` will be emitted
    /// multiple times in a row for contiguous invalid chunks. Each invalid
    /// chunk in an `Err` has a maximum length of 3 bytes.
    pub fn utf8_chunks(&self) -> impl Iterator<Item = Result<&str, &[u8]>> + '_ {
        Utf8Chunks::new(self.as_bytes()).flat_map(|chunk| {
            let valid = chunk.valid();
            let invalid = chunk.invalid();
            (!valid.is_empty())
                .then_some(Ok(valid))
                .into_iter()
                .chain((!invalid.is_empty()).then_some(Err(invalid)))
        })
    }

    /// Converts known-UTF-8 bytes to a `ProtoStr` without a check.
    ///
    /// # Safety
    /// `bytes` must be valid UTF-8 if the current runtime requires it.
    pub unsafe fn from_utf8_unchecked(bytes: &[u8]) -> &Self {
        // SAFETY:
        // - `ProtoStr` is `#[repr(transparent)]` over `[u8]`, so it has the same
        //   layout.
        // - `ProtoStr` has the same pointer metadata and element size as `[u8]`.
        unsafe { &*(bytes as *const [u8] as *const Self) }
    }

    /// Interprets a string slice as a `&ProtoStr`.
    pub fn from_str(string: &str) -> &Self {
        // SAFETY: `string.as_bytes()` is valid UTF-8.
        unsafe { Self::from_utf8_unchecked(string.as_bytes()) }
    }
}

impl AsRef<[u8]> for ProtoStr {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl<'msg> From<&'msg ProtoStr> for &'msg [u8] {
    fn from(val: &'msg ProtoStr) -> &'msg [u8] {
        val.as_bytes()
    }
}

impl<'msg> From<&'msg str> for &'msg ProtoStr {
    fn from(val: &'msg str) -> &'msg ProtoStr {
        ProtoStr::from_str(val)
    }
}

impl<'msg> TryFrom<&'msg ProtoStr> for &'msg str {
    type Error = Utf8Error;

    fn try_from(val: &'msg ProtoStr) -> Result<&'msg str, Utf8Error> {
        val.to_str()
    }
}

impl<'msg> TryFrom<&'msg [u8]> for &'msg ProtoStr {
    type Error = Utf8Error;

    fn try_from(val: &'msg [u8]) -> Result<&'msg ProtoStr, Utf8Error> {
        Ok(ProtoStr::from_str(std::str::from_utf8(val)?))
    }
}

impl fmt::Debug for ProtoStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&Utf8Chunks::new(self.as_bytes()).debug(), f)
    }
}

impl fmt::Display for ProtoStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use std::fmt::Write as _;
        for chunk in Utf8Chunks::new(self.as_bytes()) {
            fmt::Display::fmt(chunk.valid(), f)?;
            if !chunk.invalid().is_empty() {
                // One invalid chunk is emitted per detected invalid sequence.
                f.write_char(char::REPLACEMENT_CHARACTER)?;
            }
        }
        Ok(())
    }
}

impl Hash for ProtoStr {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_bytes().hash(state)
    }
}

impl Eq for ProtoStr {}
impl Ord for ProtoStr {
    fn cmp(&self, other: &ProtoStr) -> Ordering {
        self.as_bytes().cmp(other.as_bytes())
    }
}

impl Proxied for ProtoStr {
    type View<'msg> = &'msg ProtoStr;
}

impl<'msg> ViewProxy<'msg> for &'msg ProtoStr {
    type Proxied = ProtoStr;

    fn as_view(&self) -> &ProtoStr {
        self
    }

    fn into_view<'shorter>(self) -> &'shorter ProtoStr
    where
        'msg: 'shorter,
    {
        self
    }
}

// TODO: remove after IntoProxied has been implemented for
// ProtoStr.
impl AsRef<ProtoStr> for String {
    fn as_ref(&self) -> &ProtoStr {
        ProtoStr::from_str(self.as_str())
    }
}

// TODO: remove after IntoProxied has been implemented for
// ProtoStr.
impl AsRef<ProtoStr> for &str {
    fn as_ref(&self) -> &ProtoStr {
        ProtoStr::from_str(self)
    }
}

// TODO: remove after IntoProxied has been implemented for
// ProtoStr.
impl AsRef<ProtoStr> for &ProtoStr {
    fn as_ref(&self) -> &ProtoStr {
        self
    }
}

/// Implements `PartialCmp` and `PartialEq` for the `lhs` against the `rhs`
/// using `AsRef<[u8]>`.
// TODO: consider improving to not require a `<()>` if no generics are
// needed
macro_rules! impl_bytes_partial_cmp {
    ($(<($($generics:tt)*)> $lhs:ty => $rhs:ty),+ $(,)?) => {
        $(
            impl<$($generics)*> PartialEq<$rhs> for $lhs {
                fn eq(&self, other: &$rhs) -> bool {
                    AsRef::<[u8]>::as_ref(self) == AsRef::<[u8]>::as_ref(other)
                }
            }
            impl<$($generics)*> PartialOrd<$rhs> for $lhs {
                fn partial_cmp(&self, other: &$rhs) -> Option<Ordering> {
                    AsRef::<[u8]>::as_ref(self).partial_cmp(AsRef::<[u8]>::as_ref(other))
                }
            }
        )*
    };
}

impl_bytes_partial_cmp!(
    // `ProtoStr` against protobuf types
    <()> ProtoStr => ProtoStr,

    // `ProtoStr` against foreign types
    <()> ProtoStr => str,
    <()> str => ProtoStr,
);

#[cfg(test)]
mod tests {
    use super::*;

    // TODO: Add unit tests

    // Shorter and safe utility function to construct `ProtoStr` from bytes for
    // testing.
    fn test_proto_str(bytes: &[u8]) -> &ProtoStr {
        // SAFETY: The runtime that this test executes under does not elide UTF-8 checks
        // inside of `ProtoStr`.
        unsafe { ProtoStr::from_utf8_unchecked(bytes) }
    }

    // UTF-8 test cases copied from:
    // https://github.com/rust-lang/rust/blob/e8ee0b7/library/core/tests/str_lossy.rs

    #[test]
    fn proto_str_debug() {
        assert_eq!(&format!("{:?}", test_proto_str(b"Hello There")), "\"Hello There\"");
        assert_eq!(
            &format!(
                "{:?}",
                test_proto_str(b"Hello\xC0\x80 There\xE6\x83 Goodbye\xf4\x8d\x93\xaa"),
            ),
            "\"Hello\\xC0\\x80 There\\xE6\\x83 Goodbye\\u{10d4ea}\"",
        );
    }

    #[test]
    fn proto_str_display() {
        assert_eq!(&test_proto_str(b"Hello There").to_string(), "Hello There");
        assert_eq!(
            &test_proto_str(b"Hello\xC0\x80 There\xE6\x83 Goodbye\xf4\x8d\x93\xaa").to_string(),
            "Hello�� There� Goodbye\u{10d4ea}",
        );
    }

    #[test]
    fn proto_str_to_rust_str() {
        assert_eq!(test_proto_str(b"hello").to_str(), Ok("hello"));
        assert_eq!(test_proto_str("ศไทย中华Việt Nam".as_bytes()).to_str(), Ok("ศไทย中华Việt Nam"));
        for expect_fail in [
            &b"Hello\xC2 There\xFF Goodbye"[..],
            b"Hello\xC0\x80 There\xE6\x83 Goodbye",
            b"\xF5foo\xF5\x80bar",
            b"\xF1foo\xF1\x80bar\xF1\x80\x80baz",
            b"\xF4foo\xF4\x80bar\xF4\xBFbaz",
            b"\xF0\x80\x80\x80foo\xF0\x90\x80\x80bar",
            b"\xED\xA0\x80foo\xED\xBF\xBFbar",
        ] {
            assert_eq!(test_proto_str(expect_fail).to_str(), Err(Utf8Error(())), "{expect_fail:?}");
        }
    }

    #[test]
    fn proto_str_to_cow() {
        assert_eq!(test_proto_str(b"hello").to_cow_lossy(), Cow::Borrowed("hello"));
        assert_eq!(
            test_proto_str("ศไทย中华Việt Nam".as_bytes()).to_cow_lossy(),
            Cow::Borrowed("ศไทย中华Việt Nam")
        );
        for (bytes, lossy_str) in [
            (&b"Hello\xC2 There\xFF Goodbye"[..], "Hello� There� Goodbye"),
            (b"Hello\xC0\x80 There\xE6\x83 Goodbye", "Hello�� There� Goodbye"),
            (b"\xF5foo\xF5\x80bar", "�foo��bar"),
            (b"\xF1foo\xF1\x80bar\xF1\x80\x80baz", "�foo�bar�baz"),
            (b"\xF4foo\xF4\x80bar\xF4\xBFbaz", "�foo�bar��baz"),
            (b"\xF0\x80\x80\x80foo\xF0\x90\x80\x80bar", "����foo\u{10000}bar"),
            (b"\xED\xA0\x80foo\xED\xBF\xBFbar", "���foo���bar"),
        ] {
            let cow = test_proto_str(bytes).to_cow_lossy();
            assert!(matches!(cow, Cow::Owned(_)));
            assert_eq!(&*cow, lossy_str, "{bytes:?}");
        }
    }

    #[test]
    fn proto_str_utf8_chunks() {
        macro_rules! assert_chunks {
            ($bytes:expr, $($chunks:expr),* $(,)?) => {
                let bytes = $bytes;
                let chunks: &[Result<&str, &[u8]>] = &[$($chunks),*];
                let s = test_proto_str(bytes);
                let mut got_chunks = s.utf8_chunks();
                let mut expected_chars = chunks.iter().copied();
                assert!(got_chunks.eq(expected_chars), "{bytes:?} -> {chunks:?}");
            };
        }
        assert_chunks!(b"hello", Ok("hello"));
        assert_chunks!("ศไทย中华Việt Nam".as_bytes(), Ok("ศไทย中华Việt Nam"));
        assert_chunks!(
            b"Hello\xC2 There\xFF Goodbye",
            Ok("Hello"),
            Err(b"\xC2"),
            Ok(" There"),
            Err(b"\xFF"),
            Ok(" Goodbye"),
        );
        assert_chunks!(
            b"Hello\xC0\x80 There\xE6\x83 Goodbye",
            Ok("Hello"),
            Err(b"\xC0"),
            Err(b"\x80"),
            Ok(" There"),
            Err(b"\xE6\x83"),
            Ok(" Goodbye"),
        );
        assert_chunks!(
            b"\xF5foo\xF5\x80bar",
            Err(b"\xF5"),
            Ok("foo"),
            Err(b"\xF5"),
            Err(b"\x80"),
            Ok("bar"),
        );
        assert_chunks!(
            b"\xF1foo\xF1\x80bar\xF1\x80\x80baz",
            Err(b"\xF1"),
            Ok("foo"),
            Err(b"\xF1\x80"),
            Ok("bar"),
            Err(b"\xF1\x80\x80"),
            Ok("baz"),
        );
        assert_chunks!(
            b"\xF4foo\xF4\x80bar\xF4\xBFbaz",
            Err(b"\xF4"),
            Ok("foo"),
            Err(b"\xF4\x80"),
            Ok("bar"),
            Err(b"\xF4"),
            Err(b"\xBF"),
            Ok("baz"),
        );
        assert_chunks!(
            b"\xF0\x80\x80\x80foo\xF0\x90\x80\x80bar",
            Err(b"\xF0"),
            Err(b"\x80"),
            Err(b"\x80"),
            Err(b"\x80"),
            Ok("foo\u{10000}bar"),
        );
        assert_chunks!(
            b"\xED\xA0\x80foo\xED\xBF\xBFbar",
            Err(b"\xED"),
            Err(b"\xA0"),
            Err(b"\x80"),
            Ok("foo"),
            Err(b"\xED"),
            Err(b"\xBF"),
            Err(b"\xBF"),
            Ok("bar"),
        );
    }

    #[test]
    fn proto_str_chars() {
        macro_rules! assert_chars {
            ($bytes:expr, $chars:expr) => {
                let bytes = $bytes;
                let chars = $chars;
                let s = test_proto_str(bytes);
                let mut got_chars = s.chars();
                let mut expected_chars = chars.into_iter();
                assert!(got_chars.eq(expected_chars), "{bytes:?} -> {chars:?}");
            };
        }
        assert_chars!(b"hello", ['h', 'e', 'l', 'l', 'o']);
        assert_chars!(
            "ศไทย中华Việt Nam".as_bytes(),
            ['ศ', 'ไ', 'ท', 'ย', '中', '华', 'V', 'i', 'ệ', 't', ' ', 'N', 'a', 'm']
        );
        assert_chars!(
            b"Hello\xC2 There\xFF Goodbye",
            [
                'H', 'e', 'l', 'l', 'o', '�', ' ', 'T', 'h', 'e', 'r', 'e', '�', ' ', 'G', 'o',
                'o', 'd', 'b', 'y', 'e'
            ]
        );
        assert_chars!(
            b"Hello\xC0\x80 There\xE6\x83 Goodbye",
            [
                'H', 'e', 'l', 'l', 'o', '�', '�', ' ', 'T', 'h', 'e', 'r', 'e', '�', ' ', 'G',
                'o', 'o', 'd', 'b', 'y', 'e'
            ]
        );
        assert_chars!(b"\xF5foo\xF5\x80bar", ['�', 'f', 'o', 'o', '�', '�', 'b', 'a', 'r']);
        assert_chars!(
            b"\xF1foo\xF1\x80bar\xF1\x80\x80baz",
            ['�', 'f', 'o', 'o', '�', 'b', 'a', 'r', '�', 'b', 'a', 'z']
        );
        assert_chars!(
            b"\xF4foo\xF4\x80bar\xF4\xBFbaz",
            ['�', 'f', 'o', 'o', '�', 'b', 'a', 'r', '�', '�', 'b', 'a', 'z']
        );
        assert_chars!(
            b"\xF0\x80\x80\x80foo\xF0\x90\x80\x80bar",
            ['�', '�', '�', '�', 'f', 'o', 'o', '\u{10000}', 'b', 'a', 'r']
        );
        assert_chars!(
            b"\xED\xA0\x80foo\xED\xBF\xBFbar",
            ['�', '�', '�', 'f', 'o', 'o', '�', '�', '�', 'b', 'a', 'r']
        );
    }
}
