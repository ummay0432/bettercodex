use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

const ICU_COMPRESSION_LEVEL: i32 = 22;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build.rs");

    let compressed = zstd::stream::encode_all(deno_core_icudata::ICU_DATA, ICU_COMPRESSION_LEVEL)?;
    let output_directory =
        env::var_os("OUT_DIR").ok_or_else(|| io::Error::other("OUT_DIR must be set"))?;
    let output = PathBuf::from(output_directory).join("icudtl.dat.zst");
    fs::write(output, compressed)?;
    Ok(())
}
