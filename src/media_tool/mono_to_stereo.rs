use crate::common;
use anyhow::Context;

extern crate ffmpeg_next as ffmpeg;

pub async fn mono_to_stereo(
    input: &str,
    output: Option<&str>,
    verbose: bool,
    seek: Option<i64>,
) -> anyhow::Result<()> {
    common::common::transcode_audio(input, output, verbose, seek, Some("pan=stereo|c0=c0|c1=c0"))
        .await
        .context("transcode audio failed")?;
    Ok(())
}
