//! Protocol 2.30 title layouts: the `title_layout_state` record that
//! follows a snapshot's `title_animation_state` records round-trips exactly,
//! carries the worst-case layout within the line limit, and refuses
//! everything outside the contract.

use core::num::NonZeroU128;

use fm_animation::Rgba;
use fm_protocol::{
    CodecError, MAX_LINE_BYTES, MAX_TITLE_CANVAS_SIDE, MAX_TITLE_ELEMENT_NAME_BYTES,
    MAX_TITLE_LAYOUT_ELEMENTS, TitleLayoutStateMessage, WireInputId, WireMessage,
    WireTitleElementId, WireTitleElementKind, WireTitleElementOutline, WireTitleLayout,
    decode_line, encode_line,
};

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn element_id(value: u128) -> WireTitleElementId {
    WireTitleElementId::new(NonZeroU128::new(value).unwrap())
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let word = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        for index in 0..=chunk.len() {
            out.push(char::from(
                ALPHABET[(word >> (18 - 6 * index) & 0x3f) as usize],
            ));
        }
    }
    out
}

/// A visible text element "Bar" at (-2, 5) sized 300x40.
fn bar() -> WireTitleElementOutline {
    WireTitleElementOutline {
        id: element_id(1),
        name: "Bar".into(),
        kind: WireTitleElementKind::Text,
        x: -2,
        y: 5,
        width: 300,
        height: 40,
        opacity: 200,
        fill: Rgba::new(1, 2, 3, 4),
        visible: true,
    }
}

/// `bar`'s exact element encoding.
const BAR_BYTES: [u8; 17] = [
    1, // id
    0, // kind: text
    1, // flags: visible
    3, b'B', b'a', b'r', // name
    3,    // x = -2, zigzag
    10,   // y = 5, zigzag
    0xAC, 0x02, // width 300
    40,   // height
    200,  // opacity
    1, 2, 3, 4, // fill
];

fn state(layout: WireTitleLayout) -> WireMessage {
    WireMessage::TitleLayoutState(TitleLayoutStateMessage {
        revision: 12,
        input: input(7),
        layout: Box::new(layout),
    })
}

fn layout(elements: Vec<WireTitleElementOutline>) -> WireTitleLayout {
    WireTitleLayout {
        width: 1_920,
        height: 1_080,
        elements,
    }
}

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

/// The record with an `elements` field holding `bytes`.
fn line_with(width: &str, height: &str, bytes: &[u8]) -> String {
    format!(
        "title_layout_state\trevision=12\tinput=7\twidth={width}\theight={height}\telements={}\n",
        base64url(bytes)
    )
}

fn elements_refused(result: &Result<WireMessage, CodecError>) {
    assert!(
        matches!(
            result,
            Err(CodecError::InvalidField {
                field: "elements",
                ..
            })
        ),
        "{result:?}"
    );
}

#[test]
fn layout_states_round_trip_in_their_exact_encoding() {
    let mut image = bar();
    image.id = element_id(u128::MAX);
    image.name = "Logo · ñ".into();
    image.kind = WireTitleElementKind::Image;
    image.visible = false;
    let line = round_trip(&state(layout(vec![bar()])));
    let mut bytes = vec![1];
    bytes.extend_from_slice(&BAR_BYTES);
    assert_eq!(line, line_with("1920", "1080", &bytes));
    // Every kind, hidden elements, wide ids, and UTF-8 names travel.
    let mut elements = vec![bar(), image];
    for (index, kind) in WireTitleElementKind::ALL.into_iter().enumerate() {
        let mut element = bar();
        element.id = element_id(10 + index as u128);
        element.kind = kind;
        element.x = i32::MIN;
        element.y = i32::MAX;
        element.width = u32::MAX;
        element.height = 0;
        element.name = String::new();
        elements.push(element);
    }
    round_trip(&state(layout(elements)));
    // A title without a template: the empty layout.
    let line = round_trip(&state(WireTitleLayout::default()));
    assert_eq!(line, line_with("0", "0", &[0]));
}

#[test]
fn the_largest_layout_fits_one_line() {
    let name = "é".repeat(MAX_TITLE_ELEMENT_NAME_BYTES / 2);
    assert_eq!(name.len(), MAX_TITLE_ELEMENT_NAME_BYTES);
    let elements = (0..MAX_TITLE_LAYOUT_ELEMENTS)
        .map(|index| WireTitleElementOutline {
            id: element_id(u128::MAX - index as u128),
            name: name.clone(),
            kind: WireTitleElementKind::ProductionClock,
            x: i32::MIN,
            y: i32::MIN,
            width: u32::MAX,
            height: u32::MAX,
            opacity: 255,
            fill: Rgba::new(255, 255, 255, 255),
            visible: true,
        })
        .collect();
    let message = state(WireTitleLayout {
        width: MAX_TITLE_CANVAS_SIDE,
        height: MAX_TITLE_CANVAS_SIDE,
        elements,
    });
    let line = round_trip(&message);
    assert!(line.len() <= MAX_LINE_BYTES, "{} bytes", line.len());
}

#[test]
fn the_encoder_refuses_layouts_outside_the_contract() {
    let refused = |layout: WireTitleLayout, field: &str| {
        let result = encode_line(&state(layout));
        assert!(
            matches!(&result, Err(CodecError::InvalidField { field: refused, .. }) if *refused == field),
            "{field}: {result:?}"
        );
    };
    // The canvas is both zero or both within the bound.
    refused(
        WireTitleLayout {
            width: 0,
            height: 1_080,
            elements: Vec::new(),
        },
        "height",
    );
    refused(
        WireTitleLayout {
            width: MAX_TITLE_CANVAS_SIDE + 1,
            height: 1_080,
            elements: Vec::new(),
        },
        "width",
    );
    refused(
        WireTitleLayout {
            width: 1_920,
            height: MAX_TITLE_CANVAS_SIDE + 1,
            elements: Vec::new(),
        },
        "height",
    );
    // Elements need a canvas.
    refused(
        WireTitleLayout {
            width: 0,
            height: 0,
            elements: vec![bar()],
        },
        "elements",
    );
    // At most the element bound, unique ids, bounded names without
    // control characters.
    let many = (0..=MAX_TITLE_LAYOUT_ELEMENTS)
        .map(|index| WireTitleElementOutline {
            id: element_id(index as u128 + 1),
            ..bar()
        })
        .collect();
    refused(layout(many), "elements");
    refused(layout(vec![bar(), bar()]), "elements");
    for name in [
        "x".repeat(MAX_TITLE_ELEMENT_NAME_BYTES + 1),
        "a\tb".to_owned(),
        "line\n".to_owned(),
    ] {
        refused(
            layout(vec![WireTitleElementOutline { name, ..bar() }]),
            "elements",
        );
    }
}

#[test]
fn the_decoder_refuses_non_canonical_or_out_of_bound_layouts() {
    let with_bar = |edit: &dyn Fn(&mut Vec<u8>)| {
        let mut bytes = vec![1];
        bytes.extend_from_slice(&BAR_BYTES);
        edit(&mut bytes);
        decode_line(&line_with("1920", "1080", &bytes))
    };
    // The exact bytes decode.
    assert!(with_bar(&|_| {}).is_ok());
    // A trailing byte, a truncated element, a non-minimal count.
    elements_refused(&with_bar(&|bytes| bytes.push(0)));
    elements_refused(&with_bar(&|bytes| {
        bytes.pop();
    }));
    elements_refused(&with_bar(&|bytes| {
        bytes.splice(0..1, [0x81, 0x00]);
    }));
    // A zero id, an unknown kind, unknown flags.
    elements_refused(&with_bar(&|bytes| bytes[1] = 0));
    elements_refused(&with_bar(&|bytes| bytes[2] = 6));
    elements_refused(&with_bar(&|bytes| bytes[3] = 2));
    // A non-minimal width varint, and a width past u32.
    elements_refused(&with_bar(&|bytes| {
        bytes.splice(10..12, [0xAC, 0x82, 0x00]);
    }));
    elements_refused(&with_bar(&|bytes| {
        bytes.splice(10..12, [0xFF, 0xFF, 0xFF, 0xFF, 0x1F]);
    }));
    // A name that is not UTF-8, one with a control character, and one past
    // the bound.
    elements_refused(&with_bar(&|bytes| bytes[5] = 0xFF));
    elements_refused(&with_bar(&|bytes| bytes[5] = b'\n'));
    elements_refused(&with_bar(&|bytes| {
        let name = vec![b'x'; MAX_TITLE_ELEMENT_NAME_BYTES + 1];
        bytes.splice(
            4..8,
            [u8::try_from(name.len()).unwrap()].into_iter().chain(name),
        );
    }));
    // A duplicate id.
    elements_refused(&with_bar(&|bytes| {
        bytes[0] = 2;
        bytes.extend_from_slice(&BAR_BYTES);
    }));
    // More elements than the bound (the count alone is refused).
    elements_refused(&decode_line(&line_with("1920", "1080", &[0xC1, 0x01])));
    // Elements without a canvas; a half canvas; canvas sides past the bound
    // or not canonical.
    let mut bytes = vec![1];
    bytes.extend_from_slice(&BAR_BYTES);
    elements_refused(&decode_line(&line_with("0", "0", &bytes)));
    for (width, height) in [
        ("0", "1080"),
        ("1920", "0"),
        ("8193", "1080"),
        ("01920", "1080"),
        ("-1", "1080"),
        ("4294967296", "1"),
    ] {
        assert!(
            decode_line(&line_with(width, height, &[0])).is_err(),
            "{width}x{height}"
        );
    }
    // Padded or non-canonical base64url, an empty field, a missing or an
    // unknown field.
    let empty = line_with("1920", "1080", &[0]);
    assert!(decode_line(&empty).is_ok());
    elements_refused(&decode_line(&empty.replace("elements=AA", "elements=AA==")));
    elements_refused(&decode_line(&empty.replace("elements=AA", "elements=AB")));
    assert!(decode_line(&empty.replace("elements=AA", "elements=")).is_err());
    assert!(decode_line(&empty.replace("\telements=AA", "")).is_err());
    assert!(decode_line(&empty.replace("\telements=", "\tz=1\telements=")).is_err());
    // A field of the largest size is refused (it holds no valid layout).
    let oversized = format!(
        "title_layout_state\trevision=12\tinput=7\twidth=1920\theight=1080\telements={}\n",
        "A".repeat(48 * 1024)
    );
    elements_refused(&decode_line(&oversized));
}
