use micu_devtools::{
    Result,
    mock::{MockMicu, png},
    stdio::Session,
};
use serde_json::json;
use std::{fs, path::Path};

#[test]
fn invalid_images_masks_and_reference_counts_are_rejected_before_upload() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = MockMicu::new()?;
    let valid = root.path().join("valid.png");
    fs::write(&valid, png(32, 24, 1)?)?;
    let oversized = root.path().join("large.png");
    let mut data = png(32, 24, 2)?;
    data.resize(4 * 1024 * 1024 + 1, 0);
    fs::write(&oversized, data)?;
    let truncated = root.path().join("truncated.png");
    fs::write(&truncated, &png(32, 24, 3)?[..24])?;
    let spoof = root.path().join("fake.jpg");
    fs::write(&spoof, b"not an image")?;
    let wrong_mask = root.path().join("mask.png");
    fs::write(&wrong_mask, png(16, 16, 4)?)?;
    let rgb_mask = root.path().join("rgb-mask.png");
    image::RgbImage::from_pixel(32, 24, image::Rgb([0, 0, 0])).save(&rgb_mask)?;
    let padded = root.path().join("padded.png");
    let mut data = png(32, 24, 5)?;
    data.resize(4 * 1024 * 1024 - 1024, 0);
    fs::write(&padded, data)?;
    let bomb = root.path().join("bomb.png");
    let mut data = png(32, 24, 6)?;
    data[16..20].copy_from_slice(&100_000_u32.to_be_bytes());
    data[20..24].copy_from_slice(&100_000_u32.to_be_bytes());
    fs::write(&bomb, data)?;
    let bad_webp = root.path().join("bad.webp");
    fs::write(&bad_webp, b"RIFF\x18\x00\x00\x00WEBPVP8 invalid data")?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &api.env(),
    )?;
    server.initialize()?;
    for path in [&oversized, &truncated, &spoof, &bomb, &bad_webp] {
        assert_eq!(
            server.call("image_edit", json!({"prompt":"reject","image_path":path}))?["ok"],
            false
        );
    }
    assert_eq!(
        server.call(
            "image_edit",
            json!({"prompt":"reject","image_path":valid,"mask_path":wrong_mask})
        )?["ok"],
        false
    );
    assert_eq!(
        server.call(
            "image_edit",
            json!({"prompt":"reject","image_path":valid,"mask_path":rgb_mask})
        )?["ok"],
        false
    );
    for paths in [
        vec![],
        vec![valid.clone()],
        vec![valid.clone(); 11],
        vec![oversized.clone(); 3],
        vec![padded; 3],
    ] {
        assert_eq!(
            server.call(
                "image_multi_reference",
                json!({"prompt":"reject","image_paths":paths})
            )?["ok"],
            false
        );
    }
    assert!(api.requests().is_empty());
    Ok(())
}

#[test]
fn input_and_output_roots_and_non_clobber_saves_remain_product_boundaries() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = MockMicu::new()?;
    let allowed = root.path().join("allowed");
    fs::create_dir(&allowed)?;
    let external = root.path().join("outside.png");
    fs::write(&external, png(32, 24, 1)?)?;
    let mut env = api.env();
    env.push(("MICU_INPUT_ROOT", allowed.to_str().unwrap()));
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &env,
    )?;
    server.initialize()?;
    let denied = server.call(
        "image_edit",
        json!({"prompt":"reject","image_path":external}),
    )?;
    assert_eq!(denied["ok"], false);
    let outside = tempfile::tempdir()?;
    let denied = server.call(
        "image_generate",
        json!({"prompt":"reject","save_dir":outside.path()}),
    )?;
    assert_eq!(denied["ok"], false);
    assert!(api.requests().is_empty());
    let first = server.call(
        "image_generate",
        json!({"prompt":"first","basename":"same"}),
    )?;
    let before = fs::read(first["saved"][0]["path"].as_str().unwrap())?;
    let second = server.call(
        "image_generate",
        json!({"prompt":"second","basename":"same"}),
    )?;
    assert_ne!(first["saved"][0]["path"], second["saved"][0]["path"]);
    assert_eq!(
        fs::read(first["saved"][0]["path"].as_str().unwrap())?,
        before
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinks_cannot_escape_the_input_or_output_root() -> Result<()> {
    let root = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let api = MockMicu::new()?;
    fs::write(outside.path().join("image.png"), png(32, 24, 1)?)?;
    std::os::unix::fs::symlink(outside.path(), root.path().join("escape"))?;
    let mut env = api.env();
    env.push(("MICU_INPUT_ROOT", root.path().to_str().unwrap()));
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &env,
    )?;
    server.initialize()?;
    assert_eq!(
        server.call(
            "image_edit",
            json!({"prompt":"reject","image_path":root.path().join("escape/image.png")})
        )?["ok"],
        false
    );
    assert_eq!(
        server.call(
            "image_generate",
            json!({"prompt":"reject","save_dir":root.path().join("escape")})
        )?["ok"],
        false
    );
    assert!(api.requests().is_empty());
    Ok(())
}
