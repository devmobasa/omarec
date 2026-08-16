# Test fixtures

Pinned upstream evidence used by parsers, planners, and parity tests. Every captured or synthetic document has a sibling `provenance.json`.

## Layout

```text
gsr/captured/nvidia-6.0.0/   sanitized GPU Screen Recorder 6.0.0 probes
gsr/synthetic/               AMD, Intel, hybrid, camera, and portal documents
gsr/malformed/               parser-negative documents
omarchy/regular/             pinned regular Omarchy recorder script
omarchy/quattro/             pinned Quattro recorder script
omarchy/parity/cases.json    machine-readable argument and behavior cases
hyprland/monitors.json       transformed multi-monitor topology
```

Hardware rows for AMD, Intel, hybrid, camera, and portal remain untested until a matching machine records evidence with `scripts/hardware-qualify.sh`. Default CI uses these fixtures only.

Additional fixtures:

- `ffprobe/` bounded ffprobe JSON for video and audio-only containers
- `hardware/matrix.json` M8 topology qualification rows
- `quattro/watch-stream.ndjson` first-party watch reducer cases
