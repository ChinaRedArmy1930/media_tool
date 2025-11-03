use anyhow::Context;
use ffmpeg_next::format::context;
use std::fs::File;
use std::io::Write;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom};
use std::mem::ManuallyDrop;
use std::path::Path;
use std::ptr;

pub struct AVFormatContextWrapper {
    pub avformat_input_wrapper: AVFormatInputWrapper,
    pub avformat_output_wrapper: AVFormatOutputWrapper,
}

pub struct AVFormatInputWrapper {
    pub format_ctx: *mut ffmpeg_next::ffi::AVFormatContext,
    pub reader: Option<ManuallyDrop<Box<BufReader<File>>>>,
}

pub struct AVFormatOutputWrapper {
    format_ctx: *mut ffmpeg_next::ffi::AVFormatContext,
    writer: Option<ManuallyDrop<Box<BufWriter<File>>>>,
}

unsafe extern "C" fn interrupt_callback(arg1: *mut libc::c_void) -> libc::c_int {
    println!("interrupt_callback => {:?}", arg1);
    0
}

impl AVFormatContextWrapper {
    pub fn new<T: AsRef<Path>>(path: T, input: bool, output: bool) -> Option<Self> {
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

                let file = File::open(path.as_ref())
                    .context(format!("打开文件失败: {:?}", path.as_ref()))
                    .ok()?;

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
        Ok(0) => ffmpeg_next::ffi::AVERROR_EOF,
        Ok(size) => size as libc::c_int,
        Err(_) => ffmpeg_next::ffi::AVERROR(libc::EIO),
    }
}

pub unsafe extern "C" fn self_seek_for_output(
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

#[allow(dead_code)]
pub unsafe extern "C" fn self_seek(
    opaque: *mut libc::c_void,
    offset: i64,
    whence: libc::c_int,
) -> i64 {
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
