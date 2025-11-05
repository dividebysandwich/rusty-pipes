# Rusty Pipes

[![Watch the video](https://img.youtube.com/vi/-APjtDI8Rdk/0.jpg)](https://www.youtube.com/watch?v=-APjtDI8Rdk)

(Click to play video)

## What is it?

Rusty Pipes is a digital organ instrument compatible with GrandOrgue sample sets. It features a TUI user interface and can be controlled via MIDI. Unlike GrandOrgue, Rusty Pipes streams samples from disk and does not load them into RAM.

## Features

* Streaming-based sample streaming
* Extremely low memory requirements (a few megabytes)
* Works with very large sample sets regardless of installed system RAM
* Polyphony limited only by CPU power
* MIDI controlled
* MIDI file playback

## Missing features / Limitations

* Will not work well on HDDs or slow SDDs
* Support for different manuals and switches
* Mapping of MIDI channels to manuals
* Crossfade from attack to release sample should only start after new sample has been loaded
* ...or better yet, the release sample could be pre-loaded as soon as the attack sample has started playing
* There's some minor clicking noises when toggling a drawbar
* Does not work as a plugin in DAWs

*Contributions are welcome!*

## Starting

Note: RustyPipes will create 16-bit versions of all samples on startup. It will not overwrite the original files, though the original files can be deleted after the first start to save on disk space.

### Control via MIDI input

```rusty_pipes /path/to/name.organ```

### Play MIDI file

```rusty_pipes /path/to/name.organ /path/to/file.mid```

## User Interface

<img width="2559" height="1600" alt="image" src="https://github.com/user-attachments/assets/7d2172f1-3d25-440f-a44d-9d38ddfbf0eb" />

| Input | Action |
| ----------- | ----------- |
| Cursor keys| Select Drawbar / Register |
| Space | Toggle Drawbar / Register on or off | 
| A | Pull (turn on) all registers |
| N | Push (turn off) all registers |
| P | Panic (All notes turn off) |
| Q | Quit |

## Where to get organ samples

There's plenty of places where you can find sample sets for GrandOrgue. Some are paid, but there's free ones available too. Here's two sources:

* [Lars Virtual Pipe Organ Site](https://familjenpalo.se/vpo/)

* [Piotr Grabowsky](https://piotrgrabowski.pl/)

## Compiling

```cargo build --release```


