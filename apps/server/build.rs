use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn collect(root: &Path, current: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for item in fs::read_dir(current)? {
        let item = item?;
        let path = item.path();
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_dir() {
            collect(root, &path, files)?;
        } else if path.is_file() {
            files.push(
                path.strip_prefix(root)
                    .expect("asset is under root")
                    .to_path_buf(),
            );
        }
    }
    Ok(())
}

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    println!("cargo:rerun-if-env-changed=LITECR_WEB_DIST");
    let web_dist = env::var_os("LITECR_WEB_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("../../apps/web/dist"));
    let index = web_dist.join("index.html");
    if !index.is_file() {
        panic!(
            "React production assets are missing at {}; run npm ci && npm run build in apps/web before building litecr",
            web_dist.display()
        );
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("build output dir"));
    let assets_root = out_dir.join("litecr-web");
    fs::create_dir_all(&assets_root).expect("create embedded web output directory");
    let mut files = Vec::new();
    collect(&web_dist, &web_dist, &mut files).expect("enumerate React production assets");
    files.sort();

    let mut generated = String::from("pub static EMBEDDED_WEB_ASSETS: &[(&str, &[u8])] = &[\n");
    for relative in &files {
        let source = web_dist.join(relative);
        let destination = assets_root.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).expect("create embedded web asset directory");
        }
        fs::copy(&source, &destination).expect("copy React production asset");
        let path = relative.to_string_lossy().replace('\\', "/");
        generated.push_str(&format!(
            "    (\"/{path}\", include_bytes!(concat!(env!(\"OUT_DIR\"), \"/litecr-web/{path}\"))),\n"
        ));
    }
    generated.push_str("];\n");
    fs::write(out_dir.join("embedded_web.rs"), generated).expect("write embedded web index");
    println!("cargo:rerun-if-changed={}", web_dist.display());
}
