//! Embed only a verified, explicitly prepared frontend build. Cargo never starts
//! npm, accesses the network or serves a placeholder. Checksums detect stale or
//! incomplete outputs; they are freshness checks, not adversarial signatures.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

fn main() -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join("frontend");
    for input in [
        "src",
        "scripts",
        "index.html",
        "package.json",
        "package-lock.json",
        "tsconfig.json",
        "vite.config.ts",
        "dist/build-seal.json",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(input).display());
    }
    let instructions =
        "Prepare web assets: cd crates/assistant-web/frontend && npm ci && npm run build";
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("dist/build-seal.json"))
            .map_err(|error| format!("Missing frontend build ({error}). {instructions}"))?,
    )?;
    let mut expected = Vec::new();
    for name in [
        "index.html",
        "package.json",
        "package-lock.json",
        "tsconfig.json",
        "vite.config.ts",
    ] {
        expected.push(name.to_owned());
    }
    for directory in ["src", "scripts"] {
        walk(&root, &root.join(directory), &mut expected)?;
    }
    expected.sort();
    let mut actual = Vec::new();
    for entry in manifest["inputs"]
        .as_array()
        .ok_or("invalid frontend seal inputs")?
    {
        let path = entry["path"].as_str().ok_or("invalid input path")?;
        safe_path(path)?;
        actual.push(path.to_owned());
        let checksum = checksum(&fs::read(root.join(path))?);
        if entry["checksum"].as_str() != Some(checksum.as_str()) {
            return Err(format!("Stale frontend input: {path}. {instructions}").into());
        }
    }
    actual.sort();
    if actual != expected {
        return Err(format!("Frontend input set changed. {instructions}").into());
    }
    let embedded = PathBuf::from(std::env::var("OUT_DIR")?).join("webui");
    let mut assets = BTreeMap::new();
    for entry in manifest["assets"].as_array().ok_or("invalid asset set")? {
        let name = entry["path"].as_str().ok_or("invalid asset path")?;
        safe_path(name)?;
        let path = root.join("dist").join(name);
        let gzip_path = root.join("dist").join(format!("{name}.gz"));
        // Freeze verified bytes, not a second read of mutable dist files.
        let frozen = embedded.join(name);
        let frozen_gzip = embedded.join(format!("{name}.gz"));
        if let Some(parent) = frozen.parent() {
            fs::create_dir_all(parent)?;
        }
        for (path, field, output) in [
            (&path, "checksum", &frozen),
            (&gzip_path, "gzip_checksum", &frozen_gzip),
        ] {
            println!("cargo:rerun-if-changed={}", path.display());
            let bytes = fs::read(path)?;
            if entry[field].as_str() != Some(checksum(&bytes).as_str()) {
                return Err(format!("Corrupt frontend asset: {name}. {instructions}").into());
            }
            fs::write(output, bytes)?;
        }
        assets.insert(name.to_owned(), (frozen, frozen_gzip));
    }
    if !assets.contains_key("index.html") {
        return Err("frontend index missing".into());
    }
    write_registry(assets)
}
/// Emit a lookup over frozen assets; file names come only from the verified seal.
fn write_registry(assets: BTreeMap<String, (PathBuf, PathBuf)>) -> Result<(), Box<dyn Error>> {
    let mut output = String::from(
        "pub(super) fn asset(path: &str) -> Option<(&'static [u8], &'static [u8], &'static str)> { match path {\n",
    );
    for (name, (path, gzip_path)) in assets {
        let mime = match Path::new(&name).extension().and_then(|ext| ext.to_str()) {
            Some("html") => "text/html; charset=utf-8",
            Some("js") => "text/javascript; charset=utf-8",
            Some("css") => "text/css; charset=utf-8",
            Some("woff2") => "font/woff2",
            Some("txt") => "text/plain; charset=utf-8",
            _ => return Err(format!("unexpected asset type: {name}").into()),
        };
        writeln!(
            output,
            "{name:?} => Some((include_bytes!({path:?}), include_bytes!({gzip_path:?}), {mime:?})),",
            path = path.to_string_lossy(),
            gzip_path = gzip_path.to_string_lossy()
        )?;
    }
    output.push_str("_ => None, } }\n");
    fs::write(
        PathBuf::from(std::env::var("OUT_DIR")?).join("web_assets.rs"),
        output,
    )?;
    Ok(())
}
fn safe_path(path: &str) -> Result<(), Box<dyn Error>> {
    if path.is_empty()
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err("invalid frontend relative path".into());
    }
    Ok(())
}
fn walk(root: &Path, directory: &Path, paths: &mut Vec<String>) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            walk(root, &path, paths)?;
        } else {
            paths.push(
                path.strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    Ok(())
}
fn checksum(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    });
    format!("{hash:016x}")
}
