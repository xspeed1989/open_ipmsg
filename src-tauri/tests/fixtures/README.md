# Sticker fixtures

Small, locally generated FFmpeg fixtures (no external media):

- `static.webp`: 16×16 lossless WebP image.
- `animated.webp`: two 16×16 red/blue frames, looping animated WebP.
- `sticker.webm`: 16×16 VP9 WebM, two frames, no audio.

Used to verify content detection, byte-preserving import/export and send staging.

```sh
ffmpeg -f lavfi -i 'testsrc2=size=16x16:rate=1' -frames:v 1 -c:v libwebp -lossless 1 static.webp
ffmpeg -f lavfi -i 'color=c=red:size=16x16:rate=5:duration=0.2' -f lavfi -i 'color=c=blue:size=16x16:rate=5:duration=0.2' -filter_complex '[0:v][1:v]concat=n=2:v=1:a=0' -an -c:v libwebp_anim -lossless 1 -loop 0 animated.webp
ffmpeg -f lavfi -i 'testsrc2=size=16x16:rate=5:duration=0.4' -an -c:v libvpx-vp9 -b:v 0 -crf 32 -fflags +bitexact -flags:v +bitexact sticker.webm
```
