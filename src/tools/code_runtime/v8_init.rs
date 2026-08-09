use std::io::Read;
use std::sync::OnceLock;

use sha2::Digest;

struct V8Initialization {
    _platform: v8::SharedRef<v8::Platform>,
}

static V8_INITIALIZATION: OnceLock<Result<V8Initialization, String>> = OnceLock::new();
static ICU_DATA: OnceLock<Result<Box<[u8]>, String>> = OnceLock::new();
static ASYNC_V8_INITIALIZATION: tokio::sync::OnceCell<Result<(), String>> =
    tokio::sync::OnceCell::const_new();
const COMPRESSED_ICU_DATA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icudtl.dat.lzma2"));
const ICU_DATA_SHA256: &[u8; 32] = include_bytes!(concat!(env!("OUT_DIR"), "/icudtl.sha256"));
const ICU_DATA_BYTES: usize = 10_822_192;
const ICU_DICTIONARY_SIZE: u32 = 16 * 1024 * 1024;

pub(crate) fn prewarm_v8() {
    if ASYNC_V8_INITIALIZATION.get().is_some() {
        return;
    }
    // A cold initialization decompresses and verifies the embedded ICU data before V8 can start.
    // Begin that bounded work while the model is sampling so the first exec cell normally only
    // observes the completed result. OnceCell coalesces concurrent turns onto one initializer.
    drop(tokio::spawn(async {
        let _ = ensure_v8_initialized_async().await;
    }));
}

pub(crate) async fn ensure_v8_initialized_async() -> Result<(), String> {
    ASYNC_V8_INITIALIZATION
        .get_or_init(|| async {
            tokio::task::spawn_blocking(ensure_v8_initialized)
                .await
                .map_err(|error| format!("V8 initialization task failed: {error}"))?
        })
        .await
        .clone()
}

pub(crate) fn ensure_v8_initialized() -> Result<(), String> {
    match V8_INITIALIZATION.get_or_init(initialize_v8) {
        Ok(_) => Ok(()),
        Err(error_text) => Err(error_text.clone()),
    }
}

fn initialize_v8() -> Result<V8Initialization, String> {
    let icu_data = match ICU_DATA.get_or_init(|| {
        let mut output = Vec::with_capacity(ICU_DATA_BYTES);
        lzma_rust2::Lzma2Reader::new(COMPRESSED_ICU_DATA, ICU_DICTIONARY_SIZE, None)
            .take(ICU_DATA_BYTES as u64 + 1)
            .read_to_end(&mut output)
            .map_err(|error| format!("failed to decompress embedded ICU data: {error}"))?;
        if output.len() != ICU_DATA_BYTES {
            return Err(format!(
                "embedded ICU data decompressed to {} bytes, expected {ICU_DATA_BYTES}",
                output.len()
            ));
        }
        if sha2::Sha256::digest(&output)[..] != ICU_DATA_SHA256[..] {
            return Err("embedded ICU data failed its SHA-256 integrity check".to_string());
        }
        Ok(output.into_boxed_slice())
    }) {
        Ok(data) => data,
        Err(error_text) => return Err(error_text.clone()),
    };
    v8::icu::set_common_data_77(icu_data)
        .map_err(|error_code| format!("failed to initialize ICU data: {error_code}"))?;
    let platform = v8::new_default_platform(0, false).make_shared();
    v8::V8::initialize_platform(platform.clone());
    v8::V8::initialize();
    Ok(V8Initialization {
        _platform: platform,
    })
}
