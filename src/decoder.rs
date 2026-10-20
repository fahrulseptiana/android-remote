//! H.264 decoding through Android's MediaCodec, rendered to RGBA for egui.

use crate::session::VideoFrame;
use ndk::media::{
    image_reader::{AcquireResult, Image, ImageFormat, ImageReader},
    media_codec::{
        DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec, MediaCodecDirection,
    },
    media_format::MediaFormat,
};
use std::time::{Duration, Instant};

const BUFFER_FLAG_KEY_FRAME: u32 = 1;
const BUFFER_FLAG_CODEC_CONFIG: u32 = 2;
const BUFFER_FLAG_END_OF_STREAM: u32 = 4;

pub struct Decoder {
    // The codec must be dropped before the image reader it renders into.
    codec: MediaCodec,
    reader: ImageReader,
}

impl Decoder {
    pub fn new(width: usize, height: usize) -> Result<Self, String> {
        let reader = ImageReader::new(width as i32, height as i32, ImageFormat::YUV_420_888, 4)
            .map_err(|error| format!("ImageReader: {error}"))?;
        let window = reader
            .window()
            .map_err(|error| format!("ImageReader window: {error}"))?;

        let codec = MediaCodec::from_decoder_type("video/avc")
            .ok_or_else(|| "No H.264 decoder available".to_owned())?;
        let mut format = MediaFormat::new();
        format.set_str("mime", "video/avc");
        format.set_i32("width", width as i32);
        format.set_i32("height", height as i32);
        format.set_i32("max-input-size", 2 * 1024 * 1024);
        codec
            .configure(&format, Some(&window), MediaCodecDirection::Decoder)
            .map_err(|error| format!("MediaCodec configure: {error}"))?;
        codec
            .start()
            .map_err(|error| format!("MediaCodec start: {error}"))?;

        Ok(Self { codec, reader })
    }

    /// Queue one scrcpy video packet and return the newest decoded frame, if any.
    pub fn decode(
        &self,
        data: &[u8],
        pts: u64,
        config: bool,
        key_frame: bool,
    ) -> Result<Option<VideoFrame>, String> {
        let mut newest = None;
        let deadline = Instant::now() + Duration::from_millis(750);

        loop {
            match self.codec.dequeue_input_buffer(Duration::from_millis(5)) {
                Ok(DequeuedInputBufferResult::Buffer(mut input)) => {
                    let buffer = input.buffer_mut();
                    if buffer.len() < data.len() {
                        return Err(format!(
                            "decoder input buffer too small ({} < {})",
                            buffer.len(),
                            data.len()
                        ));
                    }
                    for (dst, src) in buffer[..data.len()].iter_mut().zip(data) {
                        dst.write(*src);
                    }

                    let flags = if config {
                        BUFFER_FLAG_CODEC_CONFIG
                    } else if key_frame {
                        BUFFER_FLAG_KEY_FRAME
                    } else {
                        0
                    };
                    let pts = pts & 0x1fff_ffff_ffff_ffff;
                    self.codec
                        .queue_input_buffer(input, 0, data.len(), pts, flags)
                        .map_err(|error| format!("MediaCodec queue: {error}"))?;
                    break;
                }
                Ok(DequeuedInputBufferResult::TryAgainLater) => {
                    if let Some(frame) = self.drain()? {
                        newest = Some(frame);
                    }
                    if Instant::now() >= deadline {
                        return Err("decoder is not accepting input".to_owned());
                    }
                }
                Err(error) => return Err(format!("MediaCodec input: {error}")),
            }
        }

        if let Some(frame) = self.drain()? {
            newest = Some(frame);
        }
        Ok(newest)
    }

    fn drain(&self) -> Result<Option<VideoFrame>, String> {
        let mut newest = None;
        loop {
            match self.codec.dequeue_output_buffer(Duration::from_millis(0)) {
                Ok(DequeuedOutputBufferInfoResult::Buffer(buffer)) => {
                    let eos = buffer.info().flags() & BUFFER_FLAG_END_OF_STREAM != 0;
                    self.codec
                        .release_output_buffer(buffer, true)
                        .map_err(|error| format!("MediaCodec release: {error}"))?;

                    if let Some(frame) = self.acquire_frame()? {
                        newest = Some(frame);
                    }
                    if eos {
                        break;
                    }
                }
                Ok(DequeuedOutputBufferInfoResult::TryAgainLater) => break,
                Ok(DequeuedOutputBufferInfoResult::OutputFormatChanged)
                | Ok(DequeuedOutputBufferInfoResult::OutputBuffersChanged) => {}
                Err(error) => return Err(format!("MediaCodec output: {error}")),
            }
        }
        Ok(newest)
    }

    fn acquire_frame(&self) -> Result<Option<VideoFrame>, String> {
        match self
            .reader
            .acquire_latest_image()
            .map_err(|error| format!("ImageReader acquire: {error}"))?
        {
            AcquireResult::Image(image) => Ok(Some(self.image_to_frame(&image)?)),
            AcquireResult::NoBufferAvailable | AcquireResult::MaxImagesAcquired => Ok(None),
        }
    }

    fn image_to_frame(&self, image: &Image) -> Result<VideoFrame, String> {
        let width = image.width().map_err(|error| error.to_string())? as usize;
        let height = image.height().map_err(|error| error.to_string())? as usize;
        if width == 0 || height == 0 {
            return Err("decoder produced an empty image".to_owned());
        }

        let y_plane = image
            .plane_data(0)
            .map_err(|error| format!("Y plane: {error}"))?;
        let u_plane = image
            .plane_data(1)
            .map_err(|error| format!("U plane: {error}"))?;
        let v_plane = image
            .plane_data(2)
            .map_err(|error| format!("V plane: {error}"))?;

        let y_row = image
            .plane_row_stride(0)
            .map_err(|error| error.to_string())? as usize;
        let u_row = image
            .plane_row_stride(1)
            .map_err(|error| error.to_string())? as usize;
        let v_row = image
            .plane_row_stride(2)
            .map_err(|error| error.to_string())? as usize;
        let u_pixel = image
            .plane_pixel_stride(1)
            .map_err(|error| error.to_string())? as usize;
        let v_pixel = image
            .plane_pixel_stride(2)
            .map_err(|error| error.to_string())? as usize;

        let mut rgba = vec![0u8; width * height * 4];
        for row in 0..height {
            let y_start = row * y_row;
            let uv_start = (row / 2) * u_row;
            let out_start = row * width * 4;

            for col in 0..width {
                let y = y_plane[y_start + col] as i32;
                let uv_col = (col / 2) * u_pixel;
                let u = u_plane[uv_start + uv_col] as i32 - 128;
                let v = v_plane[(row / 2) * v_row + (col / 2) * v_pixel] as i32 - 128;

                let c = (y - 16).max(0) * 298;
                let r = ((c + 409 * v + 128) >> 8).clamp(0, 255) as u8;
                let g = ((c - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u8;
                let b = ((c + 516 * u + 128) >> 8).clamp(0, 255) as u8;

                let out = out_start + col * 4;
                rgba[out] = r;
                rgba[out + 1] = g;
                rgba[out + 2] = b;
                rgba[out + 3] = 255;
            }
        }

        Ok(VideoFrame {
            width,
            height,
            rgba,
        })
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.codec.stop();
    }
}
