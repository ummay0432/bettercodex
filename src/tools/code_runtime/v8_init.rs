use std::io::Read;
use std::sync::OnceLock;

use sha2::Digest;

struct V8Initialization {
    _platform: v8::SharedRef<v8::Platform>,
}

struct AlignedIcuData {
    storage: Vec<u8>,
    data_offset: usize,
}

impl AlignedIcuData {
    fn as_slice(&self) -> &[u8] {
        &self.storage[self.data_offset..]
    }
}

static V8_INITIALIZATION: OnceLock<Result<V8Initialization, String>> = OnceLock::new();
static ICU_DATA: OnceLock<Result<AlignedIcuData, String>> = OnceLock::new();
static ASYNC_V8_INITIALIZATION: tokio::sync::OnceCell<Result<(), String>> =
    tokio::sync::OnceCell::const_new();
const COMPRESSED_ICU_DATA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icudtl.dat.lzma2"));
const ICU_DATA_SHA256: &[u8; 32] = include_bytes!(concat!(env!("OUT_DIR"), "/icudtl.sha256"));
const ICU_DATA_BYTES: usize = 10_822_192;
const ICU_DATA_ALIGNMENT: usize = 16;
const ICU_DICTIONARY_SIZE: u32 = 3 * 1024 * 1024;

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
        // ICU requires at least 8-byte alignment and recommends 16. Leave enough capacity for
        // both the leading alignment padding and the one-byte oversized-payload probe so
        // `read_to_end` cannot reallocate a valid buffer and lose that alignment.
        let mut storage: Vec<u8> = Vec::with_capacity(ICU_DATA_BYTES + ICU_DATA_ALIGNMENT);
        let data_offset = storage.as_ptr().align_offset(ICU_DATA_ALIGNMENT);
        if data_offset >= ICU_DATA_ALIGNMENT {
            return Err("failed to align embedded ICU data".to_string());
        }
        storage.resize(data_offset, 0);
        lzma_rust2::Lzma2Reader::new(COMPRESSED_ICU_DATA, ICU_DICTIONARY_SIZE, None)
            .take(ICU_DATA_BYTES as u64 + 1)
            .read_to_end(&mut storage)
            .map_err(|error| format!("failed to decompress embedded ICU data: {error}"))?;
        let output = &storage[data_offset..];
        if output.len() != ICU_DATA_BYTES {
            return Err(format!(
                "embedded ICU data decompressed to {} bytes, expected {ICU_DATA_BYTES}",
                output.len()
            ));
        }
        if sha2::Sha256::digest(output)[..] != ICU_DATA_SHA256[..] {
            return Err("embedded ICU data failed its SHA-256 integrity check".to_string());
        }
        if output.as_ptr().align_offset(ICU_DATA_ALIGNMENT) != 0 {
            return Err("embedded ICU data lost its alignment".to_string());
        }
        Ok(AlignedIcuData {
            storage,
            data_offset,
        })
    }) {
        Ok(data) => data,
        Err(error_text) => return Err(error_text.clone()),
    };
    v8::icu::set_common_data_77(icu_data.as_slice())
        .map_err(|error_code| format!("failed to initialize ICU data: {error_code}"))?;
    let platform = v8::new_default_platform(0, false).make_shared();
    v8::V8::initialize_platform(platform.clone());
    v8::V8::initialize();
    Ok(V8Initialization {
        _platform: platform,
    })
}
