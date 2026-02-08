# Encoded H.264 Publisher

Publishes pre-encoded Annex-B H.264 access units to a LiveKit room without
re-encoding. Supports reading from a `.h264` file or a TCP stream.

## Usage

Set environment variables or pass `--url` and `--token`:

```sh
export LIVEKIT_URL="wss://your-livekit-host"
export LIVEKIT_TOKEN="your-join-token"
```

### From a file

```sh
cargo run -p encoded_h264_publisher -- file --path /path/to/video.h264 \
  --width 1280 --height 720 --fps 30 \
  --url "$LIVEKIT_URL" --token "$LIVEKIT_TOKEN"
```

### From a TCP stream

```sh
cargo run -p encoded_h264_publisher -- tcp --host 127.0.0.1 --port 5004 \
  --width 1280 --height 720 --fps 30 \
  --url "$LIVEKIT_URL" --token "$LIVEKIT_TOKEN"
```

## GStreamer TCP source

This pipeline produces Annex-B H.264 and serves it on port 5004:

```sh
gst-launch-1.0 avfvideosrc device-index=0 \
  ! videoconvert ! videorate ! videoscale \
  ! video/x-raw,format=I420,width=1280,height=720,framerate=30/1 \
  ! queue \
  ! x264enc speed-preset=ultrafast key-int-max=30 bframes=0 byte-stream=true \
  ! h264parse config-interval=1 \
  ! video/x-h264,stream-format=byte-stream \
  ! tcpserversink host=0.0.0.0 port=5004 sync=false
```

## Notes

- Input must be Annex-B with start codes (00 00 01 or 00 00 00 01).
- No pacing is applied; `--fps` is used only to generate timestamps.
