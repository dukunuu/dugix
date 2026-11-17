//! Image and video generation and editing, and saved reference helpers.

use std::fmt::Write as _;

use base64::Engine;

use super::{ToolBox, valid_relative_path};
use crate::provider::openrouter::normalize_video_params;

impl ToolBox {
    #[allow(clippy::too_many_lines)]
    pub(super) async fn run_media(&self, name: &str, args: &str) -> (String, String) {
        match name {
            "generate_image" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let prompt = v.get("prompt").and_then(|x| x.as_str()).unwrap_or("");
                let image_id = v
                    .get("image_id")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty());
                let size = v
                    .get("size")
                    .and_then(|x| x.as_str())
                    .unwrap_or("1024x1024");
                let status = if image_id.is_some() {
                    "Editing image…".to_string()
                } else {
                    "Generating image…".to_string()
                };
                let result = match &self.image_gen_backend {
                    None => "no image generation model configured — set one in /config".to_string(),
                    Some((provider, model)) => {
                        if prompt.is_empty() {
                            "prompt must not be empty".to_string()
                        } else {
                            let image_data = image_id.and_then(|id| {
                                resolve_image(&self.space_files_dir, id).or_else(|| {
                                    let stem = std::path::Path::new(id)
                                        .file_stem()
                                        .and_then(|s| s.to_str())
                                        .unwrap_or(id);
                                    let files = self.space_files_dir.as_path();
                                    std::fs::read_dir(files).ok().and_then(|e| {
                                        e.flatten()
                                            .find(|e| {
                                                e.path().file_stem().is_some_and(|s| s == stem)
                                            })
                                            .and_then(|e| std::fs::read(e.path()).ok())
                                    })
                                })
                            });
                            if let Some(id) = image_id.filter(|_| image_data.is_none()) {
                                format!("image not found: {id}")
                            } else {
                                match provider
                                    .generate_image(model, prompt, size, image_data.as_deref())
                                    .await
                                {
                                    Err(e) => format!("image generation failed: {e}"),
                                    Ok((png_bytes, ext)) => {
                                        let id = uuid::Uuid::new_v4().to_string();
                                        let filename = format!("{id}.{ext}");
                                        let img_path = self.space_files_dir.join(&filename);
                                        if let Err(e) =
                                            std::fs::create_dir_all(&self.space_files_dir)
                                        {
                                            format!("cannot create images dir: {e}")
                                        } else if let Err(e) = std::fs::write(&img_path, &png_bytes)
                                        {
                                            format!("cannot write image: {e}")
                                        } else {
                                            let _ = std::fs::create_dir_all(&self.space_files_dir);
                                            let _ = std::fs::write(
                                                self.space_files_dir.join(&filename),
                                                &png_bytes,
                                            );
                                            let description =
                                                format!("generated image of {prompt}");
                                            serde_json::json!({
                                                "id": id,
                                                "path": img_path.to_string_lossy(),
                                                "description": description,
                                            })
                                            .to_string()
                                        }
                                    }
                                }
                            }
                        }
                    }
                };
                (result, status)
            }
            "generate_video" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let prompt = v.get("prompt").and_then(|x| x.as_str()).unwrap_or("");
                let duration = u32::try_from(
                    v.get("duration")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(6),
                )
                .unwrap_or(6);
                let resolution = v
                    .get("resolution")
                    .and_then(|x| x.as_str())
                    .unwrap_or("720p");
                let aspect_ratio = v
                    .get("aspect_ratio")
                    .and_then(|x| x.as_str())
                    .unwrap_or("16:9");
                let generate_audio = v
                    .get("generate_audio")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let first_frame_id = v
                    .get("first_frame_id")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty());
                let last_frame_id = v
                    .get("last_frame_id")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty());
                let ref_image_id = v
                    .get("ref_image_id")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty());
                let character_refs: Vec<String> = v
                    .get("character_refs")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let location_refs: Vec<String> = v
                    .get("location_refs")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let seed = v
                    .get("seed")
                    .and_then(serde_json::Value::as_i64)
                    .map(|x| i32::try_from(x).unwrap_or_default());
                let source_video_id = v
                    .get("source_video_id")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty());
                let status = "Generating video…".to_string();
                let result = match &self.video_gen_backend {
                    None => "no video generation model configured — set one in /config".to_string(),
                    Some((provider, model)) => {
                        let (duration, resolution, aspect_ratio) =
                            normalize_video_params(model, duration, resolution, aspect_ratio);
                        if prompt.is_empty() {
                            "prompt must not be empty".to_string()
                        } else {
                            let first_frame = first_frame_id
                                .and_then(|id| resolve_image(&self.space_files_dir, id));
                            let last_frame = last_frame_id
                                .and_then(|id| resolve_image(&self.space_files_dir, id));
                            let ref_img = ref_image_id
                                .and_then(|id| resolve_image(&self.space_files_dir, id));
                            let named = resolve_named_references(
                                &self.space_files_dir,
                                &character_refs,
                                &location_refs,
                            );
                            let mut all_refs = Vec::new();
                            if let Some(d) = ref_img {
                                all_refs.push(d);
                            }
                            all_refs.extend(named);
                            let provider_options = source_video_id.and_then(|sid| {
                                if !valid_relative_path(sid) {
                                    return None;
                                }
                                let path = self.space_files_dir.join(format!("{sid}.mp4"));
                                let data = std::fs::read(&path).ok()?;
                                let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
                                Some(serde_json::json!({
                                    "alibaba": {
                                        "parameters": {
                                            "video": format!("data:video/mp4;base64,{}", b64)
                                        }
                                    }
                                }))
                            });
                            match provider
                                .generate_video(crate::provider::openrouter::VideoRequest {
                                    model: model.clone(),
                                    prompt: prompt.to_string(),
                                    duration,
                                    resolution: resolution.clone(),
                                    aspect_ratio: aspect_ratio.clone(),
                                    generate_audio,
                                    first_frame,
                                    last_frame,
                                    input_references: all_refs,
                                    seed,
                                    provider_options,
                                })
                                .await
                            {
                                Err(e) => format!("video generation failed: {e}"),
                                Ok((mp4_bytes, cost)) => {
                                    let id = uuid::Uuid::new_v4().to_string();
                                    let video_filename = format!("{id}.mp4");
                                    let thumb_filename = format!("{id}_first.png");
                                    let last_thumb_filename = format!("{id}_last.png");
                                    let meta_filename = format!("{id}.json");
                                    let video_path = self.space_files_dir.join(&video_filename);
                                    if let Err(e) = std::fs::create_dir_all(&self.space_files_dir) {
                                        format!("cannot create files dir: {e}")
                                    } else if let Err(e) = std::fs::write(&video_path, &mp4_bytes) {
                                        format!("cannot write video: {e}")
                                    } else {
                                        let thumb_path = self.space_files_dir.join(&thumb_filename);
                                        let last_thumb_path =
                                            self.space_files_dir.join(&last_thumb_filename);
                                        let has_ffmpeg =
                                            extract_ffmpeg_frame(&video_path, &thumb_path, false);
                                        if has_ffmpeg {
                                            extract_ffmpeg_frame(
                                                &video_path,
                                                &last_thumb_path,
                                                true,
                                            );
                                        } else if let Some(fid) = first_frame_id
                                            && let Some(data) =
                                                resolve_image(&self.space_files_dir, fid)
                                        {
                                            let _ = std::fs::write(&thumb_path, data);
                                        }
                                        let now = chrono::Utc::now().to_rfc3339();
                                        let meta = serde_json::json!({
                                            "type": "generated_video",
                                            "video_id": id,
                                            "prompt": prompt,
                                            "model": model,
                                            "duration_sec": duration,
                                            "resolution": resolution,
                                            "aspect_ratio": aspect_ratio,
                                            "has_audio": generate_audio,
                                            "character_refs": character_refs,
                                            "location_refs": location_refs,
                                            "seed": seed,
                                            "cost_usd": cost,
                                            "generated_at": now,
                                        });
                                        let _ = std::fs::write(
                                            self.space_files_dir.join(&meta_filename),
                                            serde_json::to_string_pretty(&meta).unwrap_or_default(),
                                        );
                                        let desc = format!("generated video of {prompt}");
                                        serde_json::json!({
                                            "id": id,
                                            "video_path": video_path.to_string_lossy(),
                                            "thumbnail_path": if thumb_path.exists() { thumb_path.to_string_lossy().to_string() } else { String::new() },
                                            "metadata_path": self.space_files_dir.join(&meta_filename).to_string_lossy(),
                                            "description": desc,
                                            "last_thumb": if last_thumb_path.exists() { last_thumb_path.to_string_lossy().to_string() } else { String::new() },
                                            "model": model,
                                            "duration_sec": duration,
                                            "cost_usd": cost,
                                        }).to_string()
                                    }
                                }
                            }
                        }
                    }
                };
                (result, status)
            }
            "edit_video" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let video_id = v
                    .get("video_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let lighting = v
                    .get("lighting")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty());
                let camera_move = v
                    .get("camera_move")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty());
                let intensity = v
                    .get("intensity")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.5)
                    .clamp(0.0, 1.0);
                let speed = v
                    .get("speed")
                    .and_then(serde_json::Value::as_f64)
                    .filter(|x| *x > 0.0);
                let trim_start = v
                    .get("trim_start")
                    .and_then(serde_json::Value::as_f64)
                    .filter(|x| *x >= 0.0);
                let trim_end = v
                    .get("trim_end")
                    .and_then(serde_json::Value::as_f64)
                    .filter(|x| *x >= 0.0);
                let remove_audio = v
                    .get("remove_audio")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let status = "Editing video…".to_string();
                let result = if video_id.is_empty() {
                    "video_id is required".to_string()
                } else if !valid_relative_path(&video_id) {
                    "invalid video_id".to_string()
                } else if !ffmpeg_available() {
                    "ffmpeg not found — install ffmpeg to edit videos".to_string()
                } else {
                    let src_path = self.space_files_dir.join(format!("{video_id}.mp4"));
                    if src_path.exists() {
                        let id = uuid::Uuid::new_v4().to_string();
                        let output_path = self.space_files_dir.join(format!("{id}.mp4"));
                        let thumb_path = self.space_files_dir.join(format!("{id}_first.png"));
                        let meta_path = self.space_files_dir.join(format!("{id}.json"));

                        let mut cmd = std::process::Command::new("ffmpeg");
                        cmd.arg("-y");
                        if let Some(s) = trim_start {
                            cmd.arg("-ss").arg(format!("{s}"));
                        }
                        if let Some(e) = trim_end {
                            cmd.arg("-to").arg(format!("{e}"));
                        }
                        cmd.arg("-i").arg(&src_path);

                        let mut filter_parts: Vec<String> = Vec::new();

                        if let Some(mv) = camera_move {
                            let m = intensity * 0.2;
                            let cf = build_camera_filter(mv, m);
                            filter_parts.push(cf);
                        }

                        if let Some(lt) = lighting {
                            let lf = build_lighting_filter(lt, intensity);
                            filter_parts.push(lf);
                        }

                        if let Some(sp) = speed {
                            filter_parts.push(format!("setpts={}*PTS", 1.0 / sp));
                            let atempo = format!("atempo={}", (1.0 / sp).clamp(0.5, 2.0));
                            cmd.arg("-af").arg(&atempo);
                        }

                        if !filter_parts.is_empty() {
                            cmd.arg("-vf").arg(filter_parts.join(","));
                        }

                        if remove_audio {
                            cmd.arg("-an");
                        }

                        cmd.arg(&output_path)
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null());

                        match cmd.status() {
                            Err(e) => format!("ffmpeg failed to start: {e}"),
                            Ok(s) if !s.success() => "ffmpeg returned non-zero exit".to_string(),
                            Ok(_) => {
                                extract_ffmpeg_frame(&output_path, &thumb_path, false);
                                let now = chrono::Utc::now().to_rfc3339();
                                let meta = serde_json::json!({
                                    "type": "edited_video",
                                    "video_id": id,
                                    "source_video_id": video_id,
                                    "lighting": lighting,
                                    "camera_move": camera_move,
                                    "intensity": intensity,
                                    "speed": speed,
                                    "trim_start": trim_start,
                                    "trim_end": trim_end,
                                    "remove_audio": remove_audio,
                                    "generated_at": now,
                                });
                                let _ = std::fs::write(
                                    &meta_path,
                                    serde_json::to_string_pretty(&meta).unwrap_or_default(),
                                );
                                serde_json::json!({
                                    "id": id,
                                    "video_path": output_path.to_string_lossy(),
                                    "thumbnail_path": if thumb_path.exists() { thumb_path.to_string_lossy().to_string() } else { String::new() },
                                    "metadata_path": meta_path.to_string_lossy(),
                                    "description": format!("edited video from {video_id}"),
                                }).to_string()
                            }
                        }
                    } else {
                        format!("video '{video_id}' not found")
                    }
                };
                (result, status)
            }
            "extract_frame" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let video_id = v
                    .get("video_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let time_sec = v
                    .get("time_sec")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0);
                let fmt = v.get("format").and_then(|x| x.as_str()).unwrap_or("png");
                let status = "Extracting frame…".to_string();
                let result = if video_id.is_empty() {
                    "video_id is required".to_string()
                } else if !valid_relative_path(&video_id) {
                    "invalid video_id".to_string()
                } else {
                    let src_path = self.space_files_dir.join(format!("{video_id}.mp4"));
                    if !src_path.exists() {
                        format!("video '{video_id}' not found")
                    } else if !ffmpeg_available() {
                        "ffmpeg not found — install ffmpeg to extract frames".to_string()
                    } else {
                        let id = uuid::Uuid::new_v4().to_string();
                        let ext = if fmt == "jpg" { "jpg" } else { "png" };
                        let output = self.space_files_dir.join(format!("{id}.{ext}"));
                        let status_code = std::process::Command::new("ffmpeg")
                            .arg("-y")
                            .arg("-ss")
                            .arg(format!("{time_sec}"))
                            .arg("-i")
                            .arg(&src_path)
                            .arg("-vframes")
                            .arg("1")
                            .arg("-f")
                            .arg("image2")
                            .arg(&output)
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .status();
                        match status_code {
                            Err(_) => "ffmpeg execution failed".to_string(),
                            Ok(s) if !s.success() => "ffmpeg failed to extract frame".to_string(),
                            Ok(_) => {
                                serde_json::json!({
                                    "id": id,
                                    "path": output.to_string_lossy(),
                                    "description": format!("frame at {time_sec}s from video {video_id}"),
                                }).to_string()
                            }
                        }
                    }
                };
                (result, status)
            }
            "stitch_videos" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let video_ids: Vec<String> = v
                    .get("video_ids")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let status = "Stitching videos…".to_string();
                let result = if video_ids.is_empty() {
                    "video_ids must not be empty".to_string()
                } else if !ffmpeg_available() {
                    "ffmpeg not found — install ffmpeg to stitch videos".to_string()
                } else {
                    let mut video_files = Vec::new();
                    let mut total_cost = 0.0_f64;
                    for vid_id in &video_ids {
                        if !valid_relative_path(vid_id) {
                            break;
                        }
                        let mp4 = self.space_files_dir.join(format!("{vid_id}.mp4"));
                        if !mp4.exists() {
                            break;
                        }
                        let meta_path = self.space_files_dir.join(format!("{vid_id}.json"));
                        if let Ok(json_str) = std::fs::read_to_string(&meta_path)
                            && let Ok(meta) = serde_json::from_str::<serde_json::Value>(&json_str)
                        {
                            total_cost += meta
                                .get("cost_usd")
                                .and_then(serde_json::Value::as_f64)
                                .unwrap_or(0.0);
                        }
                        video_files.push(mp4);
                    }
                    if video_files.len() == video_ids.len() {
                        let id = uuid::Uuid::new_v4().to_string();
                        let concat_name = format!("_stitch_concat_{id}.txt");
                        let concat_path = self.space_files_dir.join(&concat_name);
                        let output_name = format!("_stitch_{id}.mp4");
                        let output_path = self.space_files_dir.join(&output_name);
                        let thumb_name = format!("{id}_first.png");
                        let thumb_path = self.space_files_dir.join(&thumb_name);
                        let concat_content: String =
                            video_files.iter().fold(String::new(), |mut c, p| {
                                let _ = writeln!(c, "file '{}'", p.display());
                                c
                            });
                        if let Err(e) = std::fs::write(&concat_path, &concat_content) {
                            format!("cannot write concat file: {e}")
                        } else {
                            let stitched = std::process::Command::new("ffmpeg")
                                .arg("-y")
                                .arg("-f")
                                .arg("concat")
                                .arg("-safe")
                                .arg("0")
                                .arg("-i")
                                .arg(&concat_path)
                                .arg("-c")
                                .arg("copy")
                                .arg(&output_path)
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null())
                                .status();
                            let _ = std::fs::remove_file(&concat_path);
                            match stitched {
                                Err(_) => "ffmpeg execution failed".to_string(),
                                Ok(s) if !s.success() => "ffmpeg concat failed".to_string(),
                                Ok(_) => {
                                    if let Some(first) = video_files.first() {
                                        extract_ffmpeg_frame(first, &thumb_path, false);
                                    }
                                    let now = chrono::Utc::now().to_rfc3339();
                                    let seq_meta = serde_json::json!({
                                        "type": "video_sequence",
                                        "video_id": id,
                                        "shot_ids": video_ids,
                                        "total_cost_usd": total_cost,
                                        "generated_at": now,
                                    });
                                    let _ = std::fs::write(
                                        self.space_files_dir.join(format!("{id}.json")),
                                        serde_json::to_string_pretty(&seq_meta).unwrap_or_default(),
                                    );
                                    serde_json::json!({
                                        "id": id,
                                        "video_path": output_path.to_string_lossy(),
                                        "thumbnail_path": if thumb_path.exists() { thumb_path.to_string_lossy().to_string() } else { String::new() },
                                        "metadata_path": self.space_files_dir.join(format!("{id}.json")).to_string_lossy(),
                                        "description": format!("stitched sequence of {} clips", video_files.len()),
                                        "cost_usd": total_cost,
                                    }).to_string()
                                }
                            }
                        }
                    } else {
                        format!("some video files not found for IDs: {video_ids:?}")
                    }
                };
                (result, status)
            }
            "save_reference" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let name = v
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let ref_type = v
                    .get("type")
                    .and_then(|x| x.as_str())
                    .unwrap_or("character")
                    .to_string();
                let image_id = v
                    .get("image_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let description = v
                    .get("description")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let status = format!("Saving reference '{name}'…");
                let result = if name.is_empty() || image_id.is_empty() {
                    "name and image_id are required".to_string()
                } else {
                    let mut refs = read_video_refs(&self.space_files_dir);
                    if let Some(obj) = refs.as_object_mut() {
                        obj.insert(
                            name.clone(),
                            serde_json::json!({
                                "name": name,
                                "type": ref_type,
                                "description": description,
                                "image_id": image_id,
                                "created_at": chrono::Utc::now().to_rfc3339(),
                            }),
                        );
                    }
                    match write_video_refs(&self.space_files_dir, &refs) {
                        Err(e) => format!("failed to save reference: {e}"),
                        Ok(()) => format!(
                            "saved reference '{name}' ({ref_type}) — use in generate_video with character_refs/location_refs"
                        ),
                    }
                };
                (result, status)
            }
            "list_references" => {
                let status = "Listing references…".to_string();
                let refs = read_video_refs(&self.space_files_dir);
                let result = if refs.as_object().is_none_or(serde_json::Map::is_empty) {
                    "no references saved yet — use video_references with action=save to create one"
                        .to_string()
                } else {
                    let pretty: Vec<serde_json::Value> = refs
                        .as_object()
                        .map(|o| o.values().cloned().collect())
                        .unwrap_or_default();
                    serde_json::to_string_pretty(&pretty).unwrap_or_default()
                };
                (result, status)
            }
            "delete_reference" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let name = v
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let status = format!("Deleting reference '{name}'…");
                let result = if name.is_empty() {
                    "name is required".to_string()
                } else {
                    let mut refs = read_video_refs(&self.space_files_dir);
                    if refs.get(&name).is_none() {
                        format!("reference '{name}' not found")
                    } else {
                        refs.as_object_mut().map(|o| o.remove(&name));
                        match write_video_refs(&self.space_files_dir, &refs) {
                            Err(e) => format!("failed to delete reference: {e}"),
                            Ok(()) => format!("deleted reference '{name}'"),
                        }
                    }
                };
                (result, status)
            }
            other => (
                format!("unknown tool: {other}"),
                "Running tool…".to_string(),
            ),
        }
    }
}

// ── Video generation helpers ──────────────────────────────────────────────────

/// Read an image file from the files directory by id or `id.png`.
pub(super) fn resolve_image(files_dir: &std::path::Path, id: &str) -> Option<Vec<u8>> {
    if !valid_relative_path(id) {
        return None;
    }
    let direct = files_dir.join(id);
    std::fs::read(&direct)
        .ok()
        .or_else(|| std::fs::read(files_dir.join(format!("{id}.png"))).ok())
        // Only real images: a PDF or text file is never sent as a reference.
        .filter(|bytes| crate::extract::vision_mime(bytes).is_some())
}

/// Extract one frame from a video via ffmpeg; `use_eof` takes the last frame.
pub(super) fn extract_ffmpeg_frame(
    video_path: &std::path::Path,
    output_path: &std::path::Path,
    use_eof: bool,
) -> bool {
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.arg("-y");
    if use_eof {
        cmd.arg("-sseof").arg("-0.1");
    } else {
        cmd.arg("-ss").arg("0");
    }
    cmd.arg("-i")
        .arg(video_path)
        .arg("-vframes")
        .arg("1")
        .arg("-f")
        .arg("image2")
        .arg(output_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The `_video_refs.json` registry, or `{}` when absent.
pub(super) fn read_video_refs(files_dir: &std::path::Path) -> serde_json::Value {
    let path = files_dir.join("_video_refs.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Write the `_video_refs.json` registry.
pub(super) fn write_video_refs(
    files_dir: &std::path::Path,
    refs: &serde_json::Value,
) -> anyhow::Result<()> {
    let path = files_dir.join("_video_refs.json");
    let json = serde_json::to_string_pretty(refs)?;
    std::fs::write(&path, json)?;
    Ok(())
}

pub(super) fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Resolve named character and location references from `_video_refs.json` to
/// image data.
pub(super) fn resolve_named_references(
    files_dir: &std::path::Path,
    character_refs: &[String],
    location_refs: &[String],
) -> Vec<Vec<u8>> {
    let refs = read_video_refs(files_dir);
    let mut images = Vec::new();
    for name in character_refs.iter().chain(location_refs.iter()) {
        if let Some(entry) = refs.get(name)
            && let Some(image_id) = entry.get("image_id").and_then(|id| id.as_str())
            && let Some(data) = resolve_image(files_dir, image_id)
        {
            images.push(data);
        }
    }
    images
}

/// An ffmpeg `crop` + `scale` filter for a camera-move preset; `margin` (0–1)
/// sets how much the frame moves or zooms.
pub(super) fn build_camera_filter(move_type: &str, margin: f64) -> String {
    // t = time in sec, du = input duration in sec, iw/ih = input width/height
    match move_type {
        "dolly_in" => {
            // Start full frame, end zoomed in centered
            let zoom = margin; // e.g. 0.15 → 15% zoom
            format!(
                "crop=w='iw-(iw*{zoom})*t/du':h='ih-(ih*{zoom})*t/du':x='(iw-w)/2':y='(ih-h)/2',scale=iw:ih"
            )
        }
        "dolly_out" => {
            let zoom = margin;
            format!(
                "crop=w='iw-(iw*{zoom})*(1-t/du)':h='ih-(ih*{zoom})*(1-t/du)':x='(iw-w)/2':y='(ih-h)/2',scale=iw:ih"
            )
        }
        "pan_left" => {
            // Crop window slides from right to left
            let m = margin.max(0.05);
            format!(
                "crop=w='iw*(1-{m})':h='ih*(1-{m})':x='(iw-w)*(1-t/du)':y='(ih-h)/2',scale=iw:ih"
            )
        }
        "pan_right" => {
            let m = margin.max(0.05);
            format!("crop=w='iw*(1-{m})':h='ih*(1-{m})':x='(iw-w)*t/du':y='(ih-h)/2',scale=iw:ih")
        }
        "tilt_up" => {
            let m = margin.max(0.05);
            format!(
                "crop=w='iw*(1-{m})':h='ih*(1-{m})':x='(iw-w)/2':y='(ih-h)*(1-t/du)',scale=iw:ih"
            )
        }
        "tilt_down" => {
            let m = margin.max(0.05);
            format!("crop=w='iw*(1-{m})':h='ih*(1-{m})':x='(iw-w)/2':y='(ih-h)*t/du',scale=iw:ih")
        }
        _ => String::new(),
    }
}

/// An ffmpeg filter for a lighting preset, scaled by `intensity` (0–1).
pub(super) fn build_lighting_filter(preset: &str, intensity: f64) -> String {
    let i = intensity.clamp(0.0, 1.0);
    match preset {
        "noir" => {
            let b = -0.05 * i;
            let c = 0.3f64.mul_add(i, 1.0);
            let s = 0.8f64.mul_add(-i, 1.0);
            format!(
                "eq=brightness={b:.3}:contrast={c:.3}:saturation={s:.3},colorbalance=rh={rh:.3}:gh={gh:.3}:bh={bh:.3}",
                b = b,
                c = c,
                s = s,
                rh = -0.1 * i,
                gh = -0.05 * i,
                bh = 0.1 * i
            )
        }
        "warm" => {
            let s = 0.3f64.mul_add(i, 1.0);
            format!(
                "eq=saturation={s:.3},colorbalance=rs={rs:.3}:gs={gs:.3}:bs={bs:.3}",
                s = s,
                rs = 0.1 * i,
                gs = 0.05 * i,
                bs = -0.05 * i
            )
        }
        "cold" => {
            let s = 0.1f64.mul_add(-i, 1.0);
            format!(
                "eq=saturation={s:.3},colorbalance=rs={rs:.3}:gs={gs:.3}:bs={bs:.3}",
                s = s,
                rs = -0.05 * i,
                gs = -0.02 * i,
                bs = 0.1 * i
            )
        }
        "vintage" => {
            let b = 0.03 * i;
            let c = 0.1f64.mul_add(-i, 1.0);
            let s = 0.3f64.mul_add(-i, 1.0);
            format!(
                "eq=brightness={b:.3}:contrast={c:.3}:saturation={s:.3},colorbalance=rh={rh:.3}:rm={rm:.3}:gs={gs:.3}",
                b = b,
                c = c,
                s = s,
                rh = 0.05 * i,
                rm = 0.05 * i,
                gs = -0.05 * i
            )
        }
        "vivid" => {
            let s = 0.5f64.mul_add(i, 1.0);
            format!("eq=saturation={s:.3}:contrast=1.1:brightness=0.02")
        }
        "bleach_bypass" => {
            let c = 0.4f64.mul_add(i, 1.0);
            let s = 0.6f64.mul_add(-i, 1.0);
            format!(
                "eq=contrast={c:.3}:saturation={s:.3}:brightness=0.02:gamma={g:.3}",
                c = c,
                s = s,
                g = 0.1f64.mul_add(i, 1.0)
            )
        }
        _ => String::new(),
    }
}
