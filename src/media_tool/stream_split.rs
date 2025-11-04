use anyhow::Context;
use std::collections::HashMap;
use std::path::Path;

use crate::common;

pub async fn stream_split(input: &str, output: Option<&str>, verbose: bool) -> anyhow::Result<()> {
    unsafe {
        if verbose {
            ffmpeg_next::ffi::av_log_set_level(ffmpeg_next::ffi::AV_LOG_TRACE);
        }
    }

    let input_media_file = if input.starts_with("http") || input.starts_with("https") {
        common::utils::download_file(&input).await?.to_string()
    } else {
        input.to_string()
    };

    log::info!("input_media_file: {:?}", input_media_file);

    common::utils::analyze_input(&input_media_file)?;

    // 如果没有提供音频输出路径，就只分析不提取
    if output.is_none() {
        log::info!("未指定音频输出路径，仅执行分析");
        return Ok(());
    }

    let avformat_wrapper =
        common::common::AVFormatContextWrapper::new(input_media_file, true, false);
    let (mut self_input, _reader) = avformat_wrapper.unwrap().into_input();

    let mut video_packets = HashMap::new();
    let mut audio_packets = HashMap::new();
    let mut subtitle_packets = HashMap::new();
    let mut attachment_packets = HashMap::new();
    let mut data_packets = HashMap::new();
    let mut unknown_packets = HashMap::new();

    let mut packet_count = 0;

    for packet in self_input.packets() {
        packet_count += 1;

        let (s, p) = packet;

        match s.parameters().medium() {
            ffmpeg_next::media::Type::Video => {
                video_packets
                    .entry(s.index())
                    .or_insert_with(Vec::new)
                    .push(p.clone());
            }
            ffmpeg_next::media::Type::Audio => {
                audio_packets
                    .entry(s.index())
                    .or_insert_with(Vec::new)
                    .push(p.clone());
            }
            ffmpeg_next::media::Type::Subtitle => {
                subtitle_packets
                    .entry(s.index())
                    .or_insert_with(Vec::new)
                    .push(p.clone());
            }
            ffmpeg_next::media::Type::Attachment => {
                attachment_packets
                    .entry(s.index())
                    .or_insert_with(Vec::new)
                    .push(p.clone());
            }
            ffmpeg_next::media::Type::Data => {
                data_packets
                    .entry(s.index())
                    .or_insert_with(Vec::new)
                    .push(p.clone());
            }
            ffmpeg_next::media::Type::Unknown => {
                unknown_packets
                    .entry(s.index())
                    .or_insert_with(Vec::new)
                    .push(p.clone());
            }
        }
    }

    log::info!("\n读取完成, 已读取 {} 个packets\n", packet_count);
    let streams = self_input.streams();

    for (idx, stream) in streams.enumerate() {
        let stream_index = stream.index();
        let file_suffix: &'static str = get_best_container_for_codec(&stream);
        log::info!("file_suffix: {:?}", file_suffix);
        let output_path = output
            .as_ref()
            .ok_or(anyhow::anyhow!("output is required"))
            .map(|path| {
                let p = Path::new(path);

                let parent = p.parent().unwrap_or(Path::new("."));
                std::fs::create_dir_all(parent)
                    .context(format!("创建输出目录失败: {:?}", parent))
                    .ok();

                parent.join(format!(
                    "{}/{:?}/{}_{}.{}",
                    p.file_stem().and_then(|s| s.to_str()).unwrap_or("output"),
                    stream.parameters().medium(),
                    stream.parameters().id().name(),
                    idx,
                    file_suffix
                ))
            })?;

        log::info!("stream {} output path: {:?}", stream_index, output_path);

        let medium_type = stream.parameters().medium();

        let packets: Option<&mut Vec<ffmpeg_next::Packet>> = match medium_type {
            ffmpeg_next::media::Type::Audio => audio_packets.get_mut(&stream_index),
            ffmpeg_next::media::Type::Video => video_packets.get_mut(&stream_index),
            ffmpeg_next::media::Type::Data => data_packets.get_mut(&stream_index),
            ffmpeg_next::media::Type::Attachment => attachment_packets.get_mut(&stream_index),
            ffmpeg_next::media::Type::Subtitle => subtitle_packets.get_mut(&stream_index),
            ffmpeg_next::media::Type::Unknown => unknown_packets.get_mut(&stream_index),
        };

        if !packets.as_ref().is_some_and(|p| !p.is_empty()) {
            log::warn!("流 #{} 没有包", stream_index);
            continue;
        }

        let avformat_wrapper_output =
            common::common::AVFormatContextWrapper::new(&output_path, false, true);
        let (self_output, writer) = avformat_wrapper_output.unwrap().into_output();

        let mut output_with_custom_io =
            common::common::OutputWithCustomIO::new(self_output, writer);

        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).context(format!("创建输出目录失败: {:?}", parent))?;
        }

        let mut output_stream = output_with_custom_io
            .output_mut()
            .add_stream(ffmpeg_next::encoder::find(stream.parameters().id()))
            .context("添加流失败")?;

        output_stream.set_parameters(stream.parameters());

        output_with_custom_io
            .output_mut()
            .write_header()
            .context("写入文件头失败")?;

        if let Some(packets) = packets {
            packets.sort_by_key(|p| p.dts().unwrap_or(0));

            for p in packets {
                p.set_stream(0);
                p.write_interleaved(output_with_custom_io.output_mut())
                    .context("写入包失败")?;
            }
        }

        output_with_custom_io
            .output_mut()
            .write_trailer()
            .context("写入文件尾失败")?;
    }

    Ok(())
}

fn get_best_container_for_codec(stream: &ffmpeg_next::Stream) -> &'static str {
    let codec_name = stream.parameters().id().name();
    match stream.parameters().medium() {
        ffmpeg_next::media::Type::Video => {
            match codec_name {
                "h264" | "h265" | "hevc" | "mpeg4" | "mjpeg" => "mp4",
                "vp8" | "vp9" => "webm",
                "av1" => "mkv", // 或 "webm"
                "theora" => "ogv",
                _ => "mkv", // MKV 是万能容器，几乎支持所有格式
            }
        }
        ffmpeg_next::media::Type::Audio => {
            match codec_name {
                // AAC 系列 - 使用 M4A (MP4 Audio)
                "aac" => "m4a",

                // MP3 - 原生格式
                "mp3" => "mp3",

                // AC3/Dolby Digital - 原生格式
                "ac3" => "ac3",
                "eac3" => "eac3", // Enhanced AC3

                // Opus - 原生 Opus 文件或 WebM
                "opus" => "opus", // 或 "webm"

                // Vorbis - Ogg 容器
                "vorbis" => "ogg",

                // FLAC - 原生格式
                "flac" => "flac",

                // PCM - WAV 容器
                "pcm_s16le" | "pcm_s24le" | "pcm_s32le" => "wav",

                // DTS 系列
                "dts" => "dts",

                // TrueHD
                "truehd" => "thd",

                // ALAC (Apple Lossless)
                "alac" => "m4a",

                // WMA (Windows Media Audio)
                "wmav1" | "wmav2" => "wma",

                // AMR (语音编码)
                "amr_nb" | "amr_wb" => "amr",

                // 默认 - MKA (Matroska Audio，支持所有音频格式)
                _ => "mka",
            }
        }
        ffmpeg_next::media::Type::Attachment => {
            // 1. 尝试从 filename 获取扩展名
            let filename_keys = ["filename", "FILENAME", "file_name", "name"];
            for key in &filename_keys {
                if let Some(filename) = stream.metadata().get(key) {
                    log::info!("  文件名: {}", filename);

                    // 提取扩展名并映射到已知类型
                    if let Some(ext) = Path::new(filename).extension() {
                        if let Some(ext_str) = ext.to_str() {
                            let ext_lower = ext_str.to_lowercase();
                            log::info!("  从文件名提取扩展名: {}", ext_lower);

                            return match ext_lower.as_str() {
                                "ttf" => "ttf",
                                "otf" => "otf",
                                "woff" => "woff",
                                "woff2" => "woff2",
                                "jpg" | "jpeg" => "jpg",
                                "png" => "png",
                                "gif" => "gif",
                                "webp" => "webp",
                                "bmp" => "bmp",
                                "svg" => "svg",
                                "pdf" => "pdf",
                                "txt" => "txt",
                                _ => "bin", // 其他扩展名统一用 bin
                            };
                        }
                    }
                }
            }

            // 2. 尝试从 MIME type 推断扩展名
            let mimetype_keys = ["mimetype", "MIMETYPE", "mime_type"];
            for key in &mimetype_keys {
                if let Some(mimetype) = stream.metadata().get(key) {
                    log::info!("  MIME类型: {}", mimetype);

                    let ext = match mimetype {
                        // 字体文件
                        "font/ttf" | "application/x-truetype-font" => "ttf",
                        "font/otf" | "application/vnd.ms-opentype" => "otf",
                        "font/woff" => "woff",
                        "font/woff2" => "woff2",

                        // 图片文件
                        "image/jpeg" => "jpg",
                        "image/png" => "png",
                        "image/gif" => "gif",
                        "image/webp" => "webp",
                        "image/bmp" => "bmp",
                        "image/svg+xml" => "svg",

                        // 其他
                        "application/pdf" => "pdf",
                        "text/plain" => "txt",

                        _ => {
                            log::warn!("  未知的 MIME type: {}", mimetype);
                            "bin"
                        }
                    };

                    log::info!("  从 MIME type 推断扩展名: {}", ext);
                    return ext;
                }
            }

            log::warn!("  无法确定附件扩展名，使用 .bin");
            "bin"
        }
        ffmpeg_next::media::Type::Subtitle => {
            match codec_name {
                "ass" | "ssa" => "ass",             // Advanced SubStation Alpha
                "srt" | "subrip" => "srt",          // SubRip
                "webvtt" => "vtt",                  // WebVTT
                "mov_text" => "srt",                // MP4 字幕（通常转换为 SRT）
                "dvd_subtitle" | "dvdsub" => "sub", // DVD 字幕
                "hdmv_pgs_subtitle" => "sup",       // 蓝光字幕
                "text" => "txt",                    // 纯文本
                _ => "sub",                         // 默认
            }
        }
        ffmpeg_next::media::Type::Unknown => "bin",
        ffmpeg_next::media::Type::Data => "dat",
    }
}
