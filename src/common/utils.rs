use std::{fs::File, io::Write};

use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};

use crate::common::common;

pub fn analyze_input(input: &str) -> anyhow::Result<()> {
    // 打开输入文件
    let avformat_wrapper = common::AVFormatContextWrapper::new(input, true, false)
        .ok_or_else(|| anyhow::anyhow!("无法打开输入文件: {} ", input))?;

    let (self_input, _reader) = avformat_wrapper.into_input();

    // 获取文件基本信息
    log::info!("📁 文件信息:");

    // 统计各类型流的数量
    let video_count = self_input
        .streams()
        .filter(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .count();
    let audio_count = self_input
        .streams()
        .filter(|s| s.parameters().medium() == ffmpeg_next::media::Type::Audio)
        .count();
    let subtitle_count = self_input
        .streams()
        .filter(|s| s.parameters().medium() == ffmpeg_next::media::Type::Subtitle)
        .count();

    log::info!("总流数: {}", self_input.streams().count());
    log::info!("视频流: {} 个", video_count);
    log::info!("音频流: {} 个", audio_count);
    log::info!("字幕流: {} 个", subtitle_count);

    // 遍历所有流
    for (idx, stream) in self_input.streams().enumerate() {
        log::info!("流 #{} 信息:", idx);

        let params = stream.parameters();

        // 基本信息
        log::info!("类型: {:?}", params.medium());
        log::info!("编码格式: {:?}", params.id());
        log::info!("Stream 索引: {}", stream.index());

        // 通过 FFI 直接访问 AVCodecParameters
        unsafe {
            let codec_params = params.as_ptr();

            // 根据流类型显示不同信息
            match params.medium() {
                ffmpeg_next::media::Type::Video => {
                    log::info!("📹 视频信息:");
                    log::info!(
                        "    分辨率: {}x{}",
                        (*codec_params).width,
                        (*codec_params).height
                    );
                    log::info!(
                        "    比特率: {} bps ({:.2} Mbps)",
                        (*codec_params).bit_rate,
                        (*codec_params).bit_rate as f64 / 1_000_000.0
                    );

                    // 帧率
                    let fps = stream.avg_frame_rate();
                    if fps.1 != 0 {
                        log::info!("    帧率: {:.2} fps", fps.0 as f64 / fps.1 as f64);
                    }

                    // 像素格式
                    log::info!("    像素格式: {}", (*codec_params).format);
                }

                ffmpeg_next::media::Type::Audio => {
                    log::info!("  🔊 音频信息:");
                    log::info!("    采样率: {} Hz", (*codec_params).sample_rate);

                    // 声道数 - 需要根据 FFmpeg 版本选择
                    let channels = (*codec_params).ch_layout.nb_channels;

                    log::info!("    声道数: {}", channels);
                    log::info!(
                        "    比特率: {} bps ({:.2} kbps)",
                        (*codec_params).bit_rate,
                        (*codec_params).bit_rate as f64 / 1_000.0
                    );
                    log::info!("    采样格式: {}", (*codec_params).format);
                }

                ffmpeg_next::media::Type::Subtitle => {
                    log::info!("  💬 字幕信息:");
                    log::info!("    编码格式: {:?}", params.id());
                }

                _ => {
                    log::info!("  ❓ 其他类型流");
                }
            }
        }

        // 时长信息
        let duration = stream.duration();
        if duration > 0 {
            let time_base = stream.time_base();
            let seconds = duration as f64 * time_base.0 as f64 / time_base.1 as f64;
            log::info!("  ⏱️  时长: {:.2} 秒", seconds);
        }

        // 元数据
        let metadata = stream.metadata();
        let meta_count = metadata.iter().count();
        if meta_count > 0 {
            log::info!("  📋 元数据 ({} 项):", meta_count);
            for (key, value) in metadata.iter() {
                log::info!("    {}: {}", key, value);
            }
        }
    }

    log::info!("分析完成");

    Ok(())
}

pub async fn download_file(file_url: &str) -> anyhow::Result<String> {
    log::info!("Downloading file: {}", file_url);

    Ok(tokio::time::timeout(
        tokio::time::Duration::from_secs(600),
        download_with_process(file_url),
    )
    .await
    .map_err(|_| anyhow::anyhow!("download timeout"))??)
}

pub async fn download_with_process(file_url: &str) -> anyhow::Result<String> {
    let response = reqwest::get(file_url).await?;

    if !response.status().is_success() {
        anyhow::bail!("http request failed: {}", response.status());
    }

    let file_name = if let Ok(parse_url) = reqwest::Url::parse(file_url) {
        parse_url
            .path_segments()
            .and_then(|s| s.last())
            .filter(|n| !n.is_empty())
            .unwrap_or("unknown.bin")
            .to_string()
    } else {
        "unknown.bin".to_string()
    };

    let content_length = response.content_length();

    let file_path = std::env::temp_dir().join(&file_name);

    log::info!("file_path: {:?}", file_path);

    let pb = if let Some(size) = content_length {
        let pb = ProgressBar::new(size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{wide_bar} {pos}/{len} {eta} {msg}")
                .unwrap(),
        );
        pb.set_message(format!("Downloading {}", &file_name));
        Some(pb)
    } else {
        None
    };

    let mut file = File::create(&file_path)?;

    let mut stream = response.bytes_stream();

    let mut downloaded = 0;
    let mut last_log_time = std::time::Instant::now();

    while let Some(item) = stream.next().await {
        let chunk = item?;
        file.write_all(&chunk)?;

        downloaded += chunk.len();

        if let Some(pb) = pb.as_ref() {
            pb.set_position(downloaded as u64);
        } else {
            // 没有进度条时，每秒打印一次日志
            if last_log_time.elapsed().as_secs() >= 1 {
                log::info!("已下载: {} MB", downloaded / 1024 / 1024);
                last_log_time = std::time::Instant::now();
            }
        }
    }
    Ok(file_name)
}
