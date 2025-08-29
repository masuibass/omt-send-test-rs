use anyhow::{bail, Result};
use std::{
    ffi::CString,
    mem,
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

mod bindings;
use bindings::*;

#[derive(Debug, Clone, Copy)]
struct VideoFormat {
    codec: OMTCodec,
    width: i32,
    height: i32,
    fps_n: i32,
    fps_d: i32,
    name: &'static str,
}

impl VideoFormat {
    fn stride(&self) -> i32 {
        match self.codec {
            x if x == OMTCodec_OMTCodec_UYVY => self.width * 2,
            x if x == OMTCodec_OMTCodec_BGRA => self.width * 4,
            x if x == OMTCodec_OMTCodec_NV12 => self.width, // Y plane stride
            _ => self.width * 4,
        }
    }

    fn buffer_size(&self) -> usize {
        match self.codec {
            x if x == OMTCodec_OMTCodec_UYVY => (self.stride() * self.height) as usize,
            x if x == OMTCodec_OMTCodec_BGRA => (self.stride() * self.height) as usize,
            x if x == OMTCodec_OMTCodec_NV12 => {
                // NV12: Y plane (width * height) + UV plane (width * height / 2)
                ((self.width * self.height) + (self.width * self.height / 2)) as usize
            }
            _ => (self.stride() * self.height) as usize,
        }
    }

    fn create_test_frame(&self) -> Vec<u8> {
        let mut buf = vec![0u8; self.buffer_size()];

        match self.codec {
            x if x == OMTCodec_OMTCodec_UYVY => {
                // UYVY: Create color bars pattern
                for (y, row) in buf.chunks_exact_mut(self.stride() as usize).enumerate() {
                    for (x_pair, pair) in row.chunks_exact_mut(4).enumerate() {
                        let x = x_pair * 2;
                        let section = (x * 8) / self.width as usize;

                        // Color bar values (U, Y, V)
                        let (u, y_val, v) = match section {
                            0 => (128, 235, 128), // White
                            1 => (16, 210, 146),  // Yellow
                            2 => (166, 170, 16),  // Cyan
                            3 => (54, 145, 34),   // Green
                            4 => (202, 106, 222), // Magenta
                            5 => (90, 81, 240),   // Red
                            6 => (240, 41, 110),  // Blue
                            _ => (128, 16, 128),  // Black
                        };

                        pair[0] = u; // U
                        pair[1] = y_val; // Y0
                        pair[2] = v; // V
                        pair[3] = y_val; // Y1
                    }
                }
            }
            x if x == OMTCodec_OMTCodec_BGRA => {
                // BGRA: Create gradient pattern
                for (i, pixel) in buf.chunks_exact_mut(4).enumerate() {
                    let x = (i % self.width as usize) as f32;
                    let y = (i / self.width as usize) as f32;

                    pixel[0] = ((x / self.width as f32) * 255.0) as u8; // B
                    pixel[1] = ((y / self.height as f32) * 255.0) as u8; // G
                    pixel[2] = (((x + y) / (self.width + self.height) as f32) * 255.0) as u8; // R
                    pixel[3] = 255; // A
                }
            }
            x if x == OMTCodec_OMTCodec_NV12 => {
                // NV12: Y plane followed by interleaved UV
                let y_size = (self.width * self.height) as usize;
                // Fill Y plane
                for i in 0..y_size {
                    buf[i] = 180; // Y value
                }
                // Fill UV plane (interleaved U and V)
                let uv_start = y_size;
                let uv_size = (self.width * self.height / 2) as usize;
                for i in 0..(uv_size / 2) {
                    buf[uv_start + i * 2] = 128; // U
                    buf[uv_start + i * 2 + 1] = 128; // V
                }
            }
            _ => {}
        }

        buf
    }
}

fn interpret_return_code(rc: i32) -> &'static str {
    match rc {
        0 => "Success",
        // These appear to be status codes that still result in successful transmission
        12428 | 19448 | 29843 | 39293 => "Frame queued/processing (non-fatal)",
        26984 => "Buffer overflow or encoding error",
        -1 => "General error",
        _ if rc > 0 => "Status/warning code (may be non-fatal)",
        _ => "Unknown error",
    }
}

fn run_send_test(format: VideoFormat, duration_secs: u32, use_alpha: bool) -> Result<()> {
    unsafe {
        println!("\n=== Testing {} ===\n", format.name);

        // Set up logging
        let logfile = CString::new("/tmp/omt-send.log")?;
        omt_setloggingfilename(logfile.as_ptr());

        // Create sender
        let name = CString::new(format!("RustSend_{}", format.name))?;
        let sender = omt_send_create(name.as_ptr(), OMTQuality_OMTQuality_Medium);
        if sender.is_null() {
            bail!("omt_send_create failed");
        }

        // Wait for receiver connection
        let mut connected = false;
        println!("Waiting for receiver connection...");
        for i in 0..30 {
            if omt_send_connections(sender) > 0 {
                connected = true;
                println!("Receiver connected after {:.1}s", i as f32 * 0.1);
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        if !connected {
            eprintln!("Warning: No receivers connected, proceeding anyway");
        }

        // Set sender info
        let mut info: OMTSenderInfo = mem::zeroed();
        fn write_cstr(dst: &mut [i8], s: &str) {
            let bytes = s.as_bytes();
            let n = bytes.len().min(dst.len().saturating_sub(1));
            for i in 0..n {
                dst[i] = bytes[i] as i8;
            }
            if !dst.is_empty() {
                dst[n] = 0;
            }
        }
        write_cstr(&mut info.ProductName, "omt-send-test-rs");
        write_cstr(&mut info.Manufacturer, "Rust OMT Test");
        write_cstr(&mut info.Version, "1.0.0");
        omt_send_setsenderinformation(sender, &mut info as *mut OMTSenderInfo);

        // Create test frame
        let mut frame_buf = format.create_test_frame();

        // Setup OMTMediaFrame
        let mut frame: OMTMediaFrame = mem::zeroed();
        frame.Type = OMTFrameType_OMTFrameType_Video;
        frame.Codec = format.codec;
        frame.Width = format.width;
        frame.Height = format.height;
        frame.Stride = format.stride();
        frame.Flags = if use_alpha && format.codec == OMTCodec_OMTCodec_BGRA {
            OMTVideoFlags_OMTVideoFlags_Alpha
        } else {
            OMTVideoFlags_OMTVideoFlags_None
        };
        frame.FrameRateN = format.fps_n;
        frame.FrameRateD = format.fps_d;
        frame.AspectRatio = (format.width as f32) / (format.height as f32);
        frame.ColorSpace = if format.height < 720 {
            OMTColorSpace_OMTColorSpace_BT601
        } else {
            OMTColorSpace_OMTColorSpace_BT709
        };
        frame.Data = frame_buf.as_mut_ptr() as *mut _;
        // DataLength should be the actual data size, not buffer size
        frame.DataLength = match format.codec {
            x if x == OMTCodec_OMTCodec_NV12 => {
                // For NV12, DataLength is Y + UV size
                ((format.width * format.height) + (format.width * format.height / 2)) as i32
            }
            _ => {
                // For packed formats, it's stride * height
                (format.stride() * format.height) as i32
            }
        };

        // High-precision timing
        let ticks_per_sec = 10_000_000i64;
        let ticks_per_frame = ticks_per_sec * (format.fps_d as i64) / (format.fps_n as i64);
        let frame_duration = Duration::from_secs_f64(format.fps_d as f64 / format.fps_n as f64);

        let mut pts: i64 = 0;
        let frames_to_send = duration_secs * (format.fps_n as u32) / (format.fps_d as u32);
        let start_time = Instant::now();
        let mut next_frame_time = start_time;

        // Statistics tracking
        let mut stats_counter = 0;
        let stats_interval = format.fps_n; // Print stats every second

        println!(
            "Sending {} frames at {}x{} {}fps...",
            frames_to_send,
            format.width,
            format.height,
            format.fps_n as f64 / format.fps_d as f64
        );

        for i in 0..frames_to_send {
            frame.Timestamp = pts;

            let rc = omt_send(sender, &mut frame as *mut OMTMediaFrame);
            if rc != 0 {
                let status = interpret_return_code(rc);

                // Check if receiver disconnected
                if omt_send_connections(sender) == 0 {
                    eprintln!("Receiver disconnected, stopping");
                    break;
                }

                // For buffer overflow, wait a bit and retry
                if rc == 26984 {
                    eprintln!("Buffer overflow at frame {}, waiting...", i);
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }

                // Non-fatal status codes - continue normally
                if status.contains("non-fatal") {
                    // Frame was likely still sent, continue
                } else {
                    // Fatal error
                    eprintln!("Fatal error at frame {}: {} (rc={})", i, status, rc);
                    bail!("omt_send failed at frame {} (rc={})", i, rc);
                }
            }

            pts = pts.saturating_add(ticks_per_frame);
            stats_counter += 1;

            // Print statistics periodically
            if stats_counter >= stats_interval {
                let mut vstats: OMTStatistics = mem::zeroed();
                omt_send_getvideostatistics(sender, &mut vstats as *mut OMTStatistics);
                println!(
                    "[{:.1}s] Sent: {} bytes, {} frames, dropped: {}, codec_time: {}ms",
                    start_time.elapsed().as_secs_f64(),
                    vstats.BytesSent,
                    vstats.Frames,
                    vstats.FramesDropped,
                    vstats.CodecTimeSinceLast
                );
                stats_counter = 0;
            }

            // High-precision frame timing
            next_frame_time += frame_duration;
            let now = Instant::now();
            if next_frame_time > now {
                thread::sleep(next_frame_time - now);
            } else if (now - next_frame_time) > frame_duration * 2 {
                // If we're more than 2 frames behind, reset timing
                eprintln!("Timing drift detected, resynchronizing");
                next_frame_time = now + frame_duration;
            }
        }

        // Final statistics
        let mut vstats: OMTStatistics = mem::zeroed();
        omt_send_getvideostatistics(sender, &mut vstats as *mut OMTStatistics);
        println!("\n=== Final Statistics for {} ===", format.name);
        println!("Total bytes sent: {}", vstats.BytesSent);
        println!("Total frames sent: {}", vstats.Frames);
        println!("Frames dropped: {}", vstats.FramesDropped);
        println!(
            "Average bitrate: {:.2} Mbps",
            (vstats.BytesSent as f64 * 8.0) / (duration_secs as f64 * 1_000_000.0)
        );
        println!(
            "Success rate: {:.2}%",
            (vstats.Frames as f64 / frames_to_send as f64) * 100.0
        );

        omt_send_destroy(sender);
        println!("Test completed successfully\n");
    }

    Ok(())
}

fn main() -> Result<()> {
    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    let test_format = args.get(1).map(|s| s.as_str());

    // Test configurations
    let formats = vec![
        // Current stable format
        VideoFormat {
            codec: OMTCodec_OMTCodec_UYVY,
            width: 1280,
            height: 720,
            fps_n: 30,
            fps_d: 1,
            name: "UYVY_720p30",
        },
        // Test higher resolution UYVY
        VideoFormat {
            codec: OMTCodec_OMTCodec_UYVY,
            width: 1920,
            height: 1080,
            fps_n: 30,
            fps_d: 1,
            name: "UYVY_1080p30",
        },
        // Test BGRA 720p
        VideoFormat {
            codec: OMTCodec_OMTCodec_BGRA,
            width: 1280,
            height: 720,
            fps_n: 30,
            fps_d: 1,
            name: "BGRA_720p30",
        },
        // Test BGRA 1080p
        VideoFormat {
            codec: OMTCodec_OMTCodec_BGRA,
            width: 1920,
            height: 1080,
            fps_n: 30,
            fps_d: 1,
            name: "BGRA_1080p30",
        },
        // Test NV12 format
        VideoFormat {
            codec: OMTCodec_OMTCodec_NV12,
            width: 1280,
            height: 720,
            fps_n: 30,
            fps_d: 1,
            name: "NV12_720p30",
        },
    ];

    println!("OMT Send Test Suite");
    println!("==================");
    println!("Usage: cargo run [format_name]");
    println!(
        "Available formats: UYVY_720p30, UYVY_1080p30, BGRA_720p30, BGRA_1080p30, NV12_720p30\n"
    );

    // Filter formats based on command line argument
    let formats_to_test: Vec<VideoFormat> = if let Some(name) = test_format {
        formats.into_iter().filter(|f| f.name == name).collect()
    } else {
        formats
    };

    if formats_to_test.is_empty() {
        eprintln!("Error: Unknown format specified");
        return Ok(());
    }

    // Run tests
    for format in formats_to_test {
        if let Err(e) = run_send_test(format, 5, false) {
            eprintln!("Test failed for {}: {}", format.name, e);
            // Continue with next test instead of stopping
            thread::sleep(Duration::from_secs(2));
            continue;
        }

        // Test with alpha flag for BGRA
        if format.codec == OMTCodec_OMTCodec_BGRA {
            println!("\nTesting {} with alpha flag...", format.name);
            if let Err(e) = run_send_test(format, 5, true) {
                eprintln!("Test with alpha failed for {}: {}", format.name, e);
            }
        }

        thread::sleep(Duration::from_secs(1)); // Brief pause between tests
    }

    println!("\nAll tests completed!");

    // Check log file for errors
    println!("\nChecking log file for errors...");
    if let Ok(log_content) = std::fs::read_to_string("/tmp/omt-send.log") {
        let error_lines: Vec<&str> = log_content
            .lines()
            .filter(|line| line.contains("ERROR") || line.contains("WARN"))
            .collect();

        if !error_lines.is_empty() {
            println!("Found {} warnings/errors in log:", error_lines.len());
            for line in error_lines.iter().take(10) {
                println!("  {}", line);
            }
        } else {
            println!("No errors found in log file");
        }
    }

    Ok(())
}
