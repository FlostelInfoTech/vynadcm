//! Specific Character Set (0008,0005) conversion, DCMTK `oficonv`-like.
//!
//! [`parse_specific_character_set`] maps the declared terms to a
//! [`Decoding`]; [`decode_text`] / [`encode_text`] convert whole
//! element values (code-extension state spans the value; callers
//! split delimiters in Unicode space afterwards).
//!
//! Supported terms: `ISO_IR 6`, `ISO_IR 100/101/109/110/144/127/126/
//! 138/148/166`, `ISO_IR 192`, `ISO_IR 13`, `ISO 2022 IR 13/87/159`
//! (Japanese, incl. code-extension pairs), `ISO 2022 IR 149`
//! (Korean, G1 via SO/SI), `GB18030`, `GBK`, `GB2312`. Anything else falls
//! back to Latin-1 on decode (the historical behavior) and UTF-8
//! bytes on encode (data-preserving).

use encoding_rs as enc;

/// How to convert text values.
#[derive(Clone, Debug)]
pub enum Decoding {
    /// No character set declared: Latin-1 fallback (historical).
    Default,
    /// One stateless single/multibyte charset.
    Single(Single),
    /// Japanese code extensions (stateful ISO-2022-JP stream).
    Iso2022Jp,
    /// Korean KS X 1001 in G1 (SO/SI switching + EUC-KR runs).
    Iso2022Kr,
    /// Declared but unsupported: Latin-1 fallback on decode,
    /// UTF-8 bytes on encode.
    Unsupported,
}

/// One stateless charset.
#[derive(Clone, Copy, Debug)]
pub enum Single {
    /// ASCII; bytes ≥ 0x80 become U+FFFD.
    Ascii,
    /// Byte = code point (exact ISO_IR 100).
    Latin1,
    /// JIS X 0201 Roman + Katakana (8-bit, ESC-less form).
    JisX0201,
    /// An `encoding_rs` codec.
    Codec(&'static enc::Encoding),
}

/// Parse Specific Character Set values into a [`Decoding`].
#[must_use]
pub fn parse_specific_character_set(values: &[String]) -> Decoding {
    let terms: Vec<&str> = values
        .iter()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .collect();
    if terms.is_empty() {
        return Decoding::Default;
    }
    if terms.len() == 1 {
        return match single_for(terms[0]) {
            Some(single) => Decoding::Single(single),
            None => Decoding::Unsupported,
        };
    }
    if terms.iter().all(|t| JP_CODE_EXTENSION.contains(t)) {
        return Decoding::Iso2022Jp;
    }
    if terms.contains(&"ISO 2022 IR 149")
        && terms
            .iter()
            .all(|t| *t == "ISO 2022 IR 149" || *t == "ISO 2022 IR 6" || *t == "ISO_IR 6")
    {
        return Decoding::Iso2022Kr;
    }
    Decoding::Unsupported
}

/// Terms accepted into the Japanese code-extension stream.
const JP_CODE_EXTENSION: &[&str] = &[
    "ISO_IR 6",
    "ISO_IR 13",
    "ISO 2022 IR 6",
    "ISO 2022 IR 13",
    "ISO 2022 IR 87",
    "ISO 2022 IR 159",
];

/// Map one sole charset term.
fn single_for(term: &str) -> Option<Single> {
    Some(match term {
        "ISO_IR 6" => Single::Ascii,
        "ISO_IR 100" => Single::Latin1,
        "ISO_IR 101" => Single::Codec(enc::ISO_8859_2),
        "ISO_IR 109" => Single::Codec(enc::ISO_8859_3),
        "ISO_IR 110" => Single::Codec(enc::ISO_8859_4),
        "ISO_IR 144" => Single::Codec(enc::ISO_8859_5),
        "ISO_IR 127" => Single::Codec(enc::ISO_8859_6),
        "ISO_IR 126" => Single::Codec(enc::ISO_8859_7),
        "ISO_IR 138" => Single::Codec(enc::ISO_8859_8),
        // No ISO-8859-1/9 in WHATWG; the Windows supersets differ only
        // in the 0x80-0x9F garbage range.
        "ISO_IR 148" => Single::Codec(enc::WINDOWS_1254),
        "ISO_IR 166" => Single::Codec(enc::WINDOWS_874),
        "ISO_IR 192" => Single::Codec(enc::UTF_8),
        "ISO_IR 13" | "ISO 2022 IR 13" => Single::JisX0201,
        "GB18030" => Single::Codec(enc::GB18030),
        "GBK" => Single::Codec(enc::GBK),
        // GBK is a strict superset of GB2312: decode-exact, though
        // encoding may emit GBK-only bytes for non-GB2312 input.
        "GB2312" => Single::Codec(enc::GBK),
        _ => return None,
    })
}

/// Decode one whole (padding-stripped) text value.
#[must_use]
pub fn decode_text(raw: &[u8], decoding: &Decoding) -> String {
    match decoding {
        Decoding::Default | Decoding::Unsupported => latin1(raw),
        Decoding::Single(single) => match single {
            Single::Ascii => raw
                .iter()
                .map(|b| if *b < 0x80 { *b as char } else { '\u{FFFD}' })
                .collect(),
            Single::Latin1 => latin1(raw),
            Single::JisX0201 => decode_jis_x_0201(raw),
            Single::Codec(enc) => enc.decode_without_bom_handling(raw).0.into_owned(),
        },
        Decoding::Iso2022Jp => enc::ISO_2022_JP
            .decode_without_bom_handling(raw)
            .0
            .into_owned(),
        Decoding::Iso2022Kr => decode_iso_2022_kr(raw),
    }
}

/// Encode one whole (delimiter-joined) text value. Unrepresentable
/// characters become `?`; never fails.
#[must_use]
pub fn encode_text(text: &str, decoding: &Decoding) -> Vec<u8> {
    match decoding {
        Decoding::Default | Decoding::Unsupported => text.as_bytes().to_vec(),
        Decoding::Single(single) => match single {
            Single::Ascii => text
                .bytes()
                .map(|b| if b < 0x80 { b } else { b'?' })
                .collect(),
            Single::Latin1 => {
                let mut out = Vec::with_capacity(text.len());
                for c in text.chars() {
                    out.push(u8::try_from(c as u32).unwrap_or(b'?'));
                }
                out
            }
            Single::JisX0201 => encode_jis_x_0201(text),
            Single::Codec(enc) => encode_with(enc, text),
        },
        Decoding::Iso2022Jp => encode_with(enc::ISO_2022_JP, text),
        Decoding::Iso2022Kr => encode_iso_2022_kr(text),
    }
}

fn latin1(raw: &[u8]) -> String {
    raw.iter().map(|b| *b as char).collect()
}

/// JIS X 0201 8-bit form: ASCII GL (with Yen/Sign exceptions),
/// halfwidth katakana in GR (`0xA1-0xDF` → U+FF61-FF9F, linear),
/// SS2 (`0x8E`) single-shift supported.
fn decode_jis_x_0201(raw: &[u8]) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let b = raw[i];
        if b < 0x80 {
            out.push(match b {
                0x5C => '¥',
                0x7E => '‾',
                _ => b as char,
            });
            i += 1;
        } else if b == 0x8E && i + 1 < raw.len() && (0xA1..=0xDF).contains(&raw[i + 1]) {
            out.push(char::from_u32(0xFF61 + u32::from(raw[i + 1] - 0xA1)).unwrap());
            i += 2;
        } else if (0xA1..=0xDF).contains(&b) {
            out.push(char::from_u32(0xFF61 + u32::from(b - 0xA1)).unwrap());
            i += 1;
        } else {
            out.push('\u{FFFD}');
            i += 1;
        }
    }
    out
}

fn encode_jis_x_0201(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '¥' => out.push(0x5C),
            '‾' => out.push(0x7E),
            c if (c as u32) < 0x80 => out.push(c as u8),
            '\u{FF61}'..='\u{FF9F}' => out.push(0xA1 + (c as u32 - 0xFF61) as u8),
            _ => out.push(b'?'),
        }
    }
    out
}

/// ISO-2022-KR: ASCII G0 plus KS X 1001 in G1 (`ESC $ ) C`
/// designation, SO/SI shifts); G1 runs decode as EUC-KR.
fn decode_iso_2022_kr(raw: &[u8]) -> String {
    let mut out = String::new();
    let mut g1 = false;
    let mut run: Vec<u8> = Vec::new();
    let mut i = 0;
    // G1 runs arrive GL-invoked (0x21-0x7E via SO); shift them up to
    // EUC-KR GR form. Bare GR bytes (0xA1+) are accepted as-is for
    // writers that skip SO.
    let flush = |run: &mut Vec<u8>, out: &mut String| {
        if !run.is_empty() {
            let gr: Vec<u8> = run
                .iter()
                .map(|b| {
                    if (0x21..=0x7E).contains(b) {
                        b + 0x80
                    } else {
                        *b
                    }
                })
                .collect();
            out.push_str(&enc::EUC_KR.decode_without_bom_handling(&gr).0);
            run.clear();
        }
    };
    while i < raw.len() {
        let b = raw[i];
        if b == 0x1B && raw.get(i + 1) == Some(&b'$') {
            flush(&mut run, &mut out);
            // `ESC $ ) C` designates KS X 1001; tolerate siblings.
            i += if raw.get(i + 2).is_some() && raw.get(i + 3).is_some() {
                4
            } else {
                raw.len() - i
            };
        } else if b == 0x1B {
            flush(&mut run, &mut out);
            g1 = false;
            out.push('\u{FFFD}');
            i += 1;
        } else if b == 0x0E {
            g1 = true;
            i += 1;
        } else if b == 0x0F {
            flush(&mut run, &mut out);
            g1 = false;
            i += 1;
        } else if g1 {
            run.push(b);
            i += 1;
        } else if b < 0x80 {
            out.push(b as char);
            i += 1;
        } else {
            out.push('\u{FFFD}');
            i += 1;
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Encode ISO-2022-KR: ASCII direct, KS X 1001 runs wrapped in a
/// single designation plus SO/SI, unmappable characters as `?`.
fn encode_iso_2022_kr(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut g1 = false;
    let mut designated = false;
    let close = |out: &mut Vec<u8>, g1: &mut bool| {
        if *g1 {
            out.push(0x0F);
            *g1 = false;
        }
    };
    for c in text.chars() {
        if (c as u32) < 0x80 {
            close(&mut out, &mut g1);
            out.push(c as u8);
            continue;
        }
        let mut tmp = [0u8; 4];
        let encoded = encode_with(enc::EUC_KR, c.encode_utf8(&mut tmp));
        // EUC-KR emits GR bytes; G1 runs carry them GL-invoked.
        let gl: Vec<u8> = encoded.iter().map(|b| b.wrapping_sub(0x80)).collect();
        if encoded == b"?" {
            close(&mut out, &mut g1);
            out.push(b'?');
            continue;
        }
        if !designated {
            out.extend_from_slice(b"\x1B$)C");
            designated = true;
        }
        if !g1 {
            out.push(0x0E);
            g1 = true;
        }
        out.extend_from_slice(&gl);
    }
    close(&mut out, &mut g1);
    out
}

/// Encode with an `encoding_rs` codec, mapping unmappable characters
/// to `?` (its default NCR fallback is wrong for DICOM).
fn encode_with(enc: &'static enc::Encoding, text: &str) -> Vec<u8> {
    let mut encoder = enc.new_encoder();
    let mut out = Vec::with_capacity(text.len());
    let mut rest = text;
    loop {
        let mut buf = [0u8; 64];
        // `false` until the tail call: the stream stays open across
        // buffer refills so ISO-2022-JP keeps its shift state.
        let last = rest.is_empty();
        let (result, read, written) =
            encoder.encode_from_utf8_without_replacement(rest, &mut buf, last);
        out.extend_from_slice(&buf[..written]);
        rest = &rest[read..];
        match result {
            enc::EncoderResult::InputEmpty => {
                debug_assert!(rest.is_empty());
                break;
            }
            enc::EncoderResult::OutputFull => continue,
            enc::EncoderResult::Unmappable(_) => {
                out.push(b'?');
                let skip = rest.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
                rest = &rest[skip.min(rest.len())..];
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle vectors generated with CPython 3.12 codecs
    /// (`text.encode(codec)` round-trip asserted there).
    const ORACLE: &[(&str, &str, &[u8])] = &[
        ("latin-1", "Müller", b"M\xfcller"),
        ("iso8859-2", "Łódź", &[163, 243, 100, 188]),
        ("iso8859-5", "Москва", &[188, 222, 225, 218, 210, 208]),
        ("iso8859-7", "Αθήνα", &[193, 232, 222, 237, 225]),
        ("cp1254", "Türkçe", &[84, 252, 114, 107, 231, 101]),
        ("cp874", "กรุงเทพ", &[161, 195, 216, 167, 224, 183, 190]),
        (
            "iso2022-jp",
            "山田^太郎",
            &[
                27, 36, 66, 59, 51, 69, 68, 27, 40, 66, 94, 27, 36, 66, 66, 64, 79, 58, 27, 40, 66,
            ],
        ),
        (
            "iso2022-jp",
            "Yamada^Tarou=山田^太郎=やまだ^たろう",
            &[
                89, 97, 109, 97, 100, 97, 94, 84, 97, 114, 111, 117, 61, 27, 36, 66, 59, 51, 69,
                68, 27, 40, 66, 94, 27, 36, 66, 66, 64, 79, 58, 27, 40, 66, 61, 27, 36, 66, 36,
                100, 36, 94, 36, 64, 27, 40, 66, 94, 27, 36, 66, 36, 63, 36, 109, 36, 38, 27, 40,
                66,
            ],
        ),
        (
            "iso2022-kr",
            "홍길동",
            &[27, 36, 41, 67, 14, 72, 43, 49, 102, 53, 63, 15],
        ),
        ("gb18030", "王伟", &[205, 245, 206, 176]),
        ("gbk", "王伟", &[205, 245, 206, 176]),
        ("gb2312", "王伟", &[205, 245, 206, 176]),
    ];

    fn decoding_for(codec: &str) -> Decoding {
        let terms: Vec<String> = match codec {
            "latin-1" => vec!["ISO_IR 100"],
            "iso8859-2" => vec!["ISO_IR 101"],
            "iso8859-5" => vec!["ISO_IR 144"],
            "iso8859-7" => vec!["ISO_IR 126"],
            "cp1254" => vec!["ISO_IR 148"],
            "cp874" => vec!["ISO_IR 166"],
            "iso2022-jp" => vec!["ISO 2022 IR 13", "ISO 2022 IR 87"],
            "iso2022-kr" => vec!["ISO 2022 IR 6", "ISO 2022 IR 149"],
            "gb18030" => vec!["GB18030"],
            "gbk" => vec!["GBK"],
            "gb2312" => vec!["GB2312"],
            other => panic!("no mapping for {other}"),
        }
        .into_iter()
        .map(str::to_string)
        .collect();
        parse_specific_character_set(&terms)
    }

    #[test]
    fn oracle_vectors_decode() {
        for (codec, text, bytes) in ORACLE {
            let got = decode_text(bytes, &decoding_for(codec));
            assert_eq!(got, *text, "codec {codec}");
        }
    }

    #[test]
    fn oracle_vectors_round_trip() {
        for (codec, text, bytes) in ORACLE {
            let decoding = &decoding_for(codec);
            let encoded = encode_text(text, decoding);
            // Byte-exact for stateless codecs; the ISO-2022-JP encoder
            // may choose equivalent designations, so compare strings.
            if matches!(decoding, Decoding::Iso2022Jp) {
                assert_eq!(decode_text(&encoded, decoding), *text, "codec {codec}");
            } else {
                assert_eq!(encoded, *bytes, "codec {codec}");
            }
        }
    }

    #[test]
    fn jis_x_0201_is_mechanical() {
        // Halfwidth katakana U+FF61-FF9F ↔ 0xA1-0xDF (linear);
        // Roman Yen/Sign exceptions.
        let decoding = parse_specific_character_set(&["ISO_IR 13".to_string()]);
        assert_eq!(decode_text(&[0xB1, 0xB2, 0xB3], &decoding), "ｱｲｳ");
        assert_eq!(decode_text(&[0x5C, 0x7E], &decoding), "¥‾");
        assert_eq!(decode_text(&[0x8E, 0xB1], &decoding), "ｱ"); // SS2
        assert_eq!(decode_text(&[0x80], &decoding), "�");
        assert_eq!(
            encode_text("ｱｲｳ¥‾A", &decoding),
            vec![0xB1, 0xB2, 0xB3, 0x5C, 0x7E, b'A']
        );
        assert_eq!(encode_text("é", &decoding), vec![b'?']);
    }

    #[test]
    fn ascii_and_fallbacks_behave() {
        let ascii = parse_specific_character_set(&["ISO_IR 6".to_string()]);
        assert_eq!(decode_text(b"abc", &ascii), "abc");
        assert_eq!(decode_text(&[0xE9], &ascii), "�");
        let none = parse_specific_character_set(&[]);
        assert_eq!(decode_text(&[0xE9], &none), "é"); // Latin-1 fallback
        let bogus = parse_specific_character_set(&["NO_SUCH_CHARSET".to_string()]);
        assert_eq!(decode_text(&[0xE9], &bogus), "é");
        assert_eq!(encode_text("é", &bogus), "é".as_bytes());
        // Single unsupported term in a pair poisons the whole set.
        let mixed =
            parse_specific_character_set(&["ISO 2022 IR 13".to_string(), "NOPE".to_string()]);
        assert!(matches!(mixed, Decoding::Unsupported));
    }

    #[test]
    fn unmappable_encodes_to_question_mark() {
        let latin1 = parse_specific_character_set(&["ISO_IR 100".to_string()]);
        assert_eq!(encode_text("Müller→", &latin1), b"M\xfcller?");
        let jp = parse_specific_character_set(&[
            "ISO 2022 IR 13".to_string(),
            "ISO 2022 IR 87".to_string(),
        ]);
        // Euro/Cyrillic coverage: Ж lives in JIS X 0208 (row 7),
        // so it encodes; € has no ISO-2022-JP representation.
        assert_ne!(encode_text("Ж", &jp), b"?");
        assert_eq!(encode_text("€", &jp), b"?");
    }
}
