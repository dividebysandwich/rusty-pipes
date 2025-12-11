<img width="847" height="398" alt="image" src="https://github.com/user-attachments/assets/40048de9-cbec-4a63-adbd-ccf30452f327" />

# Rusty Pipes

[![Built With Ratatui](https://ratatui.rs/built-with-ratatui/badge.svg)](https://ratatui.rs/)

Rusty Pipes is a digital organ instrument compatible with GrandOrgue sample sets. It features both graphical and text-based user interface, can be controlled via MIDI and play back MIDI files. Rusty Pipes can stream samples from disk instead of load them into RAM, though a RAM precache mode similar to GrandOrgue and Hauptwerk is available too. 

Music samples:

Bach - Praeludium in E Minor BWV 548 - Frisach organ: [[FLAC](https://playspoon.com/files/RustyPipes-Bach-Praeludium-in-e-minor-BWV548.flac)] [[OGG](https://playspoon.com/files/RustyPipes-Bach-Praeludium-in-e-minor-BWV548.ogg)]

Vierne - Organ Syphony No.2 - Allegro - Friesach organ: [[FLAC](https://playspoon.com/files/RustyPipes-Vierne-Symphony-No2-Allegro.flac)] [[OGG](https://playspoon.com/files/RustyPipes-Vierne-Symphony-No2-Allegro.ogg)]

Vierne - Organ Symphony No.2 - Cantabile - Frisach organ: [[FLAC](https://playspoon.com/files/RustyPipes-Vierne-Symphony-No2-Cantabile.flac)] [[OGG](https://playspoon.com/files/RustyPipes-Vierne-Symphony-No2-Cantabile.ogg)]

Cesar Franck - Chorale No. 3 - Frisach organ: [[FLAC](https://playspoon.com/files/RustyPipes-CesarFrank-ChoraleNo3.flac)] [[OGG](https://playspoon.com/files/RustyPipes-CesarFrank-ChoraleNo3.ogg)]


<img width="1690" height="1351" alt="image" src="https://github.com/user-attachments/assets/069fdda8-3ba5-4d9d-9b44-6a93a44d8893" />


[![Watch the video](https://img.youtube.com/vi/Ewm-s5aoeLc/0.jpg)](https://www.youtube.com/watch?v=Ewm-s5aoeLc)

(Click to play video)

## Features

* GrandOrgue Sample Set support
* Hauptwerk Sample Set support (Experimental)
* Streaming-based sample playback
* RAM based sample playback (optional)
* Tremulant (synthesized)
* Extremely low memory requirements (in streaming mode)
* Polyphony limited only by CPU power
* MIDI controlled
* Multiple MIDI input device support with flexible channel mapping
* On-the-fly configurable MIDI channel mapping
* MIDI mappings can be quickly saved into one of 10 slots and recalled
* MIDI mappings are saved to disk for each organ (by name)
* MIDI file playback
* MIDI and Audio recording of performances
* MIDI-learning for control of stops, saved to file for each organ
* Graphical and text mode (TUI) user interface

## Missing features / Limitations / Known Issues

* Streaming mode will not work well on HDDs or slow SSDs (use precaching in such cases)
* No support for split manuals and switches
* Does not work as a plugin in DAWs
* No support for percussive sound effects

*Contributions to add the above or other features are welcome!*

## Download

Downloads are available here: [https://github.com/dividebysandwich/rusty-pipes/releases](https://github.com/dividebysandwich/rusty-pipes/releases)

On Arch linux, just run ```yay -S rusty-pipes``` or ```paru -S rusty-pipes``` to install from the AUR.

## Starting

RustyPipes starts up with a configuration dialog when started without any parameters. This UI allows for the changing of all available options. All settings except for the Midi file option are saved to disk. Press "Start Rusty Pipes" to begin playing with the options you've configured.

There's two main modes of operation: MIDI control, and MIDI file playback. If you select a MIDI file to play back, MIDI control is not available.

By default, RustyPipes will stream samples from disk in real time. This works great with modern SSDs.  See the configuration options below for alternatives for PCs with traditional hard disks.

Note: RustyPipes will create pitch-corrected samples of all pipes that have a pitch factor configured on startup. It will not overwrite the original files, but create new files with the pitch shift in the filename. This step is done automatically and only done the first time a particular organ is loaded.

## Starting on Apple OSX

Apple's OS prevents users from running unsigned programs. To bypass this mechanism you have to perform the following steps:

1. Attempt to Open the App: Double-click the RustyPipes app. You will see a warning message.
2. Access System Settings: Go to Apple menu > System Settings > Privacy & Security.
3. Allow the App: Scroll down to find RustyPipes and click "Open Anyway." You may need to enter your password to confirm.

## Main User Interface

RustyPipes defaults to a graphical user interface, but it also supports a text-based console user interface.
Both work the same: You assign MIDI channels to individual stops, and you can store those in one of 10 presets and recall them at any time.
When you start the program for the first time, no stop will have any MIDI channel assigned so no sound will be heard until this is done.

<img width="1693" height="879" alt="image" src="https://github.com/user-attachments/assets/e9a0ec69-5645-4033-a47b-a01f9d25348b" />


| Input | Action |
| ----------- | ----------- |
| Cursor keys| Select organ stop / register |
| Z,S,X,D,C... | Play notes on keyboard | 
| 1,2,3...0 | Map MIDI channel to selected stop |
| Shift+F1..Shift+F10 | Save current MIDI mapping into one of 10 slots |
| F1..F10 | Load MIDI mapping of given slot |
| Shift-A | Enable all MIDI channels on selected stop |
| Shift-N | Disable all MIDI channels on selected stop |
| Shift-R | Start/Stop audio recording |
| Shift-M | Start/Stop midi recording |
| I | Set up MIDI control for selected organ stop |
| - + | Decrease/Increase gain |
| [ ] | Decrease/Increase polyphony |
| P | Panic (All notes turn off) |
| Q | Quit |

## MIDI Control of Organ Stops

Rusty Pipes supports the activation/deactivation of organ stops via MIDI events. In GUI mode, click on a Stop name to open the configuration dialog. In TUI mode, press the [i] key.

<img width="403" height="575" alt="image" src="https://github.com/user-attachments/assets/665ce46f-6554-4bbe-8a11-341b29c678bc" />

<img width="997" height="123" alt="image" src="https://github.com/user-attachments/assets/2457f2a7-46d1-4977-a2ee-79b6e63b96e5" />

Each of the 16 virtual organ MIDI channels can be assigned a MIDI event to enable or disable the current organ Stop on that channel. It does not matter which physical MIDI device or MIDI channel that event comes from. Clicking the learn button will start listening for a midi event. Learned events can be forgotten via the clear button.

These MIDI event assignments are saved to a JSON file for each organ.

## MIDI and Audio recording of performances

Live performances can be recorded as MIDI and Audio. Both recording types can be active at the same time.
The recordings are saved in the user's config directory in the "recordings" subfolder, and are named with the organ name and the current timestamp.

<img width="344" height="91" alt="image" src="https://github.com/user-attachments/assets/4d4e4ba9-4862-435a-b8d3-a3eb389ac97e" />

The record buttons are at the bottom of the window in GUI mode. In TUI mode use the keyboard shortcuts to start/stop recording.

## Configuration Options

The configuration dialog is shown on startup in both text and graphical mode, the settings are the same for both.

<img width="660" height="832" alt="image" src="https://github.com/user-attachments/assets/7f18fc49-7b7a-43fb-bd1a-5c25816ae337" />

<img width="972" height="633" alt="image" src="https://github.com/user-attachments/assets/f8f6a1e0-8cac-44ce-ba7c-40e7121eadff" />

### Organ file

This setting needs to point to a GrandOrgue ".organ" or Hauptwerk ".Organ_Hauptwerk_xml" file that is part of an organ sample set. The configuration dialog allows the user to browse for the organ they want to load.

> [!NOTE]
> Hauptwerk support is experimental and known to still have pitch issues with some sample sets
> Some Hauptwerk organs are mixed completely dry on purpose and may require the use of the convolutional reverb setting to be used.

### Audio device

Select the audio output device. Note that if your device has more than 2 channels (professional multichannel audio interfaces, etc) then only the first 2 channels will be used for the left and right channel respectively.

### MIDI file

This option allows for the selection of a MIDI file to play back. When this is used, MIDI control with input devices is not available.

Command line example: ```rusty-pipes /path/to/name.organ /path/to/file.mid```

### MIDI device

Select one or more MIDI input devices that shall be used to play. For each device you can choose between "simple" and "complex" mapping. When not selecting a MIDI file, it is assumed that some form of MIDI input device shall be used. This is the main mode of operation for RustyPipes.

The MIDI mapping can be configured in both the graphical user interface and in the text-driven TUI mode.

#### Simple MIDI mapping

<img width="564" height="274" alt="image" src="https://github.com/user-attachments/assets/3ce985e5-9ccf-4ee9-ab59-272265dc6716" />

<img width="602" height="87" alt="image" src="https://github.com/user-attachments/assets/4d8bcc99-efc9-4466-9030-d92597ce4128" />

This maps all of the input device's midi channels to a single virtual MIDI channel within RustyPipes, which can then be mapped to one or more organ stops.

#### Complex MIDI mapping

<img width="564" height="551" alt="image" src="https://github.com/user-attachments/assets/3f484cd1-779d-4c1d-a864-fb013ba38b6f" />

<img width="531" height="312" alt="image" src="https://github.com/user-attachments/assets/f9eafa27-ede2-4a3d-9f20-ff921b3a1756" />

This allows the configuration of each of the MIDI devices' channels separately, mapping them to one or more virtual MIDI channels within RustyPipes.

#### Selecting a MIDI device via command line parameter

Instead of having to manually choose the MIDI input device from a list when the program starts, you can pass the desired input device via the command line. To do this, first use ```--list-midi-devices``` to display a list, and then pass it as parameter with ```--midi-device``` like so:

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

The selected MIDI device will be used in "simple channel mapping" mode, in other words, all MIDI events from that device will be forwarded to the respective channel in RustyPipes, so MIDI channel 1 will be channel 1 in RustyPipes, MIDI channel 2 will be channel 2 in RustyPipes, etc.

Multiple MIDI input devices are not supported via the command line. Start RustyPipes normally and configure them within the GUI.

### Convolution Reverb

This option lets you select a file containing the impulse response of the desired room/church.
RustyPipes will look in the user's config directory, subdirectory "reverb", for .wav files.
If you don't know where that directory is on your system, you can click on the folder icon next to the dropdown to open a file browser at that location.

### Reverb mix

This parameter defines how much reverb vs the original sample is used. 0.0 = no reverb, 1.0 = oops it's all reverb

### Gain

The overall output gain. This is a value between 0.0 and 1.0 which is used to scale the organ sample amplitude. 0.4 is a relatively safe value, excessive gain can lead to distortion with a lot of stops playing.

### Polyphony

The maximum number of pipes playing. This actually works a bit differently that one might expect: It doesn't prevent notes from being played - but when you release a key, the release of any pipe exceeding that limit will be shortened. This is done in order of keys released, i.e. oldest voices are faded out first, so only the latest configured number of voices has their release play out in full.

### Audio Buffer

The number of frames used in the internal audio buffer. Higher numbers work on slower computers, but introduce more latency. Fast PCs can use lower values like 256. If you get distorted or choppy audio, raise that number.
When using reverb, it is recommended to use a value that is a power of 2, like 256, 512 or 1024.

### Preload frames

Loads the first few audio frames of every sample into RAM. Whenever a sample is being played, the first few milliseconds will from straight from RAM, giving your disk time to read the rest of the sample. The amount of RAM this takes depends on how many stops and samples your organ uses. On a well-sized organ the default of 16384 frames uses around 3GB of RAM.

### Precaching

Loads all samples completely into RAM on startup. Use this if your disk is too slow or you are not happy with the latency of streaming samples. Note that if you enable precaching, the "preload frames" setting becomes meaningless since all sample data is loaded into RAM.

### Convert to 16 bit

Creates 16-bit versions of all samples and stores them in the same directories as the original samples. This may be useful for slower PCs to reduce overall workload

### Original tuning

Some sample sets provide tuning information that is just *perfect*, as the required tuning was measured after sampling. However, this is not how the organ sounds in real life. This parameter ignores all tuning information as long as the pitch shift is not greater than +/- 20 cents. Any tuning greater than 20 cents is applied as normal, since this is often done for reusing samples of a different key.

### Command Line Parameters

All options can also be set via command line parameters:

```bash
Usage: rusty-pipes [OPTIONS] [ORGAN_DEFINITION]

Arguments:
  [ORGAN_DEFINITION]  Path to organ definition file (e.g., friesach/friesach.organ or friesach/OrganDefinitions/Friesach.Organ_Hauptwerk_xml)

Options:
      --midi-file <MIDI_FILE>
          Optional path to a MIDI file to play
      --precache
          Pre-cache all samples on startup (uses more memory, reduces latency)
      --convert-to-16bit
          Convert all samples to 16-bit PCM on load (saves memory, may reduce quality)
      --log-level <LEVEL>
          Set the application log level [default: info] [possible values: error, warn, info, debug, trace]
      --ir-file <IR_FILE>
          Optional path to a convolution reverb Impulse Response (IR) file
      --reverb-mix <REVERB_MIX>
          Reverb mix level (0.0 = dry, 1.0 = fully wet) [default: 0.5]
      --original-tuning
          Preserve original (de)tuning of recorded samples up to +/- 20 cents to preserve organ character
      --list-midi-devices
          List all available MIDI input devices and exit
      --midi-device <DEVICE_NAME>
          Select a MIDI device by name
      --audio-device <AUDIO_DEVICE>
          Select an audio device by name
      --audio-buffer-frames <NUM_FRAMES>
          Audio buffer size in frames (lower values reduce latency but may cause glitches) [default: 512]
      --preload-frames <NUM_PRELOAD_FRAMES>
          How many audio frames to pre-load for each pipe's samples (uses RAM, prevents buffer underruns)
      --tui
          Run in terminal UI (TUI) mode as a fallback
  -h, --help
          Print help
  -V, --version
          Print version
```

### Command line example with reverb:

This example loads the Hauptwerk sample set "GreenPositiv", using convolutional reverb at a 70% wet, 30% dry mix, and using an impulse response file from [Lars Virtual Pipe Organ Site](https://familjenpalo.se/vpo/ir-recordings/)

```rusty-pipes --ir-file BureaChurchRev1at24-48.wav --reverb-mix 0.7 /home/user/Hauptwerk/Organs/GreenPositiv/OrganDefinitions/GreenPositiv.Organ_Hauptwerk_xml```

## Compiling

```cargo build --release```

## FAQ

### Q: I get no MIDI activity when playing.

A: Make sure you selected a MIDI input device on the config screen. Also note that if you have a MIDI file selected for playing, manual MIDI input is disabled.

### Q: I get no sound. I can see MIDI activity though.

A: You need to assign MIDI channels to one or more stops. Click on the "1" next to a stop to assign it to MIDI channel 1, and you should hear something when playing notes on that channel. Note that some stops might not have pipes at the octave you're playing, so be sure to check across the octave range.

### Q: I get red "Buffer underrun" indicators while playing

A: Your system can't load and mix sample data fast enough, which causes gaps in the audio. There are several things you can tweak:

1. Increase the Audio Buffer size. This will increase latency but also give your PC more time to get the work done in time.
2. Reduce Polyphony. This will make your organ sound less "full", but greatly reduce the work your CPU has to do. You can do this while playing.
3. Increase number of Preloaded Audio Frames. This will use more RAM but instantly get the initial sample data from memory, giving your system time to load the rest of the sample from disk.
4. Enable Precaching. This will use a lot of RAM but remove the need to constantly load samples from disk.
5. Lower the sample rate. 48kHz takes more effort than 44.1kHz.
6. Switch to 16-bit mode. Not recommended since the performance gain is minimal. Internal processing is always performed in 32-bit.

### Q: My audio output crackles or cuts out

A: This can happen either due to a too high gain, or your CPU being overstressed. If a red "Buffer underrun" warning appears, then you need to reduce polyphony with the [-Key (or the controls on the GUI), or select fewer stops. If no warning appears, reduce gain.

### Q: Where can I get organ samples?

A: There's plenty of places where you can find sample sets for GrandOrgue. Some are paid, but there's free ones available too. Here's two sources:

* [Lars Virtual Pipe Organ Site](https://familjenpalo.se/vpo/)

* [Piotr Grabowsky](https://piotrgrabowski.pl/)

### Q: Where can I get convolution reverb impulse response files?

A: IR files are just .wav files that tell the system how a room reacts to a single, discrete impulse. Here are some sources:

* [Lars Virtual Pipe Organ Site](https://familjenpalo.se/vpo/ir-recordings/) has a few recordings. Be sure to take the 24 bit 48kHz mono ones.

* Go to a concert hall or church and pop a balloon or do a single loud clap.

Please be reasonable and ask for permission before popping balloons in churches.

### Q: My latency (time between keypress and sound output) is too high

1. Reduce the buffer size on the config dialog. If the audio starts to crackle, increase it a bit. 256 should work on most modern systems, fast systems may use 128. Note that if you want to use reverb, use sizes like 64, 128, 256, 512, 1024 for better performance. Arbitrary sizes work too but cause reverb to use more CPU.
2. Use a wired MIDI controller, not Bluetooth. Same for headphones, bluetooth usually adds a lot of delay. Finally, make sure you enable the "Pro Audio" mode on your device, and if you're on Linux don't use native ALSA. Instead use PulseAudio or Jack.

