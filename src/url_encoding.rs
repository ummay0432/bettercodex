//! Percent encoding needed by OAuth forms and local Markdown links.

use percent_encoding::AsciiSet;
use percent_encoding::NON_ALPHANUMERIC;
use percent_encoding::PercentEncode;
use percent_encoding::percent_decode_str;
use percent_encoding::utf8_percent_encode;
use std::borrow::Cow;
use std::str::Utf8Error;

const URLENCODING_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

pub(crate) fn encode(input: &str) -> PercentEncode<'_> {
    utf8_percent_encode(input, URLENCODING_SET)
}

pub(crate) fn decode(input: &str) -> Result<Cow<'_, str>, Utf8Error> {
    percent_decode_str(input).decode_utf8()
}
