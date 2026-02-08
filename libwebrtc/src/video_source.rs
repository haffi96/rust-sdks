// Copyright 2025 LiveKit, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::imp::video_source as vs_imp;

#[derive(Debug, Clone)]
pub struct VideoResolution {
    pub width: u32,
    pub height: u32,
}

impl Default for VideoResolution {
    // Default to 720p
    fn default() -> Self {
        VideoResolution { width: 1280, height: 720 }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum RtcVideoSource {
    // TODO(theomonnom): Web video sources (eq. to tracks on browsers?)
    #[cfg(not(target_arch = "wasm32"))]
    Native(native::NativeVideoSource),
    #[cfg(not(target_arch = "wasm32"))]
    EncodedH264(native::EncodedH264VideoSource),
}

// TODO(theomonnom): Support enum dispatch with conditional compilation?
impl RtcVideoSource {
    pub fn video_resolution(&self) -> VideoResolution {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Native(s) => s.video_resolution(),
            #[cfg(not(target_arch = "wasm32"))]
            Self::EncodedH264(s) => s.video_resolution(),
            #[allow(unreachable_patterns)]
            _ => VideoResolution::default(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub mod native {
    use std::fmt::{Debug, Formatter};
    use std::sync::Arc;

    use cxx::SharedPtr;

    use super::*;
    use crate::video_frame::{I420Buffer, VideoBuffer, VideoFrame, VideoRotation};

    #[derive(Clone)]
    pub struct NativeVideoSource {
        pub(crate) handle: vs_imp::NativeVideoSource,
    }

    impl Debug for NativeVideoSource {
        fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
            f.debug_struct("NativeVideoSource").finish()
        }
    }

    impl Default for NativeVideoSource {
        fn default() -> Self {
            Self::new(VideoResolution::default())
        }
    }

    impl NativeVideoSource {
        pub fn new(resolution: VideoResolution) -> Self {
            Self { handle: vs_imp::NativeVideoSource::new(resolution) }
        }

        pub fn capture_frame<T: AsRef<dyn VideoBuffer>>(&self, frame: &VideoFrame<T>) {
            self.handle.capture_frame(frame)
        }

        pub fn video_resolution(&self) -> VideoResolution {
            self.handle.video_resolution()
        }
    }

    // -----------------------------------------------------------------
    // Encoded H.264 passthrough source
    // -----------------------------------------------------------------

    use webrtc_sys::passthrough_h264_encoder as pt_sys;

    /// A video source that accepts pre-encoded H.264 access units and
    /// publishes them over WebRTC without re-encoding.
    ///
    /// Internally it wraps a [`NativeVideoSource`] (which feeds dummy raw
    /// frames to keep WebRTC's encoder pipeline alive) and an
    /// [`EncodedFrameQueue`] that carries the real encoded data.  A
    /// `PassthroughH264Encoder` in the WebRTC encoder factory pops from
    /// the queue and delivers each access unit via `OnEncodedImage`.
    struct EncodedH264State {
        native_source: NativeVideoSource,
        frame_queue: SharedPtr<pt_sys::ffi::EncodedFrameQueue>,
        width: u32,
        height: u32,
        /// Cached dummy buffer reused for every dummy-frame push, avoiding a
        /// ~1.38 MB heap allocation per frame at 720p.
        dummy_buffer: parking_lot::Mutex<I420Buffer>,
    }

    #[derive(Clone)]
    pub struct EncodedH264VideoSource {
        state: Arc<EncodedH264State>,
    }

    impl Debug for EncodedH264VideoSource {
        fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
            f.debug_struct("EncodedH264VideoSource")
                .field("width", &self.state.width)
                .field("height", &self.state.height)
                .finish()
        }
    }

    impl EncodedH264VideoSource {
        pub fn new(resolution: VideoResolution) -> Self {
            let native_source = NativeVideoSource::new(resolution.clone());
            let frame_queue = pt_sys::ffi::new_encoded_frame_queue();

            // Register the queue so the encoder factory creates a
            // PassthroughH264Encoder when negotiating H.264 for this track.
            pt_sys::ffi::register_passthrough_encoder_queue(frame_queue.clone());

            let dummy_buffer = I420Buffer::new(resolution.width, resolution.height);

            Self {
                state: Arc::new(EncodedH264State {
                    native_source,
                    frame_queue,
                    width: resolution.width,
                    height: resolution.height,
                    dummy_buffer: parking_lot::Mutex::new(dummy_buffer),
                }),
            }
        }

        /// Push an Annex-B–framed H.264 access unit.  The data must contain
        /// start-code delimited NALs (e.g. `00 00 00 01 <SPS> 00 00 00 01
        /// <PPS> 00 00 00 01 <IDR>`).
        pub fn push_encoded_frame(
            &self,
            data: &[u8],
            timestamp_us: i64,
            is_keyframe: bool,
        ) {
            // Enqueue for the passthrough encoder.
            self.state.frame_queue.push(
                data,
                timestamp_us,
                is_keyframe,
                self.state.width,
                self.state.height,
            );

            // Push a dummy raw frame so that WebRTC's VideoStreamEncoder
            // calls PassthroughH264Encoder::Encode(), which pops the queue.
            // Reuse the cached buffer to avoid a ~1.38 MB allocation per frame.
            let dummy = self.state.dummy_buffer.lock();
            let frame = VideoFrame {
                rotation: VideoRotation::VideoRotation0,
                timestamp_us,
                buffer: &*dummy,
            };
            self.state.native_source.capture_frame(&frame);
        }

        pub fn video_resolution(&self) -> VideoResolution {
            VideoResolution { width: self.state.width, height: self.state.height }
        }

        /// The underlying [`NativeVideoSource`] that backs this encoded
        /// source (needed for creating a WebRTC track).
        pub fn native_source(&self) -> &NativeVideoSource {
            &self.state.native_source
        }
    }

    impl Drop for EncodedH264VideoSource {
        fn drop(&mut self) {
            if Arc::strong_count(&self.state) == 1 {
                // Clear the global passthrough queue so stale queues don't
                // hijack unrelated H.264 encoders created later.
                pt_sys::ffi::unregister_passthrough_encoder_queue();
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub mod web {}
