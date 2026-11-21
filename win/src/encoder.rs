//! Hardware H.264 through a Media Foundation transform (NVENC, AMF or
//! QuickSync behind one API). Hardware MFTs are asynchronous: they ask for
//! input and announce output through events, so the encoder runs on its own
//! thread and the event generator feeds the same channel as captured frames.
//! Nothing polls.

use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;

use windows::Win32::Foundation::E_NOTIMPL;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, HRESULT, Interface, Ref, Result, implement};

use crate::d3d::Shared;
use crate::h264;
use crate::ring::{Packet, ParameterSets, Ring, TICKS_PER_SECOND};

pub struct Settings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
}

pub enum Message {
    Frame { texture: Shared<ID3D11Texture2D>, pts: i64 },
    Event { kind: u32, status: HRESULT },
    Stop,
}

#[derive(Default)]
pub struct Counters {
    pub encoded: AtomicUsize,
    /// Frames handed to the encoder and not yet returned as output; bounds the
    /// NV12 texture pool.
    pub in_flight: AtomicUsize,
    pub errors: AtomicUsize,
}

/// Forwards each MFT event into the encoder's channel. Re-armed by the encoder
/// thread after it handles the event, so at most one request is outstanding.
#[implement(IMFAsyncCallback)]
struct EventPump {
    generator: Shared<IMFMediaEventGenerator>,
    sender: Sender<Message>,
}

impl IMFAsyncCallback_Impl for EventPump_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Invoke(&self, result: Ref<IMFAsyncResult>) -> Result<()> {
        let event = unsafe { self.generator.0.EndGetEvent(result.ok()?)? };
        let kind = unsafe { event.GetType()? };
        let status = unsafe { event.GetStatus()? };
        let _ = self.sender.send(Message::Event { kind, status });
        Ok(())
    }
}

pub struct EncoderInfo {
    pub name: String,
}

/// Friendly names of the hardware H.264 encoders, best first.
pub fn hardware_encoders(subtype: GUID) -> Vec<String> {
    enumerate(MFT_CATEGORY_VIDEO_ENCODER, MFVideoFormat_NV12, subtype, true)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

pub fn decoders(subtype: GUID) -> Vec<String> {
    enumerate(MFT_CATEGORY_VIDEO_DECODER, subtype, MFVideoFormat_NV12, false)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

fn enumerate(category: GUID, input: GUID, output: GUID, hardware: bool) -> Vec<(String, IMFActivate)> {
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: input };
    let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: output };
    let flags = if hardware {
        MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER
    } else {
        MFT_ENUM_FLAG_ALL | MFT_ENUM_FLAG_SORTANDFILTER
    };

    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    let mut found = Vec::new();
    unsafe {
        if MFTEnumEx(category, flags, Some(&input), Some(&output), &mut list, &mut count).is_err() {
            return found;
        }
        for slot in std::slice::from_raw_parts_mut(list, count as usize) {
            if let Some(activate) = slot.take() {
                let name = friendly_name(&activate).unwrap_or_else(|| "unnamed".into());
                found.push((name, activate));
            }
        }
        CoTaskMemFree(Some(list as *const _));
    }
    found
}

fn friendly_name(activate: &IMFActivate) -> Option<String> {
    let mut text = windows::core::PWSTR::null();
    let mut len = 0u32;
    unsafe {
        activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut text, &mut len).ok()?;
        let name = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const _));
        name
    }
}

pub fn start(
    device: &ID3D11Device,
    settings: Settings,
    ring: Arc<Ring>,
    counters: Arc<Counters>,
    sender: Sender<Message>,
    receiver: Receiver<Message>,
) -> Result<(EncoderInfo, JoinHandle<()>)> {
    let (name, activate) = enumerate(MFT_CATEGORY_VIDEO_ENCODER, MFVideoFormat_NV12, MFVideoFormat_H264, true)
        .into_iter()
        .next()
        .ok_or_else(|| windows::core::Error::new(MF_E_TOPO_CODEC_NOT_FOUND, "no hardware H.264 encoder"))?;

    let transform: IMFTransform = unsafe { activate.ActivateObject()? };
    let encoder = Encoder::configure(device, transform, &settings)?;
    let parameters = encoder.parameter_sets_from_type();
    if let Some(parameters) = parameters {
        ring.set_parameters(parameters);
    }

    let generator: IMFMediaEventGenerator = encoder.transform.cast()?;
    let pump: IMFAsyncCallback = EventPump {
        generator: Shared(generator.clone()),
        sender,
    }
    .into();

    let encoder = Shared(encoder);
    let generator = Shared(generator);
    let pump = Shared(pump);
    let handle = std::thread::Builder::new()
        .name("encoder".into())
        .spawn(move || {
            let encoder = encoder;
            let generator = generator;
            let pump = pump;
            if let Err(error) = encoder.0.run(&generator.0, &pump.0, &receiver, &ring, &counters, &settings) {
                eprintln!("encoder stopped: {error}");
            }
            // Hardware MFTs hold an encode session on the GPU until shut down;
            // dropping the last reference is not enough, and consumer NVENC
            // allows only a handful of sessions.
            unsafe {
                if let Ok(shutdown) = encoder.0.transform.cast::<IMFShutdown>() {
                    let _ = shutdown.Shutdown();
                }
            }
        })
        .expect("spawn encoder thread");

    Ok((EncoderInfo { name }, handle))
}

struct Encoder {
    transform: IMFTransform,
    codec: Option<ICodecAPI>,
    _manager: IMFDXGIDeviceManager,
}

fn frame_pair(a: u32, b: u32) -> u64 {
    ((a as u64) << 32) | b as u64
}

impl Encoder {
    fn configure(device: &ID3D11Device, transform: IMFTransform, s: &Settings) -> Result<Self> {
        unsafe {
            let attributes = transform.GetAttributes()?;
            attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);

            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.unwrap();
            manager.ResetDevice(device, token)?;
            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;

            let codec: Option<ICodecAPI> = transform.cast().ok();
            if let Some(codec) = &codec {
                let set = |key: &GUID, value: VARIANT, what: &str| {
                    if let Err(error) = codec.SetValue(key, &value) {
                        eprintln!("encoder: cannot set {what}: {error}");
                    }
                };
                // No B-frames and one output per input: the ring and the MP4
                // writer assume decode order == presentation order.
                set(&CODECAPI_AVLowLatencyMode, VARIANT::from(true), "low latency");
                // NVIDIA rejects this one; low-latency mode already implies it.
                let _ = codec.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &VARIANT::from(0u32));
                set(
                    &CODECAPI_AVEncCommonRateControlMode,
                    VARIANT::from(eAVEncCommonRateControlMode_UnconstrainedVBR.0 as u32),
                    "rate control",
                );
                set(&CODECAPI_AVEncCommonMeanBitRate, VARIANT::from(s.bitrate), "bitrate");
                set(&CODECAPI_AVEncMPVGOPSize, VARIANT::from(s.fps), "GOP size");
            }

            let output = MFCreateMediaType()?;
            output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            output.SetUINT32(&MF_MT_AVG_BITRATE, s.bitrate)?;
            output.SetUINT64(&MF_MT_FRAME_SIZE, frame_pair(s.width, s.height))?;
            output.SetUINT64(&MF_MT_FRAME_RATE, frame_pair(s.fps, 1))?;
            output.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, frame_pair(1, 1))?;
            output.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            transform.SetOutputType(0, &output, 0)?;

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, frame_pair(s.width, s.height))?;
            input.SetUINT64(&MF_MT_FRAME_RATE, frame_pair(s.fps, 1))?;
            input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, frame_pair(1, 1))?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            input.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
            input.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
            input.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
            input.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
            transform.SetInputType(0, &input, 0)?;

            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;

            Ok(Encoder { transform, codec, _manager: manager })
        }
    }

    /// Some encoders publish SPS/PPS on the output type instead of in-band.
    fn parameter_sets_from_type(&self) -> Option<ParameterSets> {
        unsafe {
            let output = self.transform.GetOutputCurrentType(0).ok()?;
            let size = output.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).ok()?;
            let mut blob = vec![0u8; size as usize];
            output.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None).ok()?;
            let unit = h264::to_avcc(&blob);
            Some(ParameterSets { sps: unit.sps?, pps: unit.pps? })
        }
    }

    fn run(
        &self,
        generator: &IMFMediaEventGenerator,
        pump: &IMFAsyncCallback,
        receiver: &Receiver<Message>,
        ring: &Ring,
        counters: &Counters,
        settings: &Settings,
    ) -> Result<()> {
        let frame_ticks = TICKS_PER_SECOND / settings.fps as i64;
        let mut queue: VecDeque<(Shared<ID3D11Texture2D>, i64)> = VecDeque::new();
        let mut requests = 0usize;
        let mut last_keyframe: Option<i64> = None;
        let mut draining = false;

        unsafe { generator.BeginGetEvent(pump, None)? };

        loop {
            // A drain that never completes must not hang the caller's join.
            let message = if draining {
                match receiver.recv_timeout(std::time::Duration::from_secs(2)) {
                    Ok(message) => message,
                    Err(_) => return Ok(()),
                }
            } else {
                match receiver.recv() {
                    Ok(message) => message,
                    Err(_) => return Ok(()),
                }
            };
            match message {
                Message::Frame { texture, pts } => {
                    if draining {
                        counters.in_flight.fetch_sub(1, Ordering::Relaxed);
                        continue;
                    }
                    queue.push_back((texture, pts));
                }
                Message::Stop => {
                    draining = true;
                    for _ in queue.drain(..) {
                        counters.in_flight.fetch_sub(1, Ordering::Relaxed);
                    }
                    unsafe { self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)? };
                }
                Message::Event { kind, status } => {
                    if let Err(error) = status.ok() {
                        eprintln!("encoder event {kind} failed: {error}");
                        counters.errors.fetch_add(1, Ordering::Relaxed);
                    }
                    #[allow(non_upper_case_globals)]
                    match MF_EVENT_TYPE(kind as i32) {
                        METransformNeedInput => requests += 1,
                        METransformHaveOutput => self.drain_output(ring, counters)?,
                        METransformDrainComplete => return Ok(()),
                        _ => {}
                    }
                    unsafe { generator.BeginGetEvent(pump, None)? };
                }
            }

            while requests > 0 && !draining {
                let Some((texture, pts)) = queue.pop_front() else { break };
                // A GOP counted in frames stretches arbitrarily when the screen
                // is idle (no frames), so keyframes are also forced by time:
                // a clip never starts more than a second early.
                let force = last_keyframe.is_none_or(|k| pts - k >= TICKS_PER_SECOND);
                if force {
                    last_keyframe = Some(pts);
                }
                if let Err(error) = self.feed(&texture.0, pts, frame_ticks, force) {
                    eprintln!("encoder input: {error}");
                    counters.errors.fetch_add(1, Ordering::Relaxed);
                    counters.in_flight.fetch_sub(1, Ordering::Relaxed);
                }
                requests -= 1;
            }
        }
    }

    fn feed(&self, texture: &ID3D11Texture2D, pts: i64, duration: i64, keyframe: bool) -> Result<()> {
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false)?;
            if let Ok(buffer2d) = buffer.cast::<IMF2DBuffer>() {
                buffer.SetCurrentLength(buffer2d.GetContiguousLength()?)?;
            }
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(duration)?;
            if keyframe {
                if let Some(codec) = &self.codec {
                    let _ = codec.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32));
                }
            }
            self.transform.ProcessInput(0, &sample, 0)
        }
    }

    fn drain_output(&self, ring: &Ring, counters: &Counters) -> Result<()> {
        unsafe {
            let info = self.transform.GetOutputStreamInfo(0)?;
            let provides = info.dwFlags
                & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32
                != 0;
            let own_sample = if provides {
                None
            } else {
                let sample = MFCreateSample()?;
                sample.AddBuffer(&MFCreateMemoryBuffer(info.cbSize.max(1 << 20))?)?;
                Some(sample)
            };

            let mut output = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(own_sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let result = self.transform.ProcessOutput(0, &mut output, &mut status);
            let sample = ManuallyDrop::take(&mut output[0].pSample);
            ManuallyDrop::drop(&mut output[0].pEvents);

            if let Err(error) = result {
                if error.code() == MF_E_TRANSFORM_STREAM_CHANGE {
                    let available = self.transform.GetOutputAvailableType(0, 0)?;
                    self.transform.SetOutputType(0, &available, 0)?;
                    return Ok(());
                }
                return Err(error);
            }
            let sample = sample.ok_or_else(|| windows::core::Error::from(MF_E_UNEXPECTED))?;

            let pts = sample.GetSampleTime()?;
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut data = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut data, None, Some(&mut len))?;
            let unit = h264::to_avcc(std::slice::from_raw_parts(data, len as usize));
            buffer.Unlock()?;

            if let (Some(sps), Some(pps)) = (unit.sps, unit.pps) {
                if !ring.has_parameters() {
                    ring.set_parameters(ParameterSets { sps, pps });
                }
            }
            let clean_point = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
            counters.in_flight.fetch_sub(1, Ordering::Relaxed);
            counters.encoded.fetch_add(1, Ordering::Relaxed);
            if !unit.avcc.is_empty() {
                ring.append(Packet { pts, keyframe: unit.keyframe || clean_point, data: unit.avcc });
            }
            Ok(())
        }
    }
}
