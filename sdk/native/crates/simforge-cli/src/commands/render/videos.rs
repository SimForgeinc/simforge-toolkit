use super::*;

/// Encode one video per sensor from the job's artifacts: each camera keeps
/// only the ticks on its own schedule (the platform's `wantedMicros`), lidar
/// and radar frames are rasterized every tick at the sensor-video format.
#[allow(clippy::too_many_arguments)]
pub(super) fn encode_videos(
    out: &Path,
    rig: &Rig,
    schedules: &[FixedSchedule],
    frame_times: &[f64],
    results: &Value,
    sensor_video: schedule::SensorVideoFormat,
    ffmpeg: Option<&Path>,
    mode: VideoMode,
) -> Result<Vec<Value>, CliError> {
    let Some(ffmpeg) = ffmpeg else {
        let reason = if mode == VideoMode::Off {
            "off"
        } else {
            "ffmpeg_not_found"
        };
        return Ok(rig
            .sources
            .iter()
            .map(|s| video::skipped_video(s.output_name(), reason))
            .collect());
    };
    // Artifact paths by (sensor, tick, pass).
    let mut by_key: BTreeMap<(String, u64, String), String> = BTreeMap::new();
    for a in results["artifacts"].as_array().into_iter().flatten() {
        if let (Some(sensor), Some(tick), Some(pass), Some(path)) = (
            a["sensorId"].as_str(),
            a["tick"].as_u64(),
            a["pass"].as_str(),
            a["path"].as_str(),
        ) {
            by_key.insert((sensor.to_owned(), tick, pass.to_owned()), path.to_owned());
        }
    }
    let mut videos = Vec::new();
    for source in &rig.sources {
        let name = source.output_name();
        let output = out.join("video").join(format!("{name}.mp4"));
        match source {
            RenderSource::Camera(camera) => {
                let wanted: BTreeSet<i64> = schedules
                    .iter()
                    .find(|s| s.source_id == name)
                    .map(|s| s.frame_micros().into_iter().collect())
                    .unwrap_or_default();
                let format = VideoFormat {
                    width: camera.attributes.width,
                    height: camera.attributes.height,
                    frames_per_second: camera.attributes.fps,
                };
                let mut encoder = VideoEncoder::new(ffmpeg, &output, format, VideoCodec::Libx264)?;
                for (tick, t) in frame_times.iter().enumerate() {
                    if !wanted.contains(&micros(*t)) {
                        continue;
                    }
                    let path = by_key
                        .get(&(name.to_owned(), tick as u64, "rgb".to_owned()))
                        .ok_or_else(|| {
                            CliError::new(
                                "render_failed",
                                format!("camera {name} has no rgb frame at tick {tick}"),
                            )
                        })?;
                    let (_, _, rgba) = video::decode_rgba_png(&out.join(path))?;
                    encoder.write(&rgba)?;
                }
                let done = encoder.finish()?;
                videos.push(json!({ "sourceId": name, "status": "encoded", "path": done.path, "frames": done.frames, "codec": done.codec.as_str() }));
            }
            RenderSource::Lidar(lidar) => {
                let format = VideoFormat {
                    width: sensor_video.width,
                    height: sensor_video.height,
                    frames_per_second: sensor_video.frames_per_second,
                };
                let mut raster = LidarVideoRasterizer::new(
                    format.width,
                    format.height,
                    lidar.attributes.range_m,
                    lidar.common.transform.position.y,
                )?;
                let mut encoder = VideoEncoder::new(ffmpeg, &output, format, VideoCodec::Libx264)?;
                let mut dropped = DroppedPoints::default();
                for tick in 0..frame_times.len() {
                    let path = by_key
                        .iter()
                        .find(|((s, t, _), _)| s == name && *t == tick as u64)
                        .map(|(_, p)| p.clone())
                        .ok_or_else(|| {
                            CliError::new(
                                "render_failed",
                                format!("lidar {name} has no scan at tick {tick}"),
                            )
                        })?;
                    let bytes = std::fs::read(out.join(&path))
                        .map_err(|e| CliError::new("render_failed", e.to_string()))?;
                    let scan = parse_lidar_ply(&bytes, &format!("lidar {name} tick {tick}"))?;
                    dropped.add(tick, scan.dropped);
                    let frame = raster.frame(&scan).to_vec();
                    encoder.write(&frame)?;
                }
                let done = encoder.finish()?;
                videos.push(dropped.record(json!({ "sourceId": name, "status": "encoded", "path": done.path, "frames": done.frames, "codec": done.codec.as_str() })));
            }
            RenderSource::Radar(radar) => {
                let format = VideoFormat {
                    width: sensor_video.width,
                    height: sensor_video.height,
                    frames_per_second: sensor_video.frames_per_second,
                };
                let mut raster = RadarVideoRasterizer::new(
                    format.width,
                    format.height,
                    radar.attributes.horizontal_fov_deg,
                    radar.attributes.range_m,
                )?;
                let mut encoder = VideoEncoder::new(ffmpeg, &output, format, VideoCodec::Libx264)?;
                let mut dropped = DroppedPoints::default();
                for tick in 0..frame_times.len() {
                    let path = by_key
                        .iter()
                        .find(|((s, t, _), _)| s == name && *t == tick as u64)
                        .map(|(_, p)| p.clone())
                        .ok_or_else(|| {
                            CliError::new(
                                "render_failed",
                                format!("radar {name} has no scan at tick {tick}"),
                            )
                        })?;
                    let bytes = std::fs::read(out.join(&path))
                        .map_err(|e| CliError::new("render_failed", e.to_string()))?;
                    let scan = parse_radar_csv(&bytes, &format!("radar {name} tick {tick}"))?;
                    dropped.add(tick, scan.dropped);
                    let frame = raster.frame(&scan).to_vec();
                    encoder.write(&frame)?;
                }
                let done = encoder.finish()?;
                videos.push(dropped.record(json!({ "sourceId": name, "status": "encoded", "path": done.path, "frames": done.frames, "codec": done.codec.as_str() })));
            }
        }
    }
    let _ = std::io::stdout().flush();
    Ok(videos)
}

/// Non-finite returns a sensor video left out (`sensor_points_dropped`).
#[derive(Default)]
struct DroppedPoints {
    points: usize,
    frames: usize,
    first_tick: Option<usize>,
}

impl DroppedPoints {
    fn add(&mut self, tick: usize, dropped: usize) {
        if dropped > 0 {
            self.points += dropped;
            self.frames += 1;
            self.first_tick.get_or_insert(tick);
        }
    }

    /// The video record, with `pointsDropped` when any return was dropped.
    fn record(&self, mut video: Value) -> Value {
        if self.points > 0 {
            video["pointsDropped"] = json!({ "points": self.points, "frames": self.frames, "firstTick": self.first_tick });
        }
        video
    }
}
