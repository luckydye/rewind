//! BGRA capture texture -> NV12 encoder input, on the GPU with the D3D11 video
//! processor. Hardware H.264 encoders take NV12; this is also where scaling
//! to `--height` happens.

use std::mem::ManuallyDrop;

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{Interface, Result};

pub struct Converter {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    outputs: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    next: usize,
}

impl Converter {
    pub fn new(
        device: &ID3D11Device,
        input: (u32, u32),
        output: (u32, u32),
        fps: u32,
        pool: usize,
    ) -> Result<Self> {
        let video_device: ID3D11VideoDevice = device.cast()?;
        let context = unsafe { device.GetImmediateContext()? };
        let video_context: ID3D11VideoContext = context.cast()?;

        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL { Numerator: fps, Denominator: 1 },
            InputWidth: input.0,
            InputHeight: input.1,
            OutputFrameRate: DXGI_RATIONAL { Numerator: fps, Denominator: 1 },
            OutputWidth: output.0,
            OutputHeight: output.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_QUALITY,
        };
        let enumerator = unsafe { video_device.CreateVideoProcessorEnumerator(&desc)? };
        let processor = unsafe { video_device.CreateVideoProcessor(&enumerator, 0)? };

        unsafe {
            // Drivers otherwise apply "enhancements" (sharpening, contrast),
            // which is the last thing text needs.
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            if let Ok(context1) = video_context.cast::<ID3D11VideoContext1>() {
                context1.VideoProcessorSetStreamColorSpace1(
                    &processor,
                    0,
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                );
                context1.VideoProcessorSetOutputColorSpace1(
                    &processor,
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                );
            }
        }

        let texture_desc = D3D11_TEXTURE2D_DESC {
            Width: output.0,
            Height: output.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };

        let mut outputs = Vec::with_capacity(pool);
        for _ in 0..pool {
            let mut texture = None;
            let mut view = None;
            unsafe {
                device.CreateTexture2D(&texture_desc, None, Some(&mut texture))?;
                let texture = texture.as_ref().unwrap();
                video_device.CreateVideoProcessorOutputView(texture, &enumerator, &view_desc, Some(&mut view))?;
            }
            outputs.push((texture.unwrap(), view.unwrap()));
        }

        Ok(Converter {
            video_device,
            video_context,
            enumerator,
            processor,
            outputs,
            next: 0,
        })
    }

    /// Converts into the next texture of the pool. The caller keeps the pool
    /// from wrapping onto a texture the encoder has not consumed yet.
    pub fn convert(&mut self, input: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        let (texture, output_view) = &self.outputs[self.next];
        self.next = (self.next + 1) % self.outputs.len();

        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
            },
        };
        let mut input_view = None;
        unsafe {
            self.video_device.CreateVideoProcessorInputView(
                input,
                &self.enumerator,
                &input_desc,
                Some(&mut input_view),
            )?;
        }

        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(input_view),
            ..Default::default()
        };
        let result = unsafe {
            self.video_context.VideoProcessorBlt(
                &self.processor,
                output_view,
                0,
                std::slice::from_ref(&stream),
            )
        };
        unsafe { ManuallyDrop::drop(&mut stream.pInputSurface) };
        result?;
        Ok(texture.clone())
    }
}
