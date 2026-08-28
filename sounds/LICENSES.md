# Sound licensing

## Synthesized originals

Three built-in alert sounds — Soft beep, Double beep, Chime — are original,
synthesized programmatically by this project's code (`src/sounds.rs`). No
third-party audio is involved.

## Recorded sound pack

Nine built-in alert sounds are recordings embedded at compile time
(`assets/sounds/*.wav`, pulled in via `include_bytes!` in `src/sounds.rs`):

- Alarm clock (`alarmclock.wav`)
- Barking (`barking.wav`)
- Door knock (`doorknocking.wav`)
- Shut up (`femaleshutup.wav`)
- Yelp (`femaleyelp.wav`)
- Gong (`gong.wav`)
- Rahhh (`rahhh.wav`)
- Shh (`shh.wav`)
- Sonar ping (`sonarping.wav`)

These were supplied by the project author under CC0 (public-domain
dedication). They were processed for embedding — trimmed, peak-normalized to
−3 dBFS, faded in/out, and resampled to 48 kHz mono 16-bit PCM — but are
otherwise unmodified recordings, not derivative works requiring attribution.

A tenth candidate recording (`wilhelm.wav`) was evaluated but is **not**
shipped: its licensing is unclear, so it's excluded from the built-in set.

## Custom sounds

Custom sound files chosen by the user are played from their own files and
are not distributed with this software.
