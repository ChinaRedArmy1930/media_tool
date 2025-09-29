use anyhow::Context;
use clap::Parser;
use env_logger::Builder;
use ffmpeg_next::format::{context, output};
use log::LevelFilter;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::io::{BufReader, Read, Seek, SeekFrom};
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

    /// 输出音频文件路径（可选）
    #[arg(short = 'o', long)]
    audio_output_path: Option<String>,

    /// 输出视频文件路径（可选）
    #[arg(short = 'v', long)]
    video_output_path: Option<String>,

    /// 详细输出
    #[arg(long)]
    verbose: bool,
}

struct AVFormatContextWrapper {
    format_ctx: *mut ffmpeg_next::ffi::AVFormatContext,
    reader: ManuallyDrop<Box<BufReader<File>>>,
}

impl AVFormatContextWrapper {
    fn new<T: AsRef<Path>>(path: T) -> Option<Self> {
        unsafe {
            let mut format_ctx = ffmpeg_next::ffi::avformat_alloc_context();

            let buffer_size = 4 * 1024;
            let buffer = ffmpeg_next::ffi::av_malloc(buffer_size) as *mut u8;
            if buffer.is_null() {
                log::error!("Failed to allocate buffer");
                return None;
            }

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

            Some(Self { format_ctx, reader })
        }
    }

    fn into_input(mut self) -> (context::Input, Box<BufReader<File>>) {
        unsafe {
            let format_ctx = self.format_ctx;
            self.format_ctx = ptr::null_mut();

            let reader = ManuallyDrop::take(&mut self.reader);

            (context::Input::wrap(format_ctx), reader)
        }
    }

    fn drop(&mut self) {
        unsafe {
            if !self.format_ctx.is_null() {
                let avio_context = (*self.format_ctx).pb;

                if !avio_context.is_null() {
                    let reader_data = (*avio_context).opaque;
                    if !reader_data.is_null() {
                        (*avio_context).opaque = ptr::null_mut();
                        let mut avio_ctx = avio_context;
                        ffmpeg_next::ffi::avio_context_free(&mut avio_ctx as *mut _);
                    }

                    ffmpeg_next::ffi::avformat_close_input(&mut self.format_ctx);
                    ManuallyDrop::drop(&mut self.reader);
                }
            }
        }
    }
}

impl Drop for AVFormatContextWrapper {
    fn drop(&mut self) {
        self.drop();
    }
}

unsafe extern "C" fn self_read_packet(
    opaque: *mut libc::c_void,
    buf: *mut u8,
    buf_size: libc::c_int,
) -> libc::c_int {
    let reader = unsafe { &mut *(opaque as *mut BufReader<File>) };
    let slice = unsafe { std::slice::from_raw_parts_mut(buf, buf_size as usize) };
    match reader.read(slice) {
        Ok(size) => size as libc::c_int,
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

fn create_self_output_context<T: AsRef<Path>>(path: T) -> context::Output {
    output(&path).unwrap_or_else(|e| {
        log::error!("Failed to create output context: {:?}", e);
        std::process::exit(1);
    })
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

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
        .filter_level(LevelFilter::Info)
        .init();

    analyze_input(&args.input)?;

    // 如果没有提供音频输出路径，就只分析不提取
    if args.audio_output_path.is_none() {
        log::info!("未指定音频输出路径，仅执行分析");
        return Ok(());
    }

    let avformat_wrapper = AVFormatContextWrapper::new(args.input);
    let (mut self_input, _reader) = avformat_wrapper.unwrap().into_input();

    let mut self_output_audios = Vec::new();

    let mut video_packets = HashMap::new();
    let mut audio_packets = HashMap::new();

    for packet in self_input.packets() {
        let (s, p) = packet;

        match s.parameters().medium() {
            ffmpeg_next::media::Type::Video => {
                video_packets
                    .entry(s.index())
                    .or_insert_with(Vec::new)
                    .push(p);
            }
            ffmpeg_next::media::Type::Audio => {
                audio_packets
                    .entry(s.index())
                    .or_insert_with(Vec::new)
                    .push(p);
            }

            _ => {
                log::info!("other medium: {:?}", s.parameters().medium());
            }
        }
    }

    let audio_streams = self_input
        .streams()
        .filter(|s| s.parameters().medium() == ffmpeg_next::media::Type::Audio);

    for (idx, audio_stream) in audio_streams.enumerate() {
        let stream_index = audio_stream.index();
        let file_suffix = audio_stream.parameters().id().name().to_string();
        log::info!("file_suffix: {:?}", file_suffix);
        let output_path = args
            .audio_output_path
            .as_ref()
            .ok_or(anyhow::anyhow!("audio_output is required"))
            .map(|path| {
                let p = Path::new(path);
                p.parent().unwrap_or(Path::new(".")).join(format!(
                    "{}/audio_{}.{}",
                    p.file_stem().and_then(|s| s.to_str()).unwrap_or("output"),
                    idx,
                    file_suffix
                ))
            })?;

        log::info!("output_path: {:?}", output_path);

        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).context(format!("创建输出目录失败: {:?}", parent))?;
        }

        let mut self_output_audio = create_self_output_context(output_path);

        let mut output_audio_steam = self_output_audio
            .add_stream(ffmpeg_next::encoder::find(audio_stream.parameters().id()))
            .context("添加音频流失败")?;

        output_audio_steam.set_parameters(audio_stream.parameters());

        self_output_audio
            .write_header()
            .context("写入音频文件头失败")?;

        if let Some(packets) = audio_packets.get_mut(&stream_index) {
            for p in packets {
                p.set_stream(0);
                p.write_interleaved(&mut self_output_audio)
                    .context("写入音频包失败")?;
            }
        }

        self_output_audio
            .write_trailer()
            .context("写入音频文件尾失败")?;

        self_output_audios.push(self_output_audio);
    }

    Ok(())
}
fn analyze_input(input: &str) -> anyhow::Result<()> {
    // 打开输入文件
    let avformat_wrapper = AVFormatContextWrapper::new(input)
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
