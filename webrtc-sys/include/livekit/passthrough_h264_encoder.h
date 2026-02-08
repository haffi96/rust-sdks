/*
 * Copyright 2025 LiveKit, Inc.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

#pragma once

#include <cstdint>
#include <memory>
#include <mutex>
#include <queue>
#include <vector>

#include "api/video_codecs/video_encoder.h"
#include "rust/cxx.h"

namespace livekit_ffi {

/// A single pre-encoded H.264 access unit (Annex-B framed).
struct EncodedFrameData {
  std::vector<uint8_t> data;
  int64_t timestamp_us;
  bool is_keyframe;
  uint32_t width;
  uint32_t height;
};

/// Thread-safe queue shared between the Rust FFI layer (producer) and
/// PassthroughH264Encoder (consumer).  The Rust side pushes encoded access
/// units; the encoder's Encode() pops them and delivers via OnEncodedImage.
class EncodedFrameQueue {
 public:
  /// Push an encoded access unit (Annex-B start-coded).
  void push(rust::Slice<const uint8_t> data,
            int64_t timestamp_us,
            bool is_keyframe,
            uint32_t width,
            uint32_t height) const;

  /// Pop the next frame.  Returns false if the queue is empty.
  bool pop(EncodedFrameData& out) const;

 private:
  mutable std::mutex mutex_;
  mutable std::queue<EncodedFrameData> queue_;
};

std::shared_ptr<EncodedFrameQueue> new_encoded_frame_queue();

/// Register a queue so that the next H.264 encoder created by the
/// VideoEncoderFactory will be a passthrough encoder backed by this queue.
void register_passthrough_encoder_queue(
    std::shared_ptr<EncodedFrameQueue> queue);

/// Called by VideoEncoderFactory::InternalFactory::Create() to check
/// whether a passthrough encoder should be used.  Returns nullptr when
/// no passthrough request is pending.
std::shared_ptr<EncodedFrameQueue> take_pending_passthrough_queue();

/// Clear the registered passthrough queue (call when the encoded source
/// is destroyed so stale queues don't hijack unrelated H.264 encoders).
void unregister_passthrough_encoder_queue();

// ---------------------------------------------------------------------------

/// A WebRTC VideoEncoder that does not actually encode.  Instead it
/// reads pre-encoded H.264 access units from an EncodedFrameQueue and
/// delivers them via the standard EncodedImageCallback.
///
/// The companion NativeVideoSource pushes dummy raw frames to keep
/// WebRTC's VideoStreamEncoder pipeline ticking; each Encode() call
/// pops one access unit from the queue.
class PassthroughH264Encoder : public webrtc::VideoEncoder {
 public:
  explicit PassthroughH264Encoder(std::shared_ptr<EncodedFrameQueue> queue);
  ~PassthroughH264Encoder() override;

  int32_t InitEncode(const webrtc::VideoCodec* codec_settings,
                     const Settings& settings) override;

  int32_t RegisterEncodeCompleteCallback(
      webrtc::EncodedImageCallback* callback) override;

  int32_t Release() override;

  int32_t Encode(
      const webrtc::VideoFrame& frame,
      const std::vector<webrtc::VideoFrameType>* frame_types) override;

  void SetRates(const RateControlParameters& rc_parameters) override;

  EncoderInfo GetEncoderInfo() const override;

 private:
  std::shared_ptr<EncodedFrameQueue> queue_;
  webrtc::EncodedImageCallback* callback_ = nullptr;
  uint32_t width_ = 0;
  uint32_t height_ = 0;

  // Diagnostic counters.
  uint64_t encode_calls_ = 0;
  uint64_t frames_delivered_ = 0;
  uint64_t empty_calls_ = 0;
};

}  // namespace livekit_ffi
