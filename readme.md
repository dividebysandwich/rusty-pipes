<img width="672" height="287" alt="image" src="https://github.com/user-attachments/assets/72ad0326-045e-4279-8a31-b08c0f971b3f" />

# Rusty Pipes

Rusty Pipes is a digital organ instrument compatible with GrandOrgue sample sets. It features a text-based user interface, can be controlled via MIDI and play back MIDI files. Rusty Pipes can stream samples from disk instead of load them into RAM, though a RAM precache mode similar to GrandOrgue and Hauptwerk is available too.

<img width="1692" height="992" alt="image" src="https://github.com/user-attachments/assets/58c11ee5-e420-4739-ba2c-9981edd8fde4" />


[![Watch the video](https://img.youtube.com/vi/Ewm-s5aoeLc/0.jpg)](https://www.youtube.com/watch?v=Ewm-s5aoeLc)

(Click to play video)

## Features

* GrandOrgue Sample Set support
* Hauptwerk Sample Set support (Experimental)
* Streaming-based sample playback
* RAM based sample playback (optional)
* Extremely low memory requirements (in streaming mode)
* Polyphony limited only by CPU power
* MIDI controlled
* On-the-fly configurable MIDI channel mapping
* MIDI mappings can be quickly saved into one of 10 slots and recalled
* MIDI mappings are saved to disk for each organ (by name)
* MIDI file playback
* Graphical and text mode (TUI) user interface

## Missing features / Limitations / Known Issues

* Streaming mode will not work well on HDDs or slow SSDs (use precaching in such cases)
* Support for different manuals and switches
* Does not work as a plugin in DAWs

*Contributions are welcome!*

## Download

Downloads are available here: [https://github.com/dividebysandwich/rusty-pipes/releases](https://github.com/dividebysandwich/rusty-pipes/releases)

On Arch linux, just run ```yay -S rusty-pipes``` or ```paru -S rusty-pipes```

## Starting

Note: RustyPipes will create pitch-corrected samples of all pipes that have a pitch factor configured on startup. It will not overwrite the original files, but create new files with the pitch shift in the filename. This step is done automatically and only done the first time a particular organ is loaded.

```bash
Usage: rusty-pipes [OPTIONS] <ORGAN_DEFINITION> [MIDI_FILE]

Arguments:
  <ORGAN_DEFINITION>  Path to organ definition file (e.g., friesach/friesach.organ or friesach/OrganDefinitions/Friesach.Organ_Hauptwerk_xml)
  [MIDI_FILE]         Optional path to a MIDI file to play

Options:
      --precache           Pre-cache all samples on startup (uses more memory, reduces latency)
      --convert-to-16bit   
      --log-level <LEVEL>  Set the application log level [default: info] [possible values: error, warn, info, debug, trace]
      --ir-file <IR_FILE>        Optional path to a convolution reverb Impulse Response (IR) file
      --reverb-mix <REVERB_MIX>  Reverb mix level (0.0 = dry, 1.0 = fully wet) [default: 0.5]
      --original-tuning            Preserve original (de)tuning of recorded samples up to +/- 20 cents to preserve organ character
      --list-midi-devices          List all available MIDI input devices and exit
      --midi-device <DEVICE_NAME>  Select a MIDI device by name
      --audio-buffer-frames <NUM_FRAMES>  Audio buffer size (lower values reduce latency, increase in case of glitches) [default: 512]
      --tui           Run in terminal UI (TUI) mode as a fallback
  -h, --help               Print help
  -V, --version            Print version
```

### Precaching

```--precache``` - Loads all samples into RAM on startup, just like other virtual pipe organ programs. Use this if your disk is too slow or you are not happy with the latency of streaming samples.

### 16 bit conversion

```--convert-to-16bit``` - Creates 16-bit versions of all samples and stores them in the same directories as the original samples. This may be useful for slower PCs to reduce overall workload

### Convolution Reverb

```--ir-file``` - Lets you pass a .wav file containing the impulse response of the desired room/church.

```--reverb-mix``` - This parameter defines how much reverb vs the original sample is used. 0.0 = no reverb, 1.0 = oops it's all reverb

### Original tuning

```--original-tuning``` - Some sample sets provide tuning information that is just *perfect*, as the required tuning was measured after sampling. However, this is not how the organ sounds in real life. This parameter ignores all tuning information as long as the pitch shift is not greater than +/- 20 cents. Any tuning greater than 20 cents is applied as normal, since this is often done for reusing samples of a different key.

### Control via MIDI input

```rusty-pipes /path/to/name.organ```

### Select MIDI device via command line parameter

Instead of having to manually choose the MIDI input device from a list everytime the program starts, you can pass the desired input device via the command line. To do this, first use ```--list-midi-devices``` to display a list, and then pass it as parameter with ```--midi-device``` like so:

```bash
$ rusty-pipes --list-midi-devices
Available MIDI Input Devices:
  0: Midi Through:Midi Through Port-0 14:0
  1: Scarlett 2i4 USB:Scarlett 2i4 USB MIDI 1 24:0
  2: LUMI Keys Block 80FR:LUMI Keys Block 80FR Bluetooth 129:0

$ rusty-pipes --midi-device "LUMI Keys Block 80FR:LUMI Keys Block 80FR Bluetooth 129:0" Organs/Friesach/Friesach.organ
Loading organ definition...
Loading organ from: "/home/user/Organs/Friesach/Friesach.organ"
Found 335 sections in INI.
Parsing complete. Stops found: 158. Stops filtered (noise/empty): 114. Stops added: 44.
Successfully loaded organ: Friesach
Found 44 stops.
Starting audio engine...

```

> [!NOTE]
> Don't use "2: LUMI...", only use the part after "2: "

### Play MIDI file

```rusty-pipes /path/to/name.organ /path/to/file.mid```

### Loading Hauptwerk organs

> [!NOTE]
> Hauptwerk support is experimental and known to still have pitch issues with some sample sets

This example loads the Hauptwerk sample set "GreenPositiv", using convolutional reverb at a 70% wet, 30% dry mix, and using an impulse response file from [Lars Virtual Pipe Organ Site](https://familjenpalo.se/vpo/ir-recordings/)

```rusty-pipes --ir-file BureaChurchRev1at24-48.wav --reverb-mix 0.7 /home/user/Hauptwerk/Organs/GreenPositiv/OrganDefinitions/GreenPositiv.Organ_Hauptwerk_xml```

> [!NOTE]
> Some Hauptwerk organs are mixed completely dry on purpose and may require the use of the convolutional reverb setting to be used.

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

## Where to get convolution reverb impulse response files

IR files are just .wav files that tell the system how a room reacts to a single, discrete impulse.

* [Lars Virtual Pipe Organ Site](https://familjenpalo.se/vpo/ir-recordings/) has a few recordings. Be sure to take the 24 bit 48kHz mono ones.

* Go to a concert hall or church and pop a balloon or do a single loud clap.

Please be reasonable and ask for permission before popping balloons in churches.

## Compiling

```cargo build --release```


