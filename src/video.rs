// Video frame extraction through the statically linked FFmpeg: replaces
// the external `ffmpeg -f rawvideo` pipeline of the Node build.
use crate::media::Decoded;
use ffmpeg_next as ff;
use std::path::Path;

pub fn probe_size(path: &Path) -> (u32, u32) {
    let fallback = (640, 360);
    let Ok(input) = ff::format::input(path) else { return fallback };
    input
        .streams()
        .best(ff::media::Type::Video)
        .and_then(|stream| {
            let params = stream.parameters();
            let decoder =
                ff::codec::context::Context::from_parameters(params).ok()?.decoder().video().ok()?;
            Some((decoder.width(), decoder.height()))
        })
        .filter(|&(w, h)| w > 0 && h > 0)
        .unwrap_or(fallback)
}

// GIFs go out as gif-playback MP4s, like WhatsApp expects; openh264 is
// statically linked, so the conversion stays in-process.
pub fn gif_to_mp4(src: &Path, dst: &Path) -> Option<()> {
    let mut input = ff::format::input(src).ok()?;
    let stream = input.streams().best(ff::media::Type::Video)?;
    let stream_index = stream.index();
    let context = ff::codec::context::Context::from_parameters(stream.parameters()).ok()?;
    let mut decoder = context.decoder().video().ok()?;
    let (w, h) = ((decoder.width() / 2) * 2, (decoder.height() / 2) * 2);
    if w == 0 || h == 0 {
        return None;
    }

    let codec = ff::encoder::find_by_name("libopenh264")?;
    let mut output = ff::format::output(dst).ok()?;
    let mut encoder = ff::codec::context::Context::new_with_codec(codec).encoder().video().ok()?;
    const FPS: i32 = 15;
    encoder.set_width(w);
    encoder.set_height(h);
    encoder.set_format(ff::format::Pixel::YUV420P);
    encoder.set_time_base(ff::Rational::new(1, FPS));
    encoder.set_frame_rate(Some(ff::Rational::new(FPS, 1)));
    encoder.set_bit_rate(700_000);
    let mut encoder = encoder.open_as(codec).ok()?;
    {
        let mut out_stream = output.add_stream(codec).ok()?;
        out_stream.set_parameters(&encoder);
        out_stream.set_time_base(ff::Rational::new(1, FPS));
    }
    output.write_header().ok()?;

    let mut scaler = ff::software::scaling::Context::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        ff::format::Pixel::YUV420P,
        w,
        h,
        ff::software::scaling::Flags::BILINEAR,
    )
    .ok()?;

    let mut pts: i64 = 0;
    let mut write_packets =
        |encoder: &mut ff::encoder::Video, output: &mut ff::format::context::Output| {
            let mut packet = ff::Packet::empty();
            while encoder.receive_packet(&mut packet).is_ok() {
                packet.set_stream(0);
                let _ = packet.write_interleaved(output);
            }
        };
    let mut receive = |decoder: &mut ff::decoder::Video,
                       encoder: &mut ff::encoder::Video,
                       scaler: &mut ff::software::scaling::Context,
                       output: &mut ff::format::context::Output,
                       pts: &mut i64| {
        let mut frame = ff::frame::Video::empty();
        while decoder.receive_frame(&mut frame).is_ok() {
            let mut yuv = ff::frame::Video::empty();
            if scaler.run(&frame, &mut yuv).is_err() {
                continue;
            }
            yuv.set_pts(Some(*pts));
            *pts += 1;
            if encoder.send_frame(&yuv).is_ok() {
                write_packets(encoder, output);
            }
        }
    };
    for (stream, packet) in input.packets() {
        if stream.index() != stream_index {
            continue;
        }
        if decoder.send_packet(&packet).is_ok() {
            receive(&mut decoder, &mut encoder, &mut scaler, &mut output, &mut pts);
        }
    }
    let _ = decoder.send_eof();
    receive(&mut decoder, &mut encoder, &mut scaler, &mut output, &mut pts);
    let _ = encoder.send_eof();
    write_packets(&mut encoder, &mut output);
    output.write_trailer().ok()?;
    Some(())
}

// Decodes up to `max_frames` frames at roughly `fps`, scaled so the width
// is `target_w` (even, keeping aspect), as RGBA bitmaps.
pub fn frames(path: &Path, target_w: u32, fps: f64, max_frames: usize) -> Vec<Decoded> {
    let mut out = Vec::new();
    let Ok(mut input) = ff::format::input(path) else { return out };
    let Some(stream) = input.streams().best(ff::media::Type::Video) else { return out };
    let stream_index = stream.index();
    let time_base = f64::from(stream.time_base());
    let Ok(context) = ff::codec::context::Context::from_parameters(stream.parameters()) else {
        return out;
    };
    let Ok(mut decoder) = context.decoder().video() else { return out };
    let (w, h) = (decoder.width(), decoder.height());
    if w == 0 || h == 0 {
        return out;
    }
    let tw = (target_w.min(w) / 2) * 2;
    let th = ((h as u64 * tw as u64 / w as u64) as u32 / 2) * 2;
    let Ok(mut scaler) = ff::software::scaling::Context::get(
        decoder.format(),
        w,
        h,
        ff::format::Pixel::RGBA,
        tw.max(2),
        th.max(2),
        ff::software::scaling::Flags::BILINEAR,
    ) else {
        return out;
    };
    let min_gap = if fps > 0.0 { 1.0 / fps } else { 0.0 };
    let mut next_at = 0.0f64;
    let mut receive = |decoder: &mut ff::decoder::Video, out: &mut Vec<Decoded>| {
        let mut frame = ff::frame::Video::empty();
        while decoder.receive_frame(&mut frame).is_ok() {
            if out.len() >= max_frames {
                return;
            }
            let ts = frame.pts().unwrap_or(0) as f64 * time_base;
            if ts + 1e-6 < next_at {
                continue;
            }
            next_at = ts + min_gap;
            let mut rgba = ff::frame::Video::empty();
            if scaler.run(&frame, &mut rgba).is_err() {
                continue;
            }
            // FFmpeg pads each row to its own stride; the UI wants packed.
            let stride = rgba.stride(0);
            let row_bytes = (rgba.width() * 4) as usize;
            let data = rgba.data(0);
            let mut packed = Vec::with_capacity(row_bytes * rgba.height() as usize);
            for row in 0..rgba.height() as usize {
                let start = row * stride;
                packed.extend_from_slice(&data[start..start + row_bytes]);
            }
            out.push(Decoded { w: rgba.width(), h: rgba.height(), rgba: packed });
        }
    };
    for (stream, packet) in input.packets() {
        if out.len() >= max_frames {
            break;
        }
        if stream.index() != stream_index {
            continue;
        }
        if decoder.send_packet(&packet).is_ok() {
            receive(&mut decoder, &mut out);
        }
    }
    let _ = decoder.send_eof();
    receive(&mut decoder, &mut out);
    out
}
