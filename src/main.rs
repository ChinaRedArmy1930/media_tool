use anyhow::Context;
use clap::Parser;
use env_logger::Builder;
use ffmpeg_next::format::context;
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use log::LevelFilter;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom};
use std::mem::ManuallyDrop;
use std::path::Path;
use std::ptr;
#[derive(Parser, Debug)]
#[command(name = "ffmpeg_split_audit")]
#[command(author = "syyxy")]
#[command(version = "0.0.1")]
struct Args {
    #[arg(short, long)]
    input: String,

    /// 输出文件路径
    #[arg(short = 'o', long)]
    output: Option<String>,

    /// 详细输出
    #[arg(long)]
    verbose: bool,
}

struct AVFormatContextWrapper {
    avformat_input_wrapper: AVFormatInputWrapper,
    avformat_output_wrapper: AVFormatOutputWrapper,
}

struct AVFormatInputWrapper {
    format_ctx: *mut ffmpeg_next::ffi::AVFormatContext,
    reader: Option<ManuallyDrop<Box<BufReader<File>>>>,
}

struct AVFormatOutputWrapper {
    format_ctx: *mut ffmpeg_next::ffi::AVFormatContext,
    writer: Option<ManuallyDrop<Box<BufWriter<File>>>>,
}

unsafe extern "C" fn interrupt_callback(arg1: *mut libc::c_void) -> libc::c_int {
    println!("interrupt_callback => {:?}", arg1);
    0
}

impl AVFormatContextWrapper {
    fn new<T: AsRef<Path>>(path: T, input: bool, output: bool) -> Option<Self> {
        unsafe {
            let (reader, input_format_ctx) = if input {
                let mut format_ctx = ffmpeg_next::ffi::avformat_alloc_context();

                (*format_ctx).error_recognition = 0; // 最大容错
                (*format_ctx).max_analyze_duration = 3_000_000; // 3秒分析限制
                (*format_ctx).probesize = 5_000_000; // 5MB probe限制
                (*format_ctx).flags |= ffmpeg_next::ffi::AVFMT_FLAG_GENPTS;

                (*format_ctx).interrupt_callback.callback = Some(interrupt_callback);

                let buffer_size = 4 * 1024;
                let buffer = ffmpeg_next::ffi::av_malloc(buffer_size) as *mut u8;
                if buffer.is_null() {
                    log::error!("Failed to allocate buffer");
                    return None;
                }

                if let Some(parent) = path.as_ref().parent() {
                    std::fs::create_dir_all(parent)
                        .context(format!("创建目录失败: {:?}", parent))
                        .ok();
                }

                log::info!("file path: {:?}", path.as_ref());

                let file = File::open(path.as_ref()).unwrap();
                let reader = ManuallyDrop::new(Box::new(BufReader::new(file)));
                let reader_ptr = &**reader as *const BufReader<File> as *mut BufReader<File>;

                (*format_ctx).pb = ffmpeg_next::ffi::avio_alloc_context(
                    buffer,
                    buffer_size as libc::c_int,
                    0,
                    reader_ptr as *mut libc::c_void,
                    Some(self_read_packet),
                    None,
                    Some(self_seek),
                );

                if (*format_ctx).pb.is_null() {
                    log::error!("Failed to allocate buffer");
                    ffmpeg_next::ffi::av_free(buffer as *mut _);
                    ffmpeg_next::ffi::avformat_free_context(format_ctx);
                    ManuallyDrop::drop(&mut ManuallyDrop::new(reader));
                    return None;
                }
                let result = ffmpeg_next::ffi::avformat_open_input(
                    &mut format_ctx,
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                );
                if result < 0 {
                    log::error!("Failed to open input");
                    ffmpeg_next::ffi::av_free(buffer as *mut _);
                    ffmpeg_next::ffi::avformat_free_context(format_ctx);
                    ManuallyDrop::drop(&mut ManuallyDrop::new(reader));
                    return None;
                }

                (Some(reader), format_ctx)
            } else {
                (None, ptr::null_mut())
            };

            let (writer, output_format_ctx) = if output {
                let mut format_ctx: *mut ffmpeg_next::ffi::AVFormatContext = ptr::null_mut();

                // 将路径转换为 C 字符串
                let path_str = path.as_ref().to_str().ok_or("Invalid path").ok()?;
                let path_cstr = std::ffi::CString::new(path_str).ok()?;

                let buffer_size = 4 * 1024;
                let buffer = ffmpeg_next::ffi::av_malloc(buffer_size) as *mut u8;
                if buffer.is_null() {
                    log::error!("Failed to allocate buffer");
                    return None;
                }

                if let Some(parent) = path.as_ref().parent() {
                    std::fs::create_dir_all(parent)
                        .context(format!("创建输出目录失败: {:?}", parent))
                        .ok();
                }

                let file = File::create(path.as_ref()).unwrap();
                let writer: ManuallyDrop<Box<BufWriter<File>>> =
                    ManuallyDrop::new(Box::new(BufWriter::new(file)));
                let writer_ptr = &**writer as *const BufWriter<File> as *mut BufWriter<File>;

                let result = ffmpeg_next::ffi::avformat_alloc_output_context2(
                    &mut format_ctx,
                    ptr::null_mut(),
                    ptr::null(),
                    path_cstr.as_ptr(),
                );

                if result < 0 {
                    log::error!("Failed to open output");
                    ffmpeg_next::ffi::avformat_free_context(format_ctx);
                    ManuallyDrop::drop(&mut ManuallyDrop::new(writer));
                    ffmpeg_next::ffi::av_free(buffer as *mut _);
                    return None;
                }

                (*format_ctx).pb = ffmpeg_next::ffi::avio_alloc_context(
                    buffer,
                    buffer_size as libc::c_int,
                    1,
                    writer_ptr as *mut libc::c_void,
                    None,
                    Some(self_write_packet),
                    Some(self_seek_for_output),
                );

                if (*format_ctx).pb.is_null() {
                    log::error!("Failed to allocate buffer");
                    ffmpeg_next::ffi::av_free(buffer as *mut _);
                    ffmpeg_next::ffi::avformat_free_context(format_ctx);
                    ManuallyDrop::drop(&mut ManuallyDrop::new(writer));
                    return None;
                }

                (*format_ctx).flags |=
                    ffmpeg_next::ffi::AVFMT_FLAG_CUSTOM_IO | ffmpeg_next::ffi::AVFMT_FLAG_IGNIDX;

                (*format_ctx).interrupt_callback.callback = Some(interrupt_callback);
                (Some(writer), format_ctx)
            } else {
                (None, ptr::null_mut())
            };

            Some(Self {
                avformat_input_wrapper: AVFormatInputWrapper {
                    format_ctx: input_format_ctx,
                    reader: reader,
                },
                avformat_output_wrapper: AVFormatOutputWrapper {
                    format_ctx: output_format_ctx,
                    writer: writer,
                },
            })
        }
    }

    fn into_input(mut self) -> (context::Input, Box<BufReader<File>>) {
        unsafe {
            let format_ctx = self.avformat_input_wrapper.format_ctx;
            self.avformat_input_wrapper.format_ctx = ptr::null_mut();

            let reader =
                ManuallyDrop::take(&mut self.avformat_input_wrapper.reader.take().unwrap());

            (context::Input::wrap(format_ctx), reader)
        }
    }

    fn into_output(mut self) -> (context::Output, Box<BufWriter<File>>) {
        unsafe {
            let format_ctx = self.avformat_output_wrapper.format_ctx;
            self.avformat_output_wrapper.format_ctx = ptr::null_mut();

            let writer =
                ManuallyDrop::take(&mut self.avformat_output_wrapper.writer.take().unwrap());

            (context::Output::wrap(format_ctx), writer)
        }
    }

    fn drop(&mut self) {
        unsafe {
            if let Some(ref mut reader) = self.avformat_input_wrapper.reader {
                ManuallyDrop::drop(reader);
            }

            if let Some(ref mut writer) = self.avformat_output_wrapper.writer {
                let _ = writer.flush();
                ManuallyDrop::drop(writer);
            }
        }
    }
}

impl Drop for AVFormatContextWrapper {
    fn drop(&mut self) {
        self.drop();
    }
}

// 根据平台定义不同的缓冲区指针类型
#[cfg(target_os = "macos")]
type FfmpegBufferPtr = *const u8;

#[cfg(not(target_os = "macos"))]
type FfmpegBufferPtr = *mut u8;

unsafe extern "C" fn self_read_packet(
    opaque: *mut libc::c_void,
    buf: *mut u8,
    buf_size: libc::c_int,
) -> libc::c_int {
    let reader = unsafe { &mut *(opaque as *mut BufReader<File>) };
    let slice = unsafe { std::slice::from_raw_parts_mut(buf, buf_size as usize) };
    match reader.read(slice) {
        Ok(0) => ffmpeg_next::ffi::AVERROR_EOF,
        Ok(size) => size as libc::c_int,
        Err(_) => ffmpeg_next::ffi::AVERROR(libc::EIO),
    }
}

unsafe extern "C" fn self_seek_for_output(
    opaque: *mut libc::c_void,
    offset: i64,
    whence: libc::c_int,
) -> i64 {
    let writer = unsafe { &mut *(opaque as *mut BufWriter<File>) };

    let seek_pos = match whence {
        ffmpeg_next::ffi::SEEK_CUR => SeekFrom::Current(offset),
        ffmpeg_next::ffi::SEEK_END => SeekFrom::End(offset),
        ffmpeg_next::ffi::SEEK_SET => SeekFrom::Start(offset as u64),
        _ => return -1,
    };

    match writer.seek(seek_pos) {
        Ok(pos) => pos as i64,
        Err(_) => return -1,
    }
}

unsafe extern "C" fn self_seek(opaque: *mut libc::c_void, offset: i64, whence: libc::c_int) -> i64 {
    let reader = unsafe { &mut *(opaque as *mut BufReader<File>) };

    let seek_pos = match whence {
        ffmpeg_next::ffi::SEEK_CUR => SeekFrom::Current(offset),
        ffmpeg_next::ffi::SEEK_END => SeekFrom::End(offset),
        ffmpeg_next::ffi::SEEK_SET => SeekFrom::Start(offset as u64),
        _ => return -1,
    };

    match reader.seek(seek_pos) {
        Ok(pos) => pos as i64,
        Err(_) => return -1,
    }
}

unsafe extern "C" fn self_write_packet(
    opaque: *mut libc::c_void,
    buf: FfmpegBufferPtr,
    buf_size: libc::c_int,
) -> libc::c_int {
    unsafe {
        let writer = &mut *(opaque as *mut BufWriter<File>);
        let slice = std::slice::from_raw_parts(buf, buf_size as usize);
        match writer.write(slice) {
            Ok(size) => size as libc::c_int,
            Err(_) => -1,
        }
    }
}

struct OutputWithCustomIO {
    output: ManuallyDrop<context::Output>,
    writer: Box<BufWriter<File>>,
}

impl OutputWithCustomIO {
    fn new(output: context::Output, writer: Box<BufWriter<File>>) -> Self {
        Self {
            output: ManuallyDrop::new(output),
            writer: writer,
        }
    }

    fn output_mut(&mut self) -> &mut context::Output {
        &mut self.output
    }
}

impl Drop for OutputWithCustomIO {
    fn drop(&mut self) {
        unsafe {
            let format_ctx = self.output.as_mut_ptr();

            if !(*format_ctx).pb.is_null() {
                let pb = (*format_ctx).pb;
                ffmpeg_next::ffi::avio_flush(pb);
                let buffer = (*pb).buffer;
                (*format_ctx).pb = ptr::null_mut();
                let mut pb_temp = pb;
                ffmpeg_next::ffi::avio_context_free(&mut pb_temp);
                ffmpeg_next::ffi::av_free(buffer as *mut _);
            }

            ManuallyDrop::drop(&mut self.output);
            let _ = self.writer.flush();
        }
    }
}

async fn download_file(file_url: &str) -> anyhow::Result<String> {
    log::info!("Downloading file: {}", file_url);

    Ok(tokio::time::timeout(
        tokio::time::Duration::from_secs(600),
        download_with_process(file_url),
    )
    .await
    .map_err(|_| anyhow::anyhow!("download timeout"))??)
}

async fn download_with_process(file_url: &str) -> anyhow::Result<String> {
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

    let mut file: File = File::create(&file_path)?;

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    Builder::new()
        .format(|buf, record| {
            writeln!(
                buf,
                "[{} {}:{} {}] {}",
                record.level(),
                record.file().unwrap_or("unknown"),
                record.line().unwrap_or(0),
                record.target(),
                record.args()
            )
        })
        .filter_level(LevelFilter::Debug)
        .init();

    let args = Args::parse();

    unsafe {
        if args.verbose {
            ffmpeg_next::ffi::av_log_set_level(ffmpeg_next::ffi::AV_LOG_TRACE);
        }
    }

    let input_media_file = if args.input.starts_with("http") || args.input.starts_with("https") {
        download_file(&args.input).await?.to_string()
    } else {
        args.input
    };

    log::info!("input_media_file: {:?}", input_media_file);

    analyze_input(&input_media_file)?;

    // 如果没有提供音频输出路径，就只分析不提取
    if args.output.is_none() {
        log::info!("未指定音频输出路径，仅执行分析");
        return Ok(());
    }

    let avformat_wrapper = AVFormatContextWrapper::new(input_media_file, true, false);
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
        let output_path = args
            .output
            .as_ref()
            .ok_or(anyhow::anyhow!("output is required"))
            .map(|path| {
                let p = Path::new(path);
                p.parent().unwrap_or(Path::new(".")).join(format!(
                    "{}/{:?}/{}_{}.{}",
                    p.file_stem().and_then(|s| s.to_str()).unwrap_or("output"),
                    stream.parameters().medium(),
                    stream.parameters().id().name(),
                    idx,
                    file_suffix
                ))
            })?;

        log::info!("stream {} output path: {:?}", stream_index, output_path);

        match stream.parameters().medium() {
            ffmpeg_next::media::Type::Video | ffmpeg_next::media::Type::Audio => {
                let avformat_wrapper_output =
                    AVFormatContextWrapper::new(&output_path, false, true);
                let (self_output, writer) = avformat_wrapper_output.unwrap().into_output();

                let mut output_with_custom_io = OutputWithCustomIO::new(self_output, writer);

                if let Some(parent) = output_path.parent() {
                    std::fs::create_dir_all(parent)
                        .context(format!("创建输出目录失败: {:?}", parent))?;
                }

                let mut output_audio_steam = output_with_custom_io
                    .output_mut()
                    .add_stream(ffmpeg_next::encoder::find(stream.parameters().id()))
                    .context("添加流失败")?;

                output_audio_steam.set_parameters(stream.parameters());

                output_with_custom_io
                    .output_mut()
                    .write_header()
                    .context("写入音频文件头失败")?;

                if let Some(packets) = audio_packets.get_mut(&stream_index) {
                    for p in packets {
                        p.set_stream(0);
                        p.write_interleaved(output_with_custom_io.output_mut())
                            .context("写入音频包失败")?;
                    }
                }

                output_with_custom_io
                    .output_mut()
                    .write_trailer()
                    .context("写入音频文件尾失败")?;
            }

            _ => {}
        }
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

fn analyze_input(input: &str) -> anyhow::Result<()> {
    // 打开输入文件
    let avformat_wrapper = AVFormatContextWrapper::new(input, true, false)
        .ok_or_else(|| anyhow::anyhow!("无法打开输入文件: {}", input))?;

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
