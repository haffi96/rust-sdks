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

#include "livekit/passthrough_h264_encoder.h"

#include <iostream>

#include "api/video/encoded_image.h"
#include "api/video_codecs/video_codec.h"
#include "modules/video_coding/codecs/h264/include/h264.h"
#include "modules/video_coding/include/video_codec_interface.h"
#include "modules/video_coding/include/video_error_codes.h"
#include "rtc_base/logging.h"
#include "webrtc-sys/src/passthrough_h264_encoder.rs.h"

namespace livekit_ffi {

// ---- EncodedFrameQueue ------------------------------------------------

void EncodedFrameQueue::push(rust::Slice<const uint8_t> data,
                             int64_t timestamp_us,
                             bool is_keyframe,
                             uint32_t width,
                             uint32_t height) const {
  std::lock_guard<std::mutex> lock(mutex_);
  EncodedFrameData frame;
  frame.data.assign(data.data(), data.data() + data.size());
  frame.timestamp_us = timestamp_us;
  frame.is_keyframe = is_keyframe;
  frame.width = width;
  frame.height = height;
  queue_.push(std::move(frame));
}

bool EncodedFrameQueue::pop(EncodedFrameData& out) const {
  std::lock_guard<std::mutex> lock(mutex_);
  if (queue_.empty())
    return false;
  out = std::move(queue_.front());
  queue_.pop();
  return true;
}

std::shared_ptr<EncodedFrameQueue> new_encoded_frame_queue() {
  return std::make_shared<EncodedFrameQueue>();
}

// ---- Global passthrough registry (one pending queue at a time) --------

static std::mutex g_passthrough_mutex;
static std::shared_ptr<EncodedFrameQueue> g_pending_passthrough_queue;

void register_passthrough_encoder_queue(
    std::shared_ptr<EncodedFrameQueue> queue) {
  std::lock_guard<std::mutex> lock(g_passthrough_mutex);
  g_pending_passthrough_queue = std::move(queue);
  std::cout << "[livekit] register_passthrough_encoder_queue\n";
}

std::shared_ptr<EncodedFrameQueue> take_pending_passthrough_queue() {
  std::lock_guard<std::mutex> lock(g_passthrough_mutex);
  return g_pending_passthrough_queue;  // return copy, don't clear
}

void unregister_passthrough_encoder_queue() {
  std::lock_guard<std::mutex> lock(g_passthrough_mutex);
  g_pending_passthrough_queue.reset();
  std::cout << "[livekit] unregister_passthrough_encoder_queue\n";
}

// Maximum frames to drain from the queue per Encode() call.  If more than
// this many frames are queued, only the newest kMaxDrain are inspected,
// and any excess older frames are silently dropped.  This bounds worst-case
// latency spikes from queue build-up.
static constexpr int kMaxDrain = 3;

// ---- PassthroughH264Encoder -------------------------------------------

PassthroughH264Encoder::PassthroughH264Encoder(
    std::shared_ptr<EncodedFrameQueue> queue)
    : queue_(std::move(queue)) {
  std::cout << "[livekit] PassthroughH264Encoder constructed\n";
}

PassthroughH264Encoder::~PassthroughH264Encoder() {
  Release();
}

int32_t PassthroughH264Encoder::InitEncode(
    const webrtc::VideoCodec* codec_settings,
    const Settings& /*settings*/) {
  if (codec_settings) {
    width_ = codec_settings->width;
    height_ = codec_settings->height;
  }
  std::cout << "[livekit] PassthroughH264Encoder InitEncode width=" << width_
            << " height=" << height_ << "\n";
  return WEBRTC_VIDEO_CODEC_OK;
}

int32_t PassthroughH264Encoder::RegisterEncodeCompleteCallback(
    webrtc::EncodedImageCallback* callback) {
  callback_ = callback;
  return WEBRTC_VIDEO_CODEC_OK;
}

int32_t PassthroughH264Encoder::Release() {
  callback_ = nullptr;
  return WEBRTC_VIDEO_CODEC_OK;
}

int32_t PassthroughH264Encoder::Encode(
    const webrtc::VideoFrame& input_frame,
    const std::vector<webrtc::VideoFrameType>* /*frame_types*/) {
  if (!callback_)
    return WEBRTC_VIDEO_CODEC_UNINITIALIZED;

  // Drain the queue and use the latest frame.  If VideoStreamEncoder
  // dropped dummy frames, the queue can build up; always deliver the
  // most recent data to avoid timestamp desync.
  // We cap the drain at kMaxDrain to bound worst-case latency spikes.
  EncodedFrameData frame;
  EncodedFrameData latest;
  bool found = false;
  int drained = 0;
  while (queue_->pop(frame)) {
    latest = std::move(frame);
    found = true;
    drained++;
  }
  if (drained > kMaxDrain) {
    RTC_LOG(LS_WARNING) << "PassthroughH264: queue overflow, drained "
                        << drained << " frames (cap=" << kMaxDrain << ")";
  }

  encode_calls_++;
  if (!found) {
    empty_calls_++;
    // Periodic diagnostics (every ~5 s at 30 fps).
    if (encode_calls_ % 150 == 0) {
      RTC_LOG(LS_INFO) << "PassthroughH264: encode_calls=" << encode_calls_
                       << " delivered=" << frames_delivered_
                       << " empty=" << empty_calls_;
      std::cout << "[livekit] PassthroughH264: encode_calls=" << encode_calls_
                << " delivered=" << frames_delivered_
                << " empty=" << empty_calls_ << "\n";
    }
    return WEBRTC_VIDEO_CODEC_OK;
  }

  if (drained > 1) {
    RTC_LOG(LS_WARNING) << "PassthroughH264: drained " << drained
                        << " queued frames, using latest";
    std::cout << "[livekit] PassthroughH264: drained " << drained
              << " queued frames, using latest\n";
  }

  webrtc::EncodedImage encoded_image;
  encoded_image._encodedWidth = latest.width;
  encoded_image._encodedHeight = latest.height;
  encoded_image.SetRtpTimestamp(input_frame.rtp_timestamp());
  encoded_image.SetSimulcastIndex(0);
  encoded_image.ntp_time_ms_ = input_frame.ntp_time_ms();
  encoded_image.capture_time_ms_ = input_frame.render_time_ms();
  encoded_image.rotation_ = input_frame.rotation();
  encoded_image.content_type_ = webrtc::VideoContentType::UNSPECIFIED;
  encoded_image.timing_.flags = webrtc::VideoSendTiming::kInvalid;
  encoded_image._frameType = latest.is_keyframe
                                 ? webrtc::VideoFrameType::kVideoFrameKey
                                 : webrtc::VideoFrameType::kVideoFrameDelta;
  encoded_image.SetEncodedData(webrtc::EncodedImageBuffer::Create(
      latest.data.data(), latest.data.size()));
  encoded_image.set_size(latest.data.size());
  encoded_image.qp_ = -1;  // unknown

  webrtc::CodecSpecificInfo codec_info = {};  // zero-initialized
  codec_info.codecType = webrtc::kVideoCodecH264;
  codec_info.codecSpecific.H264.packetization_mode =
      webrtc::H264PacketizationMode::NonInterleaved;

  auto result = callback_->OnEncodedImage(encoded_image, &codec_info);
  if (result.error != webrtc::EncodedImageCallback::Result::OK) {
    RTC_LOG(LS_ERROR) << "PassthroughH264Encoder: OnEncodedImage failed "
                      << result.error;
    return WEBRTC_VIDEO_CODEC_ERROR;
  }

  frames_delivered_++;

  // Periodic diagnostics (every ~5 s at 30 fps).
  if (frames_delivered_ % 150 == 0) {
    RTC_LOG(LS_INFO) << "PassthroughH264: delivered=" << frames_delivered_
                     << " encode_calls=" << encode_calls_
                     << " empty=" << empty_calls_
                     << " last_size=" << latest.data.size()
                     << " keyframe=" << latest.is_keyframe;
    std::cout << "[livekit] PassthroughH264: delivered=" << frames_delivered_
              << " encode_calls=" << encode_calls_ << " empty=" << empty_calls_
              << " last_size=" << latest.data.size()
              << " keyframe=" << latest.is_keyframe << "\n";
  }

  return WEBRTC_VIDEO_CODEC_OK;
}

void PassthroughH264Encoder::SetRates(
    const RateControlParameters& /*rc_parameters*/) {
  // Passthrough — no rate control to configure.
}

webrtc::VideoEncoder::EncoderInfo PassthroughH264Encoder::GetEncoderInfo()
    const {
  EncoderInfo info;
  info.supports_native_handle = false;
  info.implementation_name = "PassthroughH264Encoder";
  info.scaling_settings = ScalingSettings::kOff;
  info.has_trusted_rate_controller = true;
  info.is_hardware_accelerated = false;
  // Note: has_internal_source was removed in recent WebRTC versions.
  return info;
}

}  // namespace livekit_ffi
