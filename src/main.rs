use anyhow::Context;
use clap::Parser;
use std::io::Write;

mod media_tool {
    pub mod auto_pan_swirl;
    pub mod mono_to_stereo;
    pub mod stream_split;
}

mod common {
    pub mod common;
    pub mod utils;
}

use media_tool::auto_pan_swirl;
use media_tool::mono_to_stereo;
use media_tool::stream_split;

#[derive(Parser, Debug)]
#[command(name = "media_tool")]
#[command(author = "syyxy")]
#[command(version = "0.0.1")]
struct Args {
    #[arg(short, long)]
    method: String,

    #[arg(short, long)]
    input: String,

    /// 输出文件路径
    #[arg(short = 'o', long)]
    output: Option<String>,

    /// 详细输出
    #[arg(long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    ffmpeg_next::init().unwrap();

    env_logger::Builder::new()
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
        .filter_level(log::LevelFilter::Debug)
        .init();

    let args = Args::parse();
    match args.method.as_str() {
        "stream_split" => {
            stream_split::stream_split(&args.input, args.output.as_deref(), args.verbose)
                .await
                .context("stream split failed")?;
        }
        "mono_to_stereo" => {
            mono_to_stereo::mono_to_stereo(&args.input, args.output.as_deref(), args.verbose, None)
                .await
                .map_err(|e| anyhow::anyhow!("mono to stereo failed: {}", e))?;
        }

        "auto_pan_swirl" => {
            auto_pan_swirl::auto_pan_swirl(&args.input, args.output.as_deref(), args.verbose, None)
                .await
                .map_err(|e| anyhow::anyhow!("auto pan swirl failed: {}", e))?;
        }

        _ => {
            anyhow::bail!("invalid method");
        }
    }
    Ok(())
}
