use std::io::Read;
use std::sync::OnceLock;

use sha2::Digest;

struct V8Initialization {
    _platform: v8::SharedRef<v8::Platform>,
}

static V8_INITIALIZATION: OnceLock<Result<V8Initialization, String>> = OnceLock::new();
static ICU_DATA: OnceLock<Result<Box<[u8]>, String>> = OnceLock::new();
const COMPRESSED_ICU_DATA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icudtl.dat.lzma2"));
const ICU_DATA_SHA256: &[u8; 32] = include_bytes!(concat!(env!("OUT_DIR"), "/icudtl.sha256"));
const ICU_DATA_BYTES: usize = 10_822_192;
const ICU_DICTIONARY_SIZE: u32 = 16 * 1024 * 1024;

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
