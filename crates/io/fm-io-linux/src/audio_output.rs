//! Lists the server's ALSA output devices for the headphone (monitor)
//! output (AU-001, Protocol 2.102), and tells whether a chosen device is
//! present, so a device that was unplugged is reopened once it is back.
//!
//! [`AudioOutputLister::list`] reads `/proc/asound/cards` and
//! `/proc/asound/pcm` for every PCM with a playback stream, named
//! `plughw:CARD=<card id>,DEV=<device>`: the card id survives the card
//! being enumerated at another index (another USB port, another boot), and
//! `plughw` converts to the rate and channels the monitor bus renders at.
//! A USB card's `stream<device>` file gives the playback channel count.
//! ALSA's `default` PCM is listed first as the default output.
//!
//! Reading procfs never blocks on hardware, but a caller still lists off
//! any frame loop. The parsers are pure and portable.

use std::collections::BTreeMap;
use std::path::PathBuf;

use fm_types::audio_output::{AudioOutputDevice, AudioOutputInventory};

/// ALSA's default PCM, always offered when ALSA is present.
pub const ALSA_DEFAULT_DEVICE: &str = "default";

/// Lists the ALSA output devices.
#[derive(Clone, Debug)]
pub struct AudioOutputLister {
    /// The ALSA procfs directory, `/proc/asound` by default.
    pub asound: PathBuf,
}

impl Default for AudioOutputLister {
    fn default() -> Self {
        Self {
            asound: PathBuf::from("/proc/asound"),
        }
    }
}

/// One ALSA card from `/proc/asound/cards`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlsaCard {
    pub number: u32,
    /// The card id (`PCH`, `Headset`), what `CARD=` names.
    pub id: String,
    /// Its long name, for people.
    pub name: String,
}

/// One playback PCM from `/proc/asound/pcm`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlsaPlayback {
    pub card: u32,
    pub device: u32,
    pub name: String,
}

impl AudioOutputLister {
    /// Lists every output device, fitted for the wire
    /// ([`AudioOutputInventory::fitted`]). Without ALSA the inventory
    /// carries why, and nothing is listed.
    #[must_use]
    pub fn list(&self) -> AudioOutputInventory {
        match self.read() {
            Ok((cards, pcm)) => {
                let mut devices = vec![AudioOutputDevice {
                    device: ALSA_DEFAULT_DEVICE.to_owned(),
                    description: "System default output".to_owned(),
                    channels: None,
                    default: true,
                }];
                let cards = parse_cards(&cards);
                for playback in parse_playback(&pcm) {
                    let Some(card) = cards.get(&playback.card) else {
                        continue;
                    };
                    let stream = std::fs::read_to_string(
                        self.asound
                            .join(format!("card{}/stream{}", playback.card, playback.device)),
                    )
                    .unwrap_or_default();
                    devices.push(AudioOutputDevice {
                        device: playback_device_name(&card.id, playback.device),
                        description: if playback.name.is_empty() {
                            card.name.clone()
                        } else {
                            format!("{}: {}", card.name, playback.name)
                        },
                        channels: parse_stream_playback_channels(&stream),
                        default: false,
                    });
                }
                AudioOutputInventory {
                    supported: true,
                    devices,
                    error: None,
                }
                .fitted()
            }
            Err(error) => AudioOutputInventory {
                supported: true,
                devices: Vec::new(),
                error: Some(error),
            }
            .fitted(),
        }
    }

    /// Whether the ALSA device `device` is present now: `Some(false)` when
    /// it names a card (by id or number) or a card's PCM that is not
    /// there, `Some(true)` when it is, and `None` when it cannot tell (a
    /// plugin PCM such as `default` or `pulse`, or no `/proc/asound`).
    #[must_use]
    pub fn present(&self, device: &str) -> Option<bool> {
        let target = parse_card_device(device)?;
        let (cards, pcm) = self.read().ok()?;
        Some(target.present_in(&parse_cards(&cards), &parse_playback(&pcm)))
    }

    fn read(&self) -> Result<(String, String), String> {
        let cards = match std::fs::read_to_string(self.asound.join("cards")) {
            Ok(cards) => cards,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err("no ALSA driver is loaded (no /proc/asound)".to_owned());
            }
            Err(error) => return Err(format!("cannot read the ALSA cards: {error}")),
        };
        // A system without PCMs has no `pcm` file.
        let pcm = std::fs::read_to_string(self.asound.join("pcm")).unwrap_or_default();
        Ok((cards, pcm))
    }
}

/// The stable name of playback PCM `device` of the card with id `card`.
#[must_use]
pub fn playback_device_name(card: &str, device: u32) -> String {
    format!("plughw:CARD={card},DEV={device}")
}

/// Parses `/proc/asound/cards` into the cards by number.
#[must_use]
pub fn parse_cards(cards: &str) -> BTreeMap<u32, AlsaCard> {
    // ` 0 [PCH            ]: HDA-Intel - HDA Intel PCH`
    let mut parsed = BTreeMap::new();
    for line in cards.lines() {
        let Some((number, rest)) = line.trim_start().split_once(" [") else {
            continue;
        };
        let Ok(number) = number.parse::<u32>() else {
            continue;
        };
        let Some((id, rest)) = rest.split_once(']') else {
            continue;
        };
        let rest = rest.trim_start_matches(':');
        let name = rest.split_once(" - ").map_or(rest, |(_, name)| name).trim();
        parsed.insert(
            number,
            AlsaCard {
                number,
                id: id.trim().to_owned(),
                name: name.to_owned(),
            },
        );
    }
    parsed
}

/// Parses `/proc/asound/pcm` into its PCMs with a playback stream.
#[must_use]
pub fn parse_playback(pcm: &str) -> Vec<AlsaPlayback> {
    // `00-00: ALC892 Analog : ALC892 Analog : playback 1 : capture 1`
    let mut playback = Vec::new();
    for line in pcm.lines() {
        let parts: Vec<&str> = line.split(" : ").map(str::trim).collect();
        if !parts.iter().any(|part| part.starts_with("playback ")) {
            continue;
        }
        let Some((address, name)) = parts.first().and_then(|first| first.split_once(':')) else {
            continue;
        };
        let Some((card, device)) = address.split_once('-') else {
            continue;
        };
        let (Ok(card), Ok(device)) = (card.parse::<u32>(), device.parse::<u32>()) else {
            continue;
        };
        playback.push(AlsaPlayback {
            card,
            device,
            name: name.trim().to_owned(),
        });
    }
    playback
}

/// The widest playback channel count a USB card's `stream<n>` file lists,
/// if it lists one.
#[must_use]
pub fn parse_stream_playback_channels(stream: &str) -> Option<u8> {
    let mut in_playback = false;
    let mut widest: Option<u8> = None;
    for line in stream.lines() {
        let trimmed = line.trim();
        if !line.starts_with(char::is_whitespace) && trimmed.ends_with(':') {
            in_playback = trimmed == "Playback:";
            continue;
        }
        if in_playback
            && let Some(channels) = trimmed.strip_prefix("Channels:")
            && let Ok(channels) = channels.trim().parse::<u8>()
        {
            widest = Some(widest.map_or(channels, |widest| widest.max(channels)));
        }
    }
    widest
}

/// A card (and maybe a PCM on it) an ALSA device name points at.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CardDevice {
    card: CardRef,
    device: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CardRef {
    Number(u32),
    Id(String),
}

impl CardDevice {
    fn present_in(&self, cards: &BTreeMap<u32, AlsaCard>, playback: &[AlsaPlayback]) -> bool {
        let number = match &self.card {
            CardRef::Number(number) => cards.contains_key(number).then_some(*number),
            CardRef::Id(id) => cards
                .values()
                .find(|card| card.id == *id)
                .map(|card| card.number),
        };
        number.is_some_and(|number| {
            self.device.is_none_or(|device| {
                playback
                    .iter()
                    .any(|pcm| pcm.card == number && pcm.device == device)
            })
        })
    }
}

/// The card and PCM of a card-addressed ALSA name: `hw:`, `plughw:`,
/// `front:`, `iec958:`, `sysdefault:`, and the like, with `CARD=<id>` or a
/// bare card, then an optional `DEV=<n>` or device number. Plugin PCMs
/// without a card (`default`, `pulse`, `null`) are not card-addressed.
fn parse_card_device(device: &str) -> Option<CardDevice> {
    let (_, arguments) = device.split_once(':')?;
    let mut card = None;
    let mut pcm = None;
    for (index, argument) in arguments.split(',').enumerate() {
        let (key, value) = match argument.split_once('=') {
            Some((key, value)) => (Some(key), value),
            None => (None, argument),
        };
        match (key, index) {
            (Some("CARD"), _) | (None, 0) => {
                card = Some(match value.parse::<u32>() {
                    Ok(number) => CardRef::Number(number),
                    Err(_) => CardRef::Id(value.to_owned()),
                });
            }
            (Some("DEV"), _) | (None, 1) => pcm = Some(value.parse::<u32>().ok()?),
            _ => {}
        }
    }
    let card = card.filter(|card| !matches!(card, CardRef::Id(id) if id.is_empty()))?;
    Some(CardDevice { card, device: pcm })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARDS: &str = " 0 [PCH            ]: HDA-Intel - HDA Intel PCH\n\
                         \x20                     HDA Intel PCH at 0xf7f10000 irq 31\n\
                         \x201 [Headset        ]: USB-Audio - USB Headset\n\
                         \x20                     Logitech USB Headset at usb-0000:00:14.0-2\n";
    const PCM: &str = "00-00: ALC892 Analog : ALC892 Analog : playback 1 : capture 1\n\
                       00-02: ALC892 Alt Analog : ALC892 Alt Analog : capture 1\n\
                       00-03: HDMI 0 : HDMI 0 : playback 1\n\
                       01-00: USB Audio : USB Audio : playback 1 : capture 1\n\
                       07-00: Ghost : Ghost : playback 1\n";
    const STREAM: &str = "Logitech USB Headset at usb-0000:00:14.0-2, full speed : USB Audio\n\
                          \n\
                          Playback:\n\
                          \x20 Status: Stop\n\
                          \x20 Interface 1\n\
                          \x20   Altset 1\n\
                          \x20   Format: S16_LE\n\
                          \x20   Channels: 2\n\
                          \x20   Rates: 44100, 48000\n\
                          \n\
                          Capture:\n\
                          \x20 Interface 2\n\
                          \x20   Channels: 6\n";

    fn fake_asound(cards: Option<&str>) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "fm-io-linux-asound-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("card1")).unwrap();
        if let Some(cards) = cards {
            std::fs::write(dir.join("cards"), cards).unwrap();
            std::fs::write(dir.join("pcm"), PCM).unwrap();
            std::fs::write(dir.join("card1/stream0"), STREAM).unwrap();
        }
        dir
    }

    #[test]
    fn a_card_line_without_a_driver_dash_keeps_a_clean_name() {
        let cards = parse_cards(" 2 [Loopback       ]: Loopback\n");
        assert_eq!(cards[&2].id, "Loopback");
        assert_eq!(cards[&2].name, "Loopback");
        let cards = parse_cards(" 0 [PCH            ]: HDA-Intel - HDA Intel PCH\n");
        assert_eq!(cards[&0].name, "HDA Intel PCH");
    }

    #[test]
    fn playback_pcms_are_listed_by_card_id_with_usb_channels() {
        let asound = fake_asound(Some(CARDS));
        let inventory = AudioOutputLister {
            asound: asound.clone(),
        }
        .list();
        let listed: Vec<(&str, &str, Option<u8>, bool)> = inventory
            .devices
            .iter()
            .map(|device| {
                (
                    device.device.as_str(),
                    device.description.as_str(),
                    device.channels,
                    device.default,
                )
            })
            .collect();
        assert_eq!(
            listed,
            [
                ("default", "System default output", None, true),
                (
                    "plughw:CARD=Headset,DEV=0",
                    "USB Headset: USB Audio",
                    Some(2),
                    false
                ),
                (
                    "plughw:CARD=PCH,DEV=0",
                    "HDA Intel PCH: ALC892 Analog",
                    None,
                    false
                ),
                (
                    "plughw:CARD=PCH,DEV=3",
                    "HDA Intel PCH: HDMI 0",
                    None,
                    false
                ),
            ]
        );
        assert!(inventory.supported);
        assert_eq!(inventory.error, None);
        assert_eq!(inventory, inventory.fitted());
        let _ = std::fs::remove_dir_all(asound);
    }

    #[test]
    fn a_missing_alsa_is_reported() {
        let asound = fake_asound(None);
        let inventory = AudioOutputLister {
            asound: asound.clone(),
        }
        .list();
        assert!(inventory.supported);
        assert_eq!(inventory.devices, Vec::new());
        assert_eq!(
            inventory.error.as_deref(),
            Some("no ALSA driver is loaded (no /proc/asound)")
        );
        let lister = AudioOutputLister {
            asound: asound.clone(),
        };
        assert_eq!(lister.present("plughw:CARD=Headset,DEV=0"), None);
        let _ = std::fs::remove_dir_all(asound);
    }

    #[test]
    fn presence_follows_cards_by_id_or_number_and_their_pcms() {
        let asound = fake_asound(Some(CARDS));
        let lister = AudioOutputLister {
            asound: asound.clone(),
        };
        for (device, present) in [
            ("plughw:CARD=Headset,DEV=0", Some(true)),
            ("hw:CARD=Headset", Some(true)),
            ("hw:Headset,0", Some(true)),
            ("hw:1,0", Some(true)),
            ("front:CARD=PCH,DEV=0", Some(true)),
            ("plughw:CARD=PCH,DEV=2", Some(false)),
            ("plughw:CARD=Gone,DEV=0", Some(false)),
            ("hw:4", Some(false)),
            ("default", None),
            ("pulse", None),
            ("hw:CARD=,DEV=0", None),
            ("hw:PCH,x", None),
        ] {
            assert_eq!(lister.present(device), present, "{device}");
        }
        // The headset is unplugged: its card leaves the list.
        std::fs::write(
            asound.join("cards"),
            CARDS.lines().take(2).collect::<Vec<_>>().join("\n"),
        )
        .unwrap();
        assert_eq!(lister.present("plughw:CARD=Headset,DEV=0"), Some(false));
        assert!(lister.list().device("plughw:CARD=Headset,DEV=0").is_none());
        let _ = std::fs::remove_dir_all(asound);
    }

    #[test]
    fn usb_stream_files_give_playback_channels_only() {
        assert_eq!(parse_stream_playback_channels(STREAM), Some(2));
        assert_eq!(
            parse_stream_playback_channels("Capture:\n  Channels: 2\n"),
            None
        );
        assert_eq!(
            parse_stream_playback_channels(
                "Playback:\n  Altset 1\n    Channels: 2\n  Altset 2\n    Channels: 8\n"
            ),
            Some(8)
        );
        assert_eq!(parse_stream_playback_channels(""), None);
    }
}
