use std::env;
use std::fs;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;

use sha2::Digest;

const ICU_DICTIONARY_SIZE: u32 = 16 * 1024 * 1024;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build.rs");

    let mut options = lzma_rust2::Lzma2Options::with_preset(9);
    options.lzma_options.dict_size = ICU_DICTIONARY_SIZE;
    let mut encoder = lzma_rust2::Lzma2Writer::new(Vec::new(), options);
    encoder.write_all(deno_core_icudata::ICU_DATA)?;
    let compressed = encoder.finish()?;
    let mut decoded = Vec::with_capacity(deno_core_icudata::ICU_DATA.len());
    lzma_rust2::Lzma2Reader::new(compressed.as_slice(), ICU_DICTIONARY_SIZE, None)
        .read_to_end(&mut decoded)?;
    if decoded != deno_core_icudata::ICU_DATA {
        return Err(
            io::Error::other("compressed ICU data failed its build-time round trip").into(),
        );
    }
    let output_directory =
        env::var_os("OUT_DIR").ok_or_else(|| io::Error::other("OUT_DIR must be set"))?;
    let output_directory = PathBuf::from(output_directory);
    fs::write(output_directory.join("icudtl.dat.lzma2"), compressed)?;
    fs::write(
        output_directory.join("icudtl.sha256"),
        sha2::Sha256::digest(deno_core_icudata::ICU_DATA),
    )?;
    Ok(())
}
