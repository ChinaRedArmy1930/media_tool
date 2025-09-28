use env_logger::Env;
use ffmpeg_next::ffi::avformat_open_input;
use ffmpeg_next::format::{context, output};

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::ptr;

struct AVForamtContextWrapper {
    format_ctx: *mut ffmpeg_next::ffi::AVFormatContext,
}

impl AVForamtContextWrapper {
    fn new(path: &str) -> Self {
        let format_ctx = create_self_format_context(path);
        Self { format_ctx }
    }

    #[allow(dead_code)]
    fn drop(&mut self) {
        log::info!("drop AVForamtContextWrapper");
        unsafe {
            if !self.format_ctx.is_null() {
                let mut avio_context = (*self.format_ctx).pb;

                if !avio_context.is_null() {
                    let reader_data = (*avio_context).opaque;
                    if !reader_data.is_null() {
                        let reader_data_box = Box::from_raw(reader_data as *mut BufReader<File>);
                        drop(reader_data_box);
                    }

                    let avio_context_prt = &mut avio_context;
                    ffmpeg_next::ffi::avio_context_free(avio_context_prt);

                    let format_ctx_prt = &mut self.format_ctx;
                    ffmpeg_next::ffi::avformat_close_input(format_ctx_prt);
                }
            }
        }
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

fn create_self_format_context<T: AsRef<Path>>(path: T) -> *mut ffmpeg_next::ffi::AVFormatContext {
    unsafe {
        let mut format_ctx = ffmpeg_next::ffi::avformat_alloc_context();

        let buffer_size = 4 * 1024;
        let buffer = ffmpeg_next::ffi::av_malloc(buffer_size) as *mut u8;
        if buffer.is_null() {
            log::error!("Failed to allocate buffer");
            return ptr::null_mut();
        }

        let file = File::open(Path::new(path.as_ref().to_string_lossy().as_ref())).unwrap();
        let reader = BufReader::new(file);
        //put reader into box
        let reader_data_box = Box::new(reader);
        let reader_data = Box::into_raw(reader_data_box);

        (*format_ctx).pb = ffmpeg_next::ffi::avio_alloc_context(
            buffer,
            buffer_size as libc::c_int,
            0,
            reader_data as *mut libc::c_void,
            Some(self_read_packet),
            None,
            Some(self_seek),
        );

        if (*format_ctx).pb.is_null() {
            log::error!("Failed to allocate buffer");
            return ptr::null_mut();
        }

        let result = ffmpeg_next::ffi::avformat_open_input(
            &mut format_ctx,
            ptr::null(),
            ptr::null_mut(),
            ptr::null_mut(),
        );
        if result < 0 {
            log::error!("Failed to open input");
            return ptr::null_mut();
        }

        format_ctx
    }
}

fn create_self_output_context<T: AsRef<Path>>(path: T) -> context::Output {
    output(&path).unwrap()
}

fn create_self_input(mut wrapper: AVForamtContextWrapper) -> context::Input {
    unsafe {
        let result = avformat_open_input(
            &mut wrapper.format_ctx,
            ptr::null(),
            ptr::null_mut(),
            ptr::null_mut(),
        );
        if result < 0 {
            panic!("Failed to open input");
        }
        context::Input::wrap(wrapper.format_ctx)
    }
}

fn main() {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    let self_input = create_self_input(AVForamtContextWrapper::new("/tmp/input.mp4"));
    let self_output = create_self_output_context("/tmp/output.mp4");

    _ = self_input;
    _ = self_output;
}
