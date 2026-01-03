<img width="847" height="398" alt="image" src="https://github.com/user-attachments/assets/0ee17867-191f-403b-8c14-ea8402e4931f" />

# Rusty Pipes

[![Built With Ratatui](https://ratatui.rs/built-with-ratatui/badge.svg)](https://ratatui.rs/)

Rusty Pipes is a digital organ instrument compatible with GrandOrgue sample sets. It features both graphical and text-based user interface, can be controlled via MIDI and play back MIDI files. Rusty Pipes can stream samples from disk instead of load them into RAM, though a RAM precache mode similar to GrandOrgue and Hauptwerk is available too. 

Music samples:

Bach - Praeludium in E Minor BWV 548 - Friesach organ: [[FLAC](https://playspoon.com/files/RustyPipes-Bach-Praeludium-in-e-minor-BWV548.flac)] [[OGG](https://playspoon.com/files/RustyPipes-Bach-Praeludium-in-e-minor-BWV548.ogg)]

Bach - Klavier Uebung BWV 669 - Strassburg organ: [[FLAC](https://playspoon.com/files/RustyPipes-Bach-Klavier_Uebung_BWV-669-Strassburg.flac)] [[OGG](https://playspoon.com/files/RustyPipes-Bach-Klavier_Uebung_BWV-669-Strassburg.ogg)]

Vierne - Organ Symphony No.2 - Allegro - Friesach organ: [[FLAC](https://playspoon.com/files/RustyPipes-Vierne-Symphony-No2-Allegro.flac)] [[OGG](https://playspoon.com/files/RustyPipes-Vierne-Symphony-No2-Allegro.ogg)]

Vierne - Organ Symphony No.2 - Scherzo - Friesach organ: [[FLAC](https://playspoon.com/files/RustyPipes-Vierne-Symphony-No2-Scherzo.flac)] [[OGG](https://playspoon.com/files/RustyPipes-Vierne-Symphony-No2-Scherzo.ogg)]

Vierne - Organ Symphony No.2 - Cantabile - Friesach organ: [[FLAC](https://playspoon.com/files/RustyPipes-Vierne-Symphony-No2-Cantabile.flac)] [[OGG](https://playspoon.com/files/RustyPipes-Vierne-Symphony-No2-Cantabile.ogg)]

Vierne - Organ Symphony No.2 - Finale  - Friesach organ: [[FLAC](https://playspoon.com/files/RustyPipes-Vierne-Symphony-No2-Finale.flac)] [[OGG](https://playspoon.com/files/RustyPipes-Vierne-Symphony-No2-Finale.ogg)]

Cesar Franck - Chorale No. 3 - Friesach organ: [[FLAC](https://playspoon.com/files/RustyPipes-CesarFrank-ChoraleNo3.flac)] [[OGG](https://playspoon.com/files/RustyPipes-CesarFrank-ChoraleNo3.ogg)]


<img width="1694" height="1384" alt="image" src="https://github.com/user-attachments/assets/cdacf422-df26-4975-bc3f-78dc1695e75e" />


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
* REST API for remote control and physical organ consoles

## Missing features / Limitations / Known Issues

* Streaming mode will not work well on HDDs or slow SSDs (use precaching in such cases)
* No support for split manuals and switches
* Does not work as a plugin in DAWs

*Contributions to add the above or other features are welcome!*

## Download

Downloads are available here: [https://github.com/dividebysandwich/rusty-pipes/releases](https://github.com/dividebysandwich/rusty-pipes/releases)

On Arch linux, just run ```yay -S rusty-pipes``` or ```paru -S rusty-pipes``` to install from the AUR.

## Manual and Documentation

Please visit [https://rusty-pipes.com](rusty-pipes.com) for a complete user guide, installation instructions and FAQ.

