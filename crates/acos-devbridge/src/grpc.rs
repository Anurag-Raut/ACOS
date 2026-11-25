use std::io::Cursor;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::{ColorType, ImageEncoder, RgbImage};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::Stream;
use tonic::{Request, Response, Status};

pub mod pb {
    tonic::include_proto!("android.emulation.control");
}

use pb::emulator_controller_server::EmulatorController;
use pb::image_format::ImgFormat;
use pb::image_transport::TransportChannel;
use pb::rotation::SkinRotation;
use pb::vm_configuration::VmHypervisorType;
use pb::*;

use crate::bridge::BridgeSession;
use crate::keymap::TranslatedKey;
use crate::live_client::{now_micros, RawRgbFrame};

pub struct AcosControllerService {
    session: Arc<BridgeSession>,
}

impl AcosControllerService {
    pub fn new(session: Arc<BridgeSession>) -> Self {
        Self { session }
    }

    fn check_auth<T>(&self, req: &Request<T>) -> Result<(), Status> {
        let header = req
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Status::unauthenticated("missing authorization bearer token"))?;
        let expected = format!("Bearer {}", self.session.token);
        if !constant_time_eq(header.as_bytes(), expected.as_bytes()) {
            return Err(Status::unauthenticated("invalid authorization bearer token"));
        }
        Ok(())
    }

    fn unimplemented<T>(&self, req: &Request<T>, method: &str) -> Status {
        if let Err(auth_err) = self.check_auth(req) {
            return auth_err;
        }
        Status::unimplemented(format!("acos-devbridge: {method} is not supported"))
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn scaled_dimensions(src_w: u32, src_h: u32, req_w: u32, req_h: u32) -> (u32, u32) {
    if (req_w == 0 && req_h == 0) || src_w == 0 || src_h == 0 {
        return (src_w.max(1), src_h.max(1));
    }
    let max_w = if req_w == 0 { src_w } else { req_w.min(src_w) }.max(1);
    let max_h = if req_h == 0 { src_h } else { req_h.min(src_h) }.max(1);
    if max_w >= src_w && max_h >= src_h {
        return (src_w, src_h);
    }
    let scale = (f64::from(max_w) / f64::from(src_w))
        .min(f64::from(max_h) / f64::from(src_h));
    let out_w = ((f64::from(src_w) * scale).floor() as u32)
        .max(1)
        .min(max_w);
    let out_h = ((f64::from(src_h) * scale).floor() as u32)
        .max(1)
        .min(max_h);
    (out_w, out_h)
}

fn resize_rgb888(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Result<Vec<u8>, String> {
    if src_w == dst_w && src_h == dst_h {
        return Ok(src.to_vec());
    }
    let img = RgbImage::from_raw(src_w, src_h, src.to_vec())
        .ok_or_else(|| "invalid RGB888 buffer length".to_string())?;
    let resized = image::imageops::resize(&img, dst_w, dst_h, FilterType::Triangle);
    Ok(resized.into_raw())
}

fn rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity((rgb.len() / 3) * 4);
    for px in rgb.chunks_exact(3) {
        out.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    out
}

fn encode_png_rgb(rgb: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let encoder = PngEncoder::new(Cursor::new(&mut out));
    encoder
        .write_image(rgb, w, h, ColorType::Rgb8.into())
        .map_err(|e| format!("encode png: {e}"))?;
    Ok(out)
}

fn build_image_proto(
    img_format: ImgFormat,
    w: u32,
    h: u32,
    rotation: u8,
    bytes: Vec<u8>,
    seq: u32,
    timestamp_us: u64,
) -> Image {
    let skin_rot = match rotation % 4 {
        0 => SkinRotation::Portrait as i32,
        1 => SkinRotation::Landscape as i32,
        2 => SkinRotation::ReversePortrait as i32,
        3 => SkinRotation::ReverseLandscape as i32,
        _ => SkinRotation::Portrait as i32,
    };
    #[allow(deprecated)]
    Image {
        format: Some(ImageFormat {
            format: img_format as i32,
            rotation: Some(Rotation {
                rotation: skin_rot,
                x_axis: 0.0,
                y_axis: 0.0,
                z_axis: 0.0,
            }),
            width: w,
            height: h,
            display: 0,
            transport: None,
            folded_display: None,
            display_mode: 0,
        }),
        width: w,
        height: h,
        image: bytes,
        seq,
        timestamp_us,
    }
}

fn frame_to_proto(
    frame: &RawRgbFrame,
    want_format: ImgFormat,
    req_w: u32,
    req_h: u32,
    rotation: u8,
) -> Result<Image, String> {
    let (out_w, out_h) = scaled_dimensions(frame.width, frame.height, req_w, req_h);
    let rgb_buf;
    let rgb_slice: &[u8] = if out_w == frame.width && out_h == frame.height {
        frame.rgb888.as_slice()
    } else {
        rgb_buf = resize_rgb888(&frame.rgb888, frame.width, frame.height, out_w, out_h)?;
        &rgb_buf
    };
    let bytes = match want_format {
        ImgFormat::Rgb888 => rgb_slice.to_vec(),
        ImgFormat::Rgba8888 => rgb_to_rgba(rgb_slice),
        ImgFormat::Png => encode_png_rgb(rgb_slice, out_w, out_h)?,
    };
    Ok(build_image_proto(
        want_format,
        out_w,
        out_h,
        rotation,
        bytes,
        frame.seq,
        frame.timestamp_us,
    ))
}

fn callback_error_to_status(resp: &serde_json::Value, fallback: &str) -> Status {
    let code = resp.get("code").and_then(|v| v.as_str()).unwrap_or("");
    let err = resp
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or(fallback);
    match code {
        "VERIFY_WATCH_ONLY" => Status::failed_precondition(err),
        "HOME_UNAVAILABLE" | "UNSUPPORTED" => Status::unimplemented(err),
        "SESSION_NOT_FOUND" | "NOT_FOUND" => Status::not_found(err),
        "BAD_REQUEST" | "INVALID_ARGUMENT" => Status::invalid_argument(err),
        _ => Status::internal(err),
    }
}

fn parse_requested_image_format(fmt: &ImageFormat) -> Result<ImgFormat, Status> {
    if let Some(transport) = &fmt.transport {
        if transport.channel != TransportChannel::Unspecified as i32 {
            return Err(Status::unimplemented(
                "acos-devbridge: MMAP image transport is not supported",
            ));
        }
    }
    if fmt.display != 0 {
        return Err(Status::invalid_argument(format!(
            "unsupported display id {}",
            fmt.display
        )));
    }
    ImgFormat::try_from(fmt.format)
        .map_err(|_| Status::invalid_argument(format!("unknown image format {}", fmt.format)))
}

#[tonic::async_trait]
impl EmulatorController for AcosControllerService {
    type streamSensorStream = Pin<Box<dyn Stream<Item = Result<SensorValue, Status>> + Send>>;
    type streamPhysicalModelStream =
        Pin<Box<dyn Stream<Item = Result<PhysicalModelValue, Status>> + Send>>;
    type streamClipboardStream = Pin<Box<dyn Stream<Item = Result<ClipData, Status>> + Send>>;
    type streamScreenshotStream = ReceiverStream<Result<Image, Status>>;
    type streamAudioStream = Pin<Box<dyn Stream<Item = Result<AudioPacket, Status>> + Send>>;
    type streamLogcatStream = Pin<Box<dyn Stream<Item = Result<LogMessage, Status>> + Send>>;
    type streamNotificationStream =
        Pin<Box<dyn Stream<Item = Result<Notification, Status>> + Send>>;

    async fn get_status(&self, request: Request<()>) -> Result<Response<EmulatorStatus>, Status> {
        self.check_auth(&request)?;
        let _ = self.session.refresh_status().await;
        let uptime_ms = self.session.started_at.elapsed().as_millis() as u64;
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(1);
        Ok(Response::new(EmulatorStatus {
            version: format!("ACOS {}", env!("CARGO_PKG_VERSION")),
            uptime: uptime_ms,
            booted: true,
            vm_config: Some(VmConfiguration {
                hypervisor_type: VmHypervisorType::None as i32,
                number_of_cpu_cores: cpus,
                ram_size_bytes: 0,
            }),
            hardware_config: Some(EntryList {
                entry: vec![Entry {
                    key: "hw.avd.name".to_string(),
                    value: self.session.avd_name.clone(),
                }],
            }),
            heartbeat: uptime_ms / 1000,
            guest_config: Default::default(),
        }))
    }

    async fn get_screenshot(
        &self,
        request: Request<ImageFormat>,
    ) -> Result<Response<Image>, Status> {
        self.check_auth(&request)?;
        let req = request.into_inner();
        let want_format = parse_requested_image_format(&req)?;
        let wants_unscaled = req.width == 0 && req.height == 0;
        let ts_us = now_micros();

        if wants_unscaled {
            let fmt_str = match want_format {
                ImgFormat::Png => "png",
                ImgFormat::Rgba8888 => "rgba8888",
                ImgFormat::Rgb888 => "rgb888",
            };
            let (w, h, rot, bytes) = self
                .session
                .raw_screenshot(fmt_str)
                .await
                .map_err(Status::unavailable)?;
            return Ok(Response::new(build_image_proto(
                want_format,
                w,
                h,
                rot,
                bytes,
                0,
                ts_us,
            )));
        }

        let (w, h, rot, rgb) = self
            .session
            .raw_screenshot("rgb888")
            .await
            .map_err(Status::unavailable)?;
        let frame = RawRgbFrame {
            seq: 0,
            width: w,
            height: h,
            timestamp_us: ts_us,
            rgb888: Arc::new(rgb),
        };
        let img = frame_to_proto(&frame, want_format, req.width, req.height, rot)
            .map_err(Status::internal)?;
        Ok(Response::new(img))
    }

    async fn stream_screenshot(
        &self,
        request: Request<ImageFormat>,
    ) -> Result<Response<Self::streamScreenshotStream>, Status> {
        self.check_auth(&request)?;
        let req = request.into_inner();
        let want_format = parse_requested_image_format(&req)?;

        // Refresh current status (rotation / display size / stream socket availability).
        let _ = self.session.refresh_status().await;

        let Some(hub) = self.session.live_hub.as_ref().map(Arc::clone) else {
            return Err(Status::unavailable(
                "session has no live-view stream socket",
            ));
        };
        let mut sub = hub.subscribe().map_err(Status::unavailable)?;
        let session = Arc::clone(&self.session);
        let mut shutdown_rx = session.subscribe_shutdown();
        let (tx, rx) = mpsc::channel(4);

        tokio::spawn(async move {
            // Keep `sub` owned by this task until client disconnects or session stops.
            let mut last_dims: Option<(u32, u32)> = None;
            loop {
                tokio::select! {
                    _ = tx.closed() => break,
                    res = shutdown_rx.changed() => {
                        if res.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    changed = sub.rx.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        let maybe_frame = sub.rx.borrow_and_update().clone();
                        let Some(frame) = maybe_frame else {
                            continue;
                        };
                        if last_dims != Some((frame.width, frame.height)) {
                            last_dims = Some((frame.width, frame.height));
                            let _ = session.refresh_status().await;
                        }
                        let rot = session.rotation.load(Ordering::Relaxed);
                        let proto_res = tokio::task::spawn_blocking(move || {
                            frame_to_proto(&frame, want_format, req.width, req.height, rot)
                        })
                        .await;
                        match proto_res {
                            Ok(Ok(img)) => {
                                if tx.send(Ok(img)).await.is_err() {
                                    break;
                                }
                            }
                            Ok(Err(e)) => {
                                let _ = tx.send(Err(Status::internal(e))).await;
                                break;
                            }
                            Err(e) => {
                                let _ = tx.send(Err(Status::internal(format!("encode join: {e}")))).await;
                                break;
                            }
                        }
                    }
                }
            }
            drop(sub);
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn send_touch(&self, request: Request<TouchEvent>) -> Result<Response<()>, Status> {
        self.check_auth(&request)?;
        let ev = request.into_inner();
        if ev.display != 0 {
            return Err(Status::invalid_argument(format!(
                "unsupported display id {}",
                ev.display
            )));
        }
        let _input_guard = self.session.input_lock.lock().await;
        if self.session.verify_run.load(Ordering::Relaxed) {
            let resp = self
                .session
                .call_callback(json!({
                    "cmd": "touch",
                    "session_id": self.session.session_id,
                    "action": 0,
                    "down_time": 0,
                    "event_time": 0,
                    "pointers": [],
                }))
                .await
                .map_err(Status::unavailable)?;
            return Err(callback_error_to_status(&resp, "panel input ignored: verify run"));
        }
        let touches: Vec<(i32, i32, i32, i32)> = ev
            .touches
            .iter()
            .map(|t| (t.identifier, t.x, t.y, t.pressure))
            .collect();
        let w = self.session.display_w.load(Ordering::Relaxed);
        let h = self.session.display_h.load(Ordering::Relaxed);
        let frames = {
            let mut tracker = self
                .session
                .touch_tracker
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            tracker.apply(&touches, w, h)
        };
        for frame in frames {
            let ptrs: Vec<serde_json::Value> = frame
                .pointers
                .iter()
                .map(|&(id, x, y)| json!([id, x, y]))
                .collect();
            let resp = self
                .session
                .call_callback(json!({
                    "cmd": "touch",
                    "session_id": self.session.session_id,
                    "action": frame.action,
                    "down_time": frame.down_time_ns,
                    "event_time": frame.event_time_ns,
                    "pointers": ptrs,
                }))
                .await
                .map_err(Status::unavailable)?;
            if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                return Err(callback_error_to_status(&resp, "panel touch failed"));
            }
        }
        Ok(Response::new(()))
    }

    async fn send_key(&self, request: Request<KeyboardEvent>) -> Result<Response<()>, Status> {
        self.check_auth(&request)?;
        let ev = request.into_inner();
        let _input_guard = self.session.input_lock.lock().await;
        let translated = {
            let mut ks = self
                .session
                .keyboard_state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            ks.translate(&ev).map_err(Status::unimplemented)?
        };
        let req = match translated {
            TranslatedKey::Key {
                code,
                event_type,
                meta_state,
            } => json!({
                "cmd": "key",
                "session_id": self.session.session_id,
                "code": code,
                "event_type": event_type,
                "meta_state": meta_state,
                "device_id": 1,
            }),
            TranslatedKey::Text(text) => json!({
                "cmd": "text",
                "session_id": self.session.session_id,
                "text": text,
            }),
        };
        let resp = self
            .session
            .call_callback(req)
            .await
            .map_err(Status::unavailable)?;
        if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            return Err(callback_error_to_status(&resp, "panel key failed"));
        }
        Ok(Response::new(()))
    }

    // ---- Honest UNIMPLEMENTED for every other EmulatorController RPC ---- //

    async fn stream_sensor(
        &self,
        request: Request<SensorValue>,
    ) -> Result<Response<Self::streamSensorStream>, Status> {
        Err(self.unimplemented(&request, "streamSensor"))
    }
    async fn get_sensor(&self, request: Request<SensorValue>) -> Result<Response<SensorValue>, Status> {
        Err(self.unimplemented(&request, "getSensor"))
    }
    async fn set_sensor(&self, request: Request<SensorValue>) -> Result<Response<()>, Status> {
        Err(self.unimplemented(&request, "setSensor"))
    }
    async fn set_physical_model(
        &self,
        request: Request<PhysicalModelValue>,
    ) -> Result<Response<()>, Status> {
        Err(self.unimplemented(&request, "setPhysicalModel"))
    }
    async fn get_physical_model(
        &self,
        request: Request<PhysicalModelValue>,
    ) -> Result<Response<PhysicalModelValue>, Status> {
        Err(self.unimplemented(&request, "getPhysicalModel"))
    }
    async fn stream_physical_model(
        &self,
        request: Request<PhysicalModelValue>,
    ) -> Result<Response<Self::streamPhysicalModelStream>, Status> {
        Err(self.unimplemented(&request, "streamPhysicalModel"))
    }
    async fn set_clipboard(&self, request: Request<ClipData>) -> Result<Response<()>, Status> {
        Err(self.unimplemented(&request, "setClipboard"))
    }
    async fn get_clipboard(&self, request: Request<()>) -> Result<Response<ClipData>, Status> {
        Err(self.unimplemented(&request, "getClipboard"))
    }
    async fn stream_clipboard(
        &self,
        request: Request<()>,
    ) -> Result<Response<Self::streamClipboardStream>, Status> {
        Err(self.unimplemented(&request, "streamClipboard"))
    }
    async fn set_battery(&self, request: Request<BatteryState>) -> Result<Response<()>, Status> {
        Err(self.unimplemented(&request, "setBattery"))
    }
    async fn get_battery(&self, request: Request<()>) -> Result<Response<BatteryState>, Status> {
        Err(self.unimplemented(&request, "getBattery"))
    }
    async fn set_gps(&self, request: Request<GpsState>) -> Result<Response<()>, Status> {
        Err(self.unimplemented(&request, "setGps"))
    }
    async fn get_gps(&self, request: Request<()>) -> Result<Response<GpsState>, Status> {
        Err(self.unimplemented(&request, "getGps"))
    }
    async fn send_fingerprint(&self, request: Request<Fingerprint>) -> Result<Response<()>, Status> {
        Err(self.unimplemented(&request, "sendFingerprint"))
    }
    async fn send_mouse(&self, request: Request<MouseEvent>) -> Result<Response<()>, Status> {
        Err(self.unimplemented(&request, "sendMouse"))
    }
    async fn inject_wheel(
        &self,
        request: Request<tonic::Streaming<WheelEvent>>,
    ) -> Result<Response<()>, Status> {
        Err(self.unimplemented(&request, "injectWheel"))
    }
    async fn stream_input_event(
        &self,
        request: Request<tonic::Streaming<InputEvent>>,
    ) -> Result<Response<()>, Status> {
        Err(self.unimplemented(&request, "streamInputEvent"))
    }
    async fn send_phone(&self, request: Request<PhoneCall>) -> Result<Response<PhoneResponse>, Status> {
        Err(self.unimplemented(&request, "sendPhone"))
    }
    async fn send_sms(&self, request: Request<SmsMessage>) -> Result<Response<PhoneResponse>, Status> {
        Err(self.unimplemented(&request, "sendSms"))
    }
    async fn set_phone_number(
        &self,
        request: Request<PhoneNumber>,
    ) -> Result<Response<PhoneResponse>, Status> {
        Err(self.unimplemented(&request, "setPhoneNumber"))
    }
    async fn stream_audio(
        &self,
        request: Request<AudioFormat>,
    ) -> Result<Response<Self::streamAudioStream>, Status> {
        Err(self.unimplemented(&request, "streamAudio"))
    }
    async fn inject_audio(
        &self,
        request: Request<tonic::Streaming<AudioPacket>>,
    ) -> Result<Response<()>, Status> {
        Err(self.unimplemented(&request, "injectAudio"))
    }
    async fn get_logcat(&self, request: Request<LogMessage>) -> Result<Response<LogMessage>, Status> {
        Err(self.unimplemented(&request, "getLogcat"))
    }
    async fn stream_logcat(
        &self,
        request: Request<LogMessage>,
    ) -> Result<Response<Self::streamLogcatStream>, Status> {
        Err(self.unimplemented(&request, "streamLogcat"))
    }
    async fn set_vm_state(&self, request: Request<VmRunState>) -> Result<Response<()>, Status> {
        Err(self.unimplemented(&request, "setVmState"))
    }
    async fn get_vm_state(&self, request: Request<()>) -> Result<Response<VmRunState>, Status> {
        Err(self.unimplemented(&request, "getVmState"))
    }
    async fn set_display_configurations(
        &self,
        request: Request<DisplayConfigurations>,
    ) -> Result<Response<DisplayConfigurations>, Status> {
        Err(self.unimplemented(&request, "setDisplayConfigurations"))
    }
    async fn get_display_configurations(
        &self,
        request: Request<()>,
    ) -> Result<Response<DisplayConfigurations>, Status> {
        Err(self.unimplemented(&request, "getDisplayConfigurations"))
    }
    async fn stream_notification(
        &self,
        request: Request<()>,
    ) -> Result<Response<Self::streamNotificationStream>, Status> {
        Err(self.unimplemented(&request, "streamNotification"))
    }
    async fn rotate_virtual_scene_camera(
        &self,
        request: Request<RotationRadian>,
    ) -> Result<Response<()>, Status> {
        Err(self.unimplemented(&request, "rotateVirtualSceneCamera"))
    }
    async fn set_virtual_scene_camera_velocity(
        &self,
        request: Request<Velocity>,
    ) -> Result<Response<()>, Status> {
        Err(self.unimplemented(&request, "setVirtualSceneCameraVelocity"))
    }
    async fn set_posture(&self, request: Request<Posture>) -> Result<Response<()>, Status> {
        Err(self.unimplemented(&request, "setPosture"))
    }
    async fn get_brightness(
        &self,
        request: Request<BrightnessValue>,
    ) -> Result<Response<BrightnessValue>, Status> {
        Err(self.unimplemented(&request, "getBrightness"))
    }
    async fn set_brightness(
        &self,
        request: Request<BrightnessValue>,
    ) -> Result<Response<()>, Status> {
        Err(self.unimplemented(&request, "setBrightness"))
    }
    async fn get_display_mode(
        &self,
        request: Request<()>,
    ) -> Result<Response<DisplayMode>, Status> {
        Err(self.unimplemented(&request, "getDisplayMode"))
    }
    async fn set_display_mode(
        &self,
        request: Request<DisplayMode>,
    ) -> Result<Response<()>, Status> {
        Err(self.unimplemented(&request, "setDisplayMode"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    use tokio::sync::{watch, Notify};

    fn scratch_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = PathBuf::from("/tmp").join(format!(
            "acos-t-grpc-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, AtomicOrdering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn auth_request<T>(token: &str, msg: T) -> Request<T> {
        let mut req = Request::new(msg);
        req.metadata_mut().insert(
            "authorization",
            format!("Bearer {token}").parse().expect("auth header"),
        );
        req
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_send_touch_and_send_key_serialize_callbacks() {
        let dir = scratch_dir("serial-input");
        let sock = dir.join("cb.sock");
        let listener = UnixListener::bind(&sock).expect("bind callback sock");

        let trace: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let first_touch_entered = Arc::new(Notify::new());
        let first_key_entered = Arc::new(Notify::new());

        let trace_srv = Arc::clone(&trace);
        let touch_notify = Arc::clone(&first_touch_entered);
        let key_notify = Arc::clone(&first_key_entered);
        let server_task = tokio::spawn(async move {
            for _ in 0..4 {
                let (stream, _) = listener.accept().await.expect("accept");
                let trace_conn = Arc::clone(&trace_srv);
                let touch_n = Arc::clone(&touch_notify);
                let key_n = Arc::clone(&key_notify);
                tokio::spawn(async move {
                    let (r, mut w) = stream.into_split();
                    let mut reader = BufReader::new(r);
                    let mut line = String::new();
                    reader.read_line(&mut line).await.expect("read req");
                    let req: serde_json::Value = serde_json::from_str(&line).expect("json");
                    let cmd = req["cmd"].as_str().unwrap_or("");
                    let label = match cmd {
                        "touch" => {
                            let action = req["action"].as_i64().unwrap_or(-1);
                            let tag = format!("touch:{action}");
                            trace_conn.lock().unwrap().push(format!("start:{tag}"));
                            if action == 0 {
                                touch_n.notify_one();
                                tokio::time::sleep(Duration::from_millis(80)).await;
                            }
                            tag
                        }
                        "key" => {
                            let et = req["event_type"].as_str().unwrap_or("");
                            let tag = format!("key:{et}");
                            trace_conn.lock().unwrap().push(format!("start:{tag}"));
                            if et == "down" {
                                key_n.notify_one();
                                tokio::time::sleep(Duration::from_millis(80)).await;
                            }
                            tag
                        }
                        other => other.to_string(),
                    };
                    trace_conn.lock().unwrap().push(format!("end:{label}"));
                    let mut out = json!({ "ok": true }).to_string();
                    out.push('\n');
                    w.write_all(out.as_bytes()).await.expect("write resp");
                });
            }
        });

        let (shutdown_tx, _rx) = watch::channel(false);
        let session = BridgeSession::new(
            "sess-1".to_string(),
            "ACOS".to_string(),
            "tok".to_string(),
            sock,
            dir.clone(),
            1080,
            2400,
            0,
            false,
            None,
            shutdown_tx,
        );
        let svc = Arc::new(AcosControllerService::new(session));

        // 1. Concurrent touch DOWN followed closely by UP.
        let svc1 = Arc::clone(&svc);
        let down_task = tokio::spawn(async move {
            svc1.send_touch(auth_request(
                "tok",
                TouchEvent {
                    touches: vec![Touch {
                        x: 100,
                        y: 200,
                        identifier: 0,
                        pressure: 1,
                        ..Default::default()
                    }],
                    display: 0,
                },
            ))
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), first_touch_entered.notified())
            .await
            .expect("touch down callback entered");
        let svc2 = Arc::clone(&svc);
        let up_task = tokio::spawn(async move {
            svc2.send_touch(auth_request(
                "tok",
                TouchEvent {
                    touches: vec![Touch {
                        x: 100,
                        y: 200,
                        identifier: 0,
                        pressure: 0,
                        ..Default::default()
                    }],
                    display: 0,
                },
            ))
            .await
        });
        down_task.await.unwrap().expect("touch down ok");
        up_task.await.unwrap().expect("touch up ok");

        // 2. Concurrent KeyDown followed closely by KeyUp.
        let svc3 = Arc::clone(&svc);
        let kd_task = tokio::spawn(async move {
            svc3.send_key(auth_request(
                "tok",
                KeyboardEvent {
                    code_type: keyboard_event::KeyCodeType::Evdev as i32,
                    event_type: keyboard_event::KeyEventType::Keydown as i32,
                    key_code: 30,
                    ..Default::default()
                },
            ))
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), first_key_entered.notified())
            .await
            .expect("key down callback entered");
        let svc4 = Arc::clone(&svc);
        let ku_task = tokio::spawn(async move {
            svc4.send_key(auth_request(
                "tok",
                KeyboardEvent {
                    code_type: keyboard_event::KeyCodeType::Evdev as i32,
                    event_type: keyboard_event::KeyEventType::Keyup as i32,
                    key_code: 30,
                    ..Default::default()
                },
            ))
            .await
        });
        kd_task.await.unwrap().expect("key down ok");
        ku_task.await.unwrap().expect("key up ok");

        server_task.await.expect("server join");
        let events = trace.lock().unwrap().clone();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            events,
            vec![
                "start:touch:0",
                "end:touch:0",
                "start:touch:1",
                "end:touch:1",
                "start:key:down",
                "end:key:down",
                "start:key:up",
                "end:key:up",
            ]
        );
    }
}
