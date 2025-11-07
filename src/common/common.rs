use anyhow::Context;
use ffmpeg::codec;
use ffmpeg::format::context;
use ffmpeg_next::{self as ffmpeg, Rescale, filter, format, frame, media, rescale};
use std::fs::File;
use std::io::Write;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom};
use std::mem::ManuallyDrop;
use std::path::Path;
use std::ptr;

use crate::common::common;
use crate::common::utils::{analyze_input, download_file};

pub struct AVFormatContextWrapper {
    pub avformat_input_wrapper: AVFormatInputWrapper,
    pub avformat_output_wrapper: AVFormatOutputWrapper,
}

pub struct AVFormatInputWrapper {
    pub format_ctx: *mut ffmpeg::ffi::AVFormatContext,
    pub reader: Option<ManuallyDrop<Box<BufReader<File>>>>,
}

pub struct AVFormatOutputWrapper {
    format_ctx: *mut ffmpeg::ffi::AVFormatContext,
    writer: Option<ManuallyDrop<Box<BufWriter<File>>>>,
}

unsafe extern "C" fn interrupt_callback(_arg1: *mut libc::c_void) -> libc::c_int {
    0
}

impl AVFormatContextWrapper {
    pub fn new<T: AsRef<Path>>(path: T, input: bool, output: bool) -> Option<Self> {
        unsafe {
            let (reader, input_format_ctx) = if input {
                let mut format_ctx = ffmpeg::ffi::avformat_alloc_context();

                (*format_ctx).error_recognition = 0; // 最大容错
                (*format_ctx).max_analyze_duration = 3_000_000; // 3秒分析限制
                (*format_ctx).probesize = 5_000_000; // 5MB probe限制
                (*format_ctx).flags |= ffmpeg::ffi::AVFMT_FLAG_GENPTS;

                (*format_ctx).interrupt_callback.callback = Some(interrupt_callback);

                let buffer_size = 4 * 1024;
                let buffer = ffmpeg::ffi::av_malloc(buffer_size) as *mut u8;
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

                let file = File::open(path.as_ref())
                    .context(format!("打开文件失败: {:?}", path.as_ref()))
                    .ok()?;

                let reader = ManuallyDrop::new(Box::new(BufReader::new(file)));
                let reader_ptr = &**reader as *const BufReader<File> as *mut BufReader<File>;

                (*format_ctx).pb = ffmpeg::ffi::avio_alloc_context(
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
                    ffmpeg::ffi::av_free(buffer as *mut _);
                    ffmpeg::ffi::avformat_free_context(format_ctx);
                    ManuallyDrop::drop(&mut ManuallyDrop::new(reader));
                    return None;
                }
                let result = ffmpeg::ffi::avformat_open_input(
                    &mut format_ctx,
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                );
                if result < 0 {
                    log::error!("Failed to open input");
                    ffmpeg::ffi::av_free(buffer as *mut _);
                    ffmpeg::ffi::avformat_free_context(format_ctx);
                    ManuallyDrop::drop(&mut ManuallyDrop::new(reader));
                    return None;
                }

                let ret = ffmpeg::ffi::avformat_find_stream_info(format_ctx, ptr::null_mut());
                if ret < 0 {
                    log::error!("Failed to find stream info");
                    ffmpeg::ffi::av_free(buffer as *mut _);
                    ffmpeg::ffi::avformat_free_context(format_ctx);
                    ManuallyDrop::drop(&mut ManuallyDrop::new(reader));
                    return None;
                }

                (Some(reader), format_ctx)
            } else {
                (None, ptr::null_mut())
            };

            let (writer, output_format_ctx) = if output {
                let mut format_ctx: *mut ffmpeg::ffi::AVFormatContext = ptr::null_mut();

                // 将路径转换为 C 字符串
                let path_str = path.as_ref().to_str().ok_or("Invalid path").ok()?;
                let path_cstr = std::ffi::CString::new(path_str).ok()?;

                let buffer_size = 4 * 1024;
                let buffer = ffmpeg::ffi::av_malloc(buffer_size) as *mut u8;
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

                let result = ffmpeg::ffi::avformat_alloc_output_context2(
                    &mut format_ctx,
                    ptr::null_mut(),
                    ptr::null(),
                    path_cstr.as_ptr(),
                );

                if result < 0 {
                    log::error!("Failed to open output");
                    ffmpeg::ffi::avformat_free_context(format_ctx);
                    ManuallyDrop::drop(&mut ManuallyDrop::new(writer));
                    ffmpeg::ffi::av_free(buffer as *mut _);
                    return None;
                }

                (*format_ctx).pb = ffmpeg::ffi::avio_alloc_context(
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
                    ffmpeg::ffi::av_free(buffer as *mut _);
                    ffmpeg::ffi::avformat_free_context(format_ctx);
                    ManuallyDrop::drop(&mut ManuallyDrop::new(writer));
                    return None;
                }

                (*format_ctx).flags |=
                    ffmpeg::ffi::AVFMT_FLAG_CUSTOM_IO | ffmpeg::ffi::AVFMT_FLAG_IGNIDX;

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

    pub fn into_input(mut self) -> (context::Input, Box<BufReader<File>>) {
        unsafe {
            let format_ctx = self.avformat_input_wrapper.format_ctx;
            self.avformat_input_wrapper.format_ctx = ptr::null_mut();

            let reader =
                ManuallyDrop::take(&mut self.avformat_input_wrapper.reader.take().unwrap());

            (context::Input::wrap(format_ctx), reader)
        }
    }

    pub fn into_output(mut self) -> (context::Output, Box<BufWriter<File>>) {
        unsafe {
            let format_ctx = self.avformat_output_wrapper.format_ctx;
            self.avformat_output_wrapper.format_ctx = ptr::null_mut();

            let writer =
                ManuallyDrop::take(&mut self.avformat_output_wrapper.writer.take().unwrap());

            (context::Output::wrap(format_ctx), writer)
        }
    }

    pub fn drop(&mut self) {
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
pub type FfmpegBufferPtr = *const u8;

#[cfg(not(target_os = "macos"))]
pub type FfmpegBufferPtr = *mut u8;

#[allow(dead_code)]
pub unsafe extern "C" fn self_read_packet(
    opaque: *mut libc::c_void,
    buf: *mut u8,
    buf_size: libc::c_int,
) -> libc::c_int {
    let reader = unsafe { &mut *(opaque as *mut BufReader<File>) };
    let slice = unsafe { std::slice::from_raw_parts_mut(buf, buf_size as usize) };
    match reader.read(slice) {
        Ok(0) => ffmpeg::ffi::AVERROR_EOF,
        Ok(size) => size as libc::c_int,
        Err(_) => ffmpeg::ffi::AVERROR(libc::EIO),
    }
}

pub unsafe extern "C" fn self_seek_for_output(
    opaque: *mut libc::c_void,
    offset: i64,
    whence: libc::c_int,
) -> i64 {
    let writer = unsafe { &mut *(opaque as *mut BufWriter<File>) };

    let seek_pos = match whence {
        ffmpeg::ffi::SEEK_CUR => SeekFrom::Current(offset),
        ffmpeg::ffi::SEEK_END => SeekFrom::End(offset),
        ffmpeg::ffi::SEEK_SET => SeekFrom::Start(offset as u64),
        _ => return -1,
    };

    match writer.seek(seek_pos) {
        Ok(pos) => pos as i64,
        Err(_) => return -1,
    }
}

#[allow(dead_code)]
pub unsafe extern "C" fn self_seek(
    opaque: *mut libc::c_void,
    offset: i64,
    whence: libc::c_int,
) -> i64 {
    let reader = unsafe { &mut *(opaque as *mut BufReader<File>) };

    let seek_pos = match whence {
        ffmpeg::ffi::SEEK_CUR => SeekFrom::Current(offset),
        ffmpeg::ffi::SEEK_END => SeekFrom::End(offset),
        ffmpeg::ffi::SEEK_SET => SeekFrom::Start(offset as u64),
        _ => return -1,
    };

    match reader.seek(seek_pos) {
        Ok(pos) => pos as i64,
        Err(_) => return -1,
    }
}

pub unsafe extern "C" fn self_write_packet(
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

pub struct OutputWithCustomIO {
    output: ManuallyDrop<context::Output>,
    writer: Box<BufWriter<File>>,
}

impl OutputWithCustomIO {
    pub fn new(output: context::Output, writer: Box<BufWriter<File>>) -> Self {
        Self {
            output: ManuallyDrop::new(output),
            writer: writer,
        }
    }

    pub fn output_mut(&mut self) -> &mut context::Output {
        &mut self.output
    }
}

impl Drop for OutputWithCustomIO {
    fn drop(&mut self) {
        unsafe {
            let format_ctx = self.output.as_mut_ptr();
            if !(*format_ctx).pb.is_null() {
                let pb = (*format_ctx).pb;
                ffmpeg::ffi::avio_flush(pb);
                let buffer = (*pb).buffer;
                ffmpeg::ffi::av_free(buffer as *mut _);
                (*pb).buffer = ptr::null_mut();
                (*format_ctx).pb = ptr::null_mut();
                let mut pb_temp = pb;
                ffmpeg::ffi::avio_context_free(&mut pb_temp);
            }

            ManuallyDrop::drop(&mut self.output);
            let _ = self.writer.flush();
        }
    }
}

fn filter(
    spec: &str,
    decoder: &codec::decoder::Audio,
    encoder: &codec::encoder::Audio,
) -> Result<filter::Graph, ffmpeg::Error> {
    let mut filter = filter::Graph::new();

    let args = format!(
        "time_base={}:sample_rate={}:sample_fmt={}:channel_layout=0x{:x}",
        decoder.time_base(),
        decoder.rate(),
        decoder.format().name(),
        decoder.channel_layout().bits()
    );

    filter.add(&filter::find("abuffer").unwrap(), "in", &args)?;
    filter.add(&filter::find("abuffersink").unwrap(), "out", "")?;

    {
        let mut out = filter.get("out").unwrap();

        out.set_sample_format(encoder.format());
        out.set_channel_layout(encoder.channel_layout());
        out.set_sample_rate(encoder.rate());
    }

    filter.output("in", 0)?.input("out", 0)?.parse(spec)?;
    filter.validate()?;

    println!("{}", filter.dump());

    if let Some(codec) = encoder.codec() {
        if !codec
            .capabilities()
            .contains(ffmpeg::codec::capabilities::Capabilities::VARIABLE_FRAME_SIZE)
        {
            filter
                .get("out")
                .unwrap()
                .sink()
                .set_frame_size(encoder.frame_size());
        }
    }

    Ok(filter)
}

struct Transcoder {
    stream: usize,
    filter: filter::Graph,
    decoder: codec::decoder::Audio,
    encoder: codec::encoder::Audio,
    in_time_base: ffmpeg::Rational,
    out_time_base: ffmpeg::Rational,
}

fn transcoder<P: AsRef<Path> + ?Sized>(
    ictx: &mut format::context::Input,
    octx: &mut format::context::Output,
    path: &P,
    filter_spec: &str,
) -> Result<Transcoder, anyhow::Error> {
    let input = {
        ictx.streams().best(media::Type::Audio).or_else(|| {
            ictx.streams()
                .find(|s| s.parameters().medium() == media::Type::Audio)
        })
    }
    .ok_or_else(|| anyhow::anyhow!("could not find best audio stream"))?;

    let context = ffmpeg::codec::context::Context::from_parameters(input.parameters())?;
    let mut decoder = context.decoder().audio()?;
    let codec = ffmpeg::encoder::find(octx.format().codec(path, media::Type::Audio))
        .ok_or(anyhow::anyhow!("failed to find encoder"))?
        .audio()?;
    let global = octx
        .format()
        .flags()
        .contains(ffmpeg::format::flag::Flags::GLOBAL_HEADER);

    decoder.set_parameters(input.parameters())?;

    let mut output = octx.add_stream(codec)?;
    let context = ffmpeg::codec::context::Context::from_parameters(output.parameters())?;
    let mut encoder = context.encoder().audio()?;

    let channel_layout = ffmpeg::channel_layout::ChannelLayout::STEREO;

    if global {
        encoder.set_flags(ffmpeg::codec::flag::Flags::GLOBAL_HEADER);
    }

    encoder.set_rate(decoder.rate() as i32);
    encoder.set_channel_layout(channel_layout);
    encoder.set_channels(channel_layout.channels());

    encoder.set_format(
        codec
            .formats()
            .expect("unknown supported formats")
            .next()
            .unwrap(),
    );
    encoder.set_bit_rate(decoder.bit_rate());
    encoder.set_max_bit_rate(decoder.max_bit_rate());

    encoder.set_time_base((1, decoder.rate() as i32));
    output.set_time_base((1, decoder.rate() as i32));

    let encoder = encoder.open_as(codec)?;
    output.set_parameters(&encoder);

    let filter = filter(filter_spec, &decoder, &encoder)?;

    let in_time_base = decoder.time_base();
    let out_time_base = output.time_base();

    Ok(Transcoder {
        stream: input.index(),
        filter,
        decoder,
        encoder,
        in_time_base,
        out_time_base,
    })
}

impl Transcoder {
    fn send_frame_to_encoder(&mut self, frame: &ffmpeg::Frame) {
        self.encoder.send_frame(frame).unwrap();
    }

    fn send_eof_to_encoder(&mut self) {
        self.encoder.send_eof().unwrap();
    }

    fn receive_and_process_encoded_packets(&mut self, octx: &mut format::context::Output) {
        let mut encoded = ffmpeg::Packet::empty();
        while self.encoder.receive_packet(&mut encoded).is_ok() {
            encoded.set_stream(0);
            encoded.rescale_ts(self.in_time_base, self.out_time_base);
            encoded.write_interleaved(octx).unwrap();
        }
    }

    fn add_frame_to_filter(&mut self, frame: &ffmpeg::Frame) {
        self.filter.get("in").unwrap().source().add(frame).unwrap();
    }

    fn flush_filter(&mut self) {
        self.filter.get("in").unwrap().source().flush().unwrap();
    }

    fn get_and_process_filtered_frames(&mut self, octx: &mut format::context::Output) {
        let mut filtered = frame::Audio::empty();
        while self
            .filter
            .get("out")
            .unwrap()
            .sink()
            .frame(&mut filtered)
            .is_ok()
        {
            self.send_frame_to_encoder(&filtered);
            self.receive_and_process_encoded_packets(octx);
        }
    }

    fn send_packet_to_decoder(&mut self, packet: &ffmpeg::Packet) {
        self.decoder.send_packet(packet).unwrap();
    }

    fn send_eof_to_decoder(&mut self) {
        self.decoder.send_eof().unwrap();
    }

    fn receive_and_process_decoded_frames(&mut self, octx: &mut format::context::Output) {
        let mut decoded = frame::Audio::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            let timestamp = decoded.timestamp();
            decoded.set_pts(timestamp);
            self.add_frame_to_filter(&decoded);
            self.get_and_process_filtered_frames(octx);
        }
    }
}

pub async fn transcode_audio(
    input: &str,
    output: Option<&str>,
    verbose: bool,
    seek: Option<i64>,
    filter: Option<&str>,
) -> anyhow::Result<()> {
    unsafe {
        if verbose {
            ffmpeg::ffi::av_log_set_level(ffmpeg::ffi::AV_LOG_TRACE);
        }
    }

    let input_media_file = if input.starts_with("http") || input.starts_with("https") {
        download_file(&input).await?.to_string()
    } else {
        input.to_string()
    };

    analyze_input(&input_media_file)?;

    if output.is_none() {
        log::info!("未指定音频输出路径，仅执行分析");
        return Ok(());
    }

    let avformat_wrapper_input =
        common::common::AVFormatContextWrapper::new(input_media_file, true, false);
    let (mut self_input, _reader) = avformat_wrapper_input.unwrap().into_input();

    let avformat_wrapper_output =
        common::common::AVFormatContextWrapper::new(output.clone().unwrap(), false, true);
    let (so, writer) = avformat_wrapper_output.unwrap().into_output();

    let mut output_with_custom_io = common::common::OutputWithCustomIO::new(so, writer);

    let mut self_output = output_with_custom_io.output_mut();

    let mut transcoder = transcoder(
        &mut self_input,
        &mut self_output,
        &output.clone().unwrap(),
        &filter.unwrap_or("anull"),
    )
    .map_err(|e| anyhow::anyhow!("failed to create transcoder: {}", e))?;

    if let Some(position) = seek {
        // If the position was given in seconds, rescale it to ffmpegs base timebase.
        let position = position.rescale((1, 1), rescale::TIME_BASE);
        // If this seek was embedded in the transcoding loop, a call of `flush()`
        // for every opened buffer after the successful seek would be advisable.
        self_input.seek(position, ..position).unwrap();
    }

    self_output.set_metadata(self_input.metadata().to_owned());
    self_output.write_header().unwrap();

    for (stream, mut packet) in self_input.packets() {
        if stream.index() == transcoder.stream {
            packet.rescale_ts(stream.time_base(), transcoder.in_time_base);
            transcoder.send_packet_to_decoder(&packet);
            transcoder.receive_and_process_decoded_frames(&mut self_output);
        }
    }

    transcoder.send_eof_to_decoder();
    transcoder.receive_and_process_decoded_frames(&mut self_output);

    transcoder.flush_filter();
    transcoder.get_and_process_filtered_frames(&mut self_output);

    transcoder.send_eof_to_encoder();
    transcoder.receive_and_process_encoded_packets(&mut self_output);

    self_output.write_trailer().unwrap();

    Ok(())
}
