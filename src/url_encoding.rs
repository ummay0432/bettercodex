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

#[cfg(test)]
mod tests {
    use super::decode;
    use super::encode;

    #[test]
    fn matches_oauth_percent_encoding() {
        assert_eq!(encode("&a%b!c.d?e").to_string(), "%26a%25b%21c.d%3Fe");
        assert_eq!(
            encode("👾 Exterminate!").to_string(),
            "%F0%9F%91%BE%20Exterminate%21"
        );
        assert_eq!(decode("this%20that%2").expect("decode"), "this that%2");
        assert_eq!(decode("a+b").expect("decode"), "a+b");
    }
}
