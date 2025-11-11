use crate::common;
use anyhow::Context;

extern crate ffmpeg_next as ffmpeg;

pub async fn auto_pan_swirl(
    input: &str,
    output: Option<&str>,
    verbose: bool,
    seek: Option<i64>,
) -> anyhow::Result<()> {
    common::common::transcode_audio(
        input,
        output,
        verbose,
        seek,
        Some("apulsator=mode=sine:hz=0.5:width=1"),
    )
    .await
    .context("transcode audio failed")?;
    Ok(())
}
