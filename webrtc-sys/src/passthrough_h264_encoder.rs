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

use crate::impl_thread_safety;

#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    unsafe extern "C++" {
        include!("livekit/passthrough_h264_encoder.h");

        type EncodedFrameQueue;

        /// Create a new encoded-frame queue for H.264 passthrough.
        fn new_encoded_frame_queue() -> SharedPtr<EncodedFrameQueue>;

        /// Push one Annex-B–framed H.264 access unit into the queue.
        fn push(
            self: &EncodedFrameQueue,
            data: &[u8],
            timestamp_us: i64,
            is_keyframe: bool,
            width: u32,
            height: u32,
        );

        /// Register the queue so the next H.264 encoder created by the
        /// VideoEncoderFactory will be a PassthroughH264Encoder backed by it.
        fn register_passthrough_encoder_queue(queue: SharedPtr<EncodedFrameQueue>);

        /// Clear the registered passthrough queue (call on cleanup).
        fn unregister_passthrough_encoder_queue();
    }
}

impl_thread_safety!(ffi::EncodedFrameQueue, Send + Sync);
