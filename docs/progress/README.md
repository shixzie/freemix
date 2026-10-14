# Progress screenshots

Captured from the native `freemix-studio` console running headless on this
development host (Linux aarch64, Xvfb + Mesa lavapipe software Vulkan). The
supervised daemon runs in simulated mode, so audio meters report
`UNAVAILABLE` and the monitor wells show replicated routing state with the
explicit `VIDEO PREVIEW DELIVERY PENDING` note rather than output video.

| File | What it shows |
|---|---|
| `studio-console-top.png` | Default 1280x800 window: header with readiness/revision, Program/Preview monitor wells, the transition row (Cut/Fade/Wipe/AlphaFade/Slide/Zoom plus eight unconfigured stinger slots), all eight overlay channels in reachable rows, and the Fade-to-Black and manual T-bar panels. |
| `studio-console-stopped.png` | Scrolled lower console: Fade-to-Black, manual T-bar, per-input audio strips, the stream roster (local HLS ladder desired-running plus an RTMP backup), the two-row **RECORDING** panel with both recorder slots `STOPPED` and their `START` actions, and the input bank with Preview/Program tally. |
| `studio-console-recording.png` | The same console with independent per-slot recording: `REC 1 STOPPED` with its `START` action beside `REC 2 RECORDING` with its `STOP` action. |

The demo show used for the captures has six simulated inputs, one scene input
with two picture-in-picture layers, an output, a two-rung local HLS ladder
target, an RTMP target, and the scene routed to Program.

## Reproducing

```sh
# A show with inputs, a scene, an output, and stream targets.
freemix-cli demo demo.freemix program.ppm
# (author inputs/scene/output/streams with the other freemix-cli commands)

Xvfb :99 -screen 0 2560x1600x24 &
DISPLAY=:99 \
VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json \
WGPU_BACKEND=vulkan \
LIBGL_ALWAYS_SOFTWARE=1 \
  freemix-studio --project demo.freemix --daemon freemixd &

DISPLAY=:99 import -window root shot.png
```

The 1280x800 default window shows the top of the console; resize the window
taller (for example with `xdotool windowsize`) and scroll to capture the lower
panels, or use the `--record-start` CLI intent to capture the recording state.
