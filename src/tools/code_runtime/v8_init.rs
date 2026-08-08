use std::sync::OnceLock;

struct V8Initialization {
    _platform: v8::SharedRef<v8::Platform>,
}

static V8_INITIALIZATION: OnceLock<Result<V8Initialization, String>> = OnceLock::new();
static ICU_DATA: OnceLock<Result<Box<[u8]>, String>> = OnceLock::new();
const COMPRESSED_ICU_DATA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icudtl.dat.zst"));

pub(crate) fn ensure_v8_initialized() -> Result<(), String> {
    match V8_INITIALIZATION.get_or_init(initialize_v8) {
        Ok(_) => Ok(()),
        Err(error_text) => Err(error_text.clone()),
    }
}

fn initialize_v8() -> Result<V8Initialization, String> {
    let icu_data = match ICU_DATA.get_or_init(|| {
        zstd::stream::decode_all(COMPRESSED_ICU_DATA)
            .map(Vec::into_boxed_slice)
            .map_err(|error| format!("failed to decompress embedded ICU data: {error}"))
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
