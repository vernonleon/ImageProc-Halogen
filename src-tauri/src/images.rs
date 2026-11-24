use base64::{engine::general_purpose::STANDARD, Engine};
use image::{ImageDecoder, ImageReader};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff"];
#[derive(Serialize)]
struct ImageEntry {
    path: String,
    name: String,
    caption: String,
    warning: String,
}
fn supported(p: &Path) -> bool {
    p.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| EXTENSIONS.contains(&s.to_lowercase().as_str()))
}
fn collect(paths: Vec<PathBuf>) -> (Vec<ImageEntry>, Vec<String>) {
    let mut stack = paths;
    let mut seen = HashSet::new();
    let mut images = vec![];
    let mut warnings = vec![];
    while let Some(path) = stack.pop() {
        // Don't traverse symlink directories: avoids cycles and leaving the selected tree.
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            continue;
        }
        if path.is_dir() {
            match fs::read_dir(&path) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(e) => stack.push(e.path()),
                            Err(e) => warnings.push(e.to_string()),
                        }
                    }
                }
                Err(e) => warnings.push(format!("{}: {e}", path.display())),
            }
        } else if supported(&path) {
            let path = match path.canonicalize() {
                Ok(p) => p,
                Err(e) => {
                    warnings.push(e.to_string());
                    continue;
                }
            };
            if !seen.insert(path.clone()) {
                continue;
            }
            let sidecar = path.with_extension("txt");
            let (caption, warning) = if sidecar.exists() {
                match fs::read_to_string(&sidecar) {
                    Ok(s) => (s, String::new()),
                    Err(e) => (
                        String::new(),
                        format!("Could not read existing caption: {e}"),
                    ),
                }
            } else {
                (String::new(), String::new())
            };
            images.push(ImageEntry {
                name: path.file_name().unwrap().to_string_lossy().into(),
                path: path.to_string_lossy().into(),
                caption,
                warning,
            });
        }
    }
    images.sort_by(|a, b| a.path.cmp(&b.path));
    (images, warnings)
}
#[tauri::command]
pub async fn import_images(folder: bool) -> Result<Value, String> {
    let paths = if folder {
        rfd::AsyncFileDialog::new()
            .set_title("Import image folder (includes subfolders)")
            .pick_folder()
            .await
            .map(|p| vec![p.path().to_owned()])
    } else {
        rfd::AsyncFileDialog::new()
            .add_filter("Images", EXTENSIONS)
            .pick_files()
            .await
            .map(|v| v.iter().map(|p| p.path().to_owned()).collect())
    };
    let Some(paths) = paths else {
        return Ok(Value::Null);
    };
    tauri::async_runtime::spawn_blocking(move || {
        let (images, warnings) = collect(paths);
        json!({"images":images,"warnings":warnings})
    })
    .await
    .map_err(|e| e.to_string())
}
fn prepared(path: &Path, target_px: u64, fast: bool) -> Result<Value, String> {
    let reader = ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let mut img = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    let (w, h) = (img.width(), img.height());
    let scale = ((target_px.clamp(65536, 16777216) as f64 / (w as f64 * h as f64)).sqrt()).min(1.0);
    let (nw, nh) = (
        ((w as f64 * scale).floor() as u32).max(1),
        ((h as f64 * scale).floor() as u32).max(1),
    );
    if nw != w || nh != h {
        let filter = if fast {
            image::imageops::FilterType::Triangle
        } else {
            image::imageops::FilterType::Lanczos3
        };
        img = img.resize_exact(nw, nh, filter);
    }
    let mut bytes = Cursor::new(Vec::new());
    let mime = if fast {
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 75);
        img.into_rgb8()
            .write_with_encoder(encoder)
            .map_err(|e| e.to_string())?;
        "image/jpeg"
    } else {
        img.write_to(&mut bytes, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        "image/png"
    };
    Ok(
        json!({"url":format!("data:{mime};base64,{}",STANDARD.encode(bytes.into_inner())),"width":nw,"height":nh,"originalWidth":w,"originalHeight":h}),
    )
}
#[tauri::command]
pub async fn prepare_image(
    path: String,
    target_px: u64,
    fast: Option<bool>,
) -> Result<Value, String> {
    let fast = fast.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || prepared(Path::new(&path), target_px, fast))
        .await
        .map_err(|e| e.to_string())?
}
#[derive(Deserialize, Serialize)]
pub struct ExportEntry {
    path: String,
    caption: String,
    status: String,
}
fn export_to(parent: &Path, entries: &[ExportEntry], copy_images: bool) -> Result<PathBuf, String> {
    if entries.is_empty() {
        return Err("No captions to export".into());
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let dir = parent.join(format!("ImageProc-{stamp}"));
    fs::create_dir(&dir).map_err(|e| e.to_string())?;
    let result = (|| {
        let mut manifest = vec![];
        for (i, entry) in entries.iter().enumerate() {
            if entry.caption.trim().is_empty() {
                return Err("A caption is empty".to_string());
            }
            let source = Path::new(&entry.path);
            if !source.is_file() {
                return Err(format!("Source image missing: {}", source.display()));
            }
            let stem = source.file_stem().unwrap_or_default().to_string_lossy();
            let safe: String = stem
                .chars()
                .take(90)
                .map(|c| {
                    if c.is_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let base = format!("{:06}-{}", i + 1, safe);
            let ext = source.extension().and_then(|s| s.to_str()).unwrap_or("png");
            let name = format!("{base}.{ext}");
            if copy_images {
                fs::copy(source, dir.join(&name)).map_err(|e| e.to_string())?;
            }
            fs::write(dir.join(format!("{base}.txt")), entry.caption.trim())
                .map_err(|e| e.to_string())?;
            manifest.push(json!({"source":entry.path,"image":name,"caption":format!("{base}.txt"),"status":entry.status}));
        }
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&dir);
        return Err(e);
    }
    Ok(dir)
}
// Updating a dataset is additive. Prepare everything before touching existing files,
// keep rollback copies, and publish the manifest last.
fn update_dataset(
    dir: &Path,
    entries: &[ExportEntry],
    copy_images: bool,
) -> Result<PathBuf, String> {
    if entries.is_empty() {
        return Err("No captions to export".into());
    }
    let manifest_path = dir.join("manifest.json");
    if fs::symlink_metadata(&manifest_path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("The dataset manifest must not be a symbolic link".into());
    }
    let mut manifest: Vec<Value> = if manifest_path.exists() {
        serde_json::from_slice(&fs::read(&manifest_path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("Cannot read this dataset's manifest: {e}"))?
    } else {
        vec![]
    };
    let mut names = HashSet::new();
    let mut sources = HashSet::new();
    for row in &manifest {
        let source = row
            .get("source")
            .and_then(Value::as_str)
            .ok_or("Manifest entry has no source")?;
        if !sources.insert(source.to_owned()) {
            return Err("Manifest contains duplicate sources".into());
        }
        for key in ["image", "caption"] {
            let name = row
                .get(key)
                .and_then(Value::as_str)
                .ok_or("Manifest entry has no image or caption filename")?;
            if name.is_empty()
                || name.contains(['/', '\\', ':'])
                || name.starts_with('.')
                || !names.insert(name.to_lowercase())
            {
                return Err("Manifest contains unsafe or duplicate filenames".into());
            }
            if (key == "caption" && !name.ends_with(".txt"))
                || (key == "image" && !supported(Path::new(name)))
            {
                return Err("Manifest contains unexpected file extensions".into());
            }
            if fs::symlink_metadata(dir.join(name))
                .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
            {
                return Err(format!("Dataset target is not a regular file: {name}"));
            }
        }
    }
    // Include untracked files in collision checks, including case-insensitive collisions.
    for file in fs::read_dir(dir).map_err(|e| e.to_string())? {
        names.insert(
            file.map_err(|e| e.to_string())?
                .file_name()
                .to_string_lossy()
                .to_lowercase(),
        );
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let stage = dir.join(format!(".imageproc-stage-{stamp}"));
    fs::create_dir(&stage).map_err(|e| e.to_string())?;
    let mut targets: Vec<String> = vec![];
    let prepare = (|| -> Result<(), String> {
        let mut incoming = HashSet::new();
        let mut touched = HashSet::new();
        let mut sequence = 1usize;
        for entry in entries {
            if entry.caption.trim().is_empty() {
                return Err("A caption is empty".into());
            }
            let source = Path::new(&entry.path)
                .canonicalize()
                .map_err(|e| format!("Source image unavailable: {}: {e}", entry.path))?;
            if !source.is_file() {
                return Err(format!("Source is not a file: {}", entry.path));
            }
            if !incoming.insert(source.clone()) {
                return Err("Export contains duplicate source images".into());
            }
            let existing = manifest.iter().position(|row| {
                row["source"].as_str().is_some_and(|p| {
                    p == entry.path || Path::new(p).canonicalize().ok().as_ref() == Some(&source)
                }) || dir
                    .join(row["image"].as_str().unwrap())
                    .canonicalize()
                    .ok()
                    .as_ref()
                    == Some(&source)
            });
            let index = if let Some(i) = existing {
                i
            } else {
                let stem = source.file_stem().unwrap_or_default().to_string_lossy();
                let safe: String = stem
                    .chars()
                    .take(90)
                    .map(|c| {
                        if c.is_alphanumeric() || c == '-' || c == '_' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                let ext = source.extension().and_then(|s| s.to_str()).unwrap_or("png");
                let (image, caption) = loop {
                    let base = format!("{sequence:06}-{safe}");
                    sequence += 1;
                    let image = format!("{base}.{ext}");
                    let caption = format!("{base}.txt");
                    if !names.contains(&image.to_lowercase())
                        && !names.contains(&caption.to_lowercase())
                    {
                        break (image, caption);
                    }
                };
                names.insert(image.to_lowercase());
                names.insert(caption.to_lowercase());
                manifest.push(json!({"source":entry.path,"image":image,"caption":caption,"status":entry.status}));
                manifest.len() - 1
            };
            if !touched.insert(index) {
                return Err("Multiple sources refer to the same exported image".into());
            }
            let row = &mut manifest[index];
            row["status"] = json!(entry.status);
            let image = row["image"].as_str().unwrap().to_owned();
            let caption = row["caption"].as_str().unwrap().to_owned();
            if copy_images {
                fs::copy(&source, stage.join(&image)).map_err(|e| e.to_string())?;
                targets.push(image);
            }
            fs::write(stage.join(&caption), entry.caption.trim()).map_err(|e| e.to_string())?;
            targets.push(caption);
        }
        fs::write(
            stage.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        targets.push("manifest.json".into());
        fs::create_dir(stage.join("backup")).map_err(|e| e.to_string())?;
        for name in &targets {
            if dir.join(name).exists() {
                fs::copy(dir.join(name), stage.join("backup").join(name))
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    })();
    if let Err(e) = prepare {
        let _ = fs::remove_dir_all(&stage);
        return Err(e);
    }
    let mut installed: Vec<String> = vec![];
    for name in &targets {
        if let Err(e) = fs::rename(stage.join(name), dir.join(name)) {
            let mut rollback_errors = vec![];
            for previous in installed.iter().rev() {
                let backup = stage.join("backup").join(previous);
                let result = if backup.exists() {
                    fs::rename(backup, dir.join(previous))
                } else {
                    fs::remove_file(dir.join(previous))
                };
                if let Err(err) = result {
                    rollback_errors.push(err.to_string());
                }
            }
            if rollback_errors.is_empty() {
                let _ = fs::remove_dir_all(&stage);
                return Err(format!("Export failed; previous dataset restored: {e}"));
            }
            return Err(format!(
                "Export failed: {e}. Recovery copies remain in {}: {}",
                stage.display(),
                rollback_errors.join("; ")
            ));
        }
        installed.push(name.clone());
    }
    let _ = fs::remove_dir_all(&stage);
    Ok(dir.to_owned())
}

#[tauri::command]
pub async fn export_dataset(
    entries: Vec<ExportEntry>,
    copy_images: bool,
    update_existing: Option<bool>,
) -> Result<Value, String> {
    let Some(parent) = rfd::AsyncFileDialog::new()
        .set_title(if update_existing.unwrap_or(false) {
            "Choose the dataset folder to update"
        } else {
            "Choose where to save the new dataset"
        })
        .pick_folder()
        .await
    else {
        return Ok(Value::Null);
    };
    tauri::async_runtime::spawn_blocking(move || {
        let result = if update_existing.unwrap_or(false) {
            update_dataset(parent.path(), &entries, copy_images)
        } else {
            export_to(parent.path(), &entries, copy_images)
        };
        result.map(|p| json!({"path":p,"count":entries.len()}))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[cfg(test)]
mod tests {
    use super::*;
    fn temp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "imageproc-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }
    #[test]
    fn imports_recursively_deduplicates_and_reads_captions() {
        let d = temp("import");
        fs::create_dir(d.join("sub")).unwrap();
        fs::write(d.join("sub/a.PNG"), b"test").unwrap();
        fs::write(d.join("sub/a.txt"), "A hand").unwrap();
        let (rows, warnings) = collect(vec![d.clone(), d.join("sub/a.PNG")]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].caption, "A hand");
        assert!(warnings.is_empty());
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn resizes_without_upscaling_or_changing_source() {
        let d = temp("resize");
        let p = d.join("test.png");
        image::RgbImage::new(1200, 800).save(&p).unwrap();
        let original = fs::read(&p).unwrap();
        let v = prepared(&p, 240000, false).unwrap();
        assert_eq!(v["width"], 600);
        assert_eq!(v["height"], 400);
        assert!(v["url"].as_str().unwrap().starts_with("data:image/png;base64,"));
        let v = prepared(&p, 4000000, false).unwrap();
        assert_eq!(v["width"], 1200);
        assert_eq!(original, fs::read(&p).unwrap());
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn fast_preview_encodes_jpeg_and_preserves_dimensions() {
        let d = temp("fast-preview");
        let p = d.join("test.png");
        image::RgbImage::new(1200, 800).save(&p).unwrap();
        let v = prepared(&p, 65536, true).unwrap();
        assert_eq!(v["originalWidth"], 1200);
        assert_eq!(v["originalHeight"], 800);
        assert_eq!(v["width"], 313);
        assert_eq!(v["height"], 209);
        assert!(v["url"].as_str().unwrap().starts_with("data:image/jpeg;base64,"));
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn export_pairs_have_unique_names_and_preserve_sources() {
        let d = temp("export");
        let a = d.join("same.png");
        let b = d.join("same.jpg");
        fs::write(&a, b"png").unwrap();
        fs::write(&b, b"jpg").unwrap();
        let rows = vec![
            ExportEntry {
                path: a.to_string_lossy().into(),
                caption: "first".into(),
                status: "approved".into(),
            },
            ExportEntry {
                path: b.to_string_lossy().into(),
                caption: "second".into(),
                status: "approved".into(),
            },
        ];
        let out = export_to(&d, &rows, true).unwrap();
        assert_eq!(
            fs::read_to_string(out.join("000001-same.txt")).unwrap(),
            "first"
        );
        assert_eq!(
            fs::read_to_string(out.join("000002-same.txt")).unwrap(),
            "second"
        );
        assert_eq!(fs::read(out.join("000001-same.png")).unwrap(), b"png");
        assert_eq!(fs::read(a).unwrap(), b"png");
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn failed_export_leaves_no_partial_dataset() {
        let d = temp("failure");
        let rows = vec![ExportEntry {
            path: d.join("missing.png").to_string_lossy().into(),
            caption: "caption".into(),
            status: "approved".into(),
        }];
        assert!(export_to(&d, &rows, true).is_err());
        assert_eq!(fs::read_dir(&d).unwrap().count(), 0);
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn update_adds_and_revises_without_duplicate_pairs() {
        let d = temp("append");
        let a = d.join("a.png");
        let b = d.join("b.jpg");
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();
        let entry = |p: &Path, text: &str| ExportEntry {
            path: p.to_string_lossy().into(),
            caption: text.into(),
            status: "approved".into(),
        };
        let out = export_to(&d, &[entry(&a, "original")], true).unwrap();
        update_dataset(&out, &[entry(&a, "revised"), entry(&b, "new")], true).unwrap();
        let manifest: Vec<Value> =
            serde_json::from_slice(&fs::read(out.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest.len(), 2);
        assert_eq!(manifest[0]["image"], "000001-a.png");
        assert_eq!(
            fs::read_to_string(out.join("000001-a.txt")).unwrap(),
            "revised"
        );
        // Re-importing the exported image also matches the existing pair.
        update_dataset(
            &out,
            &[entry(&out.join("000001-a.png"), "reviewed again")],
            true,
        )
        .unwrap();
        let after: Vec<Value> =
            serde_json::from_slice(&fs::read(out.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(after[1], manifest[1]);
        assert_eq!(
            fs::read_to_string(out.join("000001-a.txt")).unwrap(),
            "reviewed again"
        );
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn update_failure_preserves_existing_dataset() {
        let d = temp("update-failure");
        let a = d.join("a.png");
        fs::write(&a, b"a").unwrap();
        let row = ExportEntry {
            path: a.to_string_lossy().into(),
            caption: "old".into(),
            status: "approved".into(),
        };
        let out = export_to(&d, &[row], true).unwrap();
        let before = fs::read(out.join("manifest.json")).unwrap();
        let rows = [
            ExportEntry {
                path: a.to_string_lossy().into(),
                caption: "changed".into(),
                status: "approved".into(),
            },
            ExportEntry {
                path: d.join("missing.png").to_string_lossy().into(),
                caption: "missing".into(),
                status: "approved".into(),
            },
        ];
        assert!(update_dataset(&out, &rows, true).is_err());
        assert_eq!(fs::read(out.join("manifest.json")).unwrap(), before);
        assert_eq!(fs::read_to_string(out.join("000001-a.txt")).unwrap(), "old");
        assert_eq!(fs::read_dir(&out).unwrap().count(), 3);
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn update_rejects_unsafe_manifest_and_avoids_untracked_files() {
        let d = temp("unsafe");
        let a = d.join("a.png");
        fs::write(&a, b"a").unwrap();
        let out = d.join("dataset");
        fs::create_dir(&out).unwrap();
        let row = ExportEntry {
            path: a.to_string_lossy().into(),
            caption: "caption".into(),
            status: "approved".into(),
        };
        fs::write(out.join("000001-a.txt"), "untracked").unwrap();
        update_dataset(&out, &[row], true).unwrap();
        assert_eq!(
            fs::read_to_string(out.join("000001-a.txt")).unwrap(),
            "untracked"
        );
        assert!(out.join("000002-a.png").is_file());
        fs::write(
            out.join("manifest.json"),
            r#"[{"source":"x","image":"../a.png","caption":"a.txt"}]"#,
        )
        .unwrap();
        let row = ExportEntry {
            path: a.to_string_lossy().into(),
            caption: "caption".into(),
            status: "approved".into(),
        };
        assert!(update_dataset(&out, &[row], true).is_err());
        assert_eq!(fs::read(&a).unwrap(), b"a");
        fs::remove_dir_all(d).unwrap();
    }
}
