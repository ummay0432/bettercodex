//! Focused duration-based audio token estimator retained from OpenAI Codex
//! commit 1669c2403f793d0230065397dfc25f52b844244e.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use lru::LruCache;
use sha1::Digest;
use sha1::Sha1;
use std::hash::Hash;
use std::io::Cursor;
use std::num::NonZeroUsize;
use std::sync::LazyLock;
use symphonia::core::formats::FormatOptions;
use symphonia::core::formats::TrackType;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use tokio::sync::Mutex;
use tokio::sync::MutexGuard;

use crate::truncation::approx_token_count;

const AUDIO_TOKEN_ESTIMATE_CACHE_SIZE: usize = 32;
const AUDIO_TOKENS_PER_SECOND: f64 = 10.0;

static AUDIO_TOKEN_ESTIMATE_CACHE: LazyLock<BlockingLruCache<[u8; 20], usize>> =
    LazyLock::new(|| {
        BlockingLruCache::new(
            NonZeroUsize::new(AUDIO_TOKEN_ESTIMATE_CACHE_SIZE).unwrap_or(NonZeroUsize::MIN),
        )
    });

/// Estimates audio tokens from decoded duration, falling back to the data URL size.
pub(crate) fn estimate_audio_token_count(audio_url: &str) -> usize {
    let key = sha1_digest(audio_url.as_bytes());
    AUDIO_TOKEN_ESTIMATE_CACHE.get_or_insert_with(key, || {
        let Some(duration_seconds) = audio_duration_seconds(audio_url) else {
            return approx_token_count(audio_url);
        };
        let token_count = (duration_seconds * AUDIO_TOKENS_PER_SECOND).ceil();
        if token_count >= usize::MAX as f64 {
            usize::MAX
        } else {
            token_count as usize
        }
    })
}

fn audio_duration_seconds(audio_url: &str) -> Option<f64> {
    let (metadata, payload) = audio_url.split_once(',')?;
    let metadata = metadata.get("data:".len()..)?;
    let mut metadata_parts = metadata.split(';');
    let canonical_mime = canonical_audio_mime(metadata_parts.next()?)?;
    if !metadata_parts.any(|part| part.eq_ignore_ascii_case("base64")) {
        return None;
    }

    let bytes = BASE64_STANDARD.decode(payload).ok()?;
    let media_source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    hint.mime_type(canonical_mime);
    let format = symphonia::default::get_probe()
        .probe(
            &hint,
            media_source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .ok()?;
    let track = format.default_track(TrackType::Audio)?;
    let timing = track.time_base.zip(track.duration).or_else(|| {
        format
            .media_info()
            .time_base
            .zip(format.media_info().duration)
    });
    let (time_base, duration) = timing?;
    let duration_seconds =
        duration.get() as f64 * f64::from(time_base.numer.get()) / f64::from(time_base.denom.get());
    duration_seconds.is_finite().then_some(duration_seconds)
}

fn canonical_audio_mime(mime: &str) -> Option<&'static str> {
    if mime.eq_ignore_ascii_case("audio/wav")
        || mime.eq_ignore_ascii_case("audio/x-wav")
        || mime.eq_ignore_ascii_case("audio/wave")
        || mime.eq_ignore_ascii_case("audio/vnd.wave")
    {
        Some("audio/wav")
    } else if mime.eq_ignore_ascii_case("audio/mpeg") || mime.eq_ignore_ascii_case("audio/mp3") {
        Some("audio/mpeg")
    } else if mime.eq_ignore_ascii_case("audio/mp4")
        || mime.eq_ignore_ascii_case("audio/m4a")
        || mime.eq_ignore_ascii_case("audio/x-m4a")
    {
        Some("audio/mp4")
    } else if mime.eq_ignore_ascii_case("audio/webm") {
        Some("audio/webm")
    } else if mime.eq_ignore_ascii_case("audio/ogg") {
        Some("audio/ogg")
    } else {
        None
    }
}

struct BlockingLruCache<K, V> {
    inner: Mutex<LruCache<K, V>>,
}

impl<K, V> BlockingLruCache<K, V>
where
    K: Eq + Hash,
{
    fn new(capacity: NonZeroUsize) -> Self {
        Self {
            inner: Mutex::new(LruCache::new(capacity)),
        }
    }

    fn get_or_insert_with(&self, key: K, value: impl FnOnce() -> V) -> V
    where
        V: Clone,
    {
        if let Some(mut guard) = lock_if_runtime(&self.inner) {
            if let Some(value) = guard.get(&key) {
                return value.clone();
            }
            let value = value();
            guard.put(key, value.clone());
            return value;
        }
        value()
    }
}

fn lock_if_runtime<K, V>(cache: &Mutex<LruCache<K, V>>) -> Option<MutexGuard<'_, LruCache<K, V>>>
where
    K: Eq + Hash,
{
    tokio::runtime::Handle::try_current().ok()?;
    Some(tokio::task::block_in_place(|| cache.blocking_lock()))
}

fn sha1_digest(bytes: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    let mut output = [0; 20];
    output.copy_from_slice(&result);
    output
}

#[cfg(test)]
mod tests {
    use super::estimate_audio_token_count;
    use crate::truncation::approx_token_count;

    #[test]
    fn invalid_audio_uses_data_url_size_fallback() {
        let input = "not an audio data URL";
        assert_eq!(estimate_audio_token_count(input), approx_token_count(input));
    }
}
