# Rusty Pipes

[![Watch the video](https://img.youtube.com/vi/-APjtDI8Rdk/0.jpg)](https://www.youtube.com/watch?v=-APjtDI8Rdk)

(Click to play video)

## What is it?

Rusty Pipes is a digital organ instrument compatible with GrandOrgue sample sets. It features a TUI user interface and can be controlled via MIDI. Unlike GrandOrgue, Rusty Pipes streams samples from disk and does not load them into RAM.

## Features

* Streaming-based sample playback
* Extremely low memory requirements
* Works with very large sample sets regardless of installed system RAM
* Polyphony limited only by CPU power
* MIDI controlled
* On-the-fly configurable MIDI channel mapping
* MIDI mappings can be quickly saved into one of 10 slots and recalled
* MIDI mappings are saved to disk for each organ (by name)
* MIDI file playback

## Missing features / Limitations / Known Issues

* Will not work well on HDDs or slow SSDs (little can be done about that)
* Support for different manuals and switches
* Does not work as a plugin in DAWs

*Contributions are welcome!*

## Starting

Note: RustyPipes will create pitch-corrected samples of all pipes that have a pitch factor configured on startup. It will not overwrite the original files, but create new files with the pitch shift in the filename. This step is done automatically and only done the first time a particular organ is loaded.

```bash
Usage: rusty-pipes [OPTIONS] <ORGAN_DEFINITION> [MIDI_FILE]

Arguments:
  <ORGAN_DEFINITION>  Path to the pipe organ definition file (e.g., organs/friesach/friesach.organ)
  [MIDI_FILE]         Optional path to a MIDI file to play

Options:
      --precache           Pre-cache all samples on startup (uses more memory, reduces latency)
      --convert-to-16bit   
      --log-level <LEVEL>  Set the application log level [default: info] [possible values: error, warn, info, debug, trace]
      --ir-file <IR_FILE>        Optional path to a convolution reverb Impulse Response (IR) file
      --reverb-mix <REVERB_MIX>  Reverb mix level (0.0 = dry, 1.0 = fully wet) [default: 0.5]
  -h, --help               Print help
  -V, --version            Print version
```

### Control via MIDI input

```rusty_pipes /path/to/name.organ```

### Play MIDI file

```rusty_pipes /path/to/name.organ /path/to/file.mid```

## User Interface

<img width="1384" height="734" alt="image" src="https://github.com/user-attachments/assets/3f4ada75-ed4b-4d71-8cc4-514a655d8371" />


| Input | Action |
| ----------- | ----------- |
| Cursor keys| Select Drawbar / Register |
| Space | Toggle Drawbar / Register on or off | 
| 1,2,3...0 | Map MIDI channel to selected stop |
| Shift+F1..Shift+F10 | Save current MIDI mapping into one of 10 slots |
| F1..F10 | Load MIDI mapping of given slot |
| A | Enable all MIDI channels on selected stop |
| N | Disable all MIDI channels on selected stop |
| P | Panic (All notes turn off) |
| Q | Quit |

## Where to get organ samples

There's plenty of places where you can find sample sets for GrandOrgue. Some are paid, but there's free ones available too. Here's two sources:

* [Lars Virtual Pipe Organ Site](https://familjenpalo.se/vpo/)

* [Piotr Grabowsky](https://piotrgrabowski.pl/)

## Compiling

```cargo build --release```


